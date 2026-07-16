use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::app::view::View;
use crate::core::command::{AppCommand, Command, ContentCommand};
use crate::core::input::{
    AwaitingSource, InputCoordinator, InputDecision, InputStatus, KeySequenceConfig, KeymapLayer,
    PendingSequence, continuations, longest_complete, match_sequence,
};
use crate::core::keymap::Keymap;
use crate::protocol::ids::{ContentId, SpaceId, ViewId};
use crate::protocol::key_event::KeyEvent;
use crate::protocol::scene::Scene;
use crate::protocol::space::SpaceKind;

const DEFAULT_SEQUENCE_TIMEOUT: Duration = Duration::from_millis(1_000);

pub struct Dispatcher {
    global_keymap: Keymap,
    coordinator: InputCoordinator<CommandSource>,
    sequence_config: KeySequenceConfig,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DispatchCommand {
    App(AppCommand),
    Content {
        command: ContentCommand,
        content: ContentId,
    },
    ViewContent {
        command: ContentCommand,
        view: ViewId,
        content: ContentId,
    },
    Noop,
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
enum CommandSource {
    View(ViewId),
    Global,
}

impl Dispatcher {
    pub fn new(global_keymap: Keymap) -> Self {
        Self::with_config(
            global_keymap,
            KeySequenceConfig::new(DEFAULT_SEQUENCE_TIMEOUT),
        )
    }

    pub fn with_config(global_keymap: Keymap, sequence_config: KeySequenceConfig) -> Self {
        Self {
            global_keymap,
            coordinator: InputCoordinator::default(),
            sequence_config,
        }
    }

    #[cfg(test)]
    pub fn is_pending(&self) -> bool {
        self.coordinator.pending_sequence().is_some()
    }

    pub fn dispatch(
        &mut self,
        input: DispatchInput,
        now: Instant,
        focused: SpaceId,
        scene: &Scene,
        views: &mut HashMap<ViewId, View>,
    ) -> DispatchOutcome {
        let focused_view = match focused_view_id(scene, focused) {
            Some(view) => view,
            None => return DispatchOutcome::Consumed,
        };
        let key = input.key();

        for source in self.coordinator.sources_top_down() {
            match source {
                AwaitingSource::Context(source) => {
                    let (decision, status) = capture_context(views, source, key);
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
                    return self.continue_sequence(key, now, focused_view, views);
                }
                AwaitingSource::KeySequence => continue,
            }
        }

        match input {
            DispatchInput::Normal(key) => self.start_sequence(key, now, focused_view, views),
            DispatchInput::Unmapped(key) => fallback(key, focused_view, views),
        }
    }

    pub fn sync_view(&mut self, view: ViewId, status: InputStatus, handled: bool, now: Instant) {
        self.coordinator
            .sync_context(CommandSource::View(view), status, handled, now);
    }

    pub fn invalidate_view(&mut self, view: ViewId, views: &mut HashMap<ViewId, View>) {
        if let Some(view_state) = views.get_mut(&view) {
            view_state.cancel_input();
        }
        self.coordinator.remove_context(&CommandSource::View(view));
        self.coordinator.discard_sequence();
    }

    pub fn next_deadline(&self, views: &HashMap<ViewId, View>) -> Option<Instant> {
        self.coordinator
            .next_deadline(|source| context_status(views, *source))
    }

    pub fn dispatch_timeout(
        &mut self,
        now: Instant,
        focused: SpaceId,
        scene: &Scene,
        views: &mut HashMap<ViewId, View>,
    ) -> DispatchOutcome {
        let focused_view = match focused_view_id(scene, focused) {
            Some(view) => view,
            None => return DispatchOutcome::Consumed,
        };
        let Some(due) = self
            .coordinator
            .next_due(now, |source| context_status(views, *source))
        else {
            return DispatchOutcome::Waiting;
        };
        match due {
            AwaitingSource::Context(source) => {
                if let CommandSource::View(view) = source
                    && let Some(view) = views.get_mut(&view)
                {
                    view.on_input_timeout();
                    let status = view.input_status();
                    self.coordinator.sync_context(source, status, true, now);
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
                self.resolve_aborted_sequence(pending.keys, None, focused_view, views)
            }
        }
    }

    #[allow(dead_code)] // Query seam for a future which-key frontend.
    pub fn pending_continuations(
        &self,
        focused: SpaceId,
        scene: &Scene,
        views: &HashMap<ViewId, View>,
    ) -> HashSet<KeyEvent> {
        let Some(view) = focused_view_id(scene, focused) else {
            return HashSet::new();
        };
        let Some(pending) = self.coordinator.pending_sequence() else {
            return HashSet::new();
        };
        self.with_layers(view, views, |layers| continuations(layers, &pending.keys))
    }

    fn start_sequence(
        &mut self,
        key: KeyEvent,
        now: Instant,
        focused_view: ViewId,
        views: &HashMap<ViewId, View>,
    ) -> DispatchOutcome {
        let matched =
            self.with_layers(focused_view, views, |layers| match_sequence(layers, &[key]));
        let Some(matched) = matched else {
            return fallback(key, focused_view, views);
        };
        if matched.has_children {
            let keys = vec![key];
            self.coordinator.push_sequence(PendingSequence {
                deadline: self.sequence_config.deadline(&keys, now),
                keys,
            });
            return DispatchOutcome::Waiting;
        }
        let Some(resolved) = matched.exact else {
            return fallback(key, focused_view, views);
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

    fn continue_sequence(
        &mut self,
        key: KeyEvent,
        now: Instant,
        focused_view: ViewId,
        views: &HashMap<ViewId, View>,
    ) -> DispatchOutcome {
        let mut keys = self
            .coordinator
            .pending_sequence()
            .expect("sequence source has pending data")
            .keys
            .clone();
        keys.push(key);
        let matched = self.with_layers(focused_view, views, |layers| match_sequence(layers, &keys));
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
                )
            }
        }
    }

    fn resolve_aborted_sequence(
        &self,
        keys: Vec<KeyEvent>,
        trailing: Option<DispatchInput>,
        focused_view: ViewId,
        views: &HashMap<ViewId, View>,
    ) -> DispatchOutcome {
        let complete = self.with_layers(focused_view, views, |layers| {
            longest_complete(layers, &keys)
        });
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
        query: impl FnOnce(&[KeymapLayer<'_, Command, CommandSource>]) -> R,
    ) -> R {
        let mut layers = Vec::with_capacity(2);
        if let Some(keymap) = views.get(&focused_view).and_then(View::keymap) {
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

fn capture_context(
    views: &mut HashMap<ViewId, View>,
    source: CommandSource,
    key: KeyEvent,
) -> (InputDecision<Command>, InputStatus) {
    match source {
        CommandSource::View(view) => {
            let Some(view) = views.get_mut(&view) else {
                return (InputDecision::Pass, InputStatus::Ready);
            };
            let decision = view.capture(key);
            (decision, view.input_status())
        }
        CommandSource::Global => (InputDecision::Pass, InputStatus::Ready),
    }
}

fn context_status(views: &HashMap<ViewId, View>, source: CommandSource) -> InputStatus {
    match source {
        CommandSource::View(view) => views
            .get(&view)
            .map_or(InputStatus::Ready, View::input_status),
        CommandSource::Global => InputStatus::Ready,
    }
}

fn fallback(key: KeyEvent, focused_view: ViewId, views: &HashMap<ViewId, View>) -> DispatchOutcome {
    let Some(action) = views.get(&focused_view).and_then(|view| view.fallback(key)) else {
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

fn resolve_command(
    command: Command,
    source: CommandSource,
    focused_view: ViewId,
    views: &HashMap<ViewId, View>,
) -> Option<DispatchCommand> {
    match command {
        Command::App(command) => Some(DispatchCommand::App(command)),
        Command::Noop => Some(DispatchCommand::Noop),
        Command::Content(command @ (ContentCommand::Edit(_) | ContentCommand::Mode { .. })) => {
            let view = match source {
                CommandSource::View(view) => view,
                CommandSource::Global => focused_view,
            };
            Some(DispatchCommand::ViewContent {
                command,
                view,
                content: views.get(&view)?.content(),
            })
        }
        Command::Content(command @ ContentCommand::Save) => {
            let view = match source {
                CommandSource::View(view) => view,
                CommandSource::Global => focused_view,
            };
            Some(DispatchCommand::Content {
                command,
                content: views.get(&view)?.content(),
            })
        }
    }
}

fn focused_view_id(scene: &Scene, focused: SpaceId) -> Option<ViewId> {
    match &scene.node(focused).space.kind {
        SpaceKind::Content { view, .. } => Some(*view),
        SpaceKind::Container { .. } => None,
    }
}

pub fn default_global_keymap() -> Keymap {
    let mut keymap = Keymap::new();
    keymap.bind(KeyEvent::ctrl('q'), Command::App(AppCommand::Quit));
    keymap.bind(KeyEvent::ctrl('s'), Command::Content(ContentCommand::Save));
    keymap
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::scene_model::{SceneBuilder, build_editor_scene};
    use crate::core::buffer::Buffer;
    use crate::core::content::{Content, ContentInput};
    use crate::core::content_store::ContentStore;
    use crate::core::mode::{ModeName, ModeRegistry};
    use crate::core::status_bar::StatusBar;
    use crate::protocol::ids::ContentId;

    fn fixture() -> (
        Dispatcher,
        Scene,
        SpaceId,
        HashMap<ViewId, View>,
        ModeRegistry,
        ContentStore,
    ) {
        let editor = ContentId(0);
        let status = ContentId(1);
        let mut contents = ContentStore::default();
        contents.insert(editor, Content::Buffer(Buffer::new()));
        contents.insert(status, Content::StatusBar(StatusBar::new(editor)));
        let modes = ModeRegistry::builtin();
        let mut builder = SceneBuilder::new();
        let (scene, focused) =
            build_editor_scene(&mut builder, 40, 5, ViewId(0), ViewId(1)).unwrap();
        let views = HashMap::from([
            (
                ViewId(0),
                View::new(
                    editor,
                    contents.create_view_state(editor).unwrap(),
                    modes.instantiate(&ModeName::new("vim")),
                ),
            ),
            (
                ViewId(1),
                View::new(status, contents.create_view_state(status).unwrap(), None),
            ),
        ]);
        (
            Dispatcher::new(default_global_keymap()),
            scene,
            focused,
            views,
            modes,
            contents,
        )
    }

    #[test]
    fn global_quit_resolves_to_app_command() {
        let (mut dispatcher, scene, focused, mut views, _, _) = fixture();
        assert_eq!(
            dispatcher.dispatch(
                DispatchInput::Normal(KeyEvent::ctrl('q')),
                Instant::now(),
                focused,
                &scene,
                &mut views,
            ),
            DispatchOutcome::Emit {
                command: DispatchCommand::App(AppCommand::Quit),
                replay: Vec::new(),
            }
        );
    }

    #[test]
    fn vim_gg_waits_then_resolves_to_the_view() {
        let (mut dispatcher, scene, focused, mut views, _, _) = fixture();
        let now = Instant::now();
        assert_eq!(
            dispatcher.dispatch(
                DispatchInput::Normal(KeyEvent::char('g')),
                now,
                focused,
                &scene,
                &mut views,
            ),
            DispatchOutcome::Waiting
        );
        assert!(dispatcher.is_pending());
        let outcome = dispatcher.dispatch(
            DispatchInput::Normal(KeyEvent::char('g')),
            now,
            focused,
            &scene,
            &mut views,
        );
        assert!(matches!(
            outcome,
            DispatchOutcome::Emit {
                command: DispatchCommand::ViewContent {
                    command: ContentCommand::Mode { .. },
                    view: ViewId(0),
                    content: ContentId(0),
                },
                replay,
            } if replay.is_empty()
        ));
    }

    #[test]
    fn mismatch_without_complete_binding_replays_prefix_as_unmapped() {
        let (mut dispatcher, scene, focused, mut views, _, _) = fixture();
        let now = Instant::now();
        dispatcher.dispatch(
            DispatchInput::Normal(KeyEvent::char('g')),
            now,
            focused,
            &scene,
            &mut views,
        );
        assert_eq!(
            dispatcher.dispatch(
                DispatchInput::Normal(KeyEvent::char('x')),
                now,
                focused,
                &scene,
                &mut views,
            ),
            DispatchOutcome::Replay(vec![
                DispatchInput::Unmapped(KeyEvent::char('g')),
                DispatchInput::Normal(KeyEvent::char('x')),
            ])
        );
    }

    #[test]
    fn timeout_executes_the_longest_complete_binding_and_replays_suffix() {
        let (mut dispatcher, scene, focused, mut views, _, _) = fixture();
        let g = KeyEvent::char('g');
        dispatcher
            .global_keymap
            .bind([g], Command::App(AppCommand::FocusNext));
        let start = Instant::now();
        assert_eq!(
            dispatcher.dispatch(DispatchInput::Normal(g), start, focused, &scene, &mut views,),
            DispatchOutcome::Waiting
        );
        assert_eq!(
            dispatcher.dispatch_timeout(
                start + DEFAULT_SEQUENCE_TIMEOUT,
                focused,
                &scene,
                &mut views,
            ),
            DispatchOutcome::Emit {
                command: DispatchCommand::App(AppCommand::FocusNext),
                replay: Vec::new(),
            }
        );
    }

    #[test]
    fn mismatch_executes_shorter_complete_binding_before_replaying_new_key() {
        let (mut dispatcher, scene, focused, mut views, _, _) = fixture();
        let g = KeyEvent::char('g');
        dispatcher
            .global_keymap
            .bind([g], Command::App(AppCommand::FocusNext));
        let now = Instant::now();
        assert_eq!(
            dispatcher.dispatch(DispatchInput::Normal(g), now, focused, &scene, &mut views,),
            DispatchOutcome::Waiting
        );

        assert_eq!(
            dispatcher.dispatch(
                DispatchInput::Normal(KeyEvent::char('x')),
                now,
                focused,
                &scene,
                &mut views,
            ),
            DispatchOutcome::Emit {
                command: DispatchCommand::App(AppCommand::FocusNext),
                replay: vec![DispatchInput::Normal(KeyEvent::char('x'))],
            }
        );
    }

    #[test]
    fn invalidating_a_view_discards_sequence_and_cancels_private_awaiting() {
        let (mut dispatcher, scene, focused, mut views, modes, _) = fixture();
        let now = Instant::now();
        assert_eq!(
            dispatcher.dispatch(
                DispatchInput::Normal(KeyEvent::char('g')),
                now,
                focused,
                &scene,
                &mut views,
            ),
            DispatchOutcome::Waiting
        );
        let view = views.get_mut(&ViewId(0)).unwrap();
        assert!(matches!(
            view.execute_mode_command(
                &modes,
                &ModeName::new("vim"),
                &crate::core::mode::ModeActionName::new("count-2"),
            ),
            crate::app::view::ModeCommandResult::Handled(None)
        ));
        dispatcher.sync_view(ViewId(0), view.input_status(), true, now);

        dispatcher.invalidate_view(ViewId(0), &mut views);

        assert!(!dispatcher.is_pending());
        assert_eq!(
            views.get(&ViewId(0)).unwrap().input_status(),
            InputStatus::Ready
        );
    }

    #[test]
    fn emitted_edit_can_be_executed_by_the_content_store() {
        let (mut dispatcher, scene, focused, mut views, _, mut contents) = fixture();
        let outcome = dispatcher.dispatch(
            DispatchInput::Normal(KeyEvent::char('x')),
            Instant::now(),
            focused,
            &scene,
            &mut views,
        );
        let DispatchOutcome::Emit {
            command:
                DispatchCommand::ViewContent {
                    command,
                    view,
                    content,
                },
            ..
        } = outcome
        else {
            panic!("x must emit an edit command");
        };
        let state = views.get_mut(&view).unwrap().state_mut();
        let _ = contents.execute(content, ContentInput::View { command, state });
    }
}
