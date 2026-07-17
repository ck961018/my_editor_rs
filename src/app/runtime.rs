use std::collections::VecDeque;
use std::future;
use std::io;
use std::time::Instant;

use crate::app::application::App;
use crate::app::dispatcher::{DispatchCommand, DispatchInput, DispatchOutcome};
use crate::app::layout::view_for_space;
use crate::app::query::AppQuery;
use crate::app::view::{ModeCommandResult, View};
use crate::core::command::{AppCommand, ContentCommand, EditCommand};
use crate::core::content::{ContentInput, ContentResult};
use crate::frontend::Frontend;
use crate::protocol::content_query::RenderQuery;
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::{ContentId, ViewId};
use crate::protocol::viewport::{ViewportCommand, ViewportCursorBehavior, ViewportMoveDirection};

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
            let input_deadline = self.session.dispatcher.next_deadline(&self.session.views);
            tokio::select! {
                biased;
                _ = self.kernel.tasks.cancelled() => break,
                _ = wait_for_input_deadline(input_deadline) => {
                    self.handle_input_timeout()?;
                }
                message = self.kernel.message_rx.recv() => {
                    if let Some(message) = message {
                        self.handle_app_message(message)?;
                    } else {
                        self.kernel.tasks.cancel();
                    }
                }
                ev = self.frontend.next_event() => {
                    match ev? {
                        Some(event) => self.handle_event(event).await?,
                        None => self.kernel.tasks.cancel(),
                    }
                }
            }
            if !self.kernel.tasks.is_cancelled() {
                self.render()?;
            }
        }
        Ok(())
    }

    pub(super) async fn shutdown_tasks(&mut self) -> io::Result<()> {
        self.kernel.tasks.cancel();
        self.kernel.tasks.close_detached();
        while !self.kernel.pending_saves.is_empty() {
            let message = self
                .kernel
                .message_rx
                .recv()
                .await
                .expect("pending save task must report completion");
            self.handle_app_message(message)?;
        }
        self.kernel.tasks.close_critical();
        self.kernel.tasks.wait_critical().await;
        while let Ok(message) = self.kernel.message_rx.try_recv() {
            self.handle_app_message(message)?;
        }
        Ok(())
    }

    pub(super) async fn handle_event(&mut self, event: FrontendEvent) -> io::Result<()> {
        match event {
            FrontendEvent::Resize(r) => {
                self.session.scene.size.width = r.width as i32;
                self.session.scene.size.height = r.height as i32;
                self.session.scene_revision.next();
            }
            FrontendEvent::Key(k) => {
                self.process_input_queue(VecDeque::from([DispatchInput::Normal(k)]))?;
            }
            FrontendEvent::QuitRequest => self.kernel.tasks.cancel(),
        }
        Ok(())
    }

    fn process_input_queue(&mut self, mut queue: VecDeque<DispatchInput>) -> io::Result<()> {
        while !self.kernel.tasks.is_cancelled() {
            let Some(input) = queue.pop_front() else {
                break;
            };
            let now = Instant::now();
            let outcome = self.session.dispatcher.dispatch(
                input,
                now,
                self.session.focused,
                &self.session.scene,
                &mut self.session.views,
            );
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
                self.sync_focused_input(now);
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
                .dispatcher
                .next_deadline(&self.session.views)
                .is_none_or(|deadline| deadline > now)
            {
                return Ok(());
            }
            let outcome = self.session.dispatcher.dispatch_timeout(
                now,
                self.session.focused,
                &self.session.scene,
                &mut self.session.views,
            );
            let mut replay = VecDeque::new();
            self.apply_dispatch_outcome(outcome, &mut replay, now)?;
            self.process_input_queue(replay)?;
        }
    }

    fn sync_focused_input(&mut self, now: Instant) {
        let Some(view_id) = view_for_space(&self.session.scene, self.session.focused) else {
            return;
        };
        let status = self
            .session
            .views
            .get(&view_id)
            .map_or(crate::core::input::InputStatus::Ready, View::input_status);
        self.session
            .dispatcher
            .sync_view(view_id, status, true, now);
    }

    pub(super) fn execute_command(&mut self, command: DispatchCommand) -> io::Result<()> {
        match command {
            DispatchCommand::App(command) => match command {
                AppCommand::Quit => self.kernel.tasks.cancel(),
                AppCommand::FocusNext | AppCommand::FocusPrev => {}
            },
            DispatchCommand::Content { command, content } => {
                let result = self
                    .kernel
                    .contents
                    .execute(content, ContentInput::Command(command));
                self.handle_content_result(content, result);
            }
            DispatchCommand::ViewContent {
                command,
                view,
                content,
            } => {
                let (command, mode_changed) = {
                    let target_view = self
                        .session
                        .views
                        .get_mut(&view)
                        .expect("target view exists");
                    assert_eq!(
                        target_view.content(),
                        content,
                        "view/content target mismatch"
                    );
                    match command {
                        ContentCommand::Mode { mode, action } => {
                            match target_view.execute_mode_command(
                                &self.kernel.modes,
                                &mode,
                                &action,
                            ) {
                                ModeCommandResult::Unknown => return Ok(()),
                                ModeCommandResult::Handled(Some(command)) => (command, true),
                                ModeCommandResult::Handled(None) => {
                                    target_view.touch();
                                    return Ok(());
                                }
                            }
                        }
                        command => (command, false),
                    }
                };
                let command = match command {
                    ContentCommand::Viewport(command) => {
                        let lines = self.frontend.execute_viewport_command(
                            &self.session.scene,
                            self.session.scene_revision,
                            view,
                            command,
                        )?;
                        if lines == 0 {
                            if mode_changed {
                                self.session
                                    .views
                                    .get_mut(&view)
                                    .expect("target view exists")
                                    .touch();
                            }
                            return Ok(());
                        }
                        ContentCommand::Edit(viewport_cursor_edit(command, lines))
                    }
                    command => command,
                };
                let result = {
                    let target_view = self
                        .session
                        .views
                        .get_mut(&view)
                        .expect("target view exists");
                    assert_eq!(
                        target_view.content(),
                        content,
                        "view/content target mismatch"
                    );
                    let result = self.kernel.contents.execute(
                        content,
                        ContentInput::View {
                            command,
                            state: target_view.state_mut(),
                        },
                    );
                    if mode_changed
                        || matches!(&result, ContentResult::Handled(outcome) if outcome.view_changed)
                    {
                        target_view.touch();
                    }
                    result
                };
                if let ContentResult::Handled(outcome) = &result
                    && let Some(change) = &outcome.change
                {
                    self.transform_content_views(content, Some(view), change);
                }
                self.handle_content_result(content, result);
            }
            DispatchCommand::Noop => {}
        }
        Ok(())
    }

    fn transform_content_views(
        &mut self,
        content: ContentId,
        except: Option<ViewId>,
        change: &crate::core::content::ContentChange,
    ) {
        for (view_id, view) in &mut self.session.views {
            if Some(*view_id) == except || view.content() != content {
                continue;
            }
            if self
                .kernel
                .contents
                .transform_view_state(content, view.state_mut(), change)
                .expect("view content exists")
            {
                view.touch();
            }
        }
    }

    /// 发起异步保存；同一 content 已在保存时，仅保留最新的后续快照。
    pub(super) fn render(&mut self) -> io::Result<()> {
        let query = AppQuery {
            contents: &self.kernel.contents,
            views: &self.session.views,
        };
        self.frontend.render(
            &self.session.scene,
            self.session.scene_revision,
            &query as &dyn RenderQuery,
            self.session.focused,
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
