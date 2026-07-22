use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::future;
use std::io;
use std::time::Instant;

use crate::action::{TransactionIntent, ViewAction};
use crate::application::App;
#[cfg(test)]
use crate::behavior::EffectBehavior;
use crate::command::AppCommand;
use crate::diagnostics::RuntimeDiagnostic;
use crate::dispatcher::{DispatchCommand, DispatchInput, DispatchOutcome};
use crate::execution::{ExecutionFrame, InputCheckpoint, PreparedEffect, StateRollback};
use crate::layout::LayoutError;
use crate::mode::{CursorDomain, InputFlow};
use crate::operation::{
    AppOperation, ContentOperation, ContentTarget, FaceOperation, FaceRemapTarget,
    ModeFlowPropagation, ModeTarget, OperationError, OperationOrigin, OperationOriginScope,
    OperationRequest, QueuedOperation, ResolvedModeScope, ResolvedOperation, ViewEditPlan,
    ViewOperation, ViewPrecondition, ViewTarget, adapt_dispatch_command, prepend_operations,
};
use crate::query::AppQuery;
use crate::theme::{FaceRemapOwner, ResolvedFaceOperation};
use crate::transaction::{TransactionData, TransactionRecord, ViewTransactionData};
use vell_core::command::EditCommand;
use vell_core::content::{ContentActionResult, ContentEffect, ContentInput, ContentResult};
use vell_core::transaction::TransactionDirection;
use vell_frontend::Frontend;
use vell_protocol::content_query::{ContentData, ContentQuery, RenderQuery};
use vell_protocol::frontend_event::FrontendEvent;
use vell_protocol::ids::{ContentId, ViewId};
use vell_protocol::viewport::{
    ResolvedViewportCommand, ViewportCommand, ViewportCursorBehavior, ViewportMoveDirection,
};

const MAX_RUNTIME_DIAGNOSTICS: usize = 128;

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
            Self::Viewport { view, command } => EffectBehavior::Viewport {
                view: *view,
                command: *command,
            },
            Self::Split {
                target,
                content,
                direction,
            } => EffectBehavior::Split {
                target: *target,
                content: *content,
                direction: *direction,
            },
            Self::Close { target } => EffectBehavior::Close { target: *target },
            Self::Focus { target } => EffectBehavior::Focus { target: *target },
            Self::Face(_) => EffectBehavior::Face,
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

fn operation_error(error: OperationError) -> io::Error {
    recoverable_execution_error(io::ErrorKind::InvalidData, error)
}

fn invalid_operation(message: impl Into<String>) -> io::Error {
    operation_error(OperationError::new(message))
}

#[derive(Debug)]
struct RecoverableExecutionError {
    source: Box<dyn Error + Send + Sync>,
}

impl fmt::Display for RecoverableExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.source.as_ref(), formatter)
    }
}

impl Error for RecoverableExecutionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

fn recoverable_execution_error(
    kind: io::ErrorKind,
    error: impl Error + Send + Sync + 'static,
) -> io::Error {
    io::Error::new(
        kind,
        RecoverableExecutionError {
            source: Box::new(error),
        },
    )
}

fn recoverable_message(kind: io::ErrorKind, message: impl Into<String>) -> io::Error {
    recoverable_execution_error(kind, OperationError::new(message))
}

impl<F: Frontend> App<F> {
    fn record_recoverable_error(&mut self, error: io::Error) {
        if self.runtime_diagnostics.len() >= MAX_RUNTIME_DIAGNOSTICS {
            self.runtime_diagnostics.remove(0);
        }
        self.runtime_diagnostics.push(RuntimeDiagnostic {
            message: error.to_string(),
        });
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
    }

    fn prepare_effect(&mut self, frame: &mut ExecutionFrame, effect: PreparedEffect) {
        #[cfg(test)]
        self.behavior.record_prepared(effect.behavior());
        frame.prepare(effect);
    }

    fn prepare_face_effect(
        &mut self,
        frame: &mut ExecutionFrame,
        operation: ResolvedFaceOperation,
    ) -> io::Result<()> {
        frame.prepare_face(operation).map_err(operation_error)?;
        #[cfg(test)]
        self.behavior.record_prepared(EffectBehavior::Face);
        Ok(())
    }

