use std::collections::VecDeque;
use std::future;
use std::io;
use std::time::Instant;

use crate::app::action::{TransactionIntent, ViewAction};
use crate::app::application::App;
#[cfg(test)]
use crate::app::behavior::EffectBehavior;
use crate::app::command::{AppCommand, ContentCommand, TransactionCommand};
use crate::app::dispatcher::{DispatchCommand, DispatchInput, DispatchOutcome, InputModeSnapshot};
use crate::app::execution::{ExecutionFrame, InputCheckpoint, PreparedEffect, StateRollback};
use crate::app::mode::{CursorDomain, InputFlow, ModeEffect, ResolvedViewEdit};
use crate::app::query::AppQuery;
use crate::app::transaction::{TransactionData, TransactionRecord, ViewTransactionData};
use crate::core::command::EditCommand;
use crate::core::content::{ContentActionResult, ContentEffect, ContentInput, ContentResult};
use crate::core::transaction::TransactionDirection;
use crate::frontend::Frontend;
use crate::protocol::content_query::RenderQuery;
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::{ContentId, ViewId};
use crate::protocol::revision::Revision;
use crate::protocol::viewport::{ViewportCommand, ViewportCursorBehavior, ViewportMoveDirection};

#[cfg(test)]
impl PreparedEffect {
    fn behavior(&self) -> EffectBehavior {
        match self {
            Self::HistoryCommit { content } => EffectBehavior::HistoryCommit { content: *content },
            Self::Save { content, snapshot } => EffectBehavior::Save {
                content: *content,
                bytes: snapshot.bytes.clone(),
                revision: snapshot.revision,
                state: snapshot.state,
            },
            Self::Viewport {
                view,
                command,
                lines,
            } => EffectBehavior::Viewport {
                view: *view,
                command: *command,
                lines: *lines,
            },
            Self::Quit => EffectBehavior::Quit,
        }
    }
}

async fn wait_for_input_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => future::pending::<()>().await,
    }
}

fn prepend_inputs(queue: &mut VecDeque<DispatchInput>, inputs: Vec<DispatchInput>) {
    for input in inputs.into_iter().rev() {
        queue.push_front(input);
    }
}

impl<F: Frontend> App<F> {
    fn prepare_effect(&mut self, frame: &mut ExecutionFrame, effect: PreparedEffect) {
        #[cfg(test)]
        self.behavior.record_prepared(effect.behavior());
        frame.prepare(effect);
    }

    pub async fn run(&mut self) -> io::Result<()> {
        let run_result = self.run_loop().await;
        let shutdown_result = self.shutdown_tasks().await;
        run_result.and(shutdown_result)
    }

    async fn run_loop(&mut self) -> io::Result<()> {
        self.kernel.schedule_mode_jobs();
        self.render()?;
        loop {
            let input_deadline = self
                .session
                .next_input_deadline(self.kernel.content_modes(), self.kernel.contents());
            let cancellation = self.kernel.cancellation_token();
            let should_render = tokio::select! {
                biased;
                _ = cancellation.cancelled() => break,
                _ = wait_for_input_deadline(input_deadline) => {
                    self.handle_input_timeout()?;
                    true
                }
                message = self.kernel.receive_message() => {
                    if let Some(message) = message {
                        self.handle_app_message(message)?
                    } else {
                        self.kernel.cancel();
                        false
                    }
                }
                ev = self.frontend.next_event() => {
                    match ev? {
                        Some(event) => self.handle_event(event).await?,
                        None => {
                            self.kernel.cancel();
                            false
                        }
                    }
                }
            };
            if should_render && !self.kernel.is_cancelled() {
                self.render()?;
            }
        }
        Ok(())
    }

    pub(super) async fn shutdown_tasks(&mut self) -> io::Result<()> {
        self.kernel.begin_shutdown();
        while self.kernel.has_pending_saves() {
            let message = self
                .kernel
                .receive_message()
                .await
                .expect("pending save task must report completion");
            self.handle_app_message(message)?;
        }
        self.kernel.close_critical_tasks();
        self.kernel.wait_for_critical_tasks().await;
        while let Some(message) = self.kernel.try_receive_message() {
            self.handle_app_message(message)?;
        }
        Ok(())
    }

