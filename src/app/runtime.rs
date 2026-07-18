use std::collections::VecDeque;
use std::future;
use std::io;
use std::time::Instant;

use crate::app::action::{TransactionIntent, ViewAction};
use crate::app::application::App;
use crate::app::command::{AppCommand, ContentCommand, TransactionCommand};
use crate::app::dispatcher::{DispatchCommand, DispatchInput, DispatchOutcome, InputModeSnapshot};
use crate::app::mode::{
    ContentModeOperation, ModeStateSnapshot, ResolvedViewEdit, ViewModeOperation,
};
use crate::app::query::AppQuery;
use crate::app::transaction::{TransactionData, TransactionRecord, ViewTransactionData};
use crate::core::command::EditCommand;
use crate::core::content::{
    ContentActionResult, ContentEffect, ContentInput, ContentResult, SaveSnapshot,
};
use crate::core::transaction::TransactionDirection;
use crate::frontend::Frontend;
use crate::protocol::content_query::RenderQuery;
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::{ContentId, ViewId};
use crate::protocol::revision::Revision;
use crate::protocol::viewport::{ViewportCommand, ViewportCursorBehavior, ViewportMoveDirection};

const MAX_COMMAND_CHAIN: usize = 256;

enum ModeRollback {
    Content(ContentId, ModeStateSnapshot),
    View(ViewId, ModeStateSnapshot),
    Text(TransactionRecord, TransactionDirection),
}

enum DeferredEffect {
    Save(ContentId, SaveSnapshot),
    Viewport(ViewId, ViewportCommand, usize),
    Quit,
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
    pub async fn run(&mut self) -> io::Result<()> {
        let run_result = self.run_loop().await;
        let shutdown_result = self.shutdown_tasks().await;
        run_result.and(shutdown_result)
    }

