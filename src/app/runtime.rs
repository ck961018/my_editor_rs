use std::collections::VecDeque;
use std::future;
use std::io;
use std::time::Instant;

use crate::app::application::App;
use crate::app::dispatcher::{DispatchCommand, DispatchInput, DispatchOutcome};
use crate::app::query::AppQuery;
use crate::core::command::{AppCommand, ContentCommand, EditCommand};
use crate::core::content::{ContentInput, ContentResult};
use crate::frontend::Frontend;
use crate::protocol::content_query::RenderQuery;
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::ViewId;
use crate::protocol::revision::Revision;
use crate::protocol::viewport::{ViewportCommand, ViewportCursorBehavior, ViewportMoveDirection};

const MAX_COMMAND_CHAIN: usize = 256;

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
            let input_deadline = self.session.next_input_deadline();
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
            let outcome = self.session.dispatch(input, now);
            self.apply_dispatch_outcome(outcome, &mut queue, now)?;
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
                self.session.sync_focused_input(now);
                prepend_inputs(queue, replay);
            }
        }
        Ok(())
    }

    fn handle_input_timeout(&mut self) -> io::Result<()> {
        loop {
            let now = Instant::now();
            if self
                .session
                .next_input_deadline()
                .is_none_or(|deadline| deadline > now)
            {
                return Ok(());
            }
            let outcome = self.session.dispatch_timeout(now);
            let mut replay = VecDeque::new();
            self.apply_dispatch_outcome(outcome, &mut replay, now)?;
            self.process_input_queue(replay)?;
        }
    }

    pub(super) fn execute_command(&mut self, command: DispatchCommand) -> io::Result<()> {
        let mut command = command;
        let mut mode_revisions: Vec<(ViewId, Revision)> = Vec::new();

        for _ in 0..MAX_COMMAND_CHAIN {
            let next = match command {
                DispatchCommand::App(command) => {
                    match command {
                        AppCommand::Quit => self.kernel.cancel(),
                        AppCommand::FocusNext | AppCommand::FocusPrev => {}
                    }
                    None
                }
                DispatchCommand::Content { command, content } => {
                    let result = self.kernel.execute(content, ContentInput::Command(command));
                    self.handle_content_result(content, result);
                    None
                }
                DispatchCommand::ContentWithView {
                    command,
                    view,
                    content,
                } => {
                    let result = {
                        let target_view = self.session.view_mut(view).expect("target view exists");
                        assert_eq!(
                            target_view.content(),
                            content,
                            "view/content target mismatch"
                        );
                        let result = self.kernel.execute(
                            content,
                            ContentInput::View {
                                command,
                                state: target_view.state_mut(),
                            },
                        );
                        if matches!(&result, ContentResult::Handled(outcome) if outcome.view_changed)
                        {
                            target_view.touch();
                        }
                        result
                    };
                    if let ContentResult::Handled(outcome) = &result
                        && let Some(change) = &outcome.change
                    {
                        self.session.transform_content_views(
                            self.kernel.contents(),
                            content,
                            Some(view),
                            change,
                        );
                    }
                    self.handle_content_result(content, result);
                    None
                }
                DispatchCommand::Mode {
                    command,
                    view,
                    content,
                } => {
                    let (output, revision_before) = {
                        let target_view = self.session.view_mut(view).expect("target view exists");
                        assert_eq!(
                            target_view.content(),
                            content,
                            "view/content target mismatch"
                        );
                        let output = target_view
                            .execute_mode_command(self.kernel.modes(), &command)
                            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                        (output, target_view.revision())
                    };
                    if !mode_revisions.iter().any(|(recorded, _)| *recorded == view) {
                        mode_revisions.push((view, revision_before));
                    }
                    output
                        .map(|command| {
                            self.session
                                .resolve_from_view(command, view)
                                .ok_or_else(|| {
                                    io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        format!("mode emitted a command for missing view {view:?}"),
                                    )
                                })
                        })
                        .transpose()?
                }
                DispatchCommand::Viewport {
                    command,
                    view,
                    content,
                } => {
                    let lines = self.frontend.execute_viewport_command(
                        self.session.scene(),
                        self.session.scene_revision(),
                        view,
                        command,
                    )?;
                    (lines != 0).then(|| DispatchCommand::ContentWithView {
                        command: ContentCommand::Edit(viewport_cursor_edit(command, lines)),
                        view,
                        content,
                    })
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
