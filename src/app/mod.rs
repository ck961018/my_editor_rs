//! App：tokio::select! 多路复用 evloop。不感知 tui/gui（只依赖 Frontend trait + Scene）。
//! 事件分发委托 Dispatcher（捕获链 + 前缀状态机），Command 执行委托 executor。
//! 不持 editor_content/status_content 角色 ID——从 scene/focused 推导。

mod content;
mod dispatcher;
mod executor;
mod message;
mod tasks;
mod view;

use std::collections::{HashMap, HashSet};
use std::io;

use tokio::sync::mpsc;

use crate::app::dispatcher::{DispatchCommand, Dispatcher, default_global_keymap};
use crate::app::message::AppMessage;
use crate::app::tasks::AppTasks;
use crate::app::view::View;
use crate::core::buffer::Buffer;
use crate::core::command::{AppCommand, ContentCommand};
use crate::core::content::{ContentHandler, ContentLookup};
use crate::core::status_bar::StatusBar;
use crate::frontend::Frontend;
use crate::protocol::content_query::{ContentQuery, RowRange, StatusBarData};
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::{ContentId, SpaceId};
use crate::protocol::scene::{Scene, SceneBuilder, build_editor_scene};
use crate::protocol::selection::{CursorPos, Selection, Selections};
use crate::protocol::space::SpaceKind;
use crate::protocol::status::StatusMessage;

pub struct App<F: Frontend> {
    contents: HashMap<ContentId, Box<dyn ContentHandler>>,
    scene: Scene,
    // 预留：动态 space 分配（split/panel/overlay/minibuffer）经此 builder，避免重置 SpaceId。
    #[allow(dead_code)]
    scene_builder: SceneBuilder,
    views: HashMap<SpaceId, View>,
    focused: SpaceId,
    dispatcher: Dispatcher,
    frontend: F,
    message_tx: mpsc::UnboundedSender<AppMessage>,
    message_rx: mpsc::UnboundedReceiver<AppMessage>,
    tasks: AppTasks,
    pending_saves: HashSet<ContentId>,
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
        let mut contents: HashMap<ContentId, Box<dyn ContentHandler>> = HashMap::new();
        contents.insert(editor_content, Box::new(buffer));
        contents.insert(status_content, Box::new(status_bar));
        let mut scene_builder = SceneBuilder::new();
        let (scene, editor_space) = build_editor_scene(
            &mut scene_builder,
            width as i32,
            height as i32,
            editor_content,
            status_content,
        )
        .expect("valid editor scene");
        let views = build_views(&scene);
        let dispatcher = Dispatcher::new(default_global_keymap());
        let (message_tx, message_rx) = mpsc::unbounded_channel::<AppMessage>();
        Ok(Self {
            contents,
            views,
            scene,
            scene_builder,
            focused: editor_space,
            dispatcher,
            frontend,
            message_tx,
            message_rx,
            tasks: AppTasks::new(),
            pending_saves: HashSet::new(),
        })
    }

    pub async fn run(&mut self) -> io::Result<()> {
        self.render()?;
        loop {
            tokio::select! {
                ev = self.frontend.next_event() => {
                    // frontend I/O 错误直接上抛（spec §10）：不进 shutdown_tasks，
                    // 在途 critical 保存可能丢失。前端错误通常致命，此为既定取舍。
                    match ev? {
                        Some(event) => self.handle_event(event).await?,
                        None => self.tasks.cancel(),
                    }
                }
                message = self.message_rx.recv() => {
                    if let Some(message) = message {
                        self.handle_app_message(message)?;
                    } else {
                        self.tasks.cancel();
                    }
                }
                _ = self.tasks.cancelled() => {
                    self.shutdown_tasks().await?;
                    break;
                }
            }
            if !self.tasks.is_cancelled() {
                self.render()?;
            }
        }
        Ok(())
    }

    async fn shutdown_tasks(&mut self) -> io::Result<()> {
        self.tasks.cancel();
        self.tasks.close_detached();
        self.tasks.close_critical();
        self.tasks.wait_critical().await;
        while let Ok(message) = self.message_rx.try_recv() {
            self.handle_app_message(message)?;
        }
        Ok(())
    }

    async fn handle_event(&mut self, event: FrontendEvent) -> io::Result<()> {
        match event {
            FrontendEvent::Resize(r) => {
                self.scene.resize(r.width as i32, r.height as i32);
            }
            FrontendEvent::Key(k) => {
                if let Some(command) = self
                    .dispatcher
                    .dispatch(k, self.focused, &self.scene, &self.contents)
                {
                    self.execute_command(command)?;
                }
            }
            FrontendEvent::QuitRequest => self.tasks.cancel(),
        }
        Ok(())
    }

    fn execute_command(&mut self, command: DispatchCommand) -> io::Result<()> {
        match command {
            DispatchCommand::App(command) => match command {
                AppCommand::Quit => self.tasks.cancel(),
                AppCommand::FocusNext | AppCommand::FocusPrev => {}
            },
            DispatchCommand::Content { command, content } => match command {
                ContentCommand::Save => {
                    self.spawn_save(content);
                }
                ContentCommand::Mode { mode, action } => {
                    if let Some(content) = self.contents.get_mut(&content) {
                        content.handle_mode_command(mode, action);
                    }
                }
                ContentCommand::Text(_) => {}
            },
            DispatchCommand::ViewContent {
                command,
                space,
                content,
            } => {
                if let ContentCommand::Text(command) = command {
                    let content = self
                        .contents
                        .get_mut(&content)
                        .and_then(|c| c.buffer_mut())
                        .expect("text command target is a buffer");
                    let view = self.views.get_mut(&space).expect("target view exists");
                    executor::execute_text_command(command, content, view.selections_mut());
                }
            }
            DispatchCommand::Noop => {}
        }
        Ok(())
    }

    fn handle_app_message(&mut self, message: AppMessage) -> io::Result<()> {
        match message {
            AppMessage::SaveCompleted { content, result } => {
                self.pending_saves.remove(&content);
                let buf = self
                    .contents
                    .get_mut(&content)
                    .and_then(|c| c.buffer_mut())
                    .expect("saved buffer exists");
                match result {
                    Ok(()) => {
                        buf.mark_saved();
                        buf.set_status(StatusMessage::Saved);
                    }
                    Err(_) => buf.set_status(StatusMessage::SaveFailed),
                }
            }
        }
        Ok(())
    }

    /// 发起异步保存。返回是否真正发起（同一 content 已在保存时忽略）。
    fn spawn_save(&mut self, id: ContentId) -> bool {
        if self.pending_saves.contains(&id) {
            return false;
        }
        let (path, bytes) = {
            let buf = match self.contents.get_mut(&id).and_then(|c| c.buffer_mut()) {
                Some(b) => b,
                None => return false,
            };
            let path = match buf.path().map(|p| p.to_path_buf()) {
                Some(p) => p,
                None => {
                    buf.set_status(StatusMessage::SaveFailed);
                    return false;
                }
            };
            (path, buf.slice().to_string())
        };
        let tx = self.message_tx.clone();
        self.pending_saves.insert(id);
        self.tasks.spawn_critical(async move {
            let result = tokio::fs::write(path, bytes).await;
            let _ = tx.send(AppMessage::SaveCompleted {
                content: id,
                result,
            });
        });
        true
    }

    fn render(&mut self) -> io::Result<()> {
        let query = AppQuery {
            contents: &self.contents,
            views: &self.views,
        };
        self.frontend
            .render(&self.scene, &query as &dyn ContentQuery, self.focused)
    }
}