    async fn run_loop(&mut self) -> io::Result<()> {
        self.render()?;
        loop {
            let input_deadline = self
                .session
                .next_input_deadline(self.kernel.content_modes(), self.kernel.contents());
            let cancellation = self.kernel.cancellation_token();
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => break,
                _ = wait_for_input_deadline(input_deadline) => {
                    self.handle_input_timeout()?;
                }
                message = self.kernel.receive_message() => {
                    if let Some(message) = message {
                        self.handle_app_message(message)?;
                    } else {
                        self.kernel.cancel();
                    }
                }
                ev = self.frontend.next_event() => {
                    match ev? {
                        Some(event) => self.handle_event(event).await?,
                        None => self.kernel.cancel(),
                    }
                }
            }
            if !self.kernel.is_cancelled() {
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

    pub(super) async fn handle_event(&mut self, event: FrontendEvent) -> io::Result<()> {
        match event {
            FrontendEvent::Resize(r) => self.session.resize(r.width, r.height),
            FrontendEvent::Key(k) => {
                self.process_input_queue(VecDeque::from([DispatchInput::Normal(k)]))?;
            }
            FrontendEvent::QuitRequest => self.kernel.cancel(),
        }
        Ok(())
    }

    fn process_input_queue(&mut self, mut queue: VecDeque<DispatchInput>) -> io::Result<()> {
        while !self.kernel.is_cancelled() {
            let Some(input) = queue.pop_front() else {
                break;
            };
            let now = Instant::now();
            let (contents, content_modes) = self.kernel.mode_runtime_parts();
            let (outcome, mode_snapshots, mode_revisions) =
                self.session.dispatch(input, now, content_modes, contents);
            if let Err(error) = self.apply_dispatch_outcome(outcome, &mut queue, now) {
                self.restore_input_modes(mode_snapshots);
                return Err(error);
            }
            self.touch_unchanged_mode_views(&mode_revisions);
        }
        Ok(())
    }

    fn apply_dispatch_outcome(
        &mut self,
        outcome: DispatchOutcome,
        queue: &mut VecDeque<DispatchInput>,
        now: Instant,
    ) -> io::Result<()> {
        match outcome {
            DispatchOutcome::Waiting | DispatchOutcome::Consumed => {}
            DispatchOutcome::Replay(replay) => prepend_inputs(queue, replay),
            DispatchOutcome::Emit { command, replay } => {
                self.execute_command(command)?;
                self.session.sync_focused_input(
                    now,
                    self.kernel.content_modes(),
                    self.kernel.contents(),
                );
                prepend_inputs(queue, replay);
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
            let (contents, content_modes) = self.kernel.mode_runtime_parts();
            let (outcome, mode_snapshots, mode_revisions) =
                self.session.dispatch_timeout(now, content_modes, contents);
            let mut replay = VecDeque::new();
            if let Err(error) = self.apply_dispatch_outcome(outcome, &mut replay, now) {
                self.restore_input_modes(mode_snapshots);
                return Err(error);
            }
            self.touch_unchanged_mode_views(&mode_revisions);
            self.process_input_queue(replay)?;
        }
    }

    fn restore_input_modes(&mut self, snapshots: Vec<InputModeSnapshot>) {
        for snapshot in snapshots.into_iter().rev() {
            match snapshot {
                InputModeSnapshot::Content(content, snapshot) => {
                    self.kernel.restore_content_mode(content, snapshot);
                }
                InputModeSnapshot::View(view, snapshot) => {
                    self.session.restore_view_mode(view, snapshot);
                }
            }
        }
    }

    pub(super) fn execute_command(&mut self, command: DispatchCommand) -> io::Result<()> {
        let mut rollbacks = Vec::new();
        let mut deferred_effects = Vec::new();
        let mut budget = MAX_COMMAND_CHAIN;
        let content = command.content();
        let content_snapshot = content.and_then(|content| self.kernel.snapshot_content(content));
        let selection_snapshot = content.map(|content| self.session.snapshot_selections(content));
        self.kernel.start_command_transaction(content);
        let result =
            self.execute_command_inner(command, &mut rollbacks, &mut deferred_effects, &mut budget);
        if result.is_err() {
            for rollback in rollbacks.into_iter().rev() {
                match rollback {
                    ModeRollback::Content(content, snapshot) => {
                        self.kernel.restore_content_mode(content, snapshot);
                    }
                    ModeRollback::View(view, snapshot) => {
                        self.session.restore_view_mode(view, snapshot);
                    }
                    ModeRollback::Text(record, direction) => {
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
            if let Some(snapshot) = content_snapshot {
                self.kernel.restore_content(snapshot);
            }
            if let Some(snapshot) = selection_snapshot {
                self.session.restore_selections(snapshot);
            }
        }
        self.kernel.finish_command_transaction(result.is_ok());
        if result.is_ok() {
            for effect in deferred_effects {
                match effect {
                    DeferredEffect::Save(content, snapshot) => {
                        self.kernel.queue_save(content, snapshot);
                    }
                    DeferredEffect::Viewport(view, command, lines) => {
                        self.frontend.apply_viewport_command(view, command, lines);
                    }
                    DeferredEffect::Quit => self.kernel.cancel(),
                }
            }
        }
        result
    }

    fn execute_command_inner(
        &mut self,
        command: DispatchCommand,
        rollbacks: &mut Vec<ModeRollback>,
        deferred_effects: &mut Vec<DeferredEffect>,
        budget: &mut usize,
    ) -> io::Result<()> {
        let mut command = command;
        let mut mode_revisions: Vec<(ViewId, Revision)> = Vec::new();

        while *budget > 0 {
            *budget -= 1;
            let next = match command {
                DispatchCommand::App(command) => {
                    match command {
                        AppCommand::Quit => deferred_effects.push(DeferredEffect::Quit),
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
                    let result = self.kernel.execute(content, ContentInput::Save);
                    if let ContentResult::Handled(outcome) = result
                        && let ContentEffect::Save(snapshot) = outcome.effect
                    {
                        deferred_effects.push(DeferredEffect::Save(content, snapshot));
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
                    self.execute_view_content_command(command, view, content, rollbacks)?;
                    None
                }
                DispatchCommand::Mode {
                    command,
                    view,
                    content,
                } => {
                    if self.kernel.has_content_mode(content) {
                        if !rollbacks.iter().any(|rollback| {
                            matches!(rollback, ModeRollback::Content(id, _) if *id == content)
                        }) && let Some(snapshot) = self.kernel.snapshot_content_mode(content)
                        {
                            rollbacks.push(ModeRollback::Content(content, snapshot));
                        }
                        let operations = self
                            .kernel
                            .execute_content_mode(content, &command)
                            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                        self.execute_content_mode_operations(
                            operations,
                            content,
                            rollbacks,
                            deferred_effects,
                            budget,
                        )?;
                        None
                    } else {
                        if !rollbacks.iter().any(
                            |rollback| matches!(rollback, ModeRollback::View(id, _) if *id == view),
                        ) && let Some(snapshot) = self.session.snapshot_view_mode(view)
                        {
                            rollbacks.push(ModeRollback::View(view, snapshot));
                        }
                        let target_view = self.session.view(view).expect("target view exists");
                        assert_eq!(
                            target_view.content(),
                            content,
                            "view/content target mismatch"
                        );
                        let revision_before = target_view.revision();
                        let operations = self
                            .session
                            .execute_mode(
                                view,
                                self.kernel.modes(),
                                self.kernel.contents(),
                                &command,
                            )
                            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                        if !mode_revisions.iter().any(|(recorded, _)| *recorded == view) {
                            mode_revisions.push((view, revision_before));
                        }
                        self.execute_view_mode_operations(
                            operations,
                            view,
                            content,
                            rollbacks,
                            deferred_effects,
                            budget,
                        )?;
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
                        deferred_effects.push(DeferredEffect::Viewport(view, command, lines));
                        Some(DispatchCommand::ContentWithView {
                            command: ContentCommand::Edit(viewport_cursor_edit(command, lines)),
                            view,
                            content,
                        })
                    }
                }
                DispatchCommand::ContentMode { operation, content } => {
                    self.execute_content_mode_operations(
                        vec![operation],
                        content,
                        rollbacks,
                        deferred_effects,
                        budget,
                    )?;
                    None
                }
                DispatchCommand::ContentModeOperations {
                    operations,
                    content,
                } => {
                    self.execute_content_mode_operations(
                        operations,
                        content,
                        rollbacks,
                        deferred_effects,
                        budget,
                    )?;
                    None
                }
                DispatchCommand::ViewModeOperations {
                    operations,
                    view,
                    content,
                } => {
                    self.execute_view_mode_operations(
                        operations,
                        view,
                        content,
                        rollbacks,
                        deferred_effects,
                        budget,
                    )?;
                    None
                }
                DispatchCommand::Noop => None,
            };

            let Some(next) = next else {
                self.touch_unchanged_mode_views(&mode_revisions);
                return Ok(());
            };
            command = next;
        }

        self.touch_unchanged_mode_views(&mode_revisions);
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("command chain exceeded the limit of {MAX_COMMAND_CHAIN} commands"),
        ))
    }

    fn execute_view_content_command(
        &mut self,
        command: ContentCommand,
        view: ViewId,
        content: ContentId,
        rollbacks: &mut Vec<ModeRollback>,
    ) -> io::Result<()> {
        if let ContentCommand::Sequence(commands) = command {
            for command in commands.into_commands() {
                self.execute_view_content_command(command, view, content, rollbacks)?;
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
            ContentCommand::Edit(command) => self.execute_edit(command, view, content, rollbacks),
            ContentCommand::Transaction(command) => {
                match command {
                    TransactionCommand::Begin => {
                        self.kernel.begin_transaction(content, Some(view));
                    }
                    TransactionCommand::Commit => {
                        self.kernel.commit_transaction(content);
                    }
                    TransactionCommand::Rollback => {
                        if let Some(record) = self.kernel.rollback_transaction(content) {
                            self.apply_history_record(
                                &record,
                                TransactionDirection::Inverse,
                                rollbacks,
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
                    self.apply_history_record(&record, direction, rollbacks)?;
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
        rollbacks: &mut Vec<ModeRollback>,
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
            rollbacks,
        )
    }

    fn apply_resolved_view_edit(
        &mut self,
        edit: ResolvedViewEdit,
        view: ViewId,
        content: ContentId,
        rollbacks: &mut Vec<ModeRollback>,
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
                self.apply_view_action(view, action)?;
            }
            return Ok(());
        };

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
                self.apply_view_action(view, action)?;
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
            rollbacks.push(ModeRollback::Text(
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
        operations: Vec<ViewModeOperation>,
        view: ViewId,
        content: ContentId,
        rollbacks: &mut Vec<ModeRollback>,
        deferred_effects: &mut Vec<DeferredEffect>,
        budget: &mut usize,
    ) -> io::Result<()> {
        for operation in operations {
            match operation {
                ViewModeOperation::Edit(edit) => {
                    self.apply_resolved_view_edit(edit, view, content, rollbacks)?;
                }
                ViewModeOperation::DeferredEdit(command) => {
                    self.execute_edit(command, view, content, rollbacks)?;
                }
                ViewModeOperation::View(action) => {
                    self.apply_view_action(view, action)?;
                }
                ViewModeOperation::Content(action) => {
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
                        rollbacks,
                    )?;
                }
                ViewModeOperation::Transaction(intent) => {
                    self.execute_transaction_intent(intent, Some(view), content, rollbacks)?;
                }
                ViewModeOperation::App(command) => {
                    self.execute_command_inner(
                        DispatchCommand::App(command),
                        rollbacks,
                        deferred_effects,
                        budget,
                    )?;
                }
                ViewModeOperation::Mode(command) => {
                    self.execute_command_inner(
                        DispatchCommand::Mode {
                            command,
                            view,
                            content,
                        },
                        rollbacks,
                        deferred_effects,
                        budget,
                    )?;
                }
                ViewModeOperation::Viewport(command) => {
                    self.execute_command_inner(
                        DispatchCommand::Viewport {
                            command,
                            view,
                            content,
                        },
                        rollbacks,
                        deferred_effects,
                        budget,
                    )?;
                }
                ViewModeOperation::Save => {
                    self.execute_command_inner(
                        DispatchCommand::Content {
                            command: ContentCommand::Save,
                            content,
                        },
                        rollbacks,
                        deferred_effects,
                        budget,
                    )?;
                }
                ViewModeOperation::Noop => {}
            }
        }
        Ok(())
    }

    fn execute_content_mode_operations(
        &mut self,
        operations: Vec<ContentModeOperation>,
        content: ContentId,
        rollbacks: &mut Vec<ModeRollback>,
        deferred_effects: &mut Vec<DeferredEffect>,
        budget: &mut usize,
    ) -> io::Result<()> {
        for operation in operations {
            match operation {
                ContentModeOperation::Content(action) => {
                    self.execute_content_action(action, content, rollbacks)?;
                }
                ContentModeOperation::Transaction(intent) => {
                    self.execute_transaction_intent(intent, None, content, rollbacks)?;
                }
                ContentModeOperation::Save => {
                    self.execute_command_inner(
                        DispatchCommand::Content {
                            command: ContentCommand::Save,
                            content,
                        },
                        rollbacks,
                        deferred_effects,
                        budget,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn execute_content_action(
        &mut self,
        action: crate::core::action::ContentAction,
        content: ContentId,
        rollbacks: &mut Vec<ModeRollback>,
    ) -> io::Result<()> {
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
        }
        if let Some(transaction) = transaction {
            let record = TransactionRecord {
                target: content,
                data: TransactionData {
                    content: transaction,
                    view: ViewTransactionData::None,
                },
            };
            rollbacks.push(ModeRollback::Text(
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
        rollbacks: &mut Vec<ModeRollback>,
    ) -> io::Result<()> {
        match intent {
            TransactionIntent::Begin => {
                self.kernel.begin_transaction(content, owner);
            }
            TransactionIntent::Commit => {
                self.kernel.commit_transaction(content);
            }
            TransactionIntent::Rollback => {
                if let Some(record) = self.kernel.rollback_transaction(content) {
                    self.apply_history_record(&record, TransactionDirection::Inverse, rollbacks)?;
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
                    self.apply_history_record(&record, direction, rollbacks)?;
                }
            }
        }
        Ok(())
    }

    fn apply_history_record(
        &mut self,
        record: &TransactionRecord,
        direction: TransactionDirection,
        rollbacks: &mut Vec<ModeRollback>,
    ) -> io::Result<()> {
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
        rollbacks.push(ModeRollback::Text(record.clone(), direction));
        if let Some((view, selections)) = &source
            && self
                .session
                .view(*view)
                .is_some_and(|data| data.content() == record.target)
        {
            self.apply_view_action(*view, ViewAction::SetSelections(selections.clone()))?;
        }
        if let Some(change) = &change {
            self.session.transform_content_views(
                self.kernel.contents(),
                record.target,
                source.as_ref().map(|(view, _)| *view),
                change,
            );
        }
        Ok(())
    }

    fn apply_view_action(&mut self, view: ViewId, action: ViewAction) -> io::Result<()> {
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