    pub(super) async fn handle_event(&mut self, event: FrontendEvent) -> io::Result<bool> {
        let render = match event {
            FrontendEvent::Resize(r) => {
                self.session.resize(r.width, r.height);
                true
            }
            FrontendEvent::Key(k) => {
                self.process_input_queue(VecDeque::from([DispatchInput::Normal(k)]))?;
                true
            }
            FrontendEvent::QuitRequest => {
                self.kernel.cancel();
                false
            }
        };
        Ok(render)
    }

    fn process_input_queue(&mut self, mut queue: VecDeque<DispatchInput>) -> io::Result<()> {
        while !self.kernel.is_cancelled() {
            let Some(input) = queue.pop_front() else {
                break;
            };
            self.process_input_frame(input, &mut queue)?;
        }
        Ok(())
    }

    fn begin_execution_frame(
        &mut self,
        content: Option<ContentId>,
        input: Option<InputCheckpoint>,
    ) -> ExecutionFrame {
        self.kernel.start_command_transaction(content);
        ExecutionFrame::new(content, input)
    }

    fn checkpoint_target(&mut self, frame: &mut ExecutionFrame, content: ContentId) {
        if !frame.needs_target_checkpoint(content) {
            return;
        }
        let content_snapshot = self
            .kernel
            .snapshot_content(content)
            .expect("execution target content exists");
        let selection_snapshot = self.session.snapshot_selections(content);
        frame.record_target_checkpoint(content_snapshot, selection_snapshot);
    }

    fn finish_execution_frame<T>(
        &mut self,
        frame: ExecutionFrame,
        result: io::Result<T>,
    ) -> io::Result<T> {
        let success = result.is_ok();
        let (checkpoints, effects) = frame.into_parts();
        if !success {
            let (content, selections, input, state_rollbacks) = checkpoints.into_parts();
            for rollback in state_rollbacks.into_iter().rev() {
                match rollback {
                    StateRollback::ModeContent(mode, content, snapshot) => self
                        .kernel
                        .restore_mode_content_for(mode, content, snapshot),
                    StateRollback::ModeView(mode, view, snapshot) => {
                        self.session.restore_mode_view_for(mode, view, snapshot);
                    }
                    StateRollback::Text(record, direction) => {
                        let inverse = match direction {
                            TransactionDirection::Forward => TransactionDirection::Inverse,
                            TransactionDirection::Inverse => TransactionDirection::Forward,
                        };
                        self.kernel
                            .apply_transaction_record(&record, inverse)
                            .expect("runtime rollback data was already validated");
                    }
                }
            }
            if let Some(snapshot) = content {
                self.kernel.restore_content(snapshot);
            }
            if let Some(snapshot) = selections {
                self.session.restore_selections(snapshot);
            }
            if let Some(input) = input {
                self.restore_input_modes(input.modes);
                self.session.restore_input(input.dispatcher);
            }
        }
        self.kernel.finish_command_transaction(success);
        if success {
            self.publish_prepared_effects(effects);
            self.kernel.schedule_mode_jobs();
        }
        result
    }

