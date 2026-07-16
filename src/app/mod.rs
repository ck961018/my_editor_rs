//! App：tokio::select! 多路复用 evloop。不感知 tui/gui（只依赖 Frontend trait + Scene）。
//! 事件分发委托 Dispatcher（活动输入层 + Awaiting 状态机），Command 执行通过 ContentStore。
//! 不持 editor_content/status_content 角色 ID——从 scene/focused 推导。

mod dispatcher;
mod kernel;
mod message;
mod remote;
mod scene_model;
mod session;
mod tasks;
mod view;

use std::collections::{HashMap, VecDeque};
use std::future;
use std::io;
use std::path::Path;
use std::time::Instant;

use crate::app::dispatcher::{
    DispatchCommand, DispatchInput, DispatchOutcome, Dispatcher, default_global_keymap,
};
use crate::app::kernel::{Kernel, PendingSave};
use crate::app::message::AppMessage;
use crate::app::scene_model::{
    CloseResult, SceneBuilder, SceneError, SplitResult, build_editor_scene,
};
use crate::app::session::ClientSession;
use crate::app::view::{ModeCommandResult, View};
use crate::core::buffer::Buffer;
use crate::core::command::AppCommand;
use crate::core::content::{
    Content, ContentEffect, ContentEvent, ContentInput, ContentResult, SaveSnapshot,
};
use crate::core::content_store::ContentStore;
use crate::core::mode::ModeRegistry;
use crate::core::status_bar::StatusBar;
use crate::frontend::Frontend;
use crate::protocol::content_query::{
    ContentData, ContentQuery, RenderQuery, TextPresentation, ViewData, ViewPresentation,
};
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::{ContentId, SpaceId, ViewId};
use crate::protocol::scene::Scene;
use crate::protocol::space::{Sizing, SpaceKind, SplitDirection};

pub struct App<F: Frontend> {
    kernel: Kernel,
    session: ClientSession,
    frontend: F,
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

async fn atomic_write(snapshot: SaveSnapshot) -> io::Result<()> {
    tokio::task::spawn_blocking(move || {
        use std::io::Write;

        let parent = snapshot
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(snapshot.bytes.as_bytes())?;
        if let Ok(metadata) = std::fs::metadata(&snapshot.path) {
            temporary
                .as_file()
                .set_permissions(metadata.permissions())?;
        }
        temporary.as_file().sync_all()?;
        temporary
            .persist(&snapshot.path)
            .map_err(|error| error.error)?;
        Ok(())
    })
    .await
    .map_err(io::Error::other)?
}

impl<F: Frontend> App<F> {
    pub fn new(path: Option<&str>, width: usize, height: usize, frontend: F) -> io::Result<Self> {
        let editor_content = ContentId(0);
        let status_content = ContentId(1);
        let mut buffer = Buffer::new();
        if let Some(p) = path {
            buffer.open_path(p)?;
        }
        let status_bar = StatusBar::new(editor_content);
        let mut contents = ContentStore::default();
        contents.insert(editor_content, Content::Buffer(buffer));
        contents.insert(status_content, Content::StatusBar(status_bar));
        let modes = ModeRegistry::builtin();
        let session = create_editor_session(&contents, &modes, width, height);
        let kernel = Kernel::new(contents, modes);
        Ok(Self {
            kernel,
            session,
            frontend,
        })
    }

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