/// 遍历 scene 所有 Content space，为每个建 View（绑定其 content）。
fn build_views(scene: &Scene) -> HashMap<SpaceId, View> {
    let mut views = HashMap::new();
    collect_content_spaces(scene, scene.root, &mut views);
    views
}

fn collect_content_spaces(scene: &Scene, sid: SpaceId, out: &mut HashMap<SpaceId, View>) {
    let node = scene.node(sid);
    match &node.space.kind {
        SpaceKind::Content { content } => {
            out.insert(sid, View::new(*content));
        }
        SpaceKind::Container { children, .. } => {
            for c in children {
                collect_content_spaces(scene, *c, out);
            }
        }
    }
}

/// 借 App 数据字段的查询适配器：render 时用它做 `&dyn ContentQuery`，
/// 与 `&mut self.frontend` 不冲突（字段级 split borrow）。
struct AppQuery<'a> {
    contents: &'a HashMap<ContentId, Box<dyn ContentHandler>>,
    views: &'a HashMap<SpaceId, View>,
}

impl<'a> ContentQuery for AppQuery<'a> {
    fn lines(&self, cid: ContentId, range: RowRange) -> Vec<String> {
        let Some(buf) = self.contents.get(&cid).and_then(|c| c.as_buffer()) else {
            return Vec::new();
        };
        let total = buf.len_lines();
        let start = range.start.min(total);
        let end = range.end.min(total).max(start);
        (start..end)
            .map(|i| buf.line(i).trim_end_matches('\n').to_string())
            .collect()
    }
    fn status_bar(&self, cid: ContentId) -> StatusBarData {
        let Some(c) = self.contents.get(&cid) else {
            return StatusBarData {
                file_name: None,
                modified: false,
                message: StatusMessage::None,
            };
        };
        match c.as_status_bar() {
            Some(sb) => sb.status_bar_data(self.contents as &dyn ContentLookup),
            None => StatusBarData {
                file_name: None,
                modified: false,
                message: StatusMessage::None,
            },
        }
    }
    fn selections(&self, sid: SpaceId) -> Selections {
        self.views
            .get(&sid)
            .map(|v| v.selections().clone())
            .unwrap_or_else(|| Selections::single(Selection::collapsed(CursorPos::origin())))
    }
    fn line_count(&self, cid: ContentId) -> usize {
        self.contents
            .get(&cid)
            .and_then(|c| c.as_buffer())
            .map(|b| b.len_lines())
            .unwrap_or(0)
    }
}