    fn process_input_frame(
        &mut self,
        input: DispatchInput,
        outer_queue: &mut VecDeque<DispatchInput>,
    ) -> io::Result<()> {
        let input_snapshot = self.session.snapshot_input();
        let view = self.session.view_for_space(self.session.focused());
        let content = view.and_then(|view| self.session.view(view).map(|view| view.content()));
        let mode_snapshots = view.map_or_else(Vec::new, |view| {
            let content = self.session.views()[&view].content();
            let modes = self.session.view_modes().mode_ids(view);
            let mut snapshots: Vec<_> = self
                .kernel
                .content_modes()
                .snapshots(content, modes)
                .into_iter()
                .map(|(mode, snapshot)| InputModeSnapshot::Content(mode, content, snapshot))
                .collect();
            snapshots.extend(
                self.session
                    .view_modes()
                    .snapshots(view)
                    .into_iter()
                    .map(|(mode, snapshot)| InputModeSnapshot::View(mode, view, snapshot)),
            );
            snapshots
        });
        let mut frame = self.begin_execution_frame(
            content,
            Some(InputCheckpoint {
                dispatcher: input_snapshot,
                modes: mode_snapshots,
            }),
        );
        let mut queue = VecDeque::from([input]);

        let mut result = Ok(());
        while result.is_ok() && !self.kernel.is_cancelled() {
            let Some(input) = queue.pop_front() else {
                break;
            };
            let now = Instant::now();
            let (contents, mode_contents) = self.kernel.mode_runtime_parts();
            let (outcome, _, mode_revisions) =
                self.session.dispatch(input, now, mode_contents, contents);
            match outcome {
                DispatchOutcome::Waiting | DispatchOutcome::Consumed => {}
                DispatchOutcome::Replay(replay) => {
                    if let Err(error) = frame.consume_replayed_inputs(replay.len()) {
                        result = Err(error);
                    } else {
                        prepend_inputs(&mut queue, replay);
                    }
                }
                DispatchOutcome::Emit {
                    command,
                    replay,
                    continuation,
                } => match self.execute_command_inner(command, &mut frame) {
                    Ok(flow) => {
                        self.session.sync_focused_input(
                            now,
                            self.kernel.content_modes(),
                            self.kernel.contents(),
                        );
                        if let Err(error) = frame.consume_replayed_inputs(replay.len()) {
                            result = Err(error);
                        } else {
                            prepend_inputs(&mut queue, replay);
                            if flow == InputFlow::Continue
                                && let Some(continuation) = continuation
                            {
                                queue.push_front(continuation);
                            }
                        }
                    }
                    Err(error) => result = Err(error),
                },
            }
            if result.is_ok() {
                self.touch_unchanged_mode_views(&mode_revisions);
            }
        }

        if result.is_ok()
            && let (Some(view), Some(content)) = (view, content)
            && self
                .session
                .cursor_domain(view, self.kernel.content_modes(), self.kernel.contents())
                == CursorDomain::Character
        {
            result = self.execute_edit(
                EditCommand::ClampCursorToCharacter,
                view,
                content,
                &mut frame,
            );
        }
        let result = self.finish_execution_frame(frame, result);
        if result.is_ok() {
            outer_queue.extend(queue);
        }
        result
    }

    fn publish_prepared_effects(&mut self, effects: Vec<PreparedEffect>) {
        for effect in effects {
            #[cfg(test)]
            self.behavior.record_published(effect.behavior());
            match effect {
                PreparedEffect::HistoryCommit { content } => {
                    self.kernel.commit_transaction(content);
                }
                PreparedEffect::Save { content, snapshot } => {
                    self.kernel.queue_save(content, snapshot);
                }
                PreparedEffect::Viewport {
                    view,
                    command,
                    lines,
                } => {
                    self.frontend.apply_viewport_command(view, command, lines);
                }
                PreparedEffect::Quit => self.kernel.cancel(),
            }
        }
    }

    fn apply_dispatch_outcome(
        &mut self,
        outcome: DispatchOutcome,
        queue: &mut VecDeque<DispatchInput>,
        now: Instant,
        frame: &mut ExecutionFrame,
    ) -> io::Result<()> {
        match outcome {
            DispatchOutcome::Waiting | DispatchOutcome::Consumed => {}
            DispatchOutcome::Replay(replay) => {
                frame.consume_replayed_inputs(replay.len())?;
                prepend_inputs(queue, replay);
            }
            DispatchOutcome::Emit {
                command,
                replay,
                continuation,
            } => {
                let flow = self.execute_command_in_frame(command, true, frame)?;
                self.session.sync_focused_input(
                    now,
                    self.kernel.content_modes(),
                    self.kernel.contents(),
                );
                frame.consume_replayed_inputs(replay.len())?;
                prepend_inputs(queue, replay);
                if flow == InputFlow::Continue
                    && let Some(continuation) = continuation
                {
                    queue.push_front(continuation);
                }
            }
        }
        Ok(())
    }

