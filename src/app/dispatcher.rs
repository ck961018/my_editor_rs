use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::app::command::{AppCommand, Command, ContentCommand, ModeCommand};
use crate::app::command_resolver::{focused_view_id, resolve_command};
use crate::app::mode::{
    ContentModeBinding, ContentModeInstances, ModeStateSnapshot, ViewModeContext, ViewModeInstances,
};
use crate::app::view::View;
use crate::core::content_store::ContentStore;
use crate::core::input::{
    AwaitingSource, InputCoordinator, InputDecision, InputStatus, KeySequenceConfig, KeymapLayer,
    KeymapLookup, PendingSequence, continuations, longest_complete, match_sequence,
};
use crate::core::keymap::Keymap;
use crate::protocol::ids::{ContentId, SpaceId, ViewId};
use crate::protocol::key_event::KeyEvent;
use crate::protocol::revision::Revision;
use crate::protocol::scene::Scene;
use crate::protocol::viewport::ViewportCommand;

const DEFAULT_SEQUENCE_TIMEOUT: Duration = Duration::from_millis(1_000);

pub struct Dispatcher {
    global_keymap: Keymap<Command>,
    coordinator: InputCoordinator<CommandSource>,
    sequence_config: KeySequenceConfig,
    input_mode_snapshots: Vec<InputModeSnapshot>,
    view_mode_revisions: Vec<(ViewId, Revision)>,
}

pub(crate) enum InputModeSnapshot {
    Content(ContentId, ModeStateSnapshot),
    View(ViewId, ModeStateSnapshot),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DispatchCommand {
    App(AppCommand),
    Content {
        command: ContentCommand,
        content: ContentId,
    },
    ContentWithView {
        command: ContentCommand,
        view: ViewId,
        content: ContentId,
    },
    Mode {
        command: ModeCommand,
        view: ViewId,
        content: ContentId,
    },
    Viewport {
        command: ViewportCommand,
        view: ViewId,
        content: ContentId,
    },
    ContentMode {
        operation: crate::app::mode::ContentModeOperation,
        content: ContentId,
    },
    ContentModeOperations {
        operations: Vec<crate::app::mode::ContentModeOperation>,
        content: ContentId,
    },
    ViewModeOperations {
        operations: Vec<crate::app::mode::ViewModeOperation>,
        view: ViewId,
        content: ContentId,
    },
    Noop,
}

struct ContentCommandKeymap<'a>(&'a Keymap<ContentModeBinding>);

impl KeymapLookup<Command> for ContentCommandKeymap<'_> {
    fn lookup(&self, sequence: &[KeyEvent]) -> Option<(Option<Command>, bool)> {
        let node = self.0.node(sequence)?;
        Some((
            node.action().cloned().map(Command::from),
            !node.children().is_empty(),
        ))
    }

    fn extend_continuations(&self, sequence: &[KeyEvent], continuations: &mut HashSet<KeyEvent>) {
        if let Some(node) = self.0.node(sequence) {
            continuations.extend(node.children().keys().copied());
        }
    }
}

