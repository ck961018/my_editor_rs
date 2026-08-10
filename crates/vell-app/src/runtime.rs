use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::future;
use std::io;
use std::time::{Duration, Instant};

use crate::action::{TransactionIntent, ViewAction};
use crate::application::{App, CommandTaskTarget, PendingCommandInvocation};
#[cfg(test)]
use crate::behavior::EffectBehavior;
use crate::command::{AppCommand, ContentCommand};
use crate::diagnostics::RuntimeDiagnostic;
use crate::dispatcher::{DispatchCommand, DispatchInput, DispatchOutcome};
use crate::execution::{
    ExecutionFrame, ExecutionFrameParts, InputCheckpoint, PendingCommandStart, PreparedEffect,
    StateRollback,
};
use crate::layout::LayoutError;
use crate::mode::{CursorDomain, InputFlow};
use crate::operation::{
    AppOperation, BufferViewSource, ClipboardDestination, ClipboardOperation, ClipboardSource,
    ContentLifecycleOperation, ContentOperation, ContentTarget, FaceOperation, FaceRemapTarget,
    ModeFlowPropagation, ModeTarget, OperationError, OperationOrigin, OperationOriginScope,
    OperationRequest, QueuedOperation, ResolvedContentLifecycleOperation, ResolvedModeScope,
    ResolvedOperation, ResolvedViewLifecycleOperation, SearchOperation, ViewBindingOperation,
    ViewEditPlan, ViewLifecycleOperation, ViewOperation, ViewPrecondition, ViewSpec, ViewTarget,
    adapt_dispatch_command, prepend_operations,
};
use crate::query::AppQuery;
use crate::theme::{FaceRemapOwner, ResolvedFaceOperation};
use crate::transaction::{TransactionData, TransactionRecord, ViewTransactionData};
use vell_core::clipboard::ClipboardPayload;
use vell_core::command::EditCommand;
use vell_core::content::{ContentActionResult, ContentEffect, ContentInput, ContentResult};
use vell_core::search::SearchDirection;
use vell_core::transaction::TransactionDirection;
use vell_frontend::Frontend;
use vell_mode::command_registry::{
    COMMAND_LINE_COMMAND_ID, CommandCompletion, CommandEntry, CommandError, CommandHost, CommandId,
    CommandInvocation, CommandQuery, CommandRequest, CommandResult, CommandTaskCompletion,
    CommandTaskId, CommandValue,
};
use vell_protocol::content_query::{ContentData, ContentQuery, RenderQuery};
use vell_protocol::frontend_event::FrontendEvent;
use vell_protocol::ids::{ContentId, ViewId};
use vell_protocol::selection::{Selection, Selections, TextOffset};
use vell_protocol::viewport::{
    ResolvedViewportCommand, ViewportCommand, ViewportCursorBehavior, ViewportMoveDirection,
};

const MAX_RUNTIME_DIAGNOSTICS: usize = 128;
const MAX_REGISTERED_COMMAND_DEPTH: usize = 256;

struct ScopedCommandHost<'a, F: Frontend> {
    app: &'a mut App<F>,
    frame: &'a mut ExecutionFrame,
    origin: OperationOrigin,
    depth: usize,
    deferred: VecDeque<QueuedOperation>,
}

impl<F: Frontend> CommandHost for ScopedCommandHost<'_, F> {
    fn invoke_command(&mut self, invocation: CommandInvocation) -> CommandResult {
        if self.depth == MAX_REGISTERED_COMMAND_DEPTH {
            return Err(CommandError::RecursionLimit {
                limit: MAX_REGISTERED_COMMAND_DEPTH,
            });
        }
        self.depth += 1;
        let entry = self.app.kernel.commands().resolve(&invocation.command);
        let result = entry
            .ok_or(CommandError::UnknownCommand(invocation.command))
            .and_then(|entry| entry.invoke(self, invocation.arguments));
        self.depth -= 1;
        result
    }

    fn request(&mut self, request: CommandRequest) -> CommandResult {
        match request {
            CommandRequest::CreateContent => {
                self.consume_request()?;
                let content = self.app.new_buffer();
                self.frame.record_provisional_content(content);
                Ok(CommandValue::from(content.0).into())
            }
            CommandRequest::Query(query) => {
                self.consume_request()?;
                self.query(query).map(CommandCompletion::from)
            }
            CommandRequest::Execute(operation) => {
                if matches!(operation, OperationRequest::Mode { .. }) {
                    self.deferred.push_back(QueuedOperation {
                        request: operation,
                        origin: self.origin,
                    });
                    return Ok(CommandValue::Null.into());
                }
                self.execute_request(operation)?;
                Ok(CommandValue::Null.into())
            }
            CommandRequest::ExecuteAsync(operation) => {
                let task = self.app.allocate_command_task()?;
                let effect_start = self.frame.prepared_effect_count();
                self.execute_request(operation)
                    .map_err(|error| CommandError::AsyncFailed(error.to_string()))?;
                if !self.frame.attach_task_to_save_since(effect_start, task) {
                    return Err(CommandError::AsyncFailed(
                        "asynchronous command did not start a host task".to_owned(),
                    ));
                }
                Ok(CommandCompletion::Pending(
                    vell_mode::command_registry::CommandPending::task(task),
                ))
            }
        }
    }

    fn register_command(&mut self, entry: CommandEntry) {
        self.app.register_command(entry);
    }
}