    pub(super) fn handle_input_timeout(&mut self) -> io::Result<()> {
        loop {
            let now = Instant::now();
            if self
                .session
                .next_input_deadline(self.kernel.content_modes(), self.kernel.contents())
                .is_none_or(|deadline| deadline > now)
            {
                return Ok(());
            }
            let input_snapshot = self.session.snapshot_input();
            let (contents, content_modes) = self.kernel.mode_runtime_parts();
            let (outcome, mode_snapshots, mode_revisions) =
                self.session.dispatch_timeout(now, content_modes, contents);
            let content = match &outcome {
                DispatchOutcome::Emit { command, .. } => command.content(),
                DispatchOutcome::Waiting
                | DispatchOutcome::Consumed
                | DispatchOutcome::Replay(_) => self
                    .session
                    .view_for_space(self.session.focused())
                    .and_then(|view| self.session.view(view).map(|view| view.content())),
            };
            let mut frame = self.begin_execution_frame(
                content,
                Some(InputCheckpoint {
                    dispatcher: input_snapshot,
                    modes: mode_snapshots,
                }),
            );
            let mut replay = VecDeque::new();
            let result = self.apply_dispatch_outcome(outcome, &mut replay, now, &mut frame);
            self.finish_execution_frame(frame, result)?;
            self.touch_unchanged_mode_views(&mode_revisions);
            self.process_input_queue(replay)?;
        }
    }