impl DispatchCommand {
    pub(crate) fn content(&self) -> Option<ContentId> {
        match self {
            Self::Content { content, .. }
            | Self::ContentWithView { content, .. }
            | Self::Mode { content, .. }
            | Self::Viewport { content, .. }
            | Self::ContentMode { content, .. }
            | Self::ContentModeOperations { content, .. }
            | Self::ViewModeOperations { content, .. } => Some(*content),
            Self::App(_) | Self::Noop => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DispatchInput {
    Normal(KeyEvent),
    Unmapped(KeyEvent),
}

impl DispatchInput {
    fn key(self) -> KeyEvent {
        match self {
            Self::Normal(key) | Self::Unmapped(key) => key,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DispatchOutcome {
    Waiting,
    Consumed,
    Replay(Vec<DispatchInput>),
    Emit {
        command: DispatchCommand,
        replay: Vec<DispatchInput>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommandSource {
    View(ViewId),
    Global,
}

impl CommandSource {
    pub(super) fn view_or(self, focused_view: ViewId) -> ViewId {
        match self {
            Self::View(view) => view,
            Self::Global => focused_view,
        }
    }
}

impl Dispatcher {
    pub fn new(global_keymap: Keymap<Command>) -> Self {
        Self::with_config(
            global_keymap,
            KeySequenceConfig::new(DEFAULT_SEQUENCE_TIMEOUT),
        )
    }

    pub fn with_config(global_keymap: Keymap<Command>, sequence_config: KeySequenceConfig) -> Self {
        Self {
            global_keymap,
            coordinator: InputCoordinator::default(),
            sequence_config,
            input_mode_snapshots: Vec::new(),
            view_mode_revisions: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn is_pending(&self) -> bool {
        self.coordinator.pending_sequence().is_some()
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "dispatcher borrows split app-owned stores"
    )]
    pub fn dispatch(
        &mut self,
        input: DispatchInput,
        now: Instant,
        focused: SpaceId,
        scene: &Scene,
        views: &HashMap<ViewId, View>,
        modes: &mut ViewModeInstances,
        content_modes: &mut ContentModeInstances,
        contents: &ContentStore,
    ) -> DispatchOutcome {
        self.input_mode_snapshots.clear();
        self.view_mode_revisions.clear();
        let focused_view = match focused_view_id(scene, focused) {
            Some(view) => view,
            None => return DispatchOutcome::Consumed,
        };
        let key = input.key();

        for source in self.coordinator.sources_top_down() {
            match source {
                AwaitingSource::Context(source) => {
                    if let Some(snapshot) =
                        snapshot_context_mode(views, modes, content_modes, source)
                    {
                        self.record_input_mode_snapshot(snapshot);
                    }
                    if let CommandSource::View(view) = source
                        && let Some(view_data) = views.get(&view)
                        && !content_modes.is_active(view_data.content())
                        && modes.is_active(view)
                    {
                        self.record_view_mode_revision(view, view_data.revision());
                    }
                    let (decision, status) =
                        capture_context(views, modes, content_modes, contents, source, key);
                    let handled = !matches!(decision, InputDecision::Pass);
                    self.coordinator.sync_context(source, status, handled, now);
                    match decision {
                        InputDecision::Pass => continue,
                        InputDecision::Consumed => return DispatchOutcome::Consumed,
                        InputDecision::Emit(action) => {
                            let Some(command) =
                                resolve_command(action, source, focused_view, views)
                            else {
                                return DispatchOutcome::Consumed;
                            };
                            return DispatchOutcome::Emit {
                                command,
                                replay: Vec::new(),
                            };
                        }
                    }
                }
                AwaitingSource::KeySequence if matches!(input, DispatchInput::Normal(_)) => {
                    return self.continue_sequence(
                        key,
                        now,
                        focused_view,
                        views,
                        modes,
                        content_modes,
                        contents,
                    );
                }
                AwaitingSource::KeySequence => continue,
            }
        }

        match input {
            DispatchInput::Normal(key) => self.start_sequence(
                key,
                now,
                focused_view,
                views,
                modes,
                content_modes,
                contents,
            ),
            DispatchInput::Unmapped(key) => {
                fallback(key, focused_view, views, modes, content_modes, contents)
            }
        }
    }

    pub fn sync_view(&mut self, view: ViewId, status: InputStatus, handled: bool, now: Instant) {
        self.coordinator
            .sync_context(CommandSource::View(view), status, handled, now);
    }

    pub fn take_view_mode_revisions(&mut self) -> Vec<(ViewId, Revision)> {
        std::mem::take(&mut self.view_mode_revisions)
    }

    pub fn take_input_mode_snapshots(&mut self) -> Vec<InputModeSnapshot> {
        std::mem::take(&mut self.input_mode_snapshots)
    }

    fn record_input_mode_snapshot(&mut self, snapshot: InputModeSnapshot) {
        let already_recorded =
            self.input_mode_snapshots
                .iter()
                .any(|current| match (current, &snapshot) {
                    (
                        InputModeSnapshot::Content(current, _),
                        InputModeSnapshot::Content(next, _),
                    ) => current == next,
                    (InputModeSnapshot::View(current, _), InputModeSnapshot::View(next, _)) => {
                        current == next
                    }
                    _ => false,
                });
        if !already_recorded {
            self.input_mode_snapshots.push(snapshot);
        }
    }

    fn record_view_mode_revision(&mut self, view: ViewId, revision: Revision) {
        if !self
            .view_mode_revisions
            .iter()
            .any(|(candidate, _)| *candidate == view)
        {
            self.view_mode_revisions.push((view, revision));
        }
    }

    pub fn invalidate_view(
        &mut self,
        view: ViewId,
        view_data: &View,
        content: ContentId,
        modes: &mut ViewModeInstances,
        content_modes: &mut ContentModeInstances,
        contents: &ContentStore,
    ) -> bool {
        let cancelled_view_mode = !content_modes.is_active(content) && modes.is_active(view);
        if cancelled_view_mode {
            let context = ViewModeContext::new(view, view_data, contents);
            modes.cancel(view, &context);
        }
        self.invalidate_view_binding(view);
        cancelled_view_mode
    }

    pub fn invalidate_view_mode(
        &mut self,
        view: ViewId,
        view_data: &View,
        modes: &mut ViewModeInstances,
        contents: &ContentStore,
    ) -> bool {
        let removed_view_mode = modes.is_active(view);
        if removed_view_mode {
            let context = ViewModeContext::new(view, view_data, contents);
            modes.cancel(view, &context);
        }
        self.invalidate_view_binding(view);
        removed_view_mode
    }

    fn invalidate_view_binding(&mut self, view: ViewId) {
        self.coordinator.remove_context(&CommandSource::View(view));
        if self
            .coordinator
            .pending_sequence()
            .is_some_and(|pending| pending.owner == CommandSource::View(view))
        {
            self.coordinator.discard_sequence();
        }
    }

    pub fn next_deadline(
        &self,
        views: &HashMap<ViewId, View>,
        modes: &ViewModeInstances,
        content_modes: &ContentModeInstances,
        contents: &ContentStore,
    ) -> Option<Instant> {
        self.coordinator
            .next_deadline(|source| context_status(views, modes, content_modes, contents, *source))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "dispatcher borrows split app-owned stores"
    )]
    pub fn dispatch_timeout(
        &mut self,
        now: Instant,
        focused: SpaceId,
        scene: &Scene,
        views: &HashMap<ViewId, View>,
        modes: &mut ViewModeInstances,
        content_modes: &mut ContentModeInstances,
        contents: &crate::core::content_store::ContentStore,
    ) -> DispatchOutcome {
        self.input_mode_snapshots.clear();
        self.view_mode_revisions.clear();
        let focused_view = match focused_view_id(scene, focused) {
            Some(view) => view,
            None => return DispatchOutcome::Consumed,
        };
        let Some(due) = self.coordinator.next_due(now, |source| {
            context_status(views, modes, content_modes, contents, *source)
        }) else {
            return DispatchOutcome::Waiting;
        };
        match due {
            AwaitingSource::Context(source) => {
                if let CommandSource::View(view) = source {
                    let content = views.get(&view).map(View::content);
                    let command = if let Some(content) = content
                        && content_modes.is_active(content)
                    {
                        if let Some(snapshot) = content_modes.snapshot(content) {
                            self.record_input_mode_snapshot(InputModeSnapshot::Content(
                                content, snapshot,
                            ));
                        }
                        Some(DispatchCommand::ContentModeOperations {
                            operations: content_modes.on_timeout(content, contents),
                            content,
                        })
                    } else if let Some(content) = content {
                        if let Some(snapshot) = modes.snapshot(view) {
                            self.record_input_mode_snapshot(InputModeSnapshot::View(
                                view, snapshot,
                            ));
                        }
                        if modes.is_active(view) {
                            self.record_view_mode_revision(view, views[&view].revision());
                        }
                        let context = crate::app::mode::ViewModeContext::new(
                            view,
                            views.get(&view).expect("timeout view exists"),
                            contents,
                        );
                        Some(DispatchCommand::ViewModeOperations {
                            operations: modes.on_timeout(view, &context),
                            view,
                            content,
                        })
                    } else {
                        None
                    };
                    let status = context_status(views, modes, content_modes, contents, source);
                    self.coordinator.sync_context(source, status, true, now);
                    if let Some(command) = command {
                        return DispatchOutcome::Emit {
                            command,
                            replay: Vec::new(),
                        };
                    }
                } else {
                    self.coordinator.remove_context(&source);
                }
                DispatchOutcome::Consumed
            }
            AwaitingSource::KeySequence => {
                let pending = self
                    .coordinator
                    .take_sequence()
                    .expect("due sequence exists");
                self.resolve_aborted_sequence(
                    pending.keys,
                    None,
                    focused_view,
                    views,
                    modes,
                    content_modes,
                    contents,
                )
            }
        }
    }

    #[expect(
        dead_code,
        reason = "pending continuations are retained for key-hint frontends"
    )]
    pub(super) fn pending_continuations(
        &self,
        focused: SpaceId,
        scene: &Scene,
        views: &HashMap<ViewId, View>,
        modes: &ViewModeInstances,
        content_modes: &ContentModeInstances,
        contents: &ContentStore,
    ) -> HashSet<KeyEvent> {
        let Some(view) = focused_view_id(scene, focused) else {
            return HashSet::new();
        };
        let Some(pending) = self.coordinator.pending_sequence() else {
            return HashSet::new();
        };
        self.with_layers(view, views, modes, content_modes, contents, |layers| {
            continuations(layers, &pending.keys)
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "sequence resolution uses the same split runtime context"
    )]
    fn start_sequence(
        &mut self,
        key: KeyEvent,
        now: Instant,
        focused_view: ViewId,
        views: &HashMap<ViewId, View>,
        modes: &ViewModeInstances,
        content_modes: &ContentModeInstances,
        contents: &ContentStore,
    ) -> DispatchOutcome {
        let matched = self.with_layers(
            focused_view,
            views,
            modes,
            content_modes,
            contents,
            |layers| match_sequence(layers, &[key]),
        );
        let Some(matched) = matched else {
            return fallback(key, focused_view, views, modes, content_modes, contents);
        };
        if matched.has_children {
            let keys = vec![key];
            self.coordinator.push_sequence(PendingSequence {
                owner: CommandSource::View(focused_view),
                deadline: self.sequence_config.deadline(&keys, now),
                keys,
            });
            return DispatchOutcome::Waiting;
        }
        let Some(resolved) = matched.exact else {
            return fallback(key, focused_view, views, modes, content_modes, contents);
        };
        let Some(command) = resolve_command(resolved.action, resolved.source, focused_view, views)
        else {
            return DispatchOutcome::Consumed;
        };
        DispatchOutcome::Emit {
            command,
            replay: Vec::new(),
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "sequence resolution uses the same split runtime context"
    )]
    fn continue_sequence(
        &mut self,
        key: KeyEvent,
        now: Instant,
        focused_view: ViewId,
        views: &HashMap<ViewId, View>,
        modes: &ViewModeInstances,
        content_modes: &ContentModeInstances,
        contents: &ContentStore,
    ) -> DispatchOutcome {
        let mut keys = self
            .coordinator
            .pending_sequence()
            .expect("sequence source has pending data")
            .keys
            .clone();
        keys.push(key);
        let matched = self.with_layers(
            focused_view,
            views,
            modes,
            content_modes,
            contents,
            |layers| match_sequence(layers, &keys),
        );
        match matched {
            Some(matched) if matched.has_children => {
                let deadline = self.sequence_config.deadline(&keys, now);
                let pending = self
                    .coordinator
                    .pending_sequence_mut()
                    .expect("continuing sequence exists");
                pending.keys = keys;
                pending.deadline = deadline;
                DispatchOutcome::Waiting
            }
            Some(matched) => {
                let _ = self.coordinator.take_sequence();
                let Some(resolved) = matched.exact else {
                    return DispatchOutcome::Consumed;
                };
                let Some(command) =
                    resolve_command(resolved.action, resolved.source, focused_view, views)
                else {
                    return DispatchOutcome::Consumed;
                };
                DispatchOutcome::Emit {
                    command,
                    replay: Vec::new(),
                }
            }
            None => {
                let pending = self
                    .coordinator
                    .take_sequence()
                    .expect("mismatched sequence exists");
                self.resolve_aborted_sequence(
                    pending.keys,
                    Some(DispatchInput::Normal(key)),
                    focused_view,
                    views,
                    modes,
                    content_modes,
                    contents,
                )
            }
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "sequence resolution uses the same split runtime context"
    )]
    fn resolve_aborted_sequence(
        &self,
        keys: Vec<KeyEvent>,
        trailing: Option<DispatchInput>,
        focused_view: ViewId,
        views: &HashMap<ViewId, View>,
        modes: &ViewModeInstances,
        content_modes: &ContentModeInstances,
        contents: &ContentStore,
    ) -> DispatchOutcome {
        let complete = self.with_layers(
            focused_view,
            views,
            modes,
            content_modes,
            contents,
            |layers| longest_complete(layers, &keys),
        );
        match complete {
            Some(complete) => {
                let mut replay: Vec<_> = keys[complete.consumed..]
                    .iter()
                    .copied()
                    .map(DispatchInput::Normal)
                    .collect();
                replay.extend(trailing);
                let Some(command) = resolve_command(
                    complete.resolved.action,
                    complete.resolved.source,
                    focused_view,
                    views,
                ) else {
                    return DispatchOutcome::Replay(replay);
                };
                DispatchOutcome::Emit { command, replay }
            }
            None => {
                let mut replay: Vec<_> = keys.into_iter().map(DispatchInput::Unmapped).collect();
                replay.extend(trailing);
                DispatchOutcome::Replay(replay)
            }
        }
    }

    fn with_layers<R>(
        &self,
        focused_view: ViewId,
        views: &HashMap<ViewId, View>,
        modes: &ViewModeInstances,
        content_modes: &ContentModeInstances,
        contents: &ContentStore,
        query: impl FnOnce(&[KeymapLayer<'_, Command, CommandSource>]) -> R,
    ) -> R {
        let mut layers = Vec::with_capacity(2);
        let content_keymap = views
            .get(&focused_view)
            .and_then(|view| content_modes.keymap(view.content(), contents))
            .map(ContentCommandKeymap);
        let view_keymap = content_keymap
            .is_none()
            .then(|| {
                let view = views.get(&focused_view)?;
                let context = ViewModeContext::new(focused_view, view, contents);
                modes.keymap(focused_view, &context)
            })
            .flatten();
        if let Some(keymap) = content_keymap.as_ref() {
            layers.push(KeymapLayer {
                source: CommandSource::View(focused_view),
                keymap,
            });
        } else if let Some(keymap) = view_keymap {
            layers.push(KeymapLayer {
                source: CommandSource::View(focused_view),
                keymap,
            });
        }
        layers.push(KeymapLayer {
            source: CommandSource::Global,
            keymap: &self.global_keymap,
        });
        query(&layers)
    }
}

fn snapshot_context_mode(
    views: &HashMap<ViewId, View>,
    modes: &ViewModeInstances,
    content_modes: &ContentModeInstances,
    source: CommandSource,
) -> Option<InputModeSnapshot> {
    let CommandSource::View(view) = source else {
        return None;
    };
    let content = views.get(&view)?.content();
    if content_modes.is_active(content) {
        content_modes
            .snapshot(content)
            .map(|snapshot| InputModeSnapshot::Content(content, snapshot))
    } else {
        modes
            .snapshot(view)
            .map(|snapshot| InputModeSnapshot::View(view, snapshot))
    }
}

fn capture_context(
    views: &HashMap<ViewId, View>,
    modes: &mut ViewModeInstances,
    content_modes: &mut ContentModeInstances,
    contents: &ContentStore,
    source: CommandSource,
    key: KeyEvent,
) -> (InputDecision<Command>, InputStatus) {
    match source {
        CommandSource::View(view) => {
            let Some(content) = views.get(&view).map(View::content) else {
                return (InputDecision::Pass, InputStatus::Ready);
            };
            if content_modes.is_active(content) {
                let decision = content_modes.capture(content, contents, key);
                (decision, content_modes.input_status(content, contents))
            } else {
                let context = ViewModeContext::new(
                    view,
                    views.get(&view).expect("captured view exists"),
                    contents,
                );
                let decision = modes.capture(view, &context, key);
                (decision, modes.input_status(view, &context))
            }
        }
        CommandSource::Global => (InputDecision::Pass, InputStatus::Ready),
    }
}

fn context_status(
    views: &HashMap<ViewId, View>,
    modes: &ViewModeInstances,
    content_modes: &ContentModeInstances,
    contents: &ContentStore,
    source: CommandSource,
) -> InputStatus {
    match source {
        CommandSource::View(view) => views.get(&view).map_or(InputStatus::Ready, |view_data| {
            let content = view_data.content();
            if content_modes.is_active(content) {
                content_modes.input_status(content, contents)
            } else {
                let context = ViewModeContext::new(view, view_data, contents);
                modes.input_status(view, &context)
            }
        }),
        CommandSource::Global => InputStatus::Ready,
    }
}

fn fallback(
    key: KeyEvent,
    focused_view: ViewId,
    views: &HashMap<ViewId, View>,
    modes: &ViewModeInstances,
    content_modes: &ContentModeInstances,
    contents: &ContentStore,
) -> DispatchOutcome {
    let Some(content) = views.get(&focused_view).map(View::content) else {
        return DispatchOutcome::Consumed;
    };
    let action = if content_modes.is_active(content) {
        content_modes.fallback(content, contents, key)
    } else {
        let context = ViewModeContext::new(
            focused_view,
            views.get(&focused_view).expect("focused view exists"),
            contents,
        );
        modes.fallback(focused_view, &context, key)
    };
    let Some(action) = action else {
        return DispatchOutcome::Consumed;
    };
    let Some(command) = resolve_command(
        action,
        CommandSource::View(focused_view),
        focused_view,
        views,
    ) else {
        return DispatchOutcome::Consumed;
    };
    DispatchOutcome::Emit {
        command,
        replay: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::command_resolver::default_global_keymap;
    use crate::app::mode::{ContentModeInstances, ModeRegistry, ViewModeInstances};
    use crate::app::scene_model::{SceneBuilder, build_editor_scene};
    use crate::core::buffer::Buffer;
    use crate::core::content::Content;
    use crate::core::content_store::ContentStore;
    use crate::core::mode_name::ModeName;
    use crate::core::status_bar::StatusBar;
    use crate::protocol::ids::ContentId;
    use crate::protocol::viewport::{
        ViewportCursorBehavior, ViewportMoveAmount, ViewportMoveDirection,
    };

    fn fixture() -> (
        Dispatcher,
        Scene,
        SpaceId,
        HashMap<ViewId, View>,
        ViewModeInstances,
        ContentModeInstances,
        ModeRegistry,
        ContentStore,
    ) {
        let editor = ContentId(0);
        let status = ContentId(1);
        let mut contents = ContentStore::default();
        contents
            .insert(editor, Content::Buffer(Buffer::new()))
            .unwrap();
        contents
            .insert(status, Content::StatusBar(StatusBar::new(editor)))
            .unwrap();
        let modes = ModeRegistry::builtin();
        let mut builder = SceneBuilder::new();
        let (scene, focused) =
            build_editor_scene(&mut builder, 40, 5, ViewId(0), ViewId(1)).unwrap();
        let views = HashMap::from([
            (
                ViewId(0),
                View::new(editor, contents.create_view_state(editor).unwrap()),
            ),
            (
                ViewId(1),
                View::new(status, contents.create_view_state(status).unwrap()),
            ),
        ]);
        let mut view_modes = ViewModeInstances::default();
        view_modes.insert(ViewId(0), modes.instantiate(&ModeName::new("vim")).unwrap());
        (
            Dispatcher::new(default_global_keymap()),
            scene,
            focused,
            views,
            view_modes,
            ContentModeInstances::default(),
            modes,
            contents,
        )
    }

    #[test]
    fn global_quit_resolves_to_app_command() {
        let (mut dispatcher, scene, focused, views, mut view_modes, mut content_modes, _, contents) =
            fixture();
        assert_eq!(
            dispatcher.dispatch(
                DispatchInput::Normal(KeyEvent::ctrl('q')),
                Instant::now(),
                focused,
                &scene,
                &views,
                &mut view_modes,
                &mut content_modes,
                &contents,
            ),
            DispatchOutcome::Emit {
                command: DispatchCommand::App(AppCommand::Quit),
                replay: Vec::new(),
            }
        );
    }

    #[test]
    fn immutable_mode_keymap_lookup_does_not_snapshot_mode_state() {
        let (mut dispatcher, scene, focused, views, mut view_modes, mut content_modes, _, contents) =
            fixture();

        assert_eq!(
            dispatcher.dispatch(
                DispatchInput::Normal(KeyEvent::char('g')),
                Instant::now(),
                focused,
                &scene,
                &views,
                &mut view_modes,
                &mut content_modes,
                &contents,
            ),
            DispatchOutcome::Waiting
        );
        assert!(dispatcher.take_input_mode_snapshots().is_empty());
    }

    #[test]
    fn global_viewport_command_resolves_to_the_focused_view() {
        let (mut dispatcher, scene, focused, views, mut view_modes, mut content_modes, _, contents) =
            fixture();
        let key = KeyEvent::ctrl('v');
        let viewport = ViewportCommand::new(
            ViewportMoveDirection::Down,
            ViewportMoveAmount::FullPage,
            ViewportCursorBehavior::Move,
        );
        dispatcher
            .global_keymap
            .bind(key, Command::Viewport(viewport));

        assert_eq!(
            dispatcher.dispatch(
                DispatchInput::Normal(key),
                Instant::now(),
                focused,
                &scene,
                &views,
                &mut view_modes,
                &mut content_modes,
                &contents,
            ),
            DispatchOutcome::Emit {
                command: DispatchCommand::Viewport {
                    command: viewport,
                    view: ViewId(0),
                    content: ContentId(0),
                },
                replay: Vec::new(),
            }
        );
    }

    #[test]
    fn vim_gg_waits_then_resolves_to_the_view() {
        let (mut dispatcher, scene, focused, views, mut view_modes, mut content_modes, _, contents) =
            fixture();
        let now = Instant::now();
        assert_eq!(
            dispatcher.dispatch(
                DispatchInput::Normal(KeyEvent::char('g')),
                now,
                focused,
                &scene,
                &views,
                &mut view_modes,
                &mut content_modes,
                &contents,
            ),
            DispatchOutcome::Waiting
        );
        assert!(dispatcher.is_pending());
        let outcome = dispatcher.dispatch(
            DispatchInput::Normal(KeyEvent::char('g')),
            now,
            focused,
            &scene,
            &views,
            &mut view_modes,
            &mut content_modes,
            &contents,
        );
        assert!(matches!(
            outcome,
            DispatchOutcome::Emit {
                command: DispatchCommand::Mode {
                    command: ModeCommand { .. },
                    view: ViewId(0),
                    content: ContentId(0),
                },
                replay,
            } if replay.is_empty()
        ));
    }

    #[test]
    fn mismatch_without_complete_binding_replays_prefix_as_unmapped() {
        let (mut dispatcher, scene, focused, views, mut view_modes, mut content_modes, _, contents) =
            fixture();
        let now = Instant::now();
        dispatcher.dispatch(
            DispatchInput::Normal(KeyEvent::char('g')),
            now,
            focused,
            &scene,
            &views,
            &mut view_modes,
            &mut content_modes,
            &contents,
        );
        assert_eq!(
            dispatcher.dispatch(
                DispatchInput::Normal(KeyEvent::char('x')),
                now,
                focused,
                &scene,
                &views,
                &mut view_modes,
                &mut content_modes,
                &contents,
            ),
            DispatchOutcome::Replay(vec![
                DispatchInput::Unmapped(KeyEvent::char('g')),
                DispatchInput::Normal(KeyEvent::char('x')),
            ])
        );
    }

    #[test]
    fn timeout_executes_the_longest_complete_binding_and_replays_suffix() {
        let (mut dispatcher, scene, focused, views, mut view_modes, mut content_modes, _, contents) =
            fixture();
        let g = KeyEvent::char('g');
        dispatcher
            .global_keymap
            .bind([g], Command::App(AppCommand::FocusNext));
        let start = Instant::now();
        assert_eq!(
            dispatcher.dispatch(
                DispatchInput::Normal(g),
                start,
                focused,
                &scene,
                &views,
                &mut view_modes,
                &mut content_modes,
                &contents,
            ),
            DispatchOutcome::Waiting
        );
        assert_eq!(
            dispatcher.dispatch_timeout(
                start + DEFAULT_SEQUENCE_TIMEOUT,
                focused,
                &scene,
                &views,
                &mut view_modes,
                &mut content_modes,
                &contents,
            ),
            DispatchOutcome::Emit {
                command: DispatchCommand::App(AppCommand::FocusNext),
                replay: Vec::new(),
            }
        );
    }

    #[test]
    fn mismatch_executes_shorter_complete_binding_before_replaying_new_key() {
        let (mut dispatcher, scene, focused, views, mut view_modes, mut content_modes, _, contents) =
            fixture();
        let g = KeyEvent::char('g');
        dispatcher
            .global_keymap
            .bind([g], Command::App(AppCommand::FocusNext));
        let now = Instant::now();
        assert_eq!(
            dispatcher.dispatch(
                DispatchInput::Normal(g),
                now,
                focused,
                &scene,
                &views,
                &mut view_modes,
                &mut content_modes,
                &contents,
            ),
            DispatchOutcome::Waiting
        );

        assert_eq!(
            dispatcher.dispatch(
                DispatchInput::Normal(KeyEvent::char('x')),
                now,
                focused,
                &scene,
                &views,
                &mut view_modes,
                &mut content_modes,
                &contents,
            ),
            DispatchOutcome::Emit {
                command: DispatchCommand::App(AppCommand::FocusNext),
                replay: vec![DispatchInput::Normal(KeyEvent::char('x'))],
            }
        );
    }

    #[test]
    fn invalidating_a_view_discards_sequence_and_cancels_private_awaiting() {
        let (
            mut dispatcher,
            scene,
            focused,
            views,
            mut view_modes,
            mut content_modes,
            modes,
            contents,
        ) = fixture();
        let now = Instant::now();
        assert_eq!(
            dispatcher.dispatch(
                DispatchInput::Normal(KeyEvent::char('g')),
                now,
                focused,
                &scene,
                &views,
                &mut view_modes,
                &mut content_modes,
                &contents,
            ),
            DispatchOutcome::Waiting
        );
        assert_eq!(
            view_modes.execute(
                ViewId(0),
                &modes,
                &ModeCommand {
                    mode: ModeName::new("vim"),
                    action: crate::core::mode_name::ModeActionName::new("count-2"),
                },
            ),
            Ok(None)
        );
        let context = ViewModeContext::new(ViewId(0), &views[&ViewId(0)], &contents);
        dispatcher.sync_view(
            ViewId(0),
            view_modes.input_status(ViewId(0), &context),
            true,
            now,
        );

        dispatcher.invalidate_view(
            ViewId(0),
            &views[&ViewId(0)],
            ContentId(0),
            &mut view_modes,
            &mut content_modes,
            &contents,
        );

        assert!(!dispatcher.is_pending());
        assert_eq!(
            view_modes.input_status(ViewId(0), &context),
            InputStatus::Ready
        );
    }

    #[test]
    fn invalidating_an_unrelated_view_keeps_the_focused_views_sequence() {
        let (mut dispatcher, scene, focused, views, mut view_modes, mut content_modes, _, contents) =
            fixture();
        let now = Instant::now();
        assert_eq!(
            dispatcher.dispatch(
                DispatchInput::Normal(KeyEvent::char('g')),
                now,
                focused,
                &scene,
                &views,
                &mut view_modes,
                &mut content_modes,
                &contents,
            ),
            DispatchOutcome::Waiting
        );

        dispatcher.invalidate_view(
            ViewId(1),
            &views[&ViewId(1)],
            ContentId(1),
            &mut view_modes,
            &mut content_modes,
            &contents,
        );

        assert!(dispatcher.is_pending());
        assert!(matches!(
            dispatcher.dispatch(
                DispatchInput::Normal(KeyEvent::char('g')),
                now,
                focused,
                &scene,
                &views,
                &mut view_modes,
                &mut content_modes,
                &contents,
            ),
            DispatchOutcome::Emit {
                command: DispatchCommand::Mode {
                    view: ViewId(0),
                    content: ContentId(0),
                    ..
                },
                replay,
            } if replay.is_empty()
        ));
    }

    #[test]
    fn emitted_edit_can_be_executed_by_the_content_store() {
        let (
            mut dispatcher,
            scene,
            focused,
            views,
            mut view_modes,
            mut content_modes,
            _,
            mut contents,
        ) = fixture();
        let outcome = dispatcher.dispatch(
            DispatchInput::Normal(KeyEvent::char('x')),
            Instant::now(),
            focused,
            &scene,
            &views,
            &mut view_modes,
            &mut content_modes,
            &contents,
        );
        let DispatchOutcome::Emit {
            command:
                DispatchCommand::ContentWithView {
                    command,
                    view,
                    content,
                },
            ..
        } = outcome
        else {
            panic!("x must emit an edit command");
        };
        let ContentCommand::Edit(command) = command else {
            panic!("x must emit an edit command");
        };
        let selections = views[&view].selections().unwrap().clone();
        let plan = contents.plan_edit(content, command, &selections).unwrap();
        if let Some(action) = plan.action {
            let _ = contents.apply(content, action);
        }
    }
}
