use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::app::command::{AppCommand, Command, ContentCommand, ModeCommand, ModeInputCommand};
use crate::app::command_resolver::{focused_view_id, resolve_command};
use crate::app::mode::{ModeContentStore, ModeDraftJournal, ModeViewContext, ModeViewStore};
use crate::app::view::View;
use crate::core::content_store::ContentStore;
use crate::core::input::{
    AwaitingSource, InputCoordinator, InputDecision, InputStatus, KeySequenceConfig, KeymapLayer,
    PendingSequence, continuations, longest_complete, match_sequence,
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
    view_mode_revisions: Vec<(ViewId, Revision)>,
}

#[derive(Clone)]
pub(crate) struct DispatcherInputSnapshot {
    coordinator: InputCoordinator<CommandSource>,
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
    ModeInput {
        input: ModeInputCommand,
        view: ViewId,
        content: ContentId,
    },
    Viewport {
        command: ViewportCommand,
        view: ViewId,
        content: ContentId,
    },
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "extension callbacks can emit content-scoped effects"
        )
    )]
    ModeContentOperations {
        operations: Vec<crate::app::operation::OperationRequest>,
        content: ContentId,
    },
    ModeOperations {
        operations: Vec<crate::app::operation::OperationRequest>,
        view: ViewId,
        content: ContentId,
    },
    Noop,
}

impl DispatchCommand {
    pub(crate) fn content(&self) -> Option<ContentId> {
        match self {
            Self::Content { content, .. }
            | Self::ContentWithView { content, .. }
            | Self::Mode { content, .. }
            | Self::ModeInput { content, .. }
            | Self::Viewport { content, .. }
            | Self::ModeContentOperations { content, .. }
            | Self::ModeOperations { content, .. } => Some(*content),
            Self::App(_) | Self::Noop => None,
        }
    }

