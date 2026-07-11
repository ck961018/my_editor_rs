//! App：tokio::select! 多路复用 evloop。不感知 tui/gui（只依赖 Frontend trait + Scene）。
//! 事件分发委托 Dispatcher（捕获链 + 前缀状态机），Command 执行通过 ContentStore。
//! 不持 editor_content/status_content 角色 ID——从 scene/focused 推导。

mod dispatcher;
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
use crate::core::command::AppCommand;
use crate::core::content::{Content, ContentEffect, ContentEvent, ContentInput};
use crate::core::content_store::ContentStore;
use crate::core::status_bar::StatusBar;
use crate::frontend::Frontend;
use crate::protocol::content_query::{ContentData, ContentQuery, RenderQuery};
use crate::protocol::frontend_event::FrontendEvent;
use crate::protocol::ids::{ContentId, SpaceId};
use crate::protocol::scene::{Scene, SceneBuilder, build_editor_scene};
use crate::protocol::selection::{CursorPos, Selection, Selections};
use crate::protocol::space::SpaceKind;

pub struct App<F: Frontend> {
    contents: ContentStore,
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
        let mut contents = ContentStore::default();
        contents.insert(editor_content, Content::Buffer(buffer));
        contents.insert(status_content, Content::StatusBar(status_bar));
        let mut scene_builder = SceneBuilder::new();
        let (scene, editor_space) = build_editor_scene(
            &mut scene_builder,
            width as i32,
            height as i32,
            editor_content,
            status_content,
        )
        .expect("valid editor scene");
        let views = build_views(&scene, &contents);
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
                if let Some(command) =
                    self.dispatcher
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
            DispatchCommand::Content { command, content } => {
                let effect = self
                    .contents
                    .execute(content, ContentInput::Command(command));
                self.handle_content_effect(content, effect);
            }
            DispatchCommand::ViewContent {
                command,
                space,
                content,
            } => {
                let view = self.views.get_mut(&space).expect("target view exists");
                assert_eq!(view.content(), content, "view/content target mismatch");
                let (selections, runtime) = view.selections_and_runtime_mut();
                let effect = self.contents.execute(
                    content,
                    ContentInput::View {
                        command,
                        selections,
                        runtime,
                    },
                );
                self.handle_content_effect(content, effect);
            }
            DispatchCommand::Noop => {}
        }
        Ok(())
    }

    fn handle_app_message(&mut self, message: AppMessage) -> io::Result<()> {
        match message {
            AppMessage::SaveCompleted { content, result } => {
                self.pending_saves.remove(&content);
                let effect = self.contents.execute(
                    content,
                    ContentInput::Event(ContentEvent::SaveFinished(result)),
                );
                self.handle_content_effect(content, effect);
            }
        }
        Ok(())
    }

    fn handle_content_effect(&mut self, id: ContentId, effect: ContentEffect) {
        if let ContentEffect::Save(snapshot) = effect {
            self.spawn_save(id, snapshot);
        }
    }

    /// 发起异步保存。返回是否真正发起（同一 content 已在保存时忽略）。
    fn spawn_save(&mut self, id: ContentId, snapshot: crate::core::content::SaveSnapshot) -> bool {
        if self.pending_saves.contains(&id) {
            return false;
        }
        let tx = self.message_tx.clone();
        self.pending_saves.insert(id);
        self.tasks.spawn_critical(async move {
            let result = tokio::fs::write(snapshot.path, snapshot.bytes).await;
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
            .render(&self.scene, &query as &dyn RenderQuery, self.focused)
    }
}

/// 遍历 scene 所有 Content space，为每个建 View（绑定其 content）。
fn build_views(scene: &Scene, contents: &ContentStore) -> HashMap<SpaceId, View> {
    let mut views = HashMap::new();
    collect_content_spaces(scene, scene.root, contents, &mut views);
    views
}

fn collect_content_spaces(
    scene: &Scene,
    sid: SpaceId,
    contents: &ContentStore,
    out: &mut HashMap<SpaceId, View>,
) {
    let node = scene.node(sid);
    match &node.space.kind {
        SpaceKind::Content { content } => {
            let runtime = contents
                .create_runtime(*content)
                .expect("scene content exists in content store");
            out.insert(sid, View::new(*content, runtime));
        }
        SpaceKind::Container { children, .. } => {
            for c in children {
                collect_content_spaces(scene, *c, contents, out);
            }
        }
    }
}

/// 借 App 数据字段的查询适配器：render 时用它做 `&dyn RenderQuery`，
/// 与 `&mut self.frontend` 不冲突（字段级 split borrow）。
struct AppQuery<'a> {
    contents: &'a ContentStore,
    views: &'a HashMap<SpaceId, View>,
}