impl<F: Frontend> ScopedCommandHost<'_, F> {
    fn execute_deferred(&mut self) -> Result<(), CommandError> {
        let operations = std::mem::take(&mut self.deferred);
        self.app
            .execute_operation_queue(operations, self.frame)
            .map(|_| ())
            .map_err(|error| CommandError::Failed(error.to_string()))
    }

    fn execute_request(&mut self, operation: OperationRequest) -> Result<(), CommandError> {
        let switched_content = match &operation {
            OperationRequest::ViewLifecycle(ViewLifecycleOperation::Switch {
                spec:
                    ViewSpec::Buffer {
                        source: BufferViewSource::Content(content),
                    },
            }) => Some(*content),
            _ => None,
        };
        let queued = QueuedOperation {
            request: operation,
            origin: self.origin,
        };
        self.app
            .execute_operation_queue(VecDeque::from([queued]), self.frame)
            .map_err(|error| CommandError::Failed(error.to_string()))?;
        if let Some(content) = switched_content {
            self.origin.content = Some(content);
            self.origin.view = None;
            self.origin.scope = OperationOriginScope::Content;
        }
        Ok(())
    }

    fn consume_request(&mut self) -> Result<(), CommandError> {
        self.frame
            .consume_operation()
            .map_err(|error| CommandError::Failed(error.to_string()))
    }

    fn query(&self, query: CommandQuery) -> Result<CommandValue, CommandError> {
        match query {
            CommandQuery::CurrentContent => self
                .origin
                .content
                .map(|content| CommandValue::from(content.0))
                .ok_or_else(|| CommandError::Failed("command has no current content".to_owned())),
            CommandQuery::CurrentView => self
                .origin
                .view
                .map(|view| CommandValue::from(view.0))
                .ok_or_else(|| CommandError::Failed("command has no current view".to_owned())),
            CommandQuery::CurrentText => {
                let content = self.origin.content.ok_or_else(|| {
                    CommandError::Failed("command has no current content".to_owned())
                })?;
                self.app
                    .kernel
                    .contents()
                    .text_snapshot(content)
                    .map(|snapshot| CommandValue::from(snapshot.to_owned_string()))
                    .ok_or_else(|| {
                        CommandError::Failed("current content has no text snapshot".to_owned())
                    })
            }
            CommandQuery::CurrentTextDocument => {
                let content = self.origin.content.ok_or_else(|| {
                    CommandError::Failed("command has no current content".to_owned())
                })?;
                let view = self.origin.view.ok_or_else(|| {
                    CommandError::Failed("command has no current view".to_owned())
                })?;
                let source = self
                    .app
                    .kernel
                    .contents()
                    .text_snapshot(content)
                    .map(|snapshot| snapshot.to_owned_string())
                    .ok_or_else(|| {
                        CommandError::Failed("current content has no text snapshot".to_owned())
                    })?;
                let selection = self
                    .app
                    .session
                    .view(view)
                    .and_then(|view| view.selections())
                    .map(|selections| selections.primary())
                    .ok_or_else(|| {
                        CommandError::Failed("current view has no text selection".to_owned())
                    })?;
                let resource_path = match self
                    .app
                    .kernel
                    .contents()
                    .query(content, ContentQuery::ResourcePath)
                {
                    ContentData::ResourcePath(path) => path,
                    _ => None,
                };
                Ok(serde_json::json!({
                    "content": content.0,
                    "resourcePath": resource_path,
                    "source": source,
                    "selection": {
                        "anchor": selection.anchor.char_index,
                        "head": selection.head.char_index,
                    },
                }))
            }
            CommandQuery::CommandExists(id) => Ok(CommandValue::from(
                self.app.kernel.commands().get(&id).is_some(),
            )),
        }
    }
}

fn selection_for_match(range: std::ops::Range<usize>, direction: SearchDirection) -> Selections {
    let (anchor, head) = match direction {
        SearchDirection::Forward => (range.start, range.end),
        SearchDirection::Backward => (range.end, range.start),
    };
    Selections::single(Selection {
        anchor: TextOffset { char_index: anchor },
        head: TextOffset { char_index: head },
    })
}