    pub(crate) fn view(&self) -> Option<ViewId> {
        match self {
            Self::ContentWithView { view, .. }
            | Self::Mode { view, .. }
            | Self::ModeInput { view, .. }
            | Self::Viewport { view, .. }
            | Self::ModeOperations { view, .. } => Some(*view),
            Self::App(_)
            | Self::Content { .. }
            | Self::ModeContentOperations { .. }
            | Self::Noop => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DispatchInput {
    Normal(KeyEvent),
    Unmapped(KeyEvent),
    Continue { key: KeyEvent, mode_index: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DispatchOutcome {
    Waiting,
    Consumed,
    Replay(Vec<DispatchInput>),
    Emit {
        command: DispatchCommand,
        replay: Vec<DispatchInput>,
        continuation: Option<DispatchInput>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommandSource {
    Mode { view: ViewId, index: usize },
    Global,
}

impl CommandSource {
    pub(super) fn view_or(self, focused_view: ViewId) -> ViewId {
        match self {
            Self::Mode { view, .. } => view,
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
            view_mode_revisions: Vec::new(),
        }
    }

    pub(crate) fn snapshot_input(&self) -> DispatcherInputSnapshot {
        DispatcherInputSnapshot {
            coordinator: self.coordinator.clone(),
        }
    }

    pub(crate) fn restore_input(&mut self, snapshot: DispatcherInputSnapshot) {
        self.coordinator = snapshot.coordinator;
    }

    #[cfg(test)]
    pub fn is_pending(&self) -> bool {
        self.coordinator.pending_sequence().is_some()
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "dispatcher borrows split app-owned stores"
    )]
    pub fn dispatch_in_draft(
        &mut self,
        input: DispatchInput,
        now: Instant,
        focused: SpaceId,
        scene: &Scene,
        views: &HashMap<ViewId, View>,
        modes: &mut ModeViewStore,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
        drafts: &mut ModeDraftJournal,
    ) -> DispatchOutcome {
        self.view_mode_revisions.clear();
        let focused_view = match focused_view_id(scene, focused) {
            Some(view) => view,
            None => return DispatchOutcome::Consumed,
        };
        match input {
            DispatchInput::Normal(key) => self.start_sequence(
                key,
                0,
                now,
                focused_view,
                views,
                modes,
                content_modes,
                contents,
                drafts,
            ),
            DispatchInput::Unmapped(key) => {
                let Some(view_data) = views.get(&focused_view) else {
                    return DispatchOutcome::Consumed;
                };
                let Ok(context) = ModeViewContext::new(focused_view, view_data, contents) else {
                    return DispatchOutcome::Consumed;
                };
                fallback(key, 0, &context, views, modes, content_modes, drafts)
            }
            DispatchInput::Continue { key, mode_index } => self.start_sequence(
                key,
                mode_index,
                now,
                focused_view,
                views,
                modes,
                content_modes,
                contents,
                drafts,
            ),
        }
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch(
        &mut self,
        input: DispatchInput,
        now: Instant,
        focused: SpaceId,
        scene: &Scene,
        views: &HashMap<ViewId, View>,
        modes: &mut ModeViewStore,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> DispatchOutcome {
        let mut drafts = ModeDraftJournal::default();
        let outcome = self.dispatch_in_draft(
            input,
            now,
            focused,
            scene,
            views,
            modes,
            content_modes,
            contents,
            &mut drafts,
        );
        drafts.commit_content(content_modes);
        drafts.commit_views(modes);
        outcome
    }

    pub fn sync_mode(
        &mut self,
        view: ViewId,
        index: usize,
        status: InputStatus,
        handled: bool,
        now: Instant,
    ) {
        self.coordinator
            .sync_context(CommandSource::Mode { view, index }, status, handled, now);
    }

    pub fn take_view_mode_revisions(&mut self) -> Vec<(ViewId, Revision)> {
        std::mem::take(&mut self.view_mode_revisions)
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
        _content: ContentId,
        modes: &mut ModeViewStore,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> bool {
        let cancelled_view_mode = modes.is_active(view);
        if cancelled_view_mode && let Ok(context) = ModeViewContext::new(view, view_data, contents)
        {
            modes.cancel_chain(view, &context, content_modes, contents);
        }
        self.invalidate_view_binding(view);
        cancelled_view_mode
    }

    fn invalidate_view_binding(&mut self, view: ViewId) {
        self.coordinator.remove_contexts(|source| match source {
            CommandSource::Mode { view: owner, .. } => *owner == view,
            CommandSource::Global => false,
        });
        if self
            .coordinator
            .pending_sequence()
            .is_some_and(|pending| match pending.owner {
                CommandSource::Mode { view: owner, .. } => owner == view,
                CommandSource::Global => false,
            })
        {
            self.coordinator.discard_sequence();
        }
    }

    pub(crate) fn invalidate_mode_chain(&mut self, view: ViewId) {
        self.invalidate_view_binding(view);
    }

    pub fn next_deadline(
        &self,
        views: &HashMap<ViewId, View>,
        modes: &ModeViewStore,
        content_modes: &ModeContentStore,
        contents: &ContentStore,
    ) -> Option<Instant> {
        let drafts = ModeDraftJournal::default();
        self.coordinator.next_deadline(|source| {
            context_status(views, modes, content_modes, contents, &drafts, *source)
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "dispatcher borrows split app-owned stores"
    )]
    pub fn dispatch_timeout_in_draft(
        &mut self,
        now: Instant,
        focused: SpaceId,
        scene: &Scene,
        views: &HashMap<ViewId, View>,
        modes: &mut ModeViewStore,
        content_modes: &mut ModeContentStore,
        contents: &crate::core::content_store::ContentStore,
        drafts: &mut ModeDraftJournal,
    ) -> DispatchOutcome {
        self.view_mode_revisions.clear();
        let focused_view = match focused_view_id(scene, focused) {
            Some(view) => view,
            None => return DispatchOutcome::Consumed,
        };
        let Some(due) = self.coordinator.next_due(now, |source| {
            context_status(views, modes, content_modes, contents, drafts, *source)
        }) else {
            return DispatchOutcome::Waiting;
        };
        match due {
            AwaitingSource::Context(source) => {
                if let CommandSource::Mode { view, index } = source {
                    let content = views.get(&view).map(View::content);
                    let command = content.and_then(|content| {
                        let context = ModeViewContext::new(
                            view,
                            views.get(&view).expect("timeout view exists"),
                            contents,
                        )
                        .ok()?;
                        modes
                            .timeout_at(view, index, &context, content_modes, drafts)
                            .map(|operations| {
                                self.record_view_mode_revision(view, views[&view].revision());
                                DispatchCommand::ModeOperations {
                                    operations,
                                    view,
                                    content,
                                }
                            })
                    });
                    let status =
                        context_status(views, modes, content_modes, contents, drafts, source);
                    self.coordinator.sync_context(source, status, true, now);
                    if let Some(command) = command {
                        return DispatchOutcome::Emit {
                            command,
                            replay: Vec::new(),
                            continuation: None,
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
                    pending.owner,
                    pending.keys,
                    None,
                    focused_view,
                    views,
                    modes,
                    content_modes,
                    contents,
                    drafts,
                )
            }
        }
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch_timeout(
        &mut self,
        now: Instant,
        focused: SpaceId,
        scene: &Scene,
        views: &HashMap<ViewId, View>,
        modes: &mut ModeViewStore,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
    ) -> DispatchOutcome {
        let mut drafts = ModeDraftJournal::default();
        let outcome = self.dispatch_timeout_in_draft(
            now,
            focused,
            scene,
            views,
            modes,
            content_modes,
            contents,
            &mut drafts,
        );
        drafts.commit_content(content_modes);
        drafts.commit_views(modes);
        outcome
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
        modes: &ModeViewStore,
        content_modes: &ModeContentStore,
        contents: &ContentStore,
    ) -> HashSet<KeyEvent> {
        let Some(view) = focused_view_id(scene, focused) else {
            return HashSet::new();
        };
        let Some(pending) = self.coordinator.pending_sequence() else {
            return HashSet::new();
        };
        let drafts = ModeDraftJournal::default();
        self.with_sequence_layer(
            pending.owner,
            view,
            views,
            modes,
            content_modes,
            contents,
            &drafts,
            |layer| continuations(std::slice::from_ref(layer), &pending.keys),
        )
        .unwrap_or_default()
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "sequence resolution uses the same split runtime context"
    )]
    fn start_sequence(
        &mut self,
        key: KeyEvent,
        start_mode: usize,
        now: Instant,
        focused_view: ViewId,
        views: &HashMap<ViewId, View>,
        modes: &mut ModeViewStore,
        content_modes: &mut ModeContentStore,
        contents: &ContentStore,
        drafts: &mut ModeDraftJournal,
    ) -> DispatchOutcome {
        let Some(view) = views.get(&focused_view) else {
            return DispatchOutcome::Consumed;
        };
        let Ok(context) = ModeViewContext::new(focused_view, view, contents) else {
            return DispatchOutcome::Consumed;
        };
        for index in start_mode..modes.mode_ids(focused_view).len() {
            let source = CommandSource::Mode {
                view: focused_view,
                index,
            };
            let status = modes.status_at(focused_view, index, &context, content_modes, drafts);
            if !matches!(status, InputStatus::Ready) {
                let decision =
                    modes.capture_at(focused_view, index, &context, content_modes, drafts, key);
                let handled = !matches!(decision, InputDecision::Pass);
                let status = modes.status_at(focused_view, index, &context, content_modes, drafts);
                self.coordinator.sync_context(source, status, handled, now);
                if handled {
                    self.record_view_mode_revision(focused_view, view.revision());
                }
                match decision {
                    InputDecision::Pass => {}
                    InputDecision::Consumed => return DispatchOutcome::Consumed,
                    InputDecision::Emit(action) => {
                        let Some(command) = resolve_command(action, source, focused_view, views)
                        else {
                            return DispatchOutcome::Consumed;
                        };
                        return DispatchOutcome::Emit {
                            command,
                            replay: Vec::new(),
                            continuation: Some(DispatchInput::Continue {
                                key,
                                mode_index: index + 1,
                            }),
                        };
                    }
                }
            }
            if self
                .coordinator
                .pending_sequence()
                .is_some_and(|pending| pending.owner == source)
            {
                return self.continue_sequence(
                    key,
                    now,
                    focused_view,
                    views,
                    modes,
                    content_modes,
                    contents,
                    drafts,
                );
            }
            if let Some(keymap) =
                modes.keymap_at(focused_view, index, &context, content_modes, drafts)
            {
                let layer = [KeymapLayer { source, keymap }];
                if let Some(matched) = match_sequence(&layer, &[key]) {
                    if matched.has_children {
                        let keys = vec![key];
                        self.coordinator.push_sequence(PendingSequence {
                            owner: source,
                            deadline: self.sequence_config.deadline(&keys, now),
                            keys,
                        });
                        return DispatchOutcome::Waiting;
                    }
                    if let Some(resolved) = matched.exact {
                        return emit_resolved(resolved, key, focused_view, views);
                    }
                }
            }
            if let Some(action) =
                modes.fallback_at(focused_view, index, &context, content_modes, drafts, key)
            {
                let Some(command) = resolve_command(action, source, focused_view, views) else {
                    return DispatchOutcome::Consumed;
                };
                return DispatchOutcome::Emit {
                    command,
                    replay: Vec::new(),
                    continuation: Some(DispatchInput::Continue {
                        key,
                        mode_index: index + 1,
                    }),
                };
            }
        }

        if self
            .coordinator
            .pending_sequence()
            .is_some_and(|pending| pending.owner == CommandSource::Global)
        {
            return self.continue_sequence(
                key,
                now,
                focused_view,
                views,
                modes,
                content_modes,
                contents,
                drafts,
            );
        }
        let layer = [KeymapLayer {
            source: CommandSource::Global,
            keymap: &self.global_keymap,
        }];
        let Some(matched) = match_sequence(&layer, &[key]) else {
            return DispatchOutcome::Consumed;
        };
        if matched.has_children {
            let keys = vec![key];
            self.coordinator.push_sequence(PendingSequence {
                owner: CommandSource::Global,
                deadline: self.sequence_config.deadline(&keys, now),
                keys,
            });
            return DispatchOutcome::Waiting;
        }
        let Some(resolved) = matched.exact else {
            return DispatchOutcome::Consumed;
        };
        emit_resolved(resolved, key, focused_view, views)
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
        modes: &ModeViewStore,
        content_modes: &ModeContentStore,
        contents: &ContentStore,
        drafts: &ModeDraftJournal,
    ) -> DispatchOutcome {
        let pending = self
            .coordinator
            .pending_sequence()
            .expect("sequence source has pending data");
        let owner = pending.owner;
        let mut keys = pending.keys.clone();
        keys.push(key);
        let matched = self
            .with_sequence_layer(
                owner,
                focused_view,
                views,
                modes,
                content_modes,
                contents,
                drafts,
                |layer| match_sequence(std::slice::from_ref(layer), &keys),
            )
            .flatten();
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
                emit_resolved(resolved, key, focused_view, views)
            }
            None => {
                let pending = self
                    .coordinator
                    .take_sequence()
                    .expect("mismatched sequence exists");
                self.resolve_aborted_sequence(
                    pending.owner,
                    pending.keys,
                    Some(DispatchInput::Normal(key)),
                    focused_view,
                    views,
                    modes,
                    content_modes,
                    contents,
                    drafts,
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
        owner: CommandSource,
        keys: Vec<KeyEvent>,
        trailing: Option<DispatchInput>,
        focused_view: ViewId,
        views: &HashMap<ViewId, View>,
        modes: &ModeViewStore,
        content_modes: &ModeContentStore,
        contents: &ContentStore,
        drafts: &ModeDraftJournal,
    ) -> DispatchOutcome {
        let complete = self
            .with_sequence_layer(
                owner,
                focused_view,
                views,
                modes,
                content_modes,
                contents,
                drafts,
                |layer| longest_complete(std::slice::from_ref(layer), &keys),
            )
            .flatten();
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
                let continuation = keys
                    .get(complete.consumed.saturating_sub(1))
                    .copied()
                    .and_then(|key| mode_continuation(complete.resolved.source, key));
                DispatchOutcome::Emit {
                    command,
                    replay,
                    continuation,
                }
            }
            None => {
                let mut replay: Vec<_> = keys.into_iter().map(DispatchInput::Unmapped).collect();
                replay.extend(trailing);
                DispatchOutcome::Replay(replay)
            }
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "sequence lookup borrows split runtime stores"
    )]
    fn with_sequence_layer<R>(
        &self,
        source: CommandSource,
        _focused_view: ViewId,
        views: &HashMap<ViewId, View>,
        modes: &ModeViewStore,
        content_modes: &ModeContentStore,
        contents: &ContentStore,
        drafts: &ModeDraftJournal,
        query: impl FnOnce(&KeymapLayer<'_, Command, CommandSource>) -> R,
    ) -> Option<R> {
        match source {
            CommandSource::Mode { view, index } => {
                let view_data = views.get(&view)?;
                let context = ModeViewContext::new(view, view_data, contents).ok()?;
                let keymap = modes.keymap_at(view, index, &context, content_modes, drafts)?;
                Some(query(&KeymapLayer { source, keymap }))
            }
            CommandSource::Global => Some(query(&KeymapLayer {
                source,
                keymap: &self.global_keymap,
            })),
        }
    }
}

fn emit_resolved(
    resolved: crate::core::input::ResolvedAction<Command, CommandSource>,
    key: KeyEvent,
    focused_view: ViewId,
    views: &HashMap<ViewId, View>,
) -> DispatchOutcome {
    let Some(command) = resolve_command(resolved.action, resolved.source, focused_view, views)
    else {
        return DispatchOutcome::Consumed;
    };
    DispatchOutcome::Emit {
        command,
        replay: Vec::new(),
        continuation: mode_continuation(resolved.source, key),
    }
}

fn context_status(
    views: &HashMap<ViewId, View>,
    modes: &ModeViewStore,
    content_modes: &ModeContentStore,
    contents: &ContentStore,
    drafts: &ModeDraftJournal,
    source: CommandSource,
) -> InputStatus {
    match source {
        CommandSource::Mode { view, index } => views
            .get(&view)
            .and_then(|view_data| ModeViewContext::new(view, view_data, contents).ok())
            .map_or(InputStatus::Ready, |context| {
                modes.status_at(view, index, &context, content_modes, drafts)
            }),
        CommandSource::Global => InputStatus::Ready,
    }
}

fn fallback(
    key: KeyEvent,
    start_mode: usize,
    context: &ModeViewContext<'_>,
    views: &HashMap<ViewId, View>,
    modes: &ModeViewStore,
    content_modes: &ModeContentStore,
    drafts: &ModeDraftJournal,
) -> DispatchOutcome {
    let focused_view = context.view_id();
    let action = modes.fallback_in_chain(
        focused_view,
        start_mode,
        context,
        content_modes,
        drafts,
        key,
    );
    let Some((index, action)) = action else {
        return DispatchOutcome::Consumed;
    };
    let Some(command) = resolve_command(
        action,
        CommandSource::Mode {
            view: focused_view,
            index,
        },
        focused_view,
        views,
    ) else {
        return DispatchOutcome::Consumed;
    };
    DispatchOutcome::Emit {
        command,
        replay: Vec::new(),
        continuation: Some(DispatchInput::Continue {
            key,
            mode_index: index + 1,
        }),
    }
}

fn mode_continuation(source: CommandSource, key: KeyEvent) -> Option<DispatchInput> {
    let CommandSource::Mode { index, .. } = source else {
        return None;
    };
    Some(DispatchInput::Continue {
        key,
        mode_index: index + 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::command_resolver::default_global_keymap;
    use crate::app::mode::{Mode, ModeContentStore, ModeRegistry, ModeViewStore};
    use crate::app::mode_name::{ModeActionName, ModeName};
    use crate::app::scene_model::{SceneBuilder, build_editor_scene};
    use crate::core::buffer::Buffer;
    use crate::core::command::EditCommand;
    use crate::core::content::Content;
    use crate::core::content_store::ContentStore;
    use crate::core::status_bar::StatusBar;
    use crate::protocol::ids::ContentId;
    use crate::protocol::viewport::{
        ViewportCursorBehavior, ViewportMoveAmount, ViewportMoveDirection,
    };

    struct DispatcherTestMode {
        name: ModeName,
        actions: Vec<ModeActionName>,
        keymap: Keymap<Command>,
    }

    impl DispatcherTestMode {
        fn new() -> Self {
            let name = ModeName::new("dispatcher-test");
            let action = ModeActionName::new("sequence");
            let mut keymap = Keymap::new();
            keymap.bind(
                [KeyEvent::char('g'), KeyEvent::char('g')],
                Command::Mode(ModeCommand::new(name.clone(), action.clone())),
            );
            keymap.bind(
                KeyEvent::char('x'),
                Command::Content(ContentCommand::Edit(EditCommand::Delete(1))),
            );
            Self {
                name,
                actions: vec![action],
                keymap,
            }
        }
    }

    impl Mode for DispatcherTestMode {
        fn name(&self) -> &ModeName {
            &self.name
        }

        fn actions(&self) -> &[ModeActionName] {
            &self.actions
        }

        fn adapters(&self) -> crate::app::mode::ModeAdapters {
            crate::app::mode::ModeAdapters::buffer()
        }

        fn input_keymap<'a>(
            &'a self,
            _content_state: &dyn crate::app::mode::ModeState,
            _view_state: &dyn crate::app::mode::ModeState,
            _context: &ModeViewContext<'_>,
        ) -> &'a Keymap<Command> {
            &self.keymap
        }
    }

    fn fixture() -> (
        Dispatcher,
        Scene,
        SpaceId,
        HashMap<ViewId, View>,
        ModeViewStore,
        ModeContentStore,
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
        let mut modes = ModeRegistry::new();
        modes.register(DispatcherTestMode::new()).unwrap();
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
        let mut view_modes = ModeViewStore::default();
        let mode = modes
            .instantiate(&ModeName::new("dispatcher-test"))
            .unwrap();
        let mut mode_contents = ModeContentStore::default();
        mode_contents.attach_view(editor, &mode);
        view_modes.insert(ViewId(0), mode);
        (
            Dispatcher::new(default_global_keymap()),
            scene,
            focused,
            views,
            view_modes,
            mode_contents,
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
                continuation: None,
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
                continuation: None,
            }
        );
    }

    #[test]
    fn mode_sequence_waits_then_resolves_to_the_view() {
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
                ..
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
        let g = KeyEvent::char('z');
        dispatcher
            .global_keymap
            .bind([g], Command::App(AppCommand::FocusNext));
        dispatcher.global_keymap.bind([g, g], Command::Noop);
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
                continuation: None,
            }
        );
    }

    #[test]
    fn mismatch_executes_shorter_complete_binding_before_replaying_new_key() {
        let (mut dispatcher, scene, focused, views, mut view_modes, mut content_modes, _, contents) =
            fixture();
        let g = KeyEvent::char('z');
        dispatcher
            .global_keymap
            .bind([g], Command::App(AppCommand::FocusNext));
        dispatcher.global_keymap.bind([g, g], Command::Noop);
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
                DispatchInput::Normal(KeyEvent::char('!')),
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
                replay: vec![DispatchInput::Normal(KeyEvent::char('!'))],
                continuation: None,
            }
        );
    }

    #[test]
    fn invalidating_a_view_discards_sequence_and_cancels_private_awaiting() {
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
            ViewId(0),
            &views[&ViewId(0)],
            ContentId(0),
            &mut view_modes,
            &mut content_modes,
            &contents,
        );

        assert!(!dispatcher.is_pending());
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
                ..
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