    fn restore_input_modes(&mut self, snapshots: Vec<InputModeSnapshot>) {
        for snapshot in snapshots.into_iter().rev() {
            match snapshot {
                InputModeSnapshot::Content(mode, content, snapshot) => {
                    self.kernel
                        .restore_mode_content_for(mode, content, snapshot);
                }
                InputModeSnapshot::View(mode, view, snapshot) => {
                    self.session.restore_mode_view_for(mode, view, snapshot);
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn execute_command(&mut self, command: DispatchCommand) -> io::Result<()> {
        let content = command.content();
        let mut frame = self.begin_execution_frame(content, None);
        let result = self.execute_command_in_frame(command, false, &mut frame);
        self.finish_execution_frame(frame, result).map(|_| ())
    }

    fn execute_command_in_frame(
        &mut self,
        command: DispatchCommand,
        enforce_cursor_domain: bool,
        frame: &mut ExecutionFrame,
    ) -> io::Result<InputFlow> {
        let content = command.content();
        let view = command.view();
        let mut result = self.execute_command_inner(command, frame);
        if enforce_cursor_domain
            && result.is_ok()
            && let (Some(view), Some(content)) = (view, content)
            && self
                .session
                .cursor_domain(view, self.kernel.content_modes(), self.kernel.contents())
                == CursorDomain::Character
        {
            let flow = *result.as_ref().expect("checked successful result");
            result = self
                .execute_edit(EditCommand::ClampCursorToCharacter, view, content, frame)
                .map(|_| flow);
        }
        result
    }

    fn execute_command_inner(
        &mut self,
        command: DispatchCommand,
        frame: &mut ExecutionFrame,
    ) -> io::Result<InputFlow> {
        let mut command = command;
        let mut mode_revisions: Vec<(ViewId, Revision)> = Vec::new();
        let mut input_flow = InputFlow::Stop;

        loop {
            if let Err(error) = frame.consume_operation() {
                self.touch_unchanged_mode_views(&mode_revisions);
                return Err(error);
            }
            let next = match command {
                DispatchCommand::App(command) => {
                    match command {
                        AppCommand::Quit => self.prepare_effect(frame, PreparedEffect::Quit),
                        AppCommand::FocusNext | AppCommand::FocusPrev => {}
                    }
                    None
                }
                DispatchCommand::Content { command, content } => {
                    let active_owner = matches!(command, ContentCommand::Save)
                        .then(|| self.kernel.active_transaction_owner(content))
                        .flatten();
                    if active_owner.is_some() {
                        self.kernel.commit_transaction(content);
                    }
                    self.checkpoint_target(frame, content);
                    let result = self.kernel.execute(content, ContentInput::Save);
                    if let ContentResult::Handled(outcome) = result
                        && let ContentEffect::Save(snapshot) = outcome.effect
                    {
                        self.prepare_effect(frame, PreparedEffect::Save { content, snapshot });
                    }
                    if let Some(owner) = active_owner {
                        self.kernel.begin_transaction(content, owner);
                    }
                    None
                }
                DispatchCommand::ContentWithView {
                    command,
                    view,
                    content,
                } => {
                    self.execute_view_content_command(command, view, content, frame)?;
                    None
                }
                DispatchCommand::Mode {
                    command,
                    view,
                    content,
                } => {
                    let scope = self
                        .kernel
                        .modes()
                        .command_scope(&command.mode, &command.action)
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                    let mode = self
                        .kernel
                        .modes()
                        .resolve_mode(&command.mode)
                        .expect("validated mode exists");
                    if scope == crate::app::mode::ModeActionScope::Content {
                        if !frame.has_mode_content_checkpoint(mode, content)
                            && let Some(snapshot) =
                                self.kernel.snapshot_mode_content_for(mode, content)
                        {
                            frame.record_state_rollback(StateRollback::ModeContent(
                                mode, content, snapshot,
                            ));
                        }
                        let result = self
                            .kernel
                            .execute_mode_content_action(content, &command)
                            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                        let (flow, operations) = result.into_parts();
                        input_flow = flow;
                        self.execute_mode_content_effects(operations, content, Some(view), frame)?;
                        None
                    } else {
                        if !frame.has_mode_view_checkpoint(mode, view)
                            && let Some(snapshot) = self.session.snapshot_mode_view_for(mode, view)
                        {
                            frame.record_state_rollback(StateRollback::ModeView(
                                mode, view, snapshot,
                            ));
                        }
                        let target_view = self.session.view(view).expect("target view exists");
                        assert_eq!(
                            target_view.content(),
                            content,
                            "view/content target mismatch"
                        );
                        let revision_before = target_view.revision();
                        let (contents, modes, mode_contents) = self.kernel.mode_attachment_parts();
                        let result = self
                            .session
                            .execute_mode(view, modes, contents, &command, mode_contents)
                            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                        let (flow, operations) = result.into_parts();
                        input_flow = flow;
                        if !mode_revisions.iter().any(|(recorded, _)| *recorded == view) {
                            mode_revisions.push((view, revision_before));
                        }
                        self.execute_view_mode_operations(operations, view, content, frame)?;
                        None
                    }
                }
                DispatchCommand::Viewport {
                    command,
                    view,
                    content,
                } => {
                    let lines = self.frontend.resolve_viewport_command(
                        self.session.scene(),
                        self.session.scene_revision(),
                        view,
                        command,
                    )?;
                    if lines == 0 {
                        None
                    } else {
                        self.prepare_effect(
                            frame,
                            PreparedEffect::Viewport {
                                view,
                                command,
                                lines,
                            },
                        );
                        Some(DispatchCommand::ContentWithView {
                            command: ContentCommand::Edit(viewport_cursor_edit(command, lines)),
                            view,
                            content,
                        })
                    }
                }
                DispatchCommand::ModeContentEffects { effects, content } => {
                    self.execute_mode_content_effects(effects, content, None, frame)?;
                    None
                }
                DispatchCommand::ModeEffects {
                    operations,
                    view,
                    content,
                } => {
                    self.execute_view_mode_operations(operations, view, content, frame)?;
                    None
                }
                DispatchCommand::Noop => None,
            };

            let Some(next) = next else {
                self.touch_unchanged_mode_views(&mode_revisions);
                return Ok(input_flow);
            };
            command = next;
        }
    }

    fn execute_view_content_command(
        &mut self,
        command: ContentCommand,
        view: ViewId,
        content: ContentId,
        frame: &mut ExecutionFrame,
    ) -> io::Result<()> {
        if let ContentCommand::Sequence(commands) = command {
            for command in commands.into_commands() {
                frame.consume_operation()?;
                self.execute_view_content_command(command, view, content, frame)?;
            }
            return Ok(());
        }

        let target_view = self.session.view(view).expect("target view exists");
        assert_eq!(
            target_view.content(),
            content,
            "view/content target mismatch"
        );

        match command {
            ContentCommand::Edit(command) => self.execute_edit(command, view, content, frame),
            ContentCommand::Transaction(command) => {
                match command {
                    TransactionCommand::Begin => {
                        self.kernel.begin_transaction(content, Some(view));
                    }
                    TransactionCommand::Commit => {
                        self.prepare_effect(frame, PreparedEffect::HistoryCommit { content });
                    }
                    TransactionCommand::Rollback => {
                        if let Some(record) = self.kernel.rollback_transaction(content) {
                            self.apply_history_record(
                                &record,
                                TransactionDirection::Inverse,
                                frame,
                            )?;
                        }
                    }
                }
                Ok(())
            }
            ContentCommand::Undo | ContentCommand::Redo => {
                self.kernel.commit_transaction(content);
                let record = if matches!(command, ContentCommand::Undo) {
                    self.kernel.undo_transaction(content)
                } else {
                    self.kernel.redo_transaction(content)
                };
                if let Some(record) = record {
                    let direction = if matches!(command, ContentCommand::Undo) {
                        TransactionDirection::Inverse
                    } else {
                        TransactionDirection::Forward
                    };
                    self.apply_history_record(&record, direction, frame)?;
                }
                Ok(())
            }
            ContentCommand::Save | ContentCommand::Sequence(_) => Ok(()),
        }
    }

    fn execute_edit(
        &mut self,
        command: EditCommand,
        view: ViewId,
        content: ContentId,
        frame: &mut ExecutionFrame,
    ) -> io::Result<()> {
        let before = self
            .session
            .view(view)
            .and_then(|view| view.selections())
            .expect("editable view has selections")
            .clone();
        let plan = self
            .kernel
            .plan_edit(content, command, &before)
            .expect("editable content plans edits");
        self.apply_resolved_view_edit(
            ResolvedViewEdit {
                content: plan.action,
                view: Some(ViewAction::SetSelections(plan.selections)),
                before,
            },
            view,
            content,
            frame,
        )
    }

    fn apply_resolved_view_edit(
        &mut self,
        edit: ResolvedViewEdit,
        view: ViewId,
        content: ContentId,
        frame: &mut ExecutionFrame,
    ) -> io::Result<()> {
        let ResolvedViewEdit {
            content: content_action,
            view: view_action,
            before,
        } = edit;
        if self.session.view(view).and_then(|view| view.selections()) != Some(&before) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "stale resolved view edit",
            ));
        }
        let Some(action) = content_action else {
            if let Some(action) = view_action {
                self.apply_view_action(view, action, frame)?;
            }
            return Ok(());
        };

        self.checkpoint_target(frame, content);
        let implicit = self.kernel.active_transaction_owner(content) != Some(Some(view));
        if implicit {
            self.kernel.begin_transaction(content, Some(view));
        }
        let result = self.kernel.apply_content_action(content, action);
        let ContentActionResult::Handled {
            outcome,
            transaction,
        } = result
        else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "content rejected a planned edit",
            ));
        };

        match view_action {
            Some(action) => {
                self.apply_view_action(view, action, frame)?;
                if let Some(change) = &outcome.change {
                    self.session.transform_content_views(
                        self.kernel.contents(),
                        content,
                        Some(view),
                        change,
                    );
                }
            }
            None => {
                if let Some(change) = &outcome.change {
                    self.session.transform_content_views(
                        self.kernel.contents(),
                        content,
                        None,
                        change,
                    );
                }
            }
        }
        if let Some(change) = &outcome.change {
            self.notify_mode_content_changed(content, change);
        }
        if let Some(transaction) = transaction {
            let after = self
                .session
                .view(view)
                .and_then(|view| view.selections())
                .expect("editable view has selections")
                .clone();
            let record = TransactionRecord {
                target: content,
                data: TransactionData {
                    content: transaction,
                    view: ViewTransactionData::Source {
                        view,
                        before,
                        after,
                    },
                },
            };
            frame.record_state_rollback(StateRollback::Text(
                record.clone(),
                TransactionDirection::Forward,
            ));
            self.kernel.record_transaction(record).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid outer transaction: {error:?}"),
                )
            })?;
        }
        self.handle_content_result(content, ContentResult::Handled(outcome));
        if implicit {
            self.kernel.commit_transaction(content);
        }
        Ok(())
    }

    fn execute_view_mode_operations(
        &mut self,
        operations: Vec<ModeEffect>,
        view: ViewId,
        content: ContentId,
        frame: &mut ExecutionFrame,
    ) -> io::Result<()> {
        for operation in operations {
            frame.consume_operation()?;
            match operation {
                ModeEffect::Edit(edit) => {
                    self.apply_resolved_view_edit(edit, view, content, frame)?;
                }
                ModeEffect::DeferredEdit(command) => {
                    self.execute_edit(command, view, content, frame)?;
                }
                ModeEffect::View(action) => {
                    self.apply_view_action(view, action, frame)?;
                }
                ModeEffect::Content(action) => {
                    let selections = self
                        .session
                        .view(view)
                        .and_then(|view| view.selections())
                        .expect("editable mode view has selections")
                        .clone();
                    self.apply_resolved_view_edit(
                        ResolvedViewEdit {
                            content: Some(action),
                            view: None,
                            before: selections,
                        },
                        view,
                        content,
                        frame,
                    )?;
                }
                ModeEffect::Transaction(intent) => {
                    self.execute_transaction_intent(intent, Some(view), content, frame)?;
                }
                ModeEffect::App(command) => {
                    self.execute_command_inner(DispatchCommand::App(command), frame)?;
                }
                ModeEffect::Mode(command) => {
                    frame.consume_nested_mode_call()?;
                    self.execute_command_inner(
                        DispatchCommand::Mode {
                            command,
                            view,
                            content,
                        },
                        frame,
                    )?;
                }
                ModeEffect::Viewport(command) => {
                    self.execute_command_inner(
                        DispatchCommand::Viewport {
                            command,
                            view,
                            content,
                        },
                        frame,
                    )?;
                }
                ModeEffect::Save => {
                    self.execute_command_inner(
                        DispatchCommand::Content {
                            command: ContentCommand::Save,
                            content,
                        },
                        frame,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn execute_mode_content_effects(
        &mut self,
        effects: Vec<ModeEffect>,
        content: ContentId,
        source_view: Option<ViewId>,
        frame: &mut ExecutionFrame,
    ) -> io::Result<()> {
        for effect in effects {
            frame.consume_operation()?;
            match effect {
                ModeEffect::Content(action) => {
                    self.execute_content_action(action, content, frame)?;
                }
                ModeEffect::Transaction(intent) => {
                    self.execute_transaction_intent(intent, None, content, frame)?;
                }
                ModeEffect::Save => {
                    self.execute_command_inner(
                        DispatchCommand::Content {
                            command: ContentCommand::Save,
                            content,
                        },
                        frame,
                    )?;
                }
                ModeEffect::Mode(command) => {
                    frame.consume_nested_mode_call()?;
                    let view = source_view.ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "content-scoped mode command needs a source view",
                        )
                    })?;
                    self.execute_command_inner(
                        DispatchCommand::Mode {
                            command,
                            view,
                            content,
                        },
                        frame,
                    )?;
                }
                ModeEffect::App(command) => {
                    self.execute_command_inner(DispatchCommand::App(command), frame)?;
                }
                ModeEffect::Edit(_)
                | ModeEffect::DeferredEdit(_)
                | ModeEffect::View(_)
                | ModeEffect::Viewport(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "content-scoped mode emitted a view-scoped effect",
                    ));
                }
            }
        }
        Ok(())
    }

    fn execute_content_action(
        &mut self,
        action: crate::core::action::ContentAction,
        content: ContentId,
        frame: &mut ExecutionFrame,
    ) -> io::Result<()> {
        self.checkpoint_target(frame, content);
        let implicit = self.kernel.active_transaction_owner(content) != Some(None);
        if implicit {
            self.kernel.begin_transaction(content, None);
        }
        let ContentActionResult::Handled {
            outcome,
            transaction,
        } = self.kernel.apply_content_action(content, action)
        else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "content rejected its mode action",
            ));
        };
        if let Some(change) = &outcome.change {
            self.session
                .transform_content_views(self.kernel.contents(), content, None, change);
            self.notify_mode_content_changed(content, change);
        }
        if let Some(transaction) = transaction {
            let record = TransactionRecord {
                target: content,
                data: TransactionData {
                    content: transaction,
                    view: ViewTransactionData::None,
                },
            };
            frame.record_state_rollback(StateRollback::Text(
                record.clone(),
                TransactionDirection::Forward,
            ));
            self.kernel.record_transaction(record).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid outer transaction: {error:?}"),
                )
            })?;
        }
        self.handle_content_result(content, ContentResult::Handled(outcome));
        if implicit {
            self.kernel.commit_transaction(content);
        }
        Ok(())
    }

    fn execute_transaction_intent(
        &mut self,
        intent: TransactionIntent,
        owner: Option<ViewId>,
        content: ContentId,
        frame: &mut ExecutionFrame,
    ) -> io::Result<()> {
        match intent {
            TransactionIntent::Begin => {
                self.kernel.begin_transaction(content, owner);
            }
            TransactionIntent::Commit => {
                self.prepare_effect(frame, PreparedEffect::HistoryCommit { content });
            }
            TransactionIntent::Rollback => {
                if let Some(record) = self.kernel.rollback_transaction(content) {
                    self.apply_history_record(&record, TransactionDirection::Inverse, frame)?;
                }
            }
            TransactionIntent::Undo | TransactionIntent::Redo => {
                self.kernel.commit_transaction(content);
                let record = if intent == TransactionIntent::Undo {
                    self.kernel.undo_transaction(content)
                } else {
                    self.kernel.redo_transaction(content)
                };
                if let Some(record) = record {
                    let direction = if intent == TransactionIntent::Undo {
                        TransactionDirection::Inverse
                    } else {
                        TransactionDirection::Forward
                    };
                    self.apply_history_record(&record, direction, frame)?;
                }
            }
        }
        Ok(())
    }

    fn apply_history_record(
        &mut self,
        record: &TransactionRecord,
        direction: TransactionDirection,
        frame: &mut ExecutionFrame,
    ) -> io::Result<()> {
        self.checkpoint_target(frame, record.target);
        let source = match &record.data.view {
            ViewTransactionData::Source {
                view,
                before,
                after,
            } => Some((
                *view,
                match direction {
                    TransactionDirection::Forward => after.clone(),
                    TransactionDirection::Inverse => before.clone(),
                },
            )),
            ViewTransactionData::None => None,
        };
        let change = self
            .kernel
            .apply_transaction_record(record, direction)
            .map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid history traversal: {error:?}"),
                )
            })?;
        frame.record_state_rollback(StateRollback::Text(record.clone(), direction));
        if let Some((view, selections)) = &source
            && self
                .session
                .view(*view)
                .is_some_and(|data| data.content() == record.target)
        {
            self.apply_view_action(*view, ViewAction::SetSelections(selections.clone()), frame)?;
        }
        if let Some(change) = &change {
            self.session.transform_content_views(
                self.kernel.contents(),
                record.target,
                source.as_ref().map(|(view, _)| *view),
                change,
            );
            self.notify_mode_content_changed(record.target, change);
        }
        Ok(())
    }

    fn notify_mode_content_changed(
        &mut self,
        content: ContentId,
        change: &crate::core::content::ContentChange,
    ) {
        let (contents, mode_contents) = self.kernel.mode_runtime_parts();
        mode_contents.notify_changed(content, contents, change);
        self.session
            .notify_mode_content_changed(content, mode_contents, contents, change);
    }

    fn apply_view_action(
        &mut self,
        view: ViewId,
        action: ViewAction,
        frame: &mut ExecutionFrame,
    ) -> io::Result<()> {
        let content = self
            .session
            .view(view)
            .expect("target view exists")
            .content();
        self.checkpoint_target(frame, content);
        self.session
            .apply_view_action(view, action, self.kernel.contents())
            .map(|_| ())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid view action"))
    }

    fn touch_unchanged_mode_views(&mut self, revisions: &[(ViewId, Revision)]) {
        for &(view, revision_before) in revisions {
            let target_view = self.session.view_mut(view).expect("target view exists");
            if target_view.revision() == revision_before {
                target_view.touch();
            }
        }
    }

    pub(super) fn render(&mut self) -> io::Result<()> {
        let query = AppQuery {
            contents: self.kernel.contents(),
            views: self.session.views(),
            view_modes: self.session.view_modes(),
            mode_contents: self.kernel.content_modes(),
            faces: self.session.faces(),
        };
        self.frontend.render(
            self.session.scene(),
            self.session.scene_revision(),
            &query as &dyn RenderQuery,
            self.session.focused(),
        )
    }
}

fn viewport_cursor_edit(command: ViewportCommand, lines: usize) -> EditCommand {
    match (command.direction, command.cursor_behavior) {
        (ViewportMoveDirection::Up, ViewportCursorBehavior::Move) => EditCommand::MoveUpBy(lines),
        (ViewportMoveDirection::Down, ViewportCursorBehavior::Move) => {
            EditCommand::MoveDownBy(lines)
        }
        (ViewportMoveDirection::Up, ViewportCursorBehavior::Extend) => {
            EditCommand::ExtendUpBy(lines)
        }
        (ViewportMoveDirection::Down, ViewportCursorBehavior::Extend) => {
            EditCommand::ExtendDownBy(lines)
        }
    }
}