    fn prepare_topology_effect(
        &mut self,
        frame: &mut ExecutionFrame,
        effect: PreparedEffect,
    ) -> io::Result<()> {
        #[cfg(test)]
        let behavior = effect.behavior();
        frame.prepare_topology(effect).map_err(operation_error)?;
        #[cfg(test)]
        self.behavior.record_prepared(behavior);
        Ok(())
    }

    fn prepare_viewport_effect(
        &mut self,
        frame: &mut ExecutionFrame,
        effect: PreparedEffect,
    ) -> io::Result<()> {
        #[cfg(test)]
        let behavior = effect.behavior();
        frame.prepare_viewport(effect).map_err(operation_error)?;
        #[cfg(test)]
        self.behavior.record_prepared(behavior);
        Ok(())
    }

    pub async fn run(&mut self) -> io::Result<()> {
        let run_result = self.run_loop().await;
        let shutdown_result = self.shutdown_tasks().await;
        run_result.and(shutdown_result)
    }

    async fn run_loop(&mut self) -> io::Result<()> {
        self.kernel.schedule_mode_jobs();
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
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
                    match self.handle_input_timeout() {
                        Ok(()) => true,
                        Err(error) if is_recoverable_execution_error(&error) => {
                            self.record_recoverable_error(error);
                            true
                        }
                        Err(error) => return Err(error),
                    }
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
                        Some(event) => match self.handle_event(event).await {
                            Ok(render) => render,
                            Err(error) if is_recoverable_execution_error(&error) => {
                                self.record_recoverable_error(error);
                                true
                            }
                            Err(error) => return Err(error),
                        },
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
        let (checkpoints, mut mode_drafts, view_touches, effects) = frame.into_parts();
        if !success {
            let (content, selections, input, state_rollbacks) = checkpoints.into_parts();
            for rollback in state_rollbacks.into_iter().rev() {
                match rollback {
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
                self.session.restore_input(input.dispatcher);
            }
        }
        if success {
            self.kernel.commit_mode_drafts(&mut mode_drafts);
            self.session.commit_mode_drafts(&mut mode_drafts);
            self.session.commit_view_touches(view_touches);
        } else {
            mode_drafts.commit_faults(
                self.kernel.content_modes_mut(),
                self.session.view_modes_mut(),
            );
        }
        self.kernel.finish_command_transaction(success);
        if success {
            self.publish_prepared_effects(effects);
            self.kernel.schedule_mode_jobs();
            self.session
                .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
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
        let mut frame = self.begin_execution_frame(
            content,
            Some(InputCheckpoint {
                dispatcher: input_snapshot,
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
            let (outcome, mode_revisions) =
                self.session
                    .dispatch(input, now, mode_contents, contents, frame.mode_drafts_mut());
            match outcome {
                DispatchOutcome::Waiting | DispatchOutcome::Consumed => {}
                DispatchOutcome::Replay(replay) => {
                    if let Err(error) = frame.consume_replayed_inputs(replay.len()) {
                        result = Err(operation_error(error));
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
                        self.session.sync_focused_input_in_draft(
                            now,
                            self.kernel.content_modes(),
                            self.kernel.contents(),
                            frame.mode_drafts_mut(),
                        );
                        if let Err(error) = frame.consume_replayed_inputs(replay.len()) {
                            result = Err(operation_error(error));
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
                for (view, revision) in mode_revisions {
                    frame.record_view_touch(view, revision);
                }
            }
        }

        if result.is_ok()
            && let (Some(view), Some(content)) = (view, content)
            && self.session.cursor_domain_in_draft(
                view,
                self.kernel.content_modes(),
                self.kernel.contents(),
                frame.mode_drafts_mut(),
            ) == CursorDomain::Character
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
                PreparedEffect::Viewport { view, command } => {
                    self.frontend.apply_viewport_command(view, command);
                }
                PreparedEffect::Split {
                    target,
                    content,
                    direction,
                } => {
                    self.split_space(target, content, true, direction, true)
                        .expect("validated split remains valid until frame commit");
                }
                PreparedEffect::Close { target } => {
                    self.close_space(target)
                        .expect("validated close remains valid until frame commit");
                }
                PreparedEffect::Focus { target } => {
                    let (contents, content_modes) = self.kernel.mode_runtime_parts();
                    self.session
                        .focus_space(target, content_modes, contents)
                        .expect("validated focus target remains valid until frame commit");
                }
                PreparedEffect::Face(operation) => {
                    self.session
                        .faces_mut()
                        .apply_operation(operation)
                        .expect("validated face operation remains valid until frame commit");
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
                frame
                    .consume_replayed_inputs(replay.len())
                    .map_err(operation_error)?;
                prepend_inputs(queue, replay);
            }
            DispatchOutcome::Emit {
                command,
                replay,
                continuation,
            } => {
                let flow = self.execute_command_in_frame(command, true, frame)?;
                self.session.sync_focused_input_in_draft(
                    now,
                    self.kernel.content_modes(),
                    self.kernel.contents(),
                    frame.mode_drafts_mut(),
                );
                frame
                    .consume_replayed_inputs(replay.len())
                    .map_err(operation_error)?;
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
            let content = self
                .session
                .view_for_space(self.session.focused())
                .and_then(|view| self.session.view(view).map(|view| view.content()));
            let mut frame = self.begin_execution_frame(
                content,
                Some(InputCheckpoint {
                    dispatcher: input_snapshot,
                }),
            );
            let (contents, content_modes) = self.kernel.mode_runtime_parts();
            let (outcome, mode_revisions) = self.session.dispatch_timeout(
                now,
                content_modes,
                contents,
                frame.mode_drafts_mut(),
            );
            for (view, revision) in mode_revisions {
                frame.record_view_touch(view, revision);
            }
            let mut replay = VecDeque::new();
            let result = self.apply_dispatch_outcome(outcome, &mut replay, now, &mut frame);
            self.finish_execution_frame(frame, result)?;
            self.process_input_queue(replay)?;
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
            && self.session.cursor_domain_in_draft(
                view,
                self.kernel.content_modes(),
                self.kernel.contents(),
                frame.mode_drafts_mut(),
            ) == CursorDomain::Character
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
        let operations = adapt_dispatch_command(command).map_err(operation_error)?;
        self.execute_operation_queue(VecDeque::from(operations), frame)
    }

    fn execute_operation_queue(
        &mut self,
        mut queue: VecDeque<QueuedOperation>,
        frame: &mut ExecutionFrame,
    ) -> io::Result<InputFlow> {
        let mut input_flow = InputFlow::Stop;

        while let Some(queued) = queue.pop_front() {
            frame.consume_operation().map_err(operation_error)?;
            let origin = queued.origin;
            let operation = self.resolve_operation(queued)?;
            let result = match operation {
                ResolvedOperation::App(AppOperation::Command(command)) => {
                    match command {
                        AppCommand::Quit => self.prepare_effect(frame, PreparedEffect::Quit),
                        AppCommand::Close => {
                            let target = self.session.focused();
                            match self.session.validate_close_space(target) {
                                Ok(()) => self.prepare_topology_effect(
                                    frame,
                                    PreparedEffect::Close { target },
                                )?,
                                Err(LayoutError::WouldRemoveLastFocusable(_)) => {
                                    self.prepare_topology_effect(frame, PreparedEffect::Quit)?
                                }
                                Err(error) => {
                                    return Err(recoverable_execution_error(
                                        io::ErrorKind::InvalidInput,
                                        error,
                                    ));
                                }
                            }
                        }
                        AppCommand::FocusNext | AppCommand::FocusPrev => {}
                        AppCommand::Split(direction) => {
                            let target = self.session.focused();
                            let view = self
                                .session
                                .view_for_space(target)
                                .ok_or_else(|| invalid_operation("focused space has no view"))?;
                            let content = self
                                .session
                                .view(view)
                                .ok_or_else(|| invalid_operation("focused view does not exist"))?
                                .content();
                            self.prepare_topology_effect(
                                frame,
                                PreparedEffect::Split {
                                    target,
                                    content,
                                    direction,
                                },
                            )?;
                        }
                        AppCommand::Focus(direction) => {
                            let target = self.frontend.resolve_focus_direction(
                                self.session.scene(),
                                self.session.scene_revision(),
                                self.session.focused(),
                                direction,
                            )?;
                            if let Some(target) = target {
                                if !self.session.is_focusable_space(target) {
                                    return Err(invalid_operation(
                                        "frontend returned an invalid focus target",
                                    ));
                                }
                                self.prepare_topology_effect(
                                    frame,
                                    PreparedEffect::Focus { target },
                                )?;
                            }
                        }
                    }
                    Ok(())
                }
                ResolvedOperation::Content { content, operation } => match operation {
                    ContentOperation::Apply(action) => {
                        self.execute_content_action(action, content, frame)
                    }
                    ContentOperation::Save => self.execute_save(content, frame),
                },
                ResolvedOperation::View {
                    view,
                    content,
                    operation,
                } => match operation {
                    ViewOperation::Edit(command) => {
                        self.execute_edit(command, view, content, frame)
                    }
                    ViewOperation::ApplyPlan(plan) => {
                        self.apply_view_edit_plan(plan, view, content, frame)
                    }
                    ViewOperation::ApplyContent(action) => {
                        let selections = self
                            .session
                            .view(view)
                            .and_then(|view| view.selections())
                            .ok_or_else(|| {
                                invalid_operation(
                                    "view content operation requires buffer view state",
                                )
                            })?
                            .clone();
                        self.apply_view_edit_plan(
                            ViewEditPlan {
                                expected: ViewPrecondition::Selections(selections),
                                content: Some(action),
                                view: None,
                            },
                            view,
                            content,
                            frame,
                        )
                    }
                    ViewOperation::Apply(action) => self.apply_view_action(view, action, frame),
                    ViewOperation::Viewport(command) => {
                        let cursor_row = if matches!(command, ViewportCommand::Align { .. }) {
                            let cursor = self
                                .session
                                .view(view)
                                .and_then(|view| view.selections())
                                .map(|selections| selections.primary().head())
                                .ok_or_else(|| {
                                    recoverable_message(
                                        io::ErrorKind::InvalidInput,
                                        "viewport alignment requires a text cursor",
                                    )
                                })?;
                            let ContentData::TextPoints(points) = self
                                .kernel
                                .contents()
                                .query(content, ContentQuery::TextPoints(vec![cursor]))
                            else {
                                return Err(recoverable_message(
                                    io::ErrorKind::InvalidInput,
                                    "viewport alignment requires text content",
                                ));
                            };
                            points
                                .into_iter()
                                .next()
                                .ok_or_else(|| {
                                    recoverable_message(
                                        io::ErrorKind::InvalidData,
                                        "text query omitted the viewport cursor point",
                                    )
                                })?
                                .row
                        } else {
                            0
                        };
                        let resolved = self.frontend.resolve_viewport_command(
                            self.session.scene(),
                            self.session.scene_revision(),
                            view,
                            cursor_row,
                            command,
                        )?;
                        let has_effect =
                            !matches!(resolved, ResolvedViewportCommand::Scroll { lines: 0, .. });
                        if has_effect {
                            self.prepare_viewport_effect(
                                frame,
                                PreparedEffect::Viewport {
                                    view,
                                    command: resolved,
                                },
                            )?;
                            if let Some(edit) = viewport_cursor_edit(command, resolved) {
                                prepend_operations(
                                    &mut queue,
                                    origin,
                                    vec![OperationRequest::View {
                                        target: ViewTarget::Current,
                                        operation: ViewOperation::Edit(edit),
                                    }],
                                );
                            }
                        }
                        Ok(())
                    }
                },
                ResolvedOperation::History {
                    content,
                    owner,
                    operation,
                } => self.execute_transaction_intent(operation, owner, content, frame),
                ResolvedOperation::Face(operation) => {
                    self.session
                        .faces()
                        .validate_operation(&operation)
                        .map_err(|error| invalid_operation(error.to_string()))?;
                    self.prepare_face_effect(frame, operation)
                }
                ResolvedOperation::Mode {
                    mode,
                    scope,
                    invocation,
                } => {
                    if invocation.nested {
                        frame.consume_nested_mode_call().map_err(operation_error)?;
                    }
                    match scope {
                        ResolvedModeScope::Content {
                            content,
                            source_view,
                        } => {
                            let result = self
                                .kernel
                                .execute_mode_content_action_in_draft(
                                    content,
                                    &invocation.command,
                                    frame.mode_drafts_mut(),
                                )
                                .map_err(|error| {
                                    recoverable_execution_error(io::ErrorKind::InvalidData, error)
                                })?;
                            let (flow, operations) = result.into_parts();
                            if invocation.flow == ModeFlowPropagation::Propagate {
                                input_flow = flow;
                            }
                            let mut effect_origin = OperationOrigin::content(content, source_view);
                            effect_origin.mode = Some(mode);
                            prepend_mode_operations(
                                &mut queue,
                                effect_origin,
                                operations,
                                invocation.flow,
                            );
                            Ok(())
                        }
                        ResolvedModeScope::View { view, content } => {
                            let revision_before = self
                                .session
                                .view(view)
                                .expect("target view exists")
                                .revision();
                            let (contents, modes, mode_contents) =
                                self.kernel.mode_attachment_parts();
                            let result = self
                                .session
                                .execute_mode(
                                    view,
                                    modes,
                                    contents,
                                    &invocation.command,
                                    mode_contents,
                                    frame.mode_drafts_mut(),
                                )
                                .map_err(|error| {
                                    recoverable_execution_error(io::ErrorKind::InvalidData, error)
                                })?;
                            let (flow, operations) = result.into_parts();
                            if invocation.flow == ModeFlowPropagation::Propagate {
                                input_flow = flow;
                            }
                            frame.record_view_touch(view, revision_before);
                            let mut effect_origin = OperationOrigin::view(view, content);
                            effect_origin.mode = Some(mode);
                            prepend_mode_operations(
                                &mut queue,
                                effect_origin,
                                operations,
                                invocation.flow,
                            );
                            Ok(())
                        }
                    }
                }
                ResolvedOperation::ModeInput {
                    mode,
                    view,
                    content,
                    input,
                } => {
                    let revision_before = self
                        .session
                        .view(view)
                        .expect("target view exists")
                        .revision();
                    let (contents, modes, mode_contents) = self.kernel.mode_attachment_parts();
                    let result = self
                        .session
                        .execute_mode_input(
                            view,
                            modes,
                            contents,
                            &input,
                            mode_contents,
                            frame.mode_drafts_mut(),
                        )
                        .map_err(|error| {
                            recoverable_execution_error(io::ErrorKind::InvalidData, error)
                        })?;
                    let (flow, operations) = result.into_parts();
                    input_flow = flow;
                    frame.record_view_touch(view, revision_before);
                    let mut effect_origin = OperationOrigin::view(view, content);
                    effect_origin.mode = Some(mode);
                    prepend_mode_operations(
                        &mut queue,
                        effect_origin,
                        operations,
                        ModeFlowPropagation::Propagate,
                    );
                    Ok(())
                }
            };
            result?;
        }
        Ok(input_flow)
    }

    fn resolve_operation(&self, queued: QueuedOperation) -> io::Result<ResolvedOperation> {
        let QueuedOperation { request, origin } = queued;
        match request {
            OperationRequest::App(operation) => Ok(ResolvedOperation::App(operation)),
            OperationRequest::Face(operation) => {
                let owner = origin
                    .mode
                    .map_or(FaceRemapOwner::User, FaceRemapOwner::Mode);
                let resolve_scope = |target| match target {
                    FaceRemapTarget::Session => {
                        Ok(vell_protocol::content_query::FaceRemapScope::Session)
                    }
                    FaceRemapTarget::CurrentContent => self
                        .resolve_content_target(ContentTarget::Current, origin)
                        .map(vell_protocol::content_query::FaceRemapScope::Content),
                    FaceRemapTarget::CurrentView => self
                        .resolve_view_target(ViewTarget::Current, origin)
                        .map(|(view, _)| vell_protocol::content_query::FaceRemapScope::View(view)),
                };
                let operation = match operation {
                    FaceOperation::SetBase {
                        target,
                        face,
                        expressions,
                    } => ResolvedFaceOperation::SetBase {
                        scope: resolve_scope(target)?,
                        face,
                        expressions,
                        owner,
                    },
                    FaceOperation::AddRelative {
                        target,
                        face,
                        token,
                        expressions,
                    } => ResolvedFaceOperation::AddRelative {
                        scope: resolve_scope(target)?,
                        face,
                        token,
                        expressions,
                        owner,
                    },
                    FaceOperation::RemoveRelative { token } => {
                        ResolvedFaceOperation::RemoveRelative { token, owner }
                    }
                };
                Ok(ResolvedOperation::Face(operation))
            }
            OperationRequest::Content { target, operation } => {
                let content = self.resolve_content_target(target, origin)?;
                Ok(ResolvedOperation::Content { content, operation })
            }
            OperationRequest::View { target, operation } => {
                if origin.scope != OperationOriginScope::View {
                    return Err(invalid_operation(
                        "view operation requires a view-scoped origin",
                    ));
                }
                let (view, content) = self.resolve_view_target(target, origin)?;
                Ok(ResolvedOperation::View {
                    view,
                    content,
                    operation,
                })
            }
            OperationRequest::History { target, operation } => {
                let content = self.resolve_content_target(target, origin)?;
                let owner = if origin.scope == OperationOriginScope::View {
                    let (view, view_content) =
                        self.resolve_view_target(ViewTarget::Current, origin)?;
                    if view_content != content {
                        return Err(invalid_operation("history owner targets another content"));
                    }
                    Some(view)
                } else {
                    None
                };
                Ok(ResolvedOperation::History {
                    content,
                    owner,
                    operation,
                })
            }
            OperationRequest::Mode { target, invocation } => {
                let target_matches_origin = matches!(
                    (target, origin.scope),
                    (ModeTarget::CurrentView, OperationOriginScope::View)
                        | (ModeTarget::CurrentContent, OperationOriginScope::Content)
                );
                if !target_matches_origin {
                    return Err(invalid_operation(
                        "mode target is incompatible with its origin",
                    ));
                }
                if invocation.nested && origin.view.is_none() {
                    return Err(invalid_operation(
                        "nested mode invocation needs a source view",
                    ));
                }
                let origin_content = origin
                    .content
                    .ok_or_else(|| invalid_operation("mode invocation has no content target"))?;
                let content_kind =
                    self.kernel.contents().kind(origin_content).ok_or_else(|| {
                        invalid_operation("mode invocation targets missing content")
                    })?;
                let command_scope = self
                    .kernel
                    .modes()
                    .command_scope(
                        &invocation.command.mode,
                        &invocation.command.action,
                        content_kind,
                    )
                    .map_err(|error| {
                        recoverable_execution_error(io::ErrorKind::InvalidData, error)
                    })?;
                let mode = self
                    .kernel
                    .modes()
                    .resolve_mode(&invocation.command.mode)
                    .expect("validated mode exists");
                let scope = match command_scope {
                    crate::mode::ModeActionScope::Content => {
                        let content =
                            self.resolve_content_target(ContentTarget::Current, origin)?;
                        let source_view = origin.view;
                        if target == ModeTarget::CurrentView && source_view.is_none() {
                            return Err(invalid_operation("mode invocation needs a source view"));
                        }
                        ResolvedModeScope::Content {
                            content,
                            source_view,
                        }
                    }
                    crate::mode::ModeActionScope::View => {
                        let (view, content) =
                            self.resolve_view_target(ViewTarget::Current, origin)?;
                        ResolvedModeScope::View { view, content }
                    }
                };
                Ok(ResolvedOperation::Mode {
                    mode,
                    scope,
                    invocation,
                })
            }
            OperationRequest::ModeInput { target, input } => {
                if origin.scope != OperationOriginScope::View {
                    return Err(invalid_operation(
                        "mode input requires a view-scoped origin",
                    ));
                }
                let (view, content) = self.resolve_view_target(target, origin)?;
                let content_kind = self
                    .kernel
                    .contents()
                    .kind(content)
                    .ok_or_else(|| invalid_operation("mode input targets missing content"))?;
                let mode = self
                    .kernel
                    .modes()
                    .resolve_mode(input.mode())
                    .ok_or_else(|| invalid_operation("mode input targets unknown mode"))?;
                if self.kernel.modes().adapter(mode, content_kind).is_none() {
                    return Err(invalid_operation(
                        "mode input targets an unsupported content kind",
                    ));
                }
                Ok(ResolvedOperation::ModeInput {
                    mode,
                    view,
                    content,
                    input,
                })
            }
        }
    }

    fn resolve_content_target(
        &self,
        target: ContentTarget,
        origin: OperationOrigin,
    ) -> io::Result<ContentId> {
        let content = match target {
            ContentTarget::Current => origin
                .content
                .ok_or_else(|| invalid_operation("operation has no current content"))?,
            ContentTarget::Id(content) => content,
        };
        if !self.kernel.contents().contains(content) {
            return Err(invalid_operation("operation targets missing content"));
        }
        Ok(content)
    }

    fn resolve_view_target(
        &self,
        target: ViewTarget,
        origin: OperationOrigin,
    ) -> io::Result<(ViewId, ContentId)> {
        let view = match target {
            ViewTarget::Current => origin
                .view
                .ok_or_else(|| invalid_operation("operation has no current view"))?,
            ViewTarget::Id(view) => view,
        };
        let content = self
            .session
            .view(view)
            .ok_or_else(|| invalid_operation("operation targets missing view"))?
            .content();
        if target == ViewTarget::Current
            && origin.content.is_some_and(|expected| expected != content)
        {
            return Err(invalid_operation("view/content target mismatch"));
        }
        Ok((view, content))
    }

    fn execute_save(&mut self, content: ContentId, frame: &mut ExecutionFrame) -> io::Result<()> {
        let active_owner = self.kernel.active_transaction_owner(content);
        if active_owner.is_some() {
            self.kernel.commit_transaction(content);
        }
        self.checkpoint_target(frame, content);
        let result = self.kernel.execute(content, ContentInput::Save);
        if let ContentResult::Handled(outcome) = result {
            if outcome.content_changed {
                for (view, revision) in self.session.content_view_revisions(content) {
                    frame.record_view_touch(view, revision);
                }
            }
            if let ContentEffect::Save(snapshot) = outcome.effect {
                self.prepare_effect(frame, PreparedEffect::Save { content, snapshot });
            }
        }
        if let Some(owner) = active_owner {
            self.kernel.begin_transaction(content, owner);
        }
        Ok(())
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
            .ok_or_else(|| invalid_operation("editable view has no buffer state"))?
            .clone();
        let plan = self
            .kernel
            .plan_edit(content, command, &before)
            .ok_or_else(|| invalid_operation("content does not support text edits"))?;
        self.apply_view_edit_plan(
            ViewEditPlan {
                expected: ViewPrecondition::Selections(before),
                content: plan.action,
                view: Some(ViewAction::SetSelections(plan.selections)),
            },
            view,
            content,
            frame,
        )
    }

    fn apply_view_edit_plan(
        &mut self,
        plan: ViewEditPlan,
        view: ViewId,
        content: ContentId,
        frame: &mut ExecutionFrame,
    ) -> io::Result<()> {
        let ViewEditPlan {
            expected,
            content: content_action,
            view: view_action,
        } = plan;
        let target_view = self
            .session
            .view(view)
            .ok_or_else(|| invalid_operation("operation targets missing view"))?;
        let stale = match &expected {
            ViewPrecondition::Selections(expected) => target_view.selections() != Some(expected),
            ViewPrecondition::Revision(expected) => target_view.revision() != *expected,
        };
        if stale {
            return Err(recoverable_message(
                io::ErrorKind::InvalidData,
                "stale resolved view edit",
            ));
        }
        let before = target_view
            .selections()
            .ok_or_else(|| invalid_operation("editable view has no buffer state"))?
            .clone();
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
            return Err(recoverable_message(
                io::ErrorKind::InvalidData,
                "content rejected a planned edit",
            ));
        };

        match view_action {
            Some(action) => {
                self.apply_view_action(view, action, frame)?;
                if let Some(change) = &outcome.change {
                    self.session
                        .transform_content_views(
                            self.kernel.contents(),
                            content,
                            Some(view),
                            change,
                        )
                        .map_err(invalid_content_view_state)?;
                }
            }
            None => {
                if let Some(change) = &outcome.change {
                    self.session
                        .transform_content_views(self.kernel.contents(), content, None, change)
                        .map_err(invalid_content_view_state)?;
                }
            }
        }
        if let Some(change) = &outcome.change {
            self.notify_mode_content_changed(content, change, frame);
        }
        if let Some(transaction) = transaction {
            let after = self
                .session
                .view(view)
                .and_then(|view| view.selections())
                .ok_or_else(|| invalid_operation("editable view lost its buffer state"))?
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
                recoverable_message(
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

    fn execute_content_action(
        &mut self,
        action: vell_core::action::ContentAction,
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
            return Err(recoverable_message(
                io::ErrorKind::InvalidData,
                "content rejected its mode action",
            ));
        };
        if let Some(change) = &outcome.change {
            self.session
                .transform_content_views(self.kernel.contents(), content, None, change)
                .map_err(invalid_content_view_state)?;
            self.notify_mode_content_changed(content, change, frame);
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
                recoverable_message(
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
                recoverable_message(
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
            self.session
                .transform_content_views(
                    self.kernel.contents(),
                    record.target,
                    source.as_ref().map(|(view, _)| *view),
                    change,
                )
                .map_err(invalid_content_view_state)?;
            self.notify_mode_content_changed(record.target, change, frame);
        }
        Ok(())
    }

    fn notify_mode_content_changed(
        &mut self,
        content: ContentId,
        change: &vell_core::content::ContentChange,
        frame: &mut ExecutionFrame,
    ) {
        let (contents, mode_contents) = self.kernel.mode_runtime_parts();
        mode_contents.notify_changed(content, contents, change, frame.mode_drafts_mut());
        self.session.notify_mode_content_changed(
            content,
            mode_contents,
            contents,
            change,
            frame.mode_drafts_mut(),
        );
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
            .ok_or_else(|| recoverable_message(io::ErrorKind::InvalidData, "invalid view action"))
    }

    pub(super) fn render(&mut self) -> io::Result<()> {
        let display_profile = self.frontend.display_profile();
        self.session
            .faces_mut()
            .set_display_profile(display_profile);
        let query = AppQuery {
            contents: self.kernel.contents(),
            views: self.session.views(),
            presentation: self.session.presentation(),
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

fn is_recoverable_execution_error(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|source| source.downcast_ref::<RecoverableExecutionError>().is_some())
}

fn invalid_content_view_state(
    error: vell_core::content_view_state::ContentViewStateError,
) -> io::Error {
    recoverable_execution_error(io::ErrorKind::InvalidData, error)
}

fn viewport_cursor_edit(
    command: ViewportCommand,
    resolved: ResolvedViewportCommand,
) -> Option<EditCommand> {
    let ViewportCommand::Scroll {
        cursor_behavior, ..
    } = command
    else {
        return None;
    };
    let ResolvedViewportCommand::Scroll { direction, lines } = resolved else {
        return None;
    };
    Some(match (direction, cursor_behavior) {
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
    })
}

fn prepend_mode_operations(
    queue: &mut VecDeque<QueuedOperation>,
    origin: OperationOrigin,
    mut operations: Vec<OperationRequest>,
    parent_flow: ModeFlowPropagation,
) {
    if parent_flow == ModeFlowPropagation::Isolate {
        for operation in &mut operations {
            if let OperationRequest::Mode { invocation, .. } = operation {
                invocation.flow = ModeFlowPropagation::Isolate;
            }
        }
    }
    prepend_operations(queue, origin, operations);
}