    async fn shutdown_tasks(&mut self) -> io::Result<()> {
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

    async fn handle_event(&mut self, event: FrontendEvent) -> io::Result<()> {
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

    fn execute_command(&mut self, command: DispatchCommand) -> io::Result<()> {
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
                    let mut mode_changed = false;
                    let command = match command {
                        crate::core::command::ContentCommand::Mode { mode, action } => {
                            match target_view.execute_mode_command(
                                &self.kernel.modes,
                                &mode,
                                &action,
                            ) {
                                ModeCommandResult::Unknown => return Ok(()),
                                ModeCommandResult::Handled(Some(command)) => {
                                    mode_changed = true;
                                    command
                                }
                                ModeCommandResult::Handled(None) => {
                                    target_view.touch();
                                    return Ok(());
                                }
                            }
                        }
                        command => command,
                    };
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

    fn handle_app_message(&mut self, message: AppMessage) -> io::Result<()> {
        match message {
            AppMessage::SaveCompleted {
                content,
                revision,
                state,
                result,
            } => {
                let pending = self
                    .kernel
                    .pending_saves
                    .remove(&content)
                    .expect("save completion must match a pending save");
                assert_eq!(pending.revision, revision, "save revision mismatch");
                assert_eq!(pending.state, state, "save state mismatch");
                let result = self.kernel.contents.execute(
                    content,
                    ContentInput::Event(ContentEvent::SaveFinished { state, result }),
                );
                self.handle_content_result(content, result);
                if let Some(snapshot) = pending.queued {
                    self.spawn_save(content, snapshot);
                }
            }
        }
        Ok(())
    }

    fn handle_content_result(&mut self, id: ContentId, result: ContentResult) {
        if let ContentResult::Handled(outcome) = result {
            if let ContentEffect::Save(snapshot) = outcome.effect {
                self.spawn_save(id, snapshot);
            }
        }
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
    fn spawn_save(&mut self, id: ContentId, snapshot: SaveSnapshot) -> bool {
        if let Some(pending) = self.kernel.pending_saves.get_mut(&id) {
            let queued_revision = pending
                .queued
                .as_ref()
                .map_or(pending.revision, |queued| queued.revision);
            if snapshot.revision > queued_revision {
                pending.queued = Some(snapshot);
            }
            return false;
        }
        let tx = self.kernel.message_tx.clone();
        let revision = snapshot.revision;
        let state = snapshot.state;
        self.kernel.pending_saves.insert(
            id,
            PendingSave {
                revision,
                state,
                queued: None,
            },
        );
        self.kernel.tasks.spawn_critical(async move {
            let result = atomic_write(snapshot).await;
            let _ = tx.send(AppMessage::SaveCompleted {
                content: id,
                revision,
                state,
                result,
            });
        });
        true
    }

    fn render(&mut self) -> io::Result<()> {
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

    fn insert_view(&mut self, content: ContentId) -> ViewId {
        let id = ViewId(self.session.next_view_id);
        self.session.next_view_id = self
            .session
            .next_view_id
            .checked_add(1)
            .expect("view id overflow");
        let view = create_view(content, &self.kernel.contents, &self.kernel.modes);
        assert!(
            self.session.views.insert(id, view).is_none(),
            "view id must be unique"
        );
        id
    }

    #[allow(dead_code)] // 本轮只提供后端入口，不接入按键或 UI。
    fn split_space(
        &mut self,
        target: SpaceId,
        content: ContentId,
        focusable: bool,
        direction: SplitDirection,
        focus_new: bool,
    ) -> Result<SplitResult, LayoutError> {
        if !self.kernel.contents.contains(content) {
            return Err(LayoutError::MissingContent(content));
        }

        let previous = self.session.focused;
        let previous_view =
            view_for_space(&self.session.scene, previous).expect("focused space hosts a view");
        let view = self.insert_view(content);
        let result = match self.session.scene_builder.split(
            &mut self.session.scene,
            target,
            view,
            focusable,
            direction,
        ) {
            Ok(result) => result,
            Err(error) => {
                self.session.views.remove(&view);
                self.session.next_view_id = view.0;
                return Err(error.into());
            }
        };
        if focus_new {
            self.session
                .dispatcher
                .invalidate_view(previous_view, &mut self.session.views);
        }
        self.reconcile_layout(if focus_new {
            Some(result.new_space)
        } else {
            Some(previous)
        });
        self.session.scene_revision.next();
        Ok(result)
    }

    #[allow(dead_code)] // 本轮只提供后端入口，不接入按键或 UI。
    fn close_space(&mut self, target: SpaceId) -> Result<CloseResult, LayoutError> {
        if view_space_focusable(&self.session.scene, target) == Some(true)
            && focusable_view_count(&self.session.scene) == 1
        {
            return Err(LayoutError::WouldRemoveLastFocusable(target));
        }

        let removed_view = view_for_space(&self.session.scene, target)
            .ok_or(SceneError::ExpectedContentLeaf(target))?;
        let result = self
            .session
            .scene_builder
            .close(&mut self.session.scene, target)?;
        self.session
            .dispatcher
            .invalidate_view(removed_view, &mut self.session.views);
        self.session.views.remove(&removed_view);
        self.reconcile_layout(result.surviving_neighbor);
        self.session.scene_revision.next();
        Ok(result)
    }

    #[allow(dead_code)] // 本轮只提供后端入口，不接入按键或 UI。
    fn replace_space_content(
        &mut self,
        target: SpaceId,
        content: ContentId,
        focusable: bool,
    ) -> Result<(), LayoutError> {
        if !self.kernel.contents.contains(content) {
            return Err(LayoutError::MissingContent(content));
        }
        if view_space_focusable(&self.session.scene, target) == Some(true)
            && !focusable
            && focusable_view_count(&self.session.scene) == 1
        {
            return Err(LayoutError::NoFocusableSpace);
        }

        let old_view = view_for_space(&self.session.scene, target)
            .ok_or(SceneError::ExpectedContentLeaf(target))?;
        let new_view = self.insert_view(content);
        if let Err(error) = self.session.scene_builder.replace_view(
            &mut self.session.scene,
            target,
            new_view,
            focusable,
        ) {
            self.session.views.remove(&new_view);
            self.session.next_view_id = new_view.0;
            return Err(error.into());
        }
        self.session
            .dispatcher
            .invalidate_view(old_view, &mut self.session.views);
        self.session.views.remove(&old_view);
        self.reconcile_layout(Some(target));
        self.session.scene_revision.next();
        Ok(())
    }

    #[allow(dead_code)] // 本轮只提供后端入口，不接入按键或 UI。
    fn set_space_sizing(&mut self, target: SpaceId, sizing: Sizing) -> Result<(), LayoutError> {
        self.session
            .scene_builder
            .set_sizing(&mut self.session.scene, target, sizing)?;
        self.session.scene_revision.next();
        Ok(())
    }

    #[allow(dead_code)] // 由预留布局入口统一调用。
    fn reconcile_layout(&mut self, preferred: Option<SpaceId>) {
        let previous = self.session.focused;
        debug_assert!(
            scene_views(&self.session.scene)
                .into_iter()
                .all(|(_, view)| self.session.views.contains_key(&view))
        );
        self.session.focused = resolve_focus(&self.session.scene, previous, preferred)
            .expect("App rejects layouts without focusable content spaces");
    }
}

#[allow(dead_code)] // 伴随尚未接入 UI 的布局入口。
#[derive(Debug, PartialEq, Eq)]
enum LayoutError {
    MissingContent(ContentId),
    WouldRemoveLastFocusable(SpaceId),
    NoFocusableSpace,
    Scene(SceneError),
}

impl From<SceneError> for LayoutError {
    fn from(error: SceneError) -> Self {
        Self::Scene(error)
    }
}

fn create_view(content: ContentId, contents: &ContentStore, modes: &ModeRegistry) -> View {
    let state = contents
        .create_view_state(content)
        .expect("view content exists");
    let mode = contents.default_mode(content).map(|name| {
        modes
            .instantiate(&name)
            .expect("content default mode must be registered")
    });
    View::new(content, state, mode)
}

fn create_editor_session(
    contents: &ContentStore,
    modes: &ModeRegistry,
    width: usize,
    height: usize,
) -> ClientSession {
    let editor_view = ViewId(0);
    let status_view = ViewId(1);
    let mut views = HashMap::new();
    views.insert(editor_view, create_view(ContentId(0), contents, modes));
    views.insert(status_view, create_view(ContentId(1), contents, modes));
    let mut scene_builder = SceneBuilder::new();
    let (scene, editor_space) = build_editor_scene(
        &mut scene_builder,
        width as i32,
        height as i32,
        editor_view,
        status_view,
    )
    .expect("valid editor scene");
    let focused = resolve_focus(&scene, editor_space, Some(editor_space))
        .expect("initial scene has a focusable content space");
    ClientSession::new(
        scene,
        scene_builder,
        views,
        2,
        focused,
        Dispatcher::new(default_global_keymap()),
    )
}

fn collect_view_spaces(scene: &Scene, sid: SpaceId, out: &mut Vec<(SpaceId, ViewId)>) {
    let node = scene.node(sid);
    match &node.space.kind {
        SpaceKind::Content { view, .. } => {
            out.push((sid, *view));
        }
        SpaceKind::Container { .. } => {
            for c in &node.children {
                collect_view_spaces(scene, *c, out);
            }
        }
    }
}

fn scene_views(scene: &Scene) -> Vec<(SpaceId, ViewId)> {
    let mut views = Vec::new();
    collect_view_spaces(scene, scene.root(), &mut views);
    views
}

fn view_for_space(scene: &Scene, space: SpaceId) -> Option<ViewId> {
    if !scene.contains(space) {
        return None;
    }
    match &scene.node(space).space.kind {
        SpaceKind::Content { view, .. } => Some(*view),
        SpaceKind::Container { .. } => None,
    }
}

fn view_space_focusable(scene: &Scene, space: SpaceId) -> Option<bool> {
    if !scene.contains(space) {
        return None;
    }
    match &scene.node(space).space.kind {
        SpaceKind::Content { focusable, .. } => Some(*focusable),
        SpaceKind::Container { .. } => None,
    }
}

#[allow(dead_code)] // 由尚未接入 UI 的 close/replace 预检使用。
fn focusable_view_count(scene: &Scene) -> usize {
    scene_views(scene)
        .into_iter()
        .filter(|(space, _)| view_space_focusable(scene, *space) == Some(true))
        .count()
}

fn resolve_focus(scene: &Scene, previous: SpaceId, preferred: Option<SpaceId>) -> Option<SpaceId> {
    preferred
        .filter(|space| view_space_focusable(scene, *space) == Some(true))
        .or_else(|| (view_space_focusable(scene, previous) == Some(true)).then_some(previous))
        .or_else(|| {
            scene_views(scene)
                .into_iter()
                .map(|(space, _)| space)
                .find(|space| view_space_focusable(scene, *space) == Some(true))
        })
}

/// 借 App 数据字段的查询适配器：render 时用它做 `&dyn RenderQuery`，
/// 与 `&mut self.frontend` 不冲突（字段级 split borrow）。
struct AppQuery<'a> {
    contents: &'a ContentStore,
    views: &'a HashMap<ViewId, View>,
}

impl RenderQuery for AppQuery<'_> {
    fn content(&self, cid: ContentId, query: ContentQuery) -> ContentData {
        self.contents.query(cid, query)
    }

    fn view(&self, id: ViewId) -> ViewData {
        let view = self.views.get(&id).expect("scene references existing view");
        let presentation = match view.selections() {
            Some(selections) => ViewPresentation::Text(TextPresentation {
                selections: selections.clone(),
                cursor_style: view.cursor_style(),
            }),
            None => ViewPresentation::StatusBar,
        };
        ViewData {
            content: view.content(),
            presentation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::command::{Command, ContentCommand, EditCommand};
    use crate::core::content::Content;
    use crate::core::content_view_state::ContentViewState;
    use crate::frontend::Frontend;
    use crate::protocol::content_query::{
        ContentData, ContentQuery, CursorStyle, DocumentStatus, RenderQuery, RowRange,
    };
    use crate::protocol::frontend_event::ResizeEvent;
    use crate::protocol::key_event::{ArrowKey, KeyCode, KeyEvent};
    use crate::protocol::revision::Revision;
    use crate::protocol::selection::{Selection, Selections, TextOffset};
    use crate::protocol::space::{Sizing, SplitDirection};
    use crate::protocol::status::StatusMessage;
    use std::collections::VecDeque;

    struct ScriptedFrontend {
        events: VecDeque<FrontendEvent>,
        renders: usize,
        scene_revisions: Vec<Revision>,
        fail_next_event: bool,
        fail_render: bool,
    }

    impl ScriptedFrontend {
        fn new(events: Vec<FrontendEvent>) -> Self {
            Self {
                events: events.into(),
                renders: 0,
                scene_revisions: Vec::new(),
                fail_next_event: false,
                fail_render: false,
            }
        }
    }

    impl Frontend for ScriptedFrontend {
        async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
            if self.fail_next_event {
                self.fail_next_event = false;
                return Err(io::Error::other("scripted frontend failure"));
            }
            Ok(self.events.pop_front())
        }

        fn render(
            &mut self,
            _scene: &Scene,
            scene_revision: Revision,
            _query: &dyn RenderQuery,
            _focused: SpaceId,
        ) -> io::Result<()> {
            self.renders += 1;
            self.scene_revisions.push(scene_revision);
            if self.fail_render {
                self.fail_render = false;
                return Err(io::Error::other("scripted render failure"));
            }
            Ok(())
        }
    }

    fn make_app(events: Vec<FrontendEvent>, path: Option<&str>) -> App<ScriptedFrontend> {
        App::new(path, 40, 5, ScriptedFrontend::new(events)).unwrap()
    }

    fn editor_cid() -> ContentId {
        ContentId(0)
    }

    fn view_id(app: &App<ScriptedFrontend>, space: SpaceId) -> ViewId {
        view_for_space(&app.session.scene, space).expect("space hosts a view")
    }

    fn view_at(app: &App<ScriptedFrontend>, space: SpaceId) -> &View {
        &app.session.views[&view_id(app, space)]
    }

    fn text_presentation(view: &ViewData) -> &TextPresentation {
        match &view.presentation {
            ViewPresentation::Text(text) => text,
            ViewPresentation::StatusBar => panic!("expected text presentation"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sessions_sharing_one_kernel_keep_client_state_independent() {
        let mut app = make_app(vec![], None);
        let mut second = create_editor_session(&app.kernel.contents, &app.kernel.modes, 80, 20);
        let first_view = view_id(&app, app.session.focused);
        let second_view = view_for_space(&second.scene, second.focused).unwrap();

        second.scene.size.width = 100;
        second.scene.size.height = 30;
        app.handle_event(FrontendEvent::Key(KeyEvent::char('i')))
            .await
            .unwrap();
        app.handle_event(FrontendEvent::Key(KeyEvent::char('x')))
            .await
            .unwrap();

        assert_eq!(app.session.views[&first_view].content(), editor_cid());
        assert_eq!(second.views[&second_view].content(), editor_cid());
        assert_eq!(app.session.scene.size.width, 40);
        assert_eq!(second.scene.size.width, 100);
        assert_eq!(
            app.session.views[&first_view]
                .selections()
                .unwrap()
                .primary()
                .head()
                .char_index,
            1
        );
        assert_eq!(
            second.views[&second_view]
                .selections()
                .unwrap()
                .primary()
                .head(),
            TextOffset::origin()
        );
    }

    #[test]
    fn production_content_paths_have_no_dynamic_type_probes() {
        let app = include_str!("mod.rs");
        let content = include_str!("../core/content.rs");
        let content_view_state = include_str!("../core/content_view_state.rs");
        let dynamic_handler = concat!("Box<dyn ", "Content", "Handler>");
        let buffer_probe = concat!("buffer", "_mut(");
        let buffer_read_probe = concat!("as_", "buffer(");
        let forbidden = [
            ["Box<dyn ", "ContentViewState>"].concat(),
            ["Box<dyn ", "Content>"].concat(),
        ];

        assert!(!app.contains(dynamic_handler));
        assert!(!app.contains(buffer_probe));
        assert!(!content.contains(buffer_read_probe));
        for fragment in forbidden {
            assert!(!content_view_state.contains(&fragment), "{fragment}");
        }
    }

    fn text_rows(app: &App<ScriptedFrontend>, content: ContentId) -> Vec<String> {
        match app.kernel.contents.query(
            content,
            ContentQuery::TextRows(RowRange { start: 0, end: 5 }),
        ) {
            ContentData::TextRows(rows) => rows,
            data => panic!("expected text rows, got {data:?}"),
        }
    }

    fn text_point(
        app: &App<ScriptedFrontend>,
        content: ContentId,
        offset: TextOffset,
    ) -> crate::protocol::selection::TextPoint {
        match app
            .kernel
            .contents
            .query(content, ContentQuery::TextPoints(vec![offset]))
        {
            ContentData::TextPoints(mut points) => points.remove(0),
            _ => panic!("expected text point"),
        }
    }

    fn document_status(app: &App<ScriptedFrontend>, content: ContentId) -> DocumentStatus {
        match app
            .kernel
            .contents
            .query(content, ContentQuery::DocumentStatus)
        {
            ContentData::DocumentStatus(status) => status,
            data => panic!("expected document status, got {data:?}"),
        }
    }

    #[test]
    fn content_query_reads_buffer_and_view() {
        let mut app = make_app(vec![], None);
        let focused_view = view_id(&app, app.session.focused);
        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Edit(EditCommand::InsertText("hi".to_string())),
            view: focused_view,
            content: editor_cid(),
        })
        .unwrap();
        let query = AppQuery {
            contents: &app.kernel.contents,
            views: &app.session.views,
        };
        assert_eq!(
            query.content(
                editor_cid(),
                ContentQuery::TextRows(RowRange { start: 0, end: 5 })
            ),
            ContentData::TextRows(vec!["hi".to_string()])
        );
        let view = query.view(focused_view);
        let text = text_presentation(&view);
        assert_eq!(text.selections.primary().head().char_index, 2);
        assert_eq!(text.cursor_style, CursorStyle::Block);
    }

    #[test]
    fn status_bar_view_data_has_no_text_selection_or_mode_cursor() {
        let app = make_app(vec![], None);
        let status_view = app
            .session
            .views
            .iter()
            .find_map(|(id, view)| (view.content() == ContentId(1)).then_some(*id))
            .expect("status bar view exists");
        let query = AppQuery {
            contents: &app.kernel.contents,
            views: &app.session.views,
        };

        let view = query.view(status_view);
        assert_eq!(view.presentation, ViewPresentation::StatusBar);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_views_of_one_buffer_keep_independent_mode_instances() {
        let mut app = make_app(vec![], None);
        let left = app.session.focused;
        app.handle_event(FrontendEvent::Key(KeyEvent::char('i')))
            .await
            .unwrap();
        app.handle_event(FrontendEvent::Key(KeyEvent::char('a')))
            .await
            .unwrap();
        let right = app
            .split_space(left, editor_cid(), true, SplitDirection::Right, true)
            .unwrap()
            .new_space;
        assert_eq!(app.session.focused, right);

        let query = AppQuery {
            contents: &app.kernel.contents,
            views: &app.session.views,
        };
        let left_id = view_id(&app, left);
        let right_id = view_id(&app, right);
        let left_view = query.view(left_id);
        let right_view = query.view(right_id);
        let left_text = text_presentation(&left_view);
        let right_text = text_presentation(&right_view);

        assert_eq!(left_text.cursor_style, CursorStyle::Bar);
        assert_eq!(right_text.cursor_style, CursorStyle::Block);
        assert_ne!(left_id, right_id);
        assert_eq!(
            Some(&left_text.selections),
            app.session.views[&left_id].selections()
        );
        assert_eq!(
            Some(&right_text.selections),
            app.session.views[&right_id].selections()
        );
        assert_eq!(left_text.selections.primary().head().char_index, 1);
        assert_eq!(right_text.selections.primary().head(), TextOffset::origin());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unchanged_space_binding_preserves_its_view_selection() {
        let mut app = make_app(vec![], None);
        for key in ['i', 'a', 'b', 'c'] {
            app.handle_event(FrontendEvent::Key(KeyEvent::char(key)))
                .await
                .unwrap();
        }

        app.set_space_sizing(app.session.focused, Sizing::Fixed(12))
            .unwrap();

        assert_eq!(
            view_at(&app, app.session.focused)
                .selections()
                .unwrap()
                .primary()
                .head
                .char_index,
            3
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replace_content_rebuilds_view_from_origin() {
        let mut app = make_app(vec![], None);
        let other = ContentId(9);
        app.kernel
            .contents
            .insert(other, Content::Buffer(Buffer::new()));
        for key in ['i', 'a', 'b', 'c'] {
            app.handle_event(FrontendEvent::Key(KeyEvent::char(key)))
                .await
                .unwrap();
        }

        app.replace_space_content(app.session.focused, other, true)
            .unwrap();

        let view = view_at(&app, app.session.focused);
        assert_eq!(view.content(), other);
        assert_eq!(
            view.selections().unwrap().primary().head(),
            TextOffset::origin()
        );
        app.handle_event(FrontendEvent::Key(KeyEvent::char('a')))
            .await
            .unwrap();
        assert_eq!(text_rows(&app, other), vec![""]);
    }

    #[test]
    fn close_focused_space_prefers_surviving_neighbor_and_drops_its_view() {
        let mut app = make_app(vec![], None);
        let left = app.session.focused;
        let right = app
            .split_space(left, editor_cid(), true, SplitDirection::Right, true)
            .unwrap()
            .new_space;
        let right_view = view_id(&app, right);

        app.close_space(right).unwrap();

        assert_eq!(app.session.focused, left);
        assert!(!app.session.views.contains_key(&right_view));
    }

    #[test]
    fn missing_content_is_rejected_before_scene_mutation() {
        let mut app = make_app(vec![], None);
        let root = app.session.scene.root();
        let revision = app.session.scene_revision;

        assert!(matches!(
            app.split_space(root, ContentId(999), true, SplitDirection::Right, true),
            Err(LayoutError::MissingContent(ContentId(999)))
        ));
        assert_eq!(app.session.scene.root(), root);
        assert_eq!(app.session.scene_revision, revision);
    }

    #[test]
    fn successful_layout_mutation_advances_scene_revision() {
        let mut app = make_app(vec![], None);

        app.set_space_sizing(app.session.focused, Sizing::Fixed(12))
            .unwrap();

        assert_eq!(app.session.scene_revision, Revision(1));
    }

    #[test]
    fn render_passes_current_scene_revision_to_frontend() {
        let mut app = make_app(vec![], None);
        app.set_space_sizing(app.session.focused, Sizing::Fixed(12))
            .unwrap();

        app.render().unwrap();

        assert_eq!(app.frontend.scene_revisions, vec![Revision(1)]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn edit_commands_advance_view_and_content_revisions() {
        let mut app = make_app(vec![], None);
        let view = view_id(&app, app.session.focused);

        app.handle_event(FrontendEvent::Key(KeyEvent::char('i')))
            .await
            .unwrap();
        app.handle_event(FrontendEvent::Key(KeyEvent::char('x')))
            .await
            .unwrap();

        assert!(app.session.views[&view].revision() > Revision(0));
        assert!(app.kernel.contents.revision(editor_cid()).unwrap() > Revision(0));
        assert_eq!(app.session.scene_revision, Revision(0));
    }

    #[test]
    fn preferred_inert_status_space_is_not_selected() {
        let app = make_app(vec![], None);
        let status = app.session.scene.node(app.session.scene.root()).children[1];

        assert_eq!(
            resolve_focus(&app.session.scene, app.session.focused, Some(status)),
            Some(app.session.focused)
        );
    }

    #[test]
    fn closing_last_focusable_space_is_rejected() {
        let mut app = make_app(vec![], None);
        let status = app.session.scene.node(app.session.scene.root()).children[1];

        assert!(matches!(
            app.close_space(app.session.focused),
            Err(LayoutError::WouldRemoveLastFocusable(_))
        ));
        assert_ne!(app.session.focused, status);
    }

    #[test]
    fn replacing_only_focusable_content_with_inert_space_is_rejected() {
        let mut app = make_app(vec![], None);
        let focused = app.session.focused;
        let other = ContentId(9);
        app.kernel
            .contents
            .insert(other, Content::Buffer(Buffer::new()));

        assert_eq!(
            app.replace_space_content(focused, other, false),
            Err(LayoutError::NoFocusableSpace)
        );
        assert_eq!(app.session.focused, focused);
        assert!(matches!(
            &app.session.scene.node(focused).space.kind,
            SpaceKind::Content { view, .. }
                if app.session.views[view].content() == editor_cid()
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn default_vim_a_enters_insert_before_text_input() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["ia"]);
        assert!(app.kernel.tasks.is_cancelled());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn default_vim_a_appends_after_cursor_and_enters_insert() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('h')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('x')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );

        app.run().await.unwrap();

        assert_eq!(text_rows(&app, editor_cid()), vec!["abx"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn default_vim_ctrl_w_deletes_previous_word() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char(' ')),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::char('d')),
                FrontendEvent::Key(KeyEvent::ctrl('w')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );

        app.run().await.unwrap();

        assert_eq!(text_rows(&app, editor_cid()), vec!["ab "]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_w_moves_to_next_word() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('f')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::char(' ')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('r')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('w')),
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('X')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["foo Xbar"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_dollar_moves_to_line_end() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('$')),
                FrontendEvent::Key(KeyEvent::char('x')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["ab"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_x_deletes_char() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('x')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["bc"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_o_opens_line_below_and_inserts() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('f')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('r')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["foo", "bar"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_capital_a_appends_at_line_end() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('f')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('A')),
                FrontendEvent::Key(KeyEvent::char('!')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["foo!"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_capital_d_deletes_to_line_end() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('l')),
                FrontendEvent::Key(KeyEvent::char('D')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["a"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_capital_j_joins_lines() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('f')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('r')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('k')),
                FrontendEvent::Key(KeyEvent::char('J')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["foo bar"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_tilde_toggles_case() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('~')),
                FrontendEvent::Key(KeyEvent::char('~')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["AB"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_insert_ctrl_u_deletes_to_line_start() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::ctrl('u')),
                FrontendEvent::Key(KeyEvent::char('x')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["x"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_s_substitutes_char() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('s')),
                FrontendEvent::Key(KeyEvent::char('X')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["Xb"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_supports_backspace_and_arrows() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Backspace)),
                FrontendEvent::Key(KeyEvent::arrow(ArrowKey::Left)),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["a"]);
        let cursor = view_at(&app, app.session.focused)
            .selections()
            .unwrap()
            .primary()
            .head();
        assert_eq!(text_point(&app, editor_cid(), cursor).col, 0);
    }

    #[test]
    fn multi_space_edit_targets_only_focused_content() {
        let mut app = make_app(vec![], None);
        let other_cid = ContentId(9);
        app.kernel
            .contents
            .insert(other_cid, Content::Buffer(Buffer::new()));
        let other_sid = app
            .split_space(
                app.session.focused,
                other_cid,
                true,
                SplitDirection::Right,
                false,
            )
            .unwrap()
            .new_space;
        let other_view = view_id(&app, other_sid);

        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Edit(EditCommand::InsertText("Z".to_string())),
            view: other_view,
            content: other_cid,
        })
        .unwrap();

        assert_eq!(
            app.kernel.contents.query(
                editor_cid(),
                ContentQuery::TextRows(RowRange { start: 0, end: 1 }),
            ),
            ContentData::TextRows(vec!["".to_string()]),
        );
        assert_eq!(
            app.kernel.contents.query(
                other_cid,
                ContentQuery::TextRows(RowRange { start: 0, end: 1 }),
            ),
            ContentData::TextRows(vec!["Z".to_string()]),
        );
        assert_eq!(
            app.session
                .views
                .get(&other_view)
                .unwrap()
                .selections()
                .unwrap()
                .primary()
                .head()
                .char_index,
            1
        );
    }

    #[test]
    #[should_panic(expected = "view/content target mismatch")]
    fn view_content_rejects_mismatched_view_content_target() {
        let mut app = make_app(vec![], None);
        let other_cid = ContentId(9);
        app.kernel
            .contents
            .insert(other_cid, Content::Buffer(Buffer::new()));

        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Edit(EditCommand::InsertText("Z".to_string())),
            view: view_id(&app, app.session.focused),
            content: other_cid,
        })
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_forwards_resize_to_scene() {
        let mut app = make_app(
            vec![
                FrontendEvent::Resize(ResizeEvent {
                    width: 100,
                    height: 40,
                }),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(app.session.scene.size.width, 100);
        assert_eq!(app.session.scene.size.height, 40);
        assert_eq!(app.session.scene_revision, Revision(1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ctrl_s_saves_file_and_marks_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "hi").unwrap();
        let path_str = path.to_str().unwrap().to_owned();
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('X')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::ctrl('s')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            Some(&path_str),
        );
        app.run().await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Xhi");
        assert!(matches!(
            app.kernel
                .contents
                .query(editor_cid(), ContentQuery::DocumentStatus),
            ContentData::DocumentStatus(DocumentStatus {
                modified: false,
                message: StatusMessage::Saved,
                ..
            }),
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_gg_moves_to_the_first_line() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('g')),
                FrontendEvent::Key(KeyEvent::char('g')),
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('X')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );

        app.run().await.unwrap();

        assert_eq!(text_rows(&app, editor_cid()), vec!["Xa", "b"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_counted_gg_moves_to_the_requested_line() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('2')),
                FrontendEvent::Key(KeyEvent::char('g')),
                FrontendEvent::Key(KeyEvent::char('g')),
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('X')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );

        app.run().await.unwrap();

        assert_eq!(text_rows(&app, editor_cid()), vec!["a", "Xb"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_f_and_count_use_dynamic_awaiting_input() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('2')),
                FrontendEvent::Key(KeyEvent::char('f')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('X')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );

        app.run().await.unwrap();

        assert_eq!(text_rows(&app, editor_cid()), vec!["abacXa"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_capital_f_searches_backward_on_the_current_line() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('F')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('X')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );

        app.run().await.unwrap();

        assert_eq!(text_rows(&app, editor_cid()), vec!["abXa"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_counted_j_uses_private_count_state() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('g')),
                FrontendEvent::Key(KeyEvent::char('g')),
                FrontendEvent::Key(KeyEvent::char('2')),
                FrontendEvent::Key(KeyEvent::char('j')),
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('X')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );

        app.run().await.unwrap();

        assert_eq!(text_rows(&app, editor_cid()), vec!["a", "b", "Xc"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_counted_dd_deletes_whole_lines() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
                FrontendEvent::Key(KeyEvent::char('d')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('g')),
                FrontendEvent::Key(KeyEvent::char('g')),
                FrontendEvent::Key(KeyEvent::char('3')),
                FrontendEvent::Key(KeyEvent::char('d')),
                FrontendEvent::Key(KeyEvent::char('d')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );

        app.run().await.unwrap();

        assert_eq!(text_rows(&app, editor_cid()), vec!["d"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prefix_key_sequence_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.txt");
        std::fs::write(&path, "x").unwrap();
        let path_str = path.to_str().unwrap().to_owned();
        // 绑 'z' 前缀 + 's' → Save（覆盖 Ctrl+S 测试前缀路径）
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('z')),
                FrontendEvent::Key(KeyEvent::char('s')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            Some(&path_str),
        );
        let mut global = default_global_keymap();
        global.bind(
            [KeyEvent::char('z'), KeyEvent::char('s')],
            Command::Content(ContentCommand::Save),
        );
        app.session.dispatcher = Dispatcher::new(global);
        app.run().await.unwrap();
        assert_eq!(
            document_status(&app, editor_cid()).message,
            StatusMessage::Saved
        );
    }

    #[test]
    fn save_completed_ok_marks_buffer_saved() {
        let mut app = make_app(vec![], None);
        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Edit(EditCommand::InsertText("x".to_string())),
            view: view_id(&app, app.session.focused),
            content: editor_cid(),
        })
        .unwrap();
        assert!(document_status(&app, editor_cid()).modified);
        app.kernel.pending_saves.insert(
            editor_cid(),
            PendingSave {
                revision: 1,
                state: crate::core::transaction::TextStateId(1),
                queued: None,
            },
        );

        app.handle_app_message(AppMessage::SaveCompleted {
            content: editor_cid(),
            revision: 1,
            state: crate::core::transaction::TextStateId(1),
            result: Ok(()),
        })
        .unwrap();

        assert!(!app.kernel.pending_saves.contains_key(&editor_cid()));
        let status = document_status(&app, editor_cid());
        assert!(!status.modified);
        assert_eq!(status.message, StatusMessage::Saved);
    }

    #[test]
    fn save_completed_err_marks_buffer_save_failed() {
        let mut app = make_app(vec![], None);
        app.kernel.pending_saves.insert(
            editor_cid(),
            PendingSave {
                revision: 0,
                state: crate::core::transaction::TextStateId(0),
                queued: None,
            },
        );

        app.handle_app_message(AppMessage::SaveCompleted {
            content: editor_cid(),
            revision: 0,
            state: crate::core::transaction::TextStateId(0),
            result: Err(io::Error::other("boom")),
        })
        .unwrap();

        assert!(!app.kernel.pending_saves.contains_key(&editor_cid()));
        assert_eq!(
            document_status(&app, editor_cid()).message,
            StatusMessage::SaveFailed
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stale_save_completion_keeps_newer_edits_modified() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stale-save.txt");
        std::fs::write(&path, "hello").unwrap();
        let path_str = path.to_str().unwrap().to_owned();
        let mut app = make_app(vec![], Some(&path_str));

        app.execute_command(DispatchCommand::Content {
            command: ContentCommand::Save,
            content: editor_cid(),
        })
        .unwrap();
        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Edit(EditCommand::InsertText("X".to_string())),
            view: view_id(&app, app.session.focused),
            content: editor_cid(),
        })
        .unwrap();

        app.shutdown_tasks().await.unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        assert!(document_status(&app, editor_cid()).modified);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn save_during_pending_write_queues_latest_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queued-save.txt");
        std::fs::write(&path, "hello").unwrap();
        let path_str = path.to_str().unwrap().to_owned();
        let mut app = make_app(vec![], Some(&path_str));

        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Edit(EditCommand::InsertText("A".to_string())),
            view: view_id(&app, app.session.focused),
            content: editor_cid(),
        })
        .unwrap();
        app.execute_command(DispatchCommand::Content {
            command: ContentCommand::Save,
            content: editor_cid(),
        })
        .unwrap();
        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Edit(EditCommand::InsertText("B".to_string())),
            view: view_id(&app, app.session.focused),
            content: editor_cid(),
        })
        .unwrap();
        app.execute_command(DispatchCommand::Content {
            command: ContentCommand::Save,
            content: editor_cid(),
        })
        .unwrap();
        assert!(app.kernel.pending_saves.contains_key(&editor_cid()));

        app.shutdown_tasks().await.unwrap();

        assert!(!app.kernel.pending_saves.contains_key(&editor_cid()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ABhello");
        let status = document_status(&app, editor_cid());
        assert!(!status.modified);
        assert_eq!(status.message, StatusMessage::Saved);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execute_save_uses_resolved_content_target() {
        let dir = tempfile::tempdir().unwrap();
        let focused_path = dir.path().join("focused.txt");
        let other_path = dir.path().join("other.txt");
        std::fs::write(&focused_path, "focused").unwrap();
        std::fs::write(&other_path, "other").unwrap();
        let focused_path_str = focused_path.to_str().unwrap().to_owned();
        let other_path_str = other_path.to_str().unwrap().to_owned();

        let mut app = make_app(vec![], Some(&focused_path_str));
        let other_cid = ContentId(9);
        let mut other = Buffer::new();
        other.open_path(&other_path_str).unwrap();
        other.insert_char(0, 'X');
        app.kernel
            .contents
            .insert(other_cid, Content::Buffer(other));

        app.execute_command(DispatchCommand::Content {
            command: ContentCommand::Save,
            content: other_cid,
        })
        .unwrap();
        app.shutdown_tasks().await.unwrap();

        assert_eq!(std::fs::read_to_string(&focused_path).unwrap(), "focused");
        assert_eq!(std::fs::read_to_string(&other_path).unwrap(), "Xother");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_waits_for_pending_save_before_returning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wait-save.txt");
        std::fs::write(&path, "hi").unwrap();
        let path_str = path.to_str().unwrap().to_owned();
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('X')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::ctrl('s')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            Some(&path_str),
        );

        let result = tokio::time::timeout(std::time::Duration::from_secs(2), app.run()).await;
        assert!(result.is_ok());
        result.unwrap().unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Xhi");
        assert!(!app.kernel.pending_saves.contains_key(&editor_cid()));
        assert_eq!(
            document_status(&app, editor_cid()).message,
            StatusMessage::Saved
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shift_arrow_builds_selection_then_input_replaces() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::shift_arrow(ArrowKey::Left)), // 选区 [2,3)
                FrontendEvent::Key(KeyEvent::char('X')),                   // 替换 [2,3) 为 X
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["abX"]);
        let head = view_at(&app, app.session.focused)
            .selections()
            .unwrap()
            .primary()
            .head();
        assert_eq!(head.char_index, 3);
        assert_eq!(
            view_at(&app, app.session.focused)
                .selections()
                .unwrap()
                .primary()
                .anchor,
            head
        ); // collapse
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn escape_enters_normal_then_h_moves_left_of_selection() {
        // vim 语义：Insert 中 shift-Left 建选区 [2,3)；Escape 回 Normal（不 collapse）；
        // 随后 Normal 的 'h' 在非空选区上 shrink 到 min 并 collapse（head=2），再 'h' 左移到 1。
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::shift_arrow(ArrowKey::Left)), // 选区 [2,3)
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),      // 回 Normal（选区保留）
                FrontendEvent::Key(KeyEvent::char('h')),                   // shrink→head=2 collapse
                FrontendEvent::Key(KeyEvent::char('h')), // collapsed 左移 → head=1
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["abc"]); // Escape/h 不改文本
        let head = view_at(&app, app.session.focused)
            .selections()
            .unwrap()
            .primary()
            .head();
        assert_eq!(text_point(&app, editor_cid(), head).col, 1);
        assert_eq!(
            view_at(&app, app.session.focused)
                .selections()
                .unwrap()
                .primary()
                .anchor,
            head
        ); // collapsed
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_visual_counted_motion_then_delete_removes_selected_range() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::char('d')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('v')),
                FrontendEvent::Key(KeyEvent::char('2')),
                FrontendEvent::Key(KeyEvent::char('l')),
                FrontendEvent::Key(KeyEvent::char('d')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );

        app.run().await.unwrap();

        assert_eq!(text_rows(&app, editor_cid()), vec!["cd"]);
        let selection = view_at(&app, app.session.focused)
            .selections()
            .unwrap()
            .primary();
        assert_eq!(selection.head.char_index, 0);
        assert_eq!(selection.anchor, selection.head);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_visual_delete_without_motion_removes_current_char() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('v')),
                FrontendEvent::Key(KeyEvent::char('d')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );

        app.run().await.unwrap();

        assert_eq!(text_rows(&app, editor_cid()), vec!["b"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_visual_change_and_insert_is_one_undo_unit() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::char('d')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('v')),
                FrontendEvent::Key(KeyEvent::char('l')),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::char('X')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('u')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );

        app.run().await.unwrap();

        assert_eq!(text_rows(&app, editor_cid()), vec!["abcd"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_h_moves_left_after_insert() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('h')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );

        app.run().await.unwrap();

        assert_eq!(text_rows(&app, editor_cid()), vec!["ab"]);
        let head = view_at(&app, app.session.focused)
            .selections()
            .unwrap()
            .primary()
            .head();
        assert_eq!(head.char_index, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_renders_after_state_changes() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert!(app.frontend.renders >= 1);
        assert_eq!(text_rows(&app, editor_cid()), vec!["a"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_insert_session_is_one_undo_unit_and_ctrl_r_redoes_it() {
        let mut undo = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('u')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        undo.run().await.unwrap();
        assert_eq!(text_rows(&undo, editor_cid()), vec![""]);

        let mut redo = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('u')),
                FrontendEvent::Key(KeyEvent::ctrl('r')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );

        redo.run().await.unwrap();

        assert_eq!(text_rows(&redo, editor_cid()), vec!["ab"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_insert_mode_u_is_text_not_undo() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('u')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );

        app.run().await.unwrap();

        assert_eq!(text_rows(&app, editor_cid()), vec!["u"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_delete_operator_accepts_word_line_end_and_line_start_motions() {
        let mut word = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::char('n')),
                FrontendEvent::Key(KeyEvent::char('e')),
                FrontendEvent::Key(KeyEvent::char(' ')),
                FrontendEvent::Key(KeyEvent::char('t')),
                FrontendEvent::Key(KeyEvent::char('w')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('d')),
                FrontendEvent::Key(KeyEvent::char('w')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        word.run().await.unwrap();
        assert_eq!(text_rows(&word, editor_cid()), vec!["two"]);

        let mut line_end = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char('d')),
                FrontendEvent::Key(KeyEvent::char('$')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        line_end.run().await.unwrap();
        assert_eq!(text_rows(&line_end, editor_cid()), vec![""]);

        let mut line_start = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('d')),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        line_start.run().await.unwrap();
        assert_eq!(text_rows(&line_start, editor_cid()), vec![""]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vim_normal_horizontal_motion_never_lands_on_or_deletes_newline() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('a')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
                FrontendEvent::Key(KeyEvent::char('b')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('k')),
                FrontendEvent::Key(KeyEvent::char('l')),
                FrontendEvent::Key(KeyEvent::char('x')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );

        app.run().await.unwrap();

        assert_eq!(text_rows(&app, editor_cid()), vec!["", "b"]);
    }

    #[test]
    fn editing_shared_content_reconciles_other_view_selections() {
        let mut app = make_app(vec![], None);
        let left = app.session.focused;
        let left_view = view_id(&app, left);
        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Edit(EditCommand::InsertText("abc".to_string())),
            view: left_view,
            content: editor_cid(),
        })
        .unwrap();
        let right = app
            .split_space(left, editor_cid(), true, SplitDirection::Right, false)
            .unwrap()
            .new_space;
        let right_view = view_id(&app, right);
        let right_revision = app.session.views[&right_view].revision();
        match app.session.views.get_mut(&left_view).unwrap().state_mut() {
            crate::core::content_view_state::ContentViewState::Buffer(state) => {
                *state.selections_mut() = Selections::single(Selection {
                    anchor: TextOffset::origin(),
                    head: TextOffset { char_index: 3 },
                });
            }
            _ => unreachable!(),
        }
        match app.session.views.get_mut(&right_view).unwrap().state_mut() {
            crate::core::content_view_state::ContentViewState::Buffer(state) => {
                *state.selections_mut() =
                    Selections::single(Selection::collapsed(TextOffset { char_index: 3 }));
            }
            _ => unreachable!(),
        }

        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Edit(EditCommand::Delete(-1)),
            view: left_view,
            content: editor_cid(),
        })
        .unwrap();

        assert_eq!(text_rows(&app, editor_cid()), vec![""]);
        assert_eq!(
            app.session.views[&right_view]
                .selections()
                .unwrap()
                .primary()
                .head(),
            TextOffset::origin()
        );
        assert!(app.session.views[&right_view].revision() > right_revision);
    }

    #[test]
    fn shared_view_positions_follow_text_change_affinity() {
        let mut app = make_app(vec![], None);
        let left = app.session.focused;
        let left_view = view_id(&app, left);
        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Edit(EditCommand::InsertText("abc".to_string())),
            view: left_view,
            content: editor_cid(),
        })
        .unwrap();
        let right = app
            .split_space(left, editor_cid(), true, SplitDirection::Right, false)
            .unwrap()
            .new_space;
        let right_view = view_id(&app, right);
        for view in [left_view, right_view] {
            let ContentViewState::Buffer(state) =
                app.session.views.get_mut(&view).unwrap().state_mut()
            else {
                unreachable!()
            };
            *state.selections_mut() =
                Selections::single(Selection::collapsed(TextOffset { char_index: 1 }));
        }

        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Edit(EditCommand::InsertText("X".to_string())),
            view: left_view,
            content: editor_cid(),
        })
        .unwrap();

        assert_eq!(text_rows(&app, editor_cid()), vec!["aXbc"]);
        assert_eq!(
            app.session.views[&right_view]
                .selections()
                .unwrap()
                .primary()
                .head()
                .char_index,
            2
        );
    }

    #[test]
    fn shared_view_positions_follow_undo_and_redo_changes() {
        let mut app = make_app(vec![], None);
        let left = app.session.focused;
        let left_view = view_id(&app, left);
        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Edit(EditCommand::InsertText("abc".to_string())),
            view: left_view,
            content: editor_cid(),
        })
        .unwrap();
        let right = app
            .split_space(left, editor_cid(), true, SplitDirection::Right, false)
            .unwrap()
            .new_space;
        let right_view = view_id(&app, right);
        let ContentViewState::Buffer(state) =
            app.session.views.get_mut(&right_view).unwrap().state_mut()
        else {
            unreachable!()
        };
        *state.selections_mut() =
            Selections::single(Selection::collapsed(TextOffset { char_index: 3 }));

        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Undo,
            view: left_view,
            content: editor_cid(),
        })
        .unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec![""]);
        for view in [left_view, right_view] {
            assert_eq!(
                app.session.views[&view]
                    .selections()
                    .unwrap()
                    .primary()
                    .head(),
                TextOffset::origin()
            );
        }

        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Redo,
            view: left_view,
            content: editor_cid(),
        })
        .unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["abc"]);
        for view in [left_view, right_view] {
            assert_eq!(
                app.session.views[&view]
                    .selections()
                    .unwrap()
                    .primary()
                    .head()
                    .char_index,
                3
            );
        }
    }

    #[test]
    fn failed_layout_mutations_do_not_consume_view_ids() {
        let mut app = make_app(vec![], None);
        let next = app.session.next_view_id;

        assert!(
            app.split_space(
                SpaceId(999),
                editor_cid(),
                true,
                SplitDirection::Right,
                false,
            )
            .is_err()
        );
        assert_eq!(app.session.next_view_id, next);
        assert!(
            app.replace_space_content(SpaceId(999), editor_cid(), true)
                .is_err()
        );
        assert_eq!(app.session.next_view_id, next);
    }

    #[test]
    fn no_op_edit_does_not_advance_content_or_view_revision() {
        let mut app = make_app(vec![], None);
        let view = view_id(&app, app.session.focused);
        let view_revision = app.session.views[&view].revision();
        let content_revision = app.kernel.contents.revision(editor_cid()).unwrap();

        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Edit(EditCommand::MoveLeftBy(1)),
            view,
            content: editor_cid(),
        })
        .unwrap();

        assert_eq!(app.session.views[&view].revision(), view_revision);
        assert_eq!(
            app.kernel.contents.revision(editor_cid()),
            Some(content_revision)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn frontend_error_still_waits_for_pending_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("save-on-error.txt");
        std::fs::write(&path, "old").unwrap();
        let mut app = make_app(vec![], path.to_str());
        let view = view_id(&app, app.session.focused);
        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Edit(EditCommand::InsertText("new".to_string())),
            view,
            content: editor_cid(),
        })
        .unwrap();
        app.execute_command(DispatchCommand::Content {
            command: ContentCommand::Save,
            content: editor_cid(),
        })
        .unwrap();
        app.frontend.fail_next_event = true;

        assert!(app.run().await.is_err());

        assert_eq!(std::fs::read_to_string(path).unwrap(), "newold");
        assert!(app.kernel.pending_saves.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn render_error_still_waits_for_pending_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("save-on-render-error.txt");
        std::fs::write(&path, "old").unwrap();
        let mut app = make_app(vec![], path.to_str());
        let view = view_id(&app, app.session.focused);
        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Edit(EditCommand::InsertText("new".to_string())),
            view,
            content: editor_cid(),
        })
        .unwrap();
        app.execute_command(DispatchCommand::Content {
            command: ContentCommand::Save,
            content: editor_cid(),
        })
        .unwrap();
        app.frontend.fail_render = true;

        assert!(app.run().await.is_err());

        assert_eq!(std::fs::read_to_string(path).unwrap(), "newold");
        assert!(app.kernel.pending_saves.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancellation_discards_frontend_events_after_quit() {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
                FrontendEvent::Key(KeyEvent::char('x')),
            ],
            None,
        );

        app.run().await.unwrap();

        assert_eq!(text_rows(&app, editor_cid()), vec![""]);
    }
}