impl<F: Frontend> ContentQuery for App<F> {
    fn lines(&self, cid: ContentId, range: RowRange) -> Vec<String> {
        AppQuery {
            contents: &self.contents,
            views: &self.views,
        }
        .lines(cid, range)
    }
    fn status_bar(&self, cid: ContentId) -> StatusBarData {
        AppQuery {
            contents: &self.contents,
            views: &self.views,
        }
        .status_bar(cid)
    }
    fn selections(&self, sid: SpaceId) -> Selections {
        AppQuery {
            contents: &self.contents,
            views: &self.views,
        }
        .selections(sid)
    }
    fn line_count(&self, cid: ContentId) -> usize {
        AppQuery {
            contents: &self.contents,
            views: &self.views,
        }
        .line_count(cid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::command::{Command, TextCommand};
    use crate::frontend::Frontend;
    use crate::protocol::content_query::{ContentQuery, RowRange};
    use crate::protocol::frontend_event::ResizeEvent;
    use crate::protocol::key_event::{ArrowKey, KeyCode, KeyEvent};
    use std::collections::VecDeque;

    struct ScriptedFrontend {
        events: VecDeque<FrontendEvent>,
        renders: usize,
    }

    impl ScriptedFrontend {
        fn new(events: Vec<FrontendEvent>) -> Self {
            Self {
                events: events.into(),
                renders: 0,
            }
        }
    }

    impl Frontend for ScriptedFrontend {
        async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
            Ok(self.events.pop_front())
        }

        fn render(
            &mut self,
            _scene: &Scene,
            _query: &dyn ContentQuery,
            _focused: SpaceId,
        ) -> io::Result<()> {
            self.renders += 1;
            Ok(())
        }
    }

    fn make_app(events: Vec<FrontendEvent>, path: Option<&str>) -> App<ScriptedFrontend> {
        App::new(path, 40, 5, ScriptedFrontend::new(events)).unwrap()
    }

    fn editor_cid() -> ContentId {
        ContentId(0)
    }

    #[test]
    fn content_query_lines_and_selections() {
        let mut app = make_app(vec![], None);
        let buf = app
            .contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .unwrap();
        buf.insert_char(0, 'h');
        buf.insert_char(1, 'i');
        let lines = ContentQuery::lines(&app, editor_cid(), RowRange { start: 0, end: 5 });
        assert_eq!(lines, vec!["hi".to_string()]);
        assert_eq!(ContentQuery::line_count(&app, editor_cid()), 1);
        let sels = ContentQuery::selections(&app, app.focused);
        assert_eq!(sels.primary().head(), CursorPos::origin());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn default_vim_requires_insert_before_text_input() {
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
        let buf = app
            .contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .unwrap();
        assert_eq!(buf.slice().to_string(), "a");
        assert!(app.tasks.is_cancelled());
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
        let buf = app
            .contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .unwrap();
        assert_eq!(buf.slice().to_string(), "a");
        let cursor = app
            .views
            .get(&app.focused)
            .expect("view exists")
            .selections()
            .primary()
            .head();
        assert_eq!(cursor.col, 0);
    }

    #[test]
    fn execute_edit_uses_resolved_view_content_target() {
        let mut app = make_app(vec![], None);
        let other_cid = ContentId(9);
        let other_sid = app.scene_builder.content_grow(other_cid, 1);
        let scene = app
            .scene_builder
            .snapshot(
                app.scene.root,
                crate::protocol::geometry::Size {
                    width: app.scene.size.width,
                    height: app.scene.size.height,
                },
            )
            .unwrap();
        app.scene = scene;
        app.contents.insert(other_cid, Box::new(Buffer::new()));
        app.views.insert(other_sid, View::new(other_cid));

        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Text(TextCommand::InsertText("Z".to_string())),
            space: other_sid,
            content: other_cid,
        })
        .unwrap();

        let focused_buf = app
            .contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .unwrap();
        assert_eq!(focused_buf.slice().to_string(), "");

        let other_buf = app
            .contents
            .get_mut(&other_cid)
            .and_then(|c| c.buffer_mut())
            .unwrap();
        assert_eq!(other_buf.slice().to_string(), "Z");
        assert_eq!(
            app.views
                .get(&other_sid)
                .unwrap()
                .selections()
                .primary()
                .head()
                .char_index,
            1
        );
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
        assert_eq!(app.scene.size.width, 100);
        assert_eq!(app.scene.size.height, 40);
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
        let buf = app
            .contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .unwrap();
        assert!(!buf.modified());
        assert_eq!(buf.status(), StatusMessage::Saved);
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
        // 给 Buffer 绑前缀
        {
            let buf = app
                .contents
                .get_mut(&editor_cid())
                .and_then(|c| c.buffer_mut())
                .unwrap();
            let mut sub = crate::core::keymap::Keymap::new();
            sub.bind(
                KeyEvent::char('s'),
                Command::Content(ContentCommand::Save),
            );
            buf.keymap_mut().bind_prefix(KeyEvent::char('z'), sub);
        }
        app.run().await.unwrap();
        // 未修改 buffer，Save 仍 mark_saved（无变化）+ Saved 状态
        let buf = app
            .contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .unwrap();
        assert_eq!(buf.status(), StatusMessage::Saved);
    }