#[cfg(test)]
impl PreparedEffect {
    fn behavior(&self) -> EffectBehavior {
        match self {
            Self::HistoryCommit { content } => EffectBehavior::HistoryCommit { content: *content },
            Self::Save {
                content, snapshot, ..
            }
            | Self::SaveAs {
                content, snapshot, ..
            } => EffectBehavior::Save {
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
            Self::ClipboardStore { .. } => EffectBehavior::Clipboard,
            Self::ReloadCommit { .. }
            | Self::ContentOpenCommit(_)
            | Self::ContentList(_)
            | Self::ContentCreate
            | Self::ContentOpen(_)
            | Self::ViewSwitch { .. }
            | Self::ViewRebind { .. }
            | Self::ContentClose { .. } => EffectBehavior::Lifecycle,
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
    pub(super) fn complete_async_open(
        &mut self,
        content: ContentId,
        result: io::Result<crate::message::OpenedPath>,
    ) -> io::Result<bool> {
        let Some(mut completion) = self.kernel.complete_open(content, result)? else {
            return Ok(false);
        };
        completion.targets.retain(|target| {
            self.session.is_focusable_space(target.space)
                && self.session.view_for_space(target.space) == target.expected_view
        });
        if completion.targets.is_empty() && !completion.install_without_target {
            return Ok(true);
        }
        let mut frame = self.begin_execution_frame(None, None);
        let result =
            self.prepare_topology_effect(&mut frame, PreparedEffect::ContentOpenCommit(completion));
        self.finish_execution_frame(frame, result)?;
        Ok(true)
    }

    fn record_runtime_message(&mut self, message: String) {
        if self.runtime_diagnostics.len() >= MAX_RUNTIME_DIAGNOSTICS {
            self.runtime_diagnostics.remove(0);
        }
        self.session.set_status_message(message.clone());
        self.runtime_diagnostics.push(RuntimeDiagnostic { message });
    }

    pub(super) fn record_recoverable_error(&mut self, error: io::Error) {
        self.record_runtime_message(error.to_string());
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
        self.cancel_pending_commands("command cancelled because the editor is shutting down");
        let shutdown_result = self.shutdown_tasks().await;
        run_result.and(shutdown_result)
    }

    async fn run_loop(&mut self) -> io::Result<()> {
        self.kernel.schedule_mode_jobs();
        self.session
            .refresh_presentation(self.kernel.contents(), self.kernel.content_modes());
        self.render()?;
        // ponytail: poll until the platform exposes a cross-crate wake channel.
        let mut background_tick = tokio::time::interval(Duration::from_millis(16));
        background_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        background_tick.tick().await;
        loop {
            let input_deadline = self
                .session
                .next_input_deadline(self.kernel.content_modes(), self.kernel.contents());
            let cancellation = self.kernel.cancellation_token();
            let should_render = tokio::select! {
                biased;
                _ = cancellation.cancelled() => break,
                _ = background_tick.tick() => {
                    let changed = self.kernel.schedule_mode_jobs();
                    if changed {
                        self.session.refresh_presentation(
                            self.kernel.contents(),
                            self.kernel.content_modes(),
                        );
                    }
                    changed
                }
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
        self.session.clear_status_message();
        let render = match event {
            FrontendEvent::Resize(r) => {
                self.session.resize(r.width, r.height);
                true
            }
            FrontendEvent::Key(k) => {
                self.process_input_queue(VecDeque::from([DispatchInput::Normal(k)]))?;
                true
            }
            FrontendEvent::Paste(text) => {
                let view = self
                    .session
                    .view_for_space(self.session.focused())
                    .ok_or_else(|| invalid_operation("focused space has no view"))?;
                let content = self
                    .session
                    .view(view)
                    .ok_or_else(|| invalid_operation("focused view does not exist"))?
                    .document_content()
                    .ok_or_else(|| invalid_operation("focused view has no document binding"))?;
                self.execute_command(DispatchCommand::ContentWithView {
                    command: ContentCommand::Edit(EditCommand::InsertText(text)),
                    view,
                    content,
                })?;
                true
            }
            FrontendEvent::QuitRequest => {
                self.execute_command(DispatchCommand::App(AppCommand::Quit))?;
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

    fn allocate_command_task(&mut self) -> Result<CommandTaskId, CommandError> {
        let value = self.next_command_task;
        self.next_command_task = value
            .checked_add(1)
            .ok_or_else(|| CommandError::Failed("command task ids exhausted".to_owned()))?;
        Ok(CommandTaskId::new(value))
    }

    fn install_pending_command(&mut self, start: PendingCommandStart) {
        let missing_task = start
            .pending
            .tasks()
            .iter()
            .find(|task| !self.command_tasks.contains_key(task));
        let expected_state = self.kernel.contents().text_state_id(start.content);
        let reason = if let Some(task) = missing_task {
            Some(format!("command task {} was not published", task.get()))
        } else if self
            .session
            .view(start.view)
            .is_none_or(|view| view.document_content() != Some(start.content))
        {
            Some("command target view no longer exists".to_owned())
        } else if expected_state.is_none() {
            Some("command target content no longer exists".to_owned())
        } else {
            None
        };
        if let Some(reason) = reason {
            start.pending.cancel(CommandError::Failed(reason.clone()));
            self.record_runtime_message(reason);
            return;
        }
        self.pending_commands.push(PendingCommandInvocation {
            pending: start.pending,
            view: start.view,
            content: start.content,
            expected_state: expected_state.expect("validated text command target"),
        });
    }

    pub(super) fn complete_command_tasks(
        &mut self,
        content: ContentId,
        revision: u64,
        result: Result<CommandValue, CommandError>,
    ) {
        let tasks = self
            .command_tasks
            .iter()
            .filter_map(|(task, target)| {
                (target.content == content && target.revision <= revision).then_some(*task)
            })
            .collect::<Vec<_>>();
        for task in tasks {
            self.command_tasks.remove(&task);
            let mut index = 0;
            while index < self.pending_commands.len() {
                if self.pending_commands[index].pending.tasks().contains(&task) {
                    let invocation = self.pending_commands.remove(index);
                    self.resume_pending_command(invocation, task, result.clone());
                } else {
                    index += 1;
                }
            }
        }
    }

    fn resume_pending_command(
        &mut self,
        invocation: PendingCommandInvocation,
        task: CommandTaskId,
        task_result: Result<CommandValue, CommandError>,
    ) {
        let target_valid = self
            .session
            .view(invocation.view)
            .is_some_and(|view| view.document_content() == Some(invocation.content))
            && self.kernel.contents().text_state_id(invocation.content)
                == Some(invocation.expected_state);
        if !target_valid {
            let reason = CommandError::Failed(
                "command target changed while the command was suspended".to_owned(),
            );
            invocation.pending.cancel(reason.clone());
            self.record_runtime_message(reason.to_string());
            return;
        }

        let mut frame = self.begin_execution_frame(Some(invocation.content), None);
        let result = {
            let mut host = ScopedCommandHost {
                app: self,
                frame: &mut frame,
                origin: OperationOrigin::view(invocation.view, invocation.content),
                depth: 0,
                deferred: VecDeque::new(),
            };
            let result = invocation
                .pending
                .resume(&mut host, CommandTaskCompletion::new(task, task_result));
            match result {
                Ok(completion) => host.execute_deferred().map(|()| completion),
                Err(error) => Err(error),
            }
        };
        let result = match result {
            Ok(CommandCompletion::Ready(_)) => Ok(()),
            Ok(CommandCompletion::Pending(pending)) => frame
                .stage_pending_command(pending, invocation.view, invocation.content)
                .map_err(operation_error),
            Err(error) => Err(recoverable_message(
                io::ErrorKind::InvalidInput,
                error.to_string(),
            )),
        };
        if let Err(error) = self.finish_execution_frame(frame, result) {
            self.record_recoverable_error(error);
        }
    }

    fn cancel_pending_commands(&mut self, reason: &str) {
        let pending = std::mem::take(&mut self.pending_commands);
        for invocation in pending {
            invocation
                .pending
                .cancel(CommandError::Failed(reason.to_owned()));
        }
    }

    pub(super) fn cancel_pending_commands_for_view(&mut self, view: ViewId) {
        let reason = "command target view was closed while the command was suspended";
        let mut cancelled = false;
        let mut index = 0;
        while index < self.pending_commands.len() {
            if self.pending_commands[index].view == view {
                let invocation = self.pending_commands.remove(index);
                invocation
                    .pending
                    .cancel(CommandError::Failed(reason.to_owned()));
                cancelled = true;
            } else {
                index += 1;
            }
        }
        if cancelled {
            self.record_runtime_message(reason.to_owned());
        }
    }

    fn cancel_stale_pending_commands(&mut self) {
        let reason = "command target changed while the command was suspended";
        let mut cancelled = false;
        let mut index = 0;
        while index < self.pending_commands.len() {
            let invocation = &self.pending_commands[index];
            let valid = self
                .session
                .view(invocation.view)
                .is_some_and(|view| view.document_content() == Some(invocation.content))
                && self.kernel.contents().text_state_id(invocation.content)
                    == Some(invocation.expected_state);
            if valid {
                index += 1;
            } else {
                let invocation = self.pending_commands.remove(index);
                invocation
                    .pending
                    .cancel(CommandError::Failed(reason.to_owned()));
                cancelled = true;
            }
        }
        if cancelled {
            self.record_runtime_message(reason.to_owned());
        }
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
        let ExecutionFrameParts {
            checkpoints,
            mut mode_drafts,
            provisional_contents,
            view_touches,
            effects,
            pending_command,
        } = frame.into_parts();
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
            for content in provisional_contents.into_iter().rev() {
                self.session.forget_content(content);
                assert!(
                    self.kernel.remove_content(content),
                    "provisional content remains owned by its execution frame"
                );
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
            self.cancel_stale_pending_commands();
            self.publish_prepared_effects(effects);
            if let Some(pending) = pending_command {
                self.install_pending_command(pending);
            }
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
        let content = view.and_then(|view| {
            self.session
                .view(view)
                .and_then(|view| view.document_content())
        });
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
            if frame.has_pending_command() {
                queue.clear();
                break;
            }
        }

        if result.is_ok()
            && !frame.has_pending_command()
            && let (Some(view), Some(content)) = (view, content)
            && frame.targets(content)
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
                PreparedEffect::Save {
                    content,
                    snapshot,
                    force,
                    task,
                } => {
                    if let Some(task) = task {
                        let previous = self.command_tasks.insert(
                            task,
                            CommandTaskTarget {
                                content,
                                revision: snapshot.revision,
                            },
                        );
                        assert!(previous.is_none(), "command task id is unique");
                    }
                    self.kernel.queue_save(content, snapshot, force);
                }
                PreparedEffect::SaveAs {
                    content,
                    snapshot,
                    identity,
                    force,
                } => {
                    self.kernel
                        .queue_save_as(content, snapshot, identity, force)
                        .expect("validated Save As target remains available");
                }
                PreparedEffect::ReloadCommit {
                    content,
                    path,
                    baseline,
                } => {
                    self.kernel.clear_history(content);
                    self.kernel.update_buffer_baseline(content, path, baseline);
                }
                PreparedEffect::ContentOpenCommit(completion) => {
                    let report_content = completion.install_without_target;
                    let targets = completion
                        .targets
                        .iter()
                        .map(|target| target.space)
                        .collect::<Vec<_>>();
                    let (content, _installed) = self
                        .kernel
                        .install_open(completion)
                        .expect("validated async open installs once");
                    for target in targets {
                        self.replace_space_content(target, content, true)
                            .expect("validated async view target remains available");
                    }
                    if report_content {
                        self.record_runtime_message(format!("opened content {}", content.0));
                    }
                }
                PreparedEffect::ContentList(buffers) => {
                    let listing = buffers
                        .into_iter()
                        .map(|buffer| {
                            let name = buffer
                                .resource_name
                                .unwrap_or_else(|| "[untitled]".to_owned());
                            let dirty = if buffer.dirty_state
                                == vell_protocol::content_query::DirtyState::Modified
                            {
                                " *"
                            } else {
                                Default::default()
                            };
                            format!("{}: {name}{dirty}", buffer.content.0)
                        })
                        .collect::<Vec<_>>()
                        .join(" | ");
                    self.record_runtime_message(listing);
                }
                PreparedEffect::ContentCreate => {
                    let content = self.new_buffer();
                    self.record_runtime_message(format!("created content {}", content.0));
                }
                PreparedEffect::ContentOpen(path) => {
                    self.kernel.queue_content_open(path);
                }
                PreparedEffect::ViewSwitch { target, spec } => match spec {
                    ViewSpec::Buffer {
                        source: BufferViewSource::Content(content),
                    } => {
                        self.switch_view_at(target, content)
                            .expect("validated view remains switchable until frame commit");
                    }
                    ViewSpec::Buffer {
                        source: BufferViewSource::Create,
                    } => {
                        let content = self.new_buffer();
                        self.switch_view_at(target, content)
                            .expect("new content remains switchable until frame commit");
                    }
                    ViewSpec::Buffer {
                        source: BufferViewSource::Open { path },
                    } => {
                        let target_space = self
                            .session
                            .body_space_for_view(target)
                            .expect("validated view keeps its body Pane until frame commit");
                        self.kernel.queue_view_open(
                            target_space,
                            Some(target),
                            std::path::PathBuf::from(path),
                        );
                    }
                },
                PreparedEffect::ViewRebind {
                    view,
                    binding,
                    content,
                } => {
                    self.rebind_view_content(view, &binding, content)
                        .expect("validated View binding remains available until frame commit");
                }
                PreparedEffect::ContentClose { content, force } => {
                    self.close_buffer(content, force)
                        .expect("validated content remains closeable until frame commit");
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
                PreparedEffect::ClipboardStore {
                    payload,
                    write_system,
                } => {
                    let text = write_system.then(|| payload.system_text());
                    self.kernel.set_clipboard(payload);
                    if let Some(text) = text
                        && let Err(error) = self.frontend.write_clipboard(&text)
                    {
                        self.record_runtime_message(format!(
                            "system clipboard write failed: {error}"
                        ));
                    }
                }
                PreparedEffect::Quit => {
                    self.cancel_pending_commands(
                        "command cancelled because the editor is quitting",
                    );
                    self.kernel.cancel();
                }
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
                .and_then(|view| {
                    self.session
                        .view(view)
                        .and_then(|view| view.document_content())
                });
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

    pub(super) fn reload_content_in_frame(
        &mut self,
        content: ContentId,
        path: std::path::PathBuf,
        text: String,
        backing_state: vell_protocol::content_query::BufferBackingState,
    ) -> io::Result<()> {
        let mut frame = self.begin_execution_frame(Some(content), None);
        self.checkpoint_target(&mut frame, content);
        let result = (|| {
            let result = self.kernel.execute(
                content,
                ContentInput::Event(vell_core::content::ContentEvent::Reload {
                    path,
                    text,
                    backing_state,
                }),
            );
            let ContentResult::Handled(outcome) = result else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "reload target is not a buffer",
                ));
            };
            if let Some(change) = &outcome.change {
                self.session
                    .transform_content_views(self.kernel.contents(), content, None, change)
                    .map_err(invalid_content_view_state)?;
                self.notify_mode_content_changed(content, change, &mut frame);
            }
            self.session.touch_content_views(content);
            Ok(())
        })();
        let result = self.finish_execution_frame(frame, result);
        if result.is_ok() {
            self.kernel.clear_history(content);
        }
        result
    }

    pub(super) fn execute_command(&mut self, command: DispatchCommand) -> io::Result<()> {
        let content = self.command_frame_content(&command);
        let mut frame = self.begin_execution_frame(content, None);
        let result = self.execute_command_in_frame(command, false, &mut frame);
        self.finish_execution_frame(frame, result).map(|_| ())
    }

    fn retarget_execution_frame(
        &mut self,
        frame: &mut ExecutionFrame,
        content: ContentId,
    ) -> io::Result<()> {
        frame.retarget(content).map_err(operation_error)?;
        if !self.kernel.retarget_command_transaction(content) {
            return Err(invalid_operation(
                "execution frame cannot change target after mutating history",
            ));
        }
        Ok(())
    }

    fn command_frame_content(&self, command: &DispatchCommand) -> Option<ContentId> {
        let origin = command.content();
        let operations = match command {
            DispatchCommand::ModeContentOperations { operations, .. }
            | DispatchCommand::ModeOperations { operations, .. } => operations,
            _ => return origin,
        };
        let target = match operations.as_slice() {
            [OperationRequest::ContentLifecycle(operation)] => match operation {
                ContentLifecycleOperation::Close { target, .. }
                | ContentLifecycleOperation::Save { target, .. }
                | ContentLifecycleOperation::SaveAs { target, .. }
                | ContentLifecycleOperation::Reload { target, .. } => match target {
                    ContentTarget::Current => origin,
                    ContentTarget::Id(content) => Some(*content),
                },
                ContentLifecycleOperation::Create
                | ContentLifecycleOperation::Open { .. }
                | ContentLifecycleOperation::List => None,
            },
            [
                OperationRequest::ViewLifecycle(ViewLifecycleOperation::Switch {
                    spec:
                        ViewSpec::Buffer {
                            source: BufferViewSource::Content(content),
                        },
                }),
            ] => Some(*content),
            _ => return origin,
        };
        target
            .filter(|content| self.kernel.contents().contains(*content))
            .or(origin)
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
            && frame.targets(content)
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
        let command = match command {
            DispatchCommand::Registered {
                invocation,
                view,
                content,
            } => {
                let mut host = ScopedCommandHost {
                    app: self,
                    frame,
                    origin: OperationOrigin::view(view, content),
                    depth: 0,
                    deferred: VecDeque::new(),
                };
                let result = host.invoke_command(invocation);
                let result = match result {
                    Ok(completion) => host.execute_deferred().map(|()| completion),
                    Err(error) => Err(error),
                };
                return match result {
                    Ok(CommandCompletion::Ready(_)) => Ok(InputFlow::Stop),
                    Ok(CommandCompletion::Pending(pending)) => {
                        frame
                            .stage_pending_command(pending, view, content)
                            .map_err(operation_error)?;
                        Ok(InputFlow::Stop)
                    }
                    Err(error) => Err(recoverable_message(
                        io::ErrorKind::InvalidInput,
                        error.to_string(),
                    )),
                };
            }
            command => command,
        };
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
                ResolvedOperation::ExecuteCommandLine {
                    request,
                    view,
                    content,
                } => {
                    let invocation = CommandInvocation::new(
                        CommandId::new(COMMAND_LINE_COMMAND_ID)
                            .expect("command line service id is valid"),
                        vec![CommandValue::from(request.source)],
                    );
                    let mut host = ScopedCommandHost {
                        app: self,
                        frame,
                        origin: OperationOrigin::view(view, content),
                        depth: 0,
                        deferred: VecDeque::new(),
                    };
                    let result = host.invoke_command(invocation);
                    let result = match result {
                        Ok(completion) => host.execute_deferred().map(|()| completion),
                        Err(error) => Err(error),
                    };
                    match result {
                        Ok(CommandCompletion::Ready(_)) => Ok(()),
                        Ok(CommandCompletion::Pending(pending)) => {
                            host.frame
                                .stage_pending_command(pending, view, content)
                                .map_err(operation_error)?;
                            queue.clear();
                            Ok(())
                        }
                        Err(error) => Err(recoverable_message(
                            io::ErrorKind::InvalidInput,
                            error.to_string(),
                        )),
                    }
                }
                ResolvedOperation::App(AppOperation::Command(command)) => {
                    match command {
                        AppCommand::Quit => {
                            self.preflight_quit(false).map_err(|error| {
                                recoverable_message(io::ErrorKind::Other, error.to_string())
                            })?;
                            self.prepare_effect(frame, PreparedEffect::Quit);
                        }
                        AppCommand::ForceQuit => {
                            self.prepare_effect(frame, PreparedEffect::Quit);
                        }
                        AppCommand::Close => {
                            let target = self.session.focused();
                            match self.session.validate_close_space(target) {
                                Ok(()) => self.prepare_topology_effect(
                                    frame,
                                    PreparedEffect::Close { target },
                                )?,
                                Err(LayoutError::WouldRemoveLastFocusable(_)) => {
                                    self.preflight_quit(false).map_err(|error| {
                                        recoverable_message(io::ErrorKind::Other, error.to_string())
                                    })?;
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
                                .document_content()
                                .ok_or_else(|| {
                                    invalid_operation("focused view has no document binding")
                                })?;
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
                ResolvedOperation::ContentLifecycle(operation) => {
                    let exclusive =
                        !matches!(operation, ResolvedContentLifecycleOperation::Save { .. });
                    if exclusive && (!frame.can_prepare_lifecycle() || !queue.is_empty()) {
                        return Err(invalid_operation(
                            "a content lifecycle operation must be the only operation in its frame",
                        ));
                    }
                    let target = match &operation {
                        ResolvedContentLifecycleOperation::Close { content, .. }
                        | ResolvedContentLifecycleOperation::Save { content, .. }
                        | ResolvedContentLifecycleOperation::SaveAs { content, .. }
                        | ResolvedContentLifecycleOperation::Reload { content, .. } => {
                            Some(*content)
                        }
                        ResolvedContentLifecycleOperation::Create
                        | ResolvedContentLifecycleOperation::Open { .. }
                        | ResolvedContentLifecycleOperation::List => None,
                    };
                    if let Some(content) = target {
                        self.retarget_execution_frame(frame, content)?;
                    }
                    match operation {
                        ResolvedContentLifecycleOperation::Create => self
                            .prepare_topology_effect(frame, PreparedEffect::ContentCreate)
                            .map(|_| ()),
                        ResolvedContentLifecycleOperation::Open { path } => self
                            .prepare_topology_effect(
                                frame,
                                PreparedEffect::ContentOpen(std::path::PathBuf::from(path)),
                            )
                            .map(|_| ()),
                        ResolvedContentLifecycleOperation::List => {
                            let buffers = self.buffers();
                            self.prepare_effect(frame, PreparedEffect::ContentList(buffers));
                            Ok(())
                        }
                        ResolvedContentLifecycleOperation::Close { content, force } => {
                            self.validate_close_buffer(content, force)
                                .map_err(|error| {
                                    recoverable_message(io::ErrorKind::Other, error.to_string())
                                })?;
                            self.prepare_topology_effect(
                                frame,
                                PreparedEffect::ContentClose { content, force },
                            )
                            .map(|_| ())
                        }
                        ResolvedContentLifecycleOperation::Save { content, force } => {
                            self.execute_save_with_force(content, force, frame)
                        }
                        ResolvedContentLifecycleOperation::SaveAs {
                            content,
                            path,
                            force,
                        } => self.execute_save_as(content, path, force, frame),
                        ResolvedContentLifecycleOperation::Reload { content, force } => {
                            self.execute_reload(content, force, frame)
                        }
                    }
                }
                ResolvedOperation::ViewLifecycle(operation) => {
                    if !frame.can_prepare_lifecycle() || !queue.is_empty() {
                        return Err(invalid_operation(
                            "a view lifecycle operation must be the only operation in its frame",
                        ));
                    }
                    match operation {
                        ResolvedViewLifecycleOperation::Focus { view } => {
                            let target = self
                                .session
                                .body_space_for_view(view)
                                .ok_or_else(|| invalid_operation("view has no body Pane"))?;
                            if !self.session.is_focusable_space(target) {
                                return Err(invalid_operation("view is not focusable"));
                            }
                            self.prepare_topology_effect(frame, PreparedEffect::Focus { target })
                                .map(|_| ())
                        }
                        ResolvedViewLifecycleOperation::Switch { target, spec } => {
                            if let ViewSpec::Buffer {
                                source: BufferViewSource::Content(content),
                            } = &spec
                            {
                                self.retarget_execution_frame(frame, *content)?;
                                self.validate_buffer_view_content(*content)
                                    .map_err(|error| {
                                        recoverable_message(io::ErrorKind::Other, error.to_string())
                                    })?;
                            }
                            self.prepare_topology_effect(
                                frame,
                                PreparedEffect::ViewSwitch { target, spec },
                            )
                            .map(|_| ())
                        }
                    }
                }
                ResolvedOperation::ViewBinding {
                    view,
                    binding,
                    content,
                } => {
                    if !frame.can_prepare_lifecycle() || !queue.is_empty() {
                        return Err(invalid_operation(
                            "a View rebind must be the only operation in its frame",
                        ));
                    }
                    self.prepare_topology_effect(
                        frame,
                        PreparedEffect::ViewRebind {
                            view,
                            binding,
                            content,
                        },
                    )
                    .map(|_| ())
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
                        let body_space =
                            self.session.body_space_for_view(view).ok_or_else(|| {
                                recoverable_message(
                                    io::ErrorKind::InvalidInput,
                                    "viewport command targets a view without a body pane",
                                )
                            })?;
                        let resolved = self.frontend.resolve_viewport_command(
                            self.session.scene(),
                            self.session.scene_revision(),
                            body_space,
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
                ResolvedOperation::Clipboard {
                    view,
                    content,
                    operation,
                } => self.execute_clipboard(operation, view, content, frame),
                ResolvedOperation::Search {
                    view,
                    content,
                    operation,
                } => self.execute_search(operation, view, content, frame),
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
            OperationRequest::ExecuteCommandLine(request) => {
                if origin.scope != OperationOriginScope::View {
                    return Err(invalid_operation(
                        "command line execution requires a view-scoped origin",
                    ));
                }
                let (view, content) = self.resolve_view_target(ViewTarget::Current, origin)?;
                Ok(ResolvedOperation::ExecuteCommandLine {
                    request,
                    view,
                    content,
                })
            }
            OperationRequest::App(operation) => Ok(ResolvedOperation::App(operation)),
            OperationRequest::ContentLifecycle(operation) => {
                let operation = match operation {
                    ContentLifecycleOperation::Create => ResolvedContentLifecycleOperation::Create,
                    ContentLifecycleOperation::Open { path } => {
                        ResolvedContentLifecycleOperation::Open { path }
                    }
                    ContentLifecycleOperation::List => ResolvedContentLifecycleOperation::List,
                    ContentLifecycleOperation::Close { target, force } => {
                        let content = self.resolve_content_target(target, origin)?;
                        ResolvedContentLifecycleOperation::Close { content, force }
                    }
                    ContentLifecycleOperation::Save { target, force } => {
                        let content = self.resolve_content_target(target, origin)?;
                        ResolvedContentLifecycleOperation::Save { content, force }
                    }
                    ContentLifecycleOperation::SaveAs {
                        target,
                        path,
                        force,
                    } => {
                        let content = self.resolve_content_target(target, origin)?;
                        ResolvedContentLifecycleOperation::SaveAs {
                            content,
                            path,
                            force,
                        }
                    }
                    ContentLifecycleOperation::Reload { target, force } => {
                        let content = self.resolve_content_target(target, origin)?;
                        ResolvedContentLifecycleOperation::Reload { content, force }
                    }
                };
                Ok(ResolvedOperation::ContentLifecycle(operation))
            }
            OperationRequest::ViewLifecycle(operation) => {
                if origin.scope != OperationOriginScope::View {
                    return Err(invalid_operation(
                        "view lifecycle operation requires a view-scoped origin",
                    ));
                }
                let operation = match operation {
                    ViewLifecycleOperation::Focus { view } => {
                        self.resolve_view_target(ViewTarget::Id(view), origin)?;
                        ResolvedViewLifecycleOperation::Focus { view }
                    }
                    ViewLifecycleOperation::Switch { spec } => {
                        let source = origin
                            .view
                            .ok_or_else(|| invalid_operation("view switch has no source view"))?;
                        let target =
                            self.session
                                .switch_target_from_view(source)
                                .ok_or_else(|| {
                                    invalid_operation("view switch has no switchable target")
                                })?;
                        let spec = match spec {
                            ViewSpec::Buffer {
                                source: BufferViewSource::Content(content),
                            } => ViewSpec::buffer(
                                self.resolve_content_target(ContentTarget::Id(content), origin)?,
                            ),
                            spec => spec,
                        };
                        ResolvedViewLifecycleOperation::Switch { target, spec }
                    }
                };
                Ok(ResolvedOperation::ViewLifecycle(operation))
            }
            OperationRequest::ViewBinding { target, operation } => {
                if origin.scope != OperationOriginScope::View {
                    return Err(invalid_operation(
                        "View binding operation requires a view-scoped origin",
                    ));
                }
                let view = match target {
                    ViewTarget::Current => origin
                        .view
                        .ok_or_else(|| invalid_operation("operation has no current view"))?,
                    ViewTarget::Id(view) => view,
                };
                let view_data = self
                    .session
                    .view(view)
                    .ok_or_else(|| invalid_operation("operation targets missing view"))?;
                if target == ViewTarget::Current
                    && view_data
                        .document_content()
                        .is_some_and(|content| origin.content != Some(content))
                {
                    return Err(invalid_operation("view/content target mismatch"));
                }
                match operation {
                    ViewBindingOperation::Rebind { binding, content } => {
                        if view_data.binding(binding.as_str()).is_none() {
                            return Err(invalid_operation(format!(
                                "view {} has no {binding} binding",
                                view.0
                            )));
                        }
                        let content = self.resolve_content_target(content, origin)?;
                        Ok(ResolvedOperation::ViewBinding {
                            view,
                            binding,
                            content,
                        })
                    }
                }
            }
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
            OperationRequest::Clipboard { target, operation } => {
                if origin.scope != OperationOriginScope::View {
                    return Err(invalid_operation(
                        "clipboard operation requires a view-scoped origin",
                    ));
                }
                let (view, content) = self.resolve_view_target(target, origin)?;
                Ok(ResolvedOperation::Clipboard {
                    view,
                    content,
                    operation,
                })
            }
            OperationRequest::Search { target, operation } => {
                if origin.scope != OperationOriginScope::View {
                    return Err(invalid_operation(
                        "search operation requires a view-scoped origin",
                    ));
                }
                let (view, content) = self.resolve_view_target(target, origin)?;
                Ok(ResolvedOperation::Search {
                    view,
                    content,
                    operation,
                })
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
            .document_content()
            .ok_or_else(|| invalid_operation("operation targets a non-document view"))?;
        if target == ViewTarget::Current
            && origin.content.is_some_and(|expected| expected != content)
        {
            return Err(invalid_operation("view/content target mismatch"));
        }
        Ok((view, content))
    }

    fn execute_reload(
        &mut self,
        content: ContentId,
        force: bool,
        frame: &mut ExecutionFrame,
    ) -> io::Result<()> {
        let prepared = self
            .prepare_reload_buffer(content, force)
            .map_err(|error| recoverable_message(io::ErrorKind::Other, error.to_string()))?;
        self.checkpoint_target(frame, content);
        for (view, revision) in self.session.content_view_revisions(content) {
            frame.record_view_touch(view, revision);
        }
        let result = self.kernel.execute(
            content,
            ContentInput::Event(vell_core::content::ContentEvent::Reload {
                path: prepared.path.clone(),
                text: prepared.text,
                backing_state: prepared.backing_state,
            }),
        );
        let ContentResult::Handled(outcome) = result else {
            return Err(invalid_operation("reload target is not a buffer"));
        };
        if let Some(change) = &outcome.change {
            self.session
                .transform_content_views(self.kernel.contents(), content, None, change)
                .map_err(invalid_content_view_state)?;
            self.notify_mode_content_changed(content, change, frame);
        }
        self.prepare_effect(
            frame,
            PreparedEffect::ReloadCommit {
                content,
                path: prepared.path,
                baseline: prepared.baseline,
            },
        );
        Ok(())
    }

    fn execute_save_as(
        &mut self,
        content: ContentId,
        path: String,
        force: bool,
        frame: &mut ExecutionFrame,
    ) -> io::Result<()> {
        let prepared = self
            .prepare_save_buffer_as(content, std::path::Path::new(&path), force)
            .map_err(|error| recoverable_message(io::ErrorKind::Other, error.to_string()))?;
        self.prepare_effect(
            frame,
            PreparedEffect::SaveAs {
                content,
                snapshot: prepared.snapshot,
                identity: prepared.identity,
                force: prepared.force,
            },
        );
        Ok(())
    }

    fn execute_save(&mut self, content: ContentId, frame: &mut ExecutionFrame) -> io::Result<()> {
        self.execute_save_with_force(content, false, frame)
    }

    fn execute_save_with_force(
        &mut self,
        content: ContentId,
        force: bool,
        frame: &mut ExecutionFrame,
    ) -> io::Result<()> {
        if !self.kernel.has_pending_save(content)
            && let Some((path, _)) = self.kernel.buffer_path_record(content)
        {
            let path = path.clone();
            self.preflight_registered_save(content, &path, force)
                .map_err(|error| recoverable_message(io::ErrorKind::Other, error.to_string()))?;
        }
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
                self.prepare_effect(
                    frame,
                    PreparedEffect::Save {
                        content,
                        snapshot,
                        force,
                        task: None,
                    },
                );
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

    fn execute_clipboard(
        &mut self,
        operation: ClipboardOperation,
        view: ViewId,
        content: ContentId,
        frame: &mut ExecutionFrame,
    ) -> io::Result<()> {
        let selections = self
            .session
            .view(view)
            .and_then(|view| view.selections())
            .ok_or_else(|| invalid_operation("clipboard operation requires buffer view state"))?
            .clone();
        match operation {
            ClipboardOperation::Copy { kind, destination } => {
                let payload = self
                    .kernel
                    .copy_selections(content, &selections, kind)
                    .ok_or_else(|| invalid_operation("content does not support clipboard copy"))?;
                self.prepare_effect(
                    frame,
                    PreparedEffect::ClipboardStore {
                        payload,
                        write_system: destination == ClipboardDestination::InternalAndSystem,
                    },
                );
                Ok(())
            }
            ClipboardOperation::CopyForEdit {
                command,
                kind,
                destination,
            } => {
                let payload = self
                    .kernel
                    .copy_for_edit(content, &selections, command, kind)
                    .ok_or_else(|| {
                        invalid_operation("content does not support clipboard edit copy")
                    })?;
                self.prepare_effect(
                    frame,
                    PreparedEffect::ClipboardStore {
                        payload,
                        write_system: destination == ClipboardDestination::InternalAndSystem,
                    },
                );
                Ok(())
            }
            ClipboardOperation::Cut { kind, destination } => {
                let (payload, plan) = self
                    .kernel
                    .plan_cut(content, &selections, kind)
                    .ok_or_else(|| invalid_operation("content does not support clipboard cut"))?;
                self.apply_view_edit_plan(
                    ViewEditPlan {
                        expected: ViewPrecondition::Selections(selections),
                        content: plan.action,
                        view: Some(ViewAction::SetSelections(plan.selections)),
                    },
                    view,
                    content,
                    frame,
                )?;
                self.prepare_effect(
                    frame,
                    PreparedEffect::ClipboardStore {
                        payload,
                        write_system: destination == ClipboardDestination::InternalAndSystem,
                    },
                );
                Ok(())
            }
            ClipboardOperation::Paste { source, placement } => {
                let payload = match source {
                    ClipboardSource::Internal => self.kernel.clipboard().clone(),
                    ClipboardSource::System => match self.frontend.read_clipboard() {
                        Ok(Some(text)) => {
                            let payload = ClipboardPayload::character(text);
                            self.prepare_effect(
                                frame,
                                PreparedEffect::ClipboardStore {
                                    payload: payload.clone(),
                                    write_system: false,
                                },
                            );
                            payload
                        }
                        Ok(None) => {
                            self.record_runtime_message(
                                "system clipboard is unavailable; using internal clipboard"
                                    .to_owned(),
                            );
                            self.kernel.clipboard().clone()
                        }
                        Err(error) => {
                            self.record_runtime_message(format!(
                                "system clipboard read failed; using internal clipboard: {error}"
                            ));
                            self.kernel.clipboard().clone()
                        }
                    },
                };
                let plan = self
                    .kernel
                    .plan_paste(content, &selections, &payload, placement)
                    .ok_or_else(|| invalid_operation("content does not support clipboard paste"))?;
                self.apply_view_edit_plan(
                    ViewEditPlan {
                        expected: ViewPrecondition::Selections(selections),
                        content: plan.action,
                        view: Some(ViewAction::SetSelections(plan.selections)),
                    },
                    view,
                    content,
                    frame,
                )
            }
        }
    }

    fn execute_search(
        &mut self,
        operation: SearchOperation,
        view: ViewId,
        content: ContentId,
        frame: &mut ExecutionFrame,
    ) -> io::Result<()> {
        let expected_revision = match &operation {
            SearchOperation::Find {
                expected_revision, ..
            }
            | SearchOperation::ReplaceNext {
                expected_revision, ..
            }
            | SearchOperation::ReplaceAll {
                expected_revision, ..
            } => *expected_revision,
        };
        let snapshot = self
            .kernel
            .search_snapshot(content, expected_revision)
            .map_err(|error| recoverable_message(io::ErrorKind::InvalidData, error.to_string()))?
            .ok_or_else(|| invalid_operation("content does not support search"))?;
        let before = self
            .session
            .view(view)
            .and_then(|view| view.selections())
            .ok_or_else(|| invalid_operation("search requires buffer view state"))?
            .clone();
        let search_error = |error: vell_core::search::SearchError| {
            recoverable_message(io::ErrorKind::InvalidInput, error.to_string())
        };
        match operation {
            SearchOperation::Find {
                expected_revision: _,
                start,
                pattern,
                options,
            } => {
                let Some(found) = snapshot
                    .find_from(&pattern, options, start)
                    .map_err(search_error)?
                else {
                    return Ok(());
                };
                let selections = selection_for_match(
                    snapshot.text().grapheme_range(found.range),
                    options.direction,
                );
                self.apply_view_action(view, ViewAction::SetSelections(selections), frame)
            }
            SearchOperation::ReplaceNext {
                expected_revision: _,
                start,
                pattern,
                replacement,
                options,
            } => {
                let Some(edit) = snapshot
                    .replace_next(&pattern, &replacement, options, start)
                    .map_err(search_error)?
                else {
                    return Ok(());
                };
                let after = snapshot.text().apply(&edit.change).map_err(|error| {
                    recoverable_message(io::ErrorKind::InvalidData, format!("{error:?}"))
                })?;
                let selections = selection_for_match(
                    after.grapheme_range(edit.selection.range),
                    options.direction,
                );
                self.apply_view_edit_plan(
                    ViewEditPlan {
                        expected: ViewPrecondition::Selections(before),
                        content: Some(vell_core::action::ContentAction::Text(edit.change)),
                        view: Some(ViewAction::SetSelections(selections)),
                    },
                    view,
                    content,
                    frame,
                )
            }
            SearchOperation::ReplaceAll {
                expected_revision: _,
                pattern,
                replacement,
                case,
            } => {
                let change = snapshot
                    .replace_all(&pattern, &replacement, case)
                    .map_err(search_error)?;
                if change.is_empty() {
                    return Ok(());
                }
                self.apply_view_edit_plan(
                    ViewEditPlan {
                        expected: ViewPrecondition::Selections(before),
                        content: Some(vell_core::action::ContentAction::Text(change)),
                        view: None,
                    },
                    view,
                    content,
                    frame,
                )
            }
        }
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
                .is_some_and(|data| data.document_content() == Some(record.target))
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
            .and_then(|view| view.document_content())
            .ok_or_else(|| invalid_operation("view action targets a non-document view"))?;
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