impl RenderQuery for AppQuery<'_> {
    fn content(&self, cid: ContentId, query: ContentQuery) -> ContentData {
        self.contents.query(cid, query)
    }

    fn selections(&self, sid: SpaceId) -> Selections {
        self.views
            .get(&sid)
            .map(|v| v.selections().clone())
            .unwrap_or_else(|| Selections::single(Selection::collapsed(CursorPos::origin())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::command::{Command, ContentCommand, EditCommand};
    use crate::core::content::Content;
    use crate::frontend::Frontend;
    use crate::protocol::content_query::{
        ContentData, ContentQuery, DocumentStatus, RenderQuery, RowRange,
    };
    use crate::protocol::frontend_event::ResizeEvent;
    use crate::protocol::key_event::{ArrowKey, KeyCode, KeyEvent};
    use crate::protocol::status::StatusMessage;
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
            _query: &dyn RenderQuery,
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
    fn production_content_paths_have_no_dynamic_type_probes() {
        let app = include_str!("mod.rs");
        let content = include_str!("../core/content.rs");
        let dynamic_handler = concat!("Box<dyn ", "Content", "Handler>");
        let buffer_probe = concat!("buffer", "_mut(");
        let buffer_read_probe = concat!("as_", "buffer(");

        assert!(!app.contains(dynamic_handler));
        assert!(!app.contains(buffer_probe));
        assert!(!content.contains(buffer_read_probe));
    }

    fn text_rows(app: &App<ScriptedFrontend>, content: ContentId) -> Vec<String> {
        match app.contents.query(
            content,
            ContentQuery::TextRows(RowRange { start: 0, end: 5 }),
        ) {
            ContentData::TextRows(rows) => rows,
            data => panic!("expected text rows, got {data:?}"),
        }
    }

    fn document_status(app: &App<ScriptedFrontend>, content: ContentId) -> DocumentStatus {
        match app.contents.query(content, ContentQuery::DocumentStatus) {
            ContentData::DocumentStatus(status) => status,
            data => panic!("expected document status, got {data:?}"),
        }
    }

    #[test]
    fn content_query_reads_buffer_and_view() {
        let mut app = make_app(vec![], None);
        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Edit(EditCommand::InsertText("hi".to_string())),
            space: app.focused,
            content: editor_cid(),
        })
        .unwrap();
        let query = AppQuery {
            contents: &app.contents,
            views: &app.views,
        };
        assert_eq!(
            query.content(
                editor_cid(),
                ContentQuery::TextRows(RowRange { start: 0, end: 5 })
            ),
            ContentData::TextRows(vec!["hi".to_string()])
        );
        assert_eq!(
            query.content(editor_cid(), ContentQuery::TextLineCount),
            ContentData::TextLineCount(1)
        );
        let sels = query.selections(app.focused);
        assert_eq!(sels.primary().head().char_index, 2);
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
        assert_eq!(text_rows(&app, editor_cid()), vec!["a"]);
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
        assert_eq!(text_rows(&app, editor_cid()), vec!["a"]);
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
    fn multi_space_edit_targets_only_focused_content() {
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
        app.contents
            .insert(other_cid, Content::Buffer(Buffer::new()));
        let runtime = app
            .contents
            .create_runtime(other_cid)
            .expect("content exists");
        app.views.insert(other_sid, View::new(other_cid, runtime));

        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Edit(EditCommand::InsertText("Z".to_string())),
            space: other_sid,
            content: other_cid,
        })
        .unwrap();

        assert_eq!(
            app.contents.query(
                editor_cid(),
                ContentQuery::TextRows(RowRange { start: 0, end: 1 }),
            ),
            ContentData::TextRows(vec!["".to_string()]),
        );
        assert_eq!(
            app.contents.query(
                other_cid,
                ContentQuery::TextRows(RowRange { start: 0, end: 1 }),
            ),
            ContentData::TextRows(vec!["Z".to_string()]),
        );
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

    #[test]
    #[should_panic(expected = "view/content target mismatch")]
    fn view_content_rejects_mismatched_view_content_target() {
        let mut app = make_app(vec![], None);
        let other_cid = ContentId(9);
        app.contents
            .insert(other_cid, Content::Buffer(Buffer::new()));

        app.execute_command(DispatchCommand::ViewContent {
            command: ContentCommand::Edit(EditCommand::InsertText("Z".to_string())),
            space: app.focused,
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
        assert!(matches!(
            app.contents
                .query(editor_cid(), ContentQuery::DocumentStatus),
            ContentData::DocumentStatus(DocumentStatus {
                modified: false,
                message: StatusMessage::Saved,
                ..
            }),
        ));
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
        let mut buffer = Buffer::new();
        buffer.open_path(&path_str).unwrap();
        let mut sub = crate::core::keymap::Keymap::new();
        sub.bind(KeyEvent::char('s'), Command::Content(ContentCommand::Save));
        buffer.keymap_mut().bind_prefix(KeyEvent::char('z'), sub);
        app.contents.insert(editor_cid(), Content::Buffer(buffer));
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
            space: app.focused,
            content: editor_cid(),
        })
        .unwrap();
        assert!(document_status(&app, editor_cid()).modified);
        app.pending_saves.insert(editor_cid());

        app.handle_app_message(AppMessage::SaveCompleted {
            content: editor_cid(),
            result: Ok(()),
        })
        .unwrap();

        assert!(!app.pending_saves.contains(&editor_cid()));
        let status = document_status(&app, editor_cid());
        assert!(!status.modified);
        assert_eq!(status.message, StatusMessage::Saved);
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

        assert!(!app.pending_saves.contains(&editor_cid()));
        assert_eq!(
            document_status(&app, editor_cid()).message,
            StatusMessage::SaveFailed
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_save_ignores_duplicate_pending_save_for_same_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dedupe.txt");
        std::fs::write(&path, "hello").unwrap();
        let path_str = path.to_str().unwrap().to_owned();
        let mut app = make_app(vec![], Some(&path_str));

        app.execute_command(DispatchCommand::Content {
            command: ContentCommand::Save,
            content: editor_cid(),
        })
        .unwrap();
        app.execute_command(DispatchCommand::Content {
            command: ContentCommand::Save,
            content: editor_cid(),
        })
        .unwrap();
        assert!(app.pending_saves.contains(&editor_cid()));

        app.shutdown_tasks().await.unwrap();

        assert!(!app.pending_saves.contains(&editor_cid()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        assert_eq!(
            document_status(&app, editor_cid()).message,
            StatusMessage::Saved
        );
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
        app.contents.insert(other_cid, Content::Buffer(other));

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
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),      // 回 Normal（选区保留）
                FrontendEvent::Key(KeyEvent::char('h')),                   // shrink→head=2 collapse
                FrontendEvent::Key(KeyEvent::char('h')), // collapsed 左移 → head=1
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec!["abc"]); // Escape/h 不改文本
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

        assert_eq!(text_rows(&app, editor_cid()), vec!["ab"]);
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
        assert_eq!(text_rows(&app, editor_cid()), vec!["a"]);
    }
}