    #[test]
    fn save_completed_ok_marks_buffer_saved() {
        let mut app = make_app(vec![], None);
        {
            let buf = app
                .contents
                .get_mut(&editor_cid())
                .and_then(|c| c.buffer_mut())
                .unwrap();
            buf.insert_char(0, 'x');
            assert!(buf.modified());
        }
        app.pending_saves.insert(editor_cid());

        app.handle_app_message(AppMessage::SaveCompleted {
            content: editor_cid(),
            result: Ok(()),
        })
        .unwrap();

        let buf = app
            .contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .unwrap();
        assert!(!app.pending_saves.contains(&editor_cid()));
        assert!(!buf.modified());
        assert_eq!(buf.status(), StatusMessage::Saved);
    }

    #[test]
    fn save_completed_err_marks_buffer_save_failed() {
        let mut app = make_app(vec![], None);
        app.pending_saves.insert(editor_cid());

        app.handle_app_message(AppMessage::SaveCompleted {
            content: editor_cid(),
            result: Err(io::Error::other("boom")),
        })
        .unwrap();

        let buf = app
            .contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .unwrap();
        assert!(!app.pending_saves.contains(&editor_cid()));
        assert_eq!(buf.status(), StatusMessage::SaveFailed);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_save_ignores_duplicate_pending_save_for_same_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dedupe.txt");
        std::fs::write(&path, "hello").unwrap();
        let path_str = path.to_str().unwrap().to_owned();
        let mut app = make_app(vec![], Some(&path_str));

        assert!(app.spawn_save(editor_cid()));
        assert!(!app.spawn_save(editor_cid()));
        assert!(app.pending_saves.contains(&editor_cid()));

        app.shutdown_tasks().await.unwrap();

        assert!(!app.pending_saves.contains(&editor_cid()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        let buf = app
            .contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .unwrap();
        assert_eq!(buf.status(), StatusMessage::Saved);
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
        app.contents.insert(other_cid, Box::new(other));

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
        assert!(!app.pending_saves.contains(&editor_cid()));
        let buf = app
            .contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .unwrap();
        assert_eq!(buf.status(), StatusMessage::Saved);
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
        let buf = app
            .contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .unwrap();
        assert_eq!(buf.slice().to_string(), "abX");
        let head = app
            .views
            .get(&app.focused)
            .unwrap()
            .selections()
            .primary()
            .head();
        assert_eq!(head.char_index, 3);
        assert_eq!(
            app.views
                .get(&app.focused)
                .unwrap()
                .selections()
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
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)), // 回 Normal（选区保留）
                FrontendEvent::Key(KeyEvent::char('h')),              // shrink→head=2 collapse
                FrontendEvent::Key(KeyEvent::char('h')),              // collapsed 左移 → head=1
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        let buf = app
            .contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .unwrap();
        assert_eq!(buf.slice().to_string(), "abc"); // Escape/h 不改文本
        let head = app
            .views
            .get(&app.focused)
            .unwrap()
            .selections()
            .primary()
            .head();
        assert_eq!(head.col, 1);
        assert_eq!(
            app.views
                .get(&app.focused)
                .unwrap()
                .selections()
                .primary()
                .anchor,
            head
        ); // collapsed
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

        let buf = app
            .contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .unwrap();
        assert_eq!(buf.slice().to_string(), "ab");
        let head = app
            .views
            .get(&app.focused)
            .unwrap()
            .selections()
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
        let buf = app
            .contents
            .get_mut(&editor_cid())
            .and_then(|c| c.buffer_mut())
            .unwrap();
        assert_eq!(buf.slice().to_string(), "a");
    }
}
