use std::io;

use super::App;
use super::dispatcher::{DispatchCommand, Dispatcher, default_global_keymap};
use super::layout::{LayoutError, resolve_focus, view_for_space};
use super::message::AppMessage;
use super::query::AppQuery;
use super::session::ClientSession;
use super::view::View;
use crate::core::buffer::Buffer;
use crate::core::command::{Command, ContentCommand, EditCommand};
use crate::core::content::Content;
use crate::core::content_view_state::ContentViewState;
use crate::frontend::Frontend;
use crate::protocol::content_query::{
    ContentData, ContentQuery, CursorStyle, DocumentStatus, RenderQuery, RowRange,
    TextPresentation, ViewData, ViewPresentation,
};
use crate::protocol::frontend_event::{FrontendEvent, ResizeEvent};
use crate::protocol::ids::{ContentId, SpaceId, ViewId};
use crate::protocol::key_event::{ArrowKey, KeyCode, KeyEvent};
use crate::protocol::revision::Revision;
use crate::protocol::scene::Scene;
use crate::protocol::selection::{Selection, Selections, TextOffset};
use crate::protocol::space::{Sizing, SpaceKind, SplitDirection};
use crate::protocol::status::StatusMessage;
use crate::protocol::viewport::{ViewportCommand, ViewportCursorBehavior};
use std::collections::VecDeque;

struct ScriptedFrontend {
    events: VecDeque<FrontendEvent>,
    renders: usize,
    scene_revisions: Vec<Revision>,
    fail_next_event: bool,
    fail_render: bool,
    viewport_height: usize,
    viewport_commands: Vec<(ViewId, ViewportCommand)>,
}

impl ScriptedFrontend {
    fn new(events: Vec<FrontendEvent>) -> Self {
        Self {
            events: events.into(),
            renders: 0,
            scene_revisions: Vec::new(),
            fail_next_event: false,
            fail_render: false,
            viewport_height: 4,
            viewport_commands: Vec::new(),
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

    fn execute_viewport_command(
        &mut self,
        _scene: &Scene,
        _scene_revision: Revision,
        view: ViewId,
        command: ViewportCommand,
    ) -> io::Result<usize> {
        self.viewport_commands.push((view, command));
        Ok(match command.amount {
            crate::protocol::viewport::ViewportMoveAmount::HalfPage => {
                (self.viewport_height / 2).max(1)
            }
            crate::protocol::viewport::ViewportMoveAmount::FullPage => self.viewport_height,
        })
    }
}

fn make_app(events: Vec<FrontendEvent>, path: Option<&str>) -> App<ScriptedFrontend> {
    App::new(path, 40, 5, ScriptedFrontend::new(events)).unwrap()
}

fn editor_cid() -> ContentId {
    ContentId(0)
}

fn view_id(app: &App<ScriptedFrontend>, space: SpaceId) -> ViewId {
    view_for_space(app.session.scene(), space).expect("space hosts a view")
}

fn view_at(app: &App<ScriptedFrontend>, space: SpaceId) -> &View {
    &app.session.views()[&view_id(app, space)]
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
    let mut second = ClientSession::editor(app.kernel.contents(), app.kernel.modes(), 80, 20);
    let first_view = view_id(&app, app.session.focused());
    let second_view = view_for_space(second.scene(), second.focused()).unwrap();

    second.resize(100, 30);
    app.handle_event(FrontendEvent::Key(KeyEvent::char('i')))
        .await
        .unwrap();
    app.handle_event(FrontendEvent::Key(KeyEvent::char('x')))
        .await
        .unwrap();

    assert_eq!(app.session.views()[&first_view].content(), editor_cid());
    assert_eq!(second.views()[&second_view].content(), editor_cid());
    assert_eq!(app.session.scene().size.width, 40);
    assert_eq!(second.scene().size.width, 100);
    assert_eq!(
        app.session.views()[&first_view]
            .selections()
            .unwrap()
            .primary()
            .head()
            .char_index,
        1
    );
    assert_eq!(
        second.views()[&second_view]
            .selections()
            .unwrap()
            .primary()
            .head(),
        TextOffset::origin()
    );
}

#[test]
fn production_content_paths_have_no_dynamic_type_probes() {
    let app = [
        include_str!("application.rs"),
        include_str!("layout.rs"),
        include_str!("query.rs"),
        include_str!("runtime.rs"),
        include_str!("save.rs"),
    ]
    .concat();
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
    match app.kernel.contents().query(
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
        .contents()
        .query(content, ContentQuery::TextPoints(vec![offset]))
    {
        ContentData::TextPoints(mut points) => points.remove(0),
        _ => panic!("expected text point"),
    }
}

fn document_status(app: &App<ScriptedFrontend>, content: ContentId) -> DocumentStatus {
    match app
        .kernel
        .contents()
        .query(content, ContentQuery::DocumentStatus)
    {
        ContentData::DocumentStatus(status) => status,
        data => panic!("expected document status, got {data:?}"),
    }
}

#[test]
fn content_query_reads_buffer_and_view() {
    let mut app = make_app(vec![], None);
    let focused_view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ViewContent {
        command: ContentCommand::Edit(EditCommand::InsertText("hi".to_string())),
        view: focused_view,
        content: editor_cid(),
    })
    .unwrap();
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
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
        .views()
        .iter()
        .find_map(|(id, view)| (view.content() == ContentId(1)).then_some(*id))
        .expect("status bar view exists");
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
    };

    let view = query.view(status_view);
    assert_eq!(view.presentation, ViewPresentation::StatusBar);
}

#[tokio::test(flavor = "multi_thread")]
async fn two_views_of_one_buffer_keep_independent_mode_instances() {
    let mut app = make_app(vec![], None);
    let left = app.session.focused();
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
    assert_eq!(app.session.focused(), right);

    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
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
        app.session.views()[&left_id].selections()
    );
    assert_eq!(
        Some(&right_text.selections),
        app.session.views()[&right_id].selections()
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

    app.set_space_sizing(app.session.focused(), Sizing::Fixed(12))
        .unwrap();

    assert_eq!(
        view_at(&app, app.session.focused())
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
        .contents_mut()
        .insert(other, Content::Buffer(Buffer::new()));
    for key in ['i', 'a', 'b', 'c'] {
        app.handle_event(FrontendEvent::Key(KeyEvent::char(key)))
            .await
            .unwrap();
    }

    app.replace_space_content(app.session.focused(), other, true)
        .unwrap();

    let view = view_at(&app, app.session.focused());
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
    let left = app.session.focused();
    let right = app
        .split_space(left, editor_cid(), true, SplitDirection::Right, true)
        .unwrap()
        .new_space;
    let right_view = view_id(&app, right);

    app.close_space(right).unwrap();

    assert_eq!(app.session.focused(), left);
    assert!(!app.session.views().contains_key(&right_view));
}

#[test]
fn missing_content_is_rejected_before_scene_mutation() {
    let mut app = make_app(vec![], None);
    let root = app.session.scene().root();
    let revision = app.session.scene_revision();

    assert!(matches!(
        app.split_space(root, ContentId(999), true, SplitDirection::Right, true),
        Err(LayoutError::MissingContent(ContentId(999)))
    ));
    assert_eq!(app.session.scene().root(), root);
    assert_eq!(app.session.scene_revision(), revision);
}

#[test]
fn successful_layout_mutation_advances_scene_revision() {
    let mut app = make_app(vec![], None);

    app.set_space_sizing(app.session.focused(), Sizing::Fixed(12))
        .unwrap();

    assert_eq!(app.session.scene_revision(), Revision(1));
}

#[test]
fn render_passes_current_scene_revision_to_frontend() {
    let mut app = make_app(vec![], None);
    app.set_space_sizing(app.session.focused(), Sizing::Fixed(12))
        .unwrap();

    app.render().unwrap();

    assert_eq!(app.frontend.scene_revisions, vec![Revision(1)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn edit_commands_advance_view_and_content_revisions() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());

    app.handle_event(FrontendEvent::Key(KeyEvent::char('i')))
        .await
        .unwrap();
    app.handle_event(FrontendEvent::Key(KeyEvent::char('x')))
        .await
        .unwrap();

    assert!(app.session.views()[&view].revision() > Revision(0));
    assert!(app.kernel.contents().revision(editor_cid()).unwrap() > Revision(0));
    assert_eq!(app.session.scene_revision(), Revision(0));
}

#[test]
fn preferred_inert_status_space_is_not_selected() {
    let app = make_app(vec![], None);
    let status = app
        .session
        .scene()
        .node(app.session.scene().root())
        .children[1];

    assert_eq!(
        resolve_focus(app.session.scene(), app.session.focused(), Some(status)),
        Some(app.session.focused())
    );
}

#[test]
fn closing_last_focusable_space_is_rejected() {
    let mut app = make_app(vec![], None);
    let status = app
        .session
        .scene()
        .node(app.session.scene().root())
        .children[1];

    assert!(matches!(
        app.close_space(app.session.focused()),
        Err(LayoutError::WouldRemoveLastFocusable(_))
    ));
    assert_ne!(app.session.focused(), status);
}

#[test]
fn replacing_only_focusable_content_with_inert_space_is_rejected() {
    let mut app = make_app(vec![], None);
    let focused = app.session.focused();
    let other = ContentId(9);
    app.kernel
        .contents_mut()
        .insert(other, Content::Buffer(Buffer::new()));

    assert_eq!(
        app.replace_space_content(focused, other, false),
        Err(LayoutError::NoFocusableSpace)
    );
    assert_eq!(app.session.focused(), focused);
    assert!(matches!(
        &app.session.scene().node(focused).space.kind,
        SpaceKind::Content { view, .. }
            if app.session.views()[view].content() == editor_cid()
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
    assert!(app.kernel.is_cancelled());
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
    let cursor = view_at(&app, app.session.focused())
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
        .contents_mut()
        .insert(other_cid, Content::Buffer(Buffer::new()));
    let other_sid = app
        .split_space(
            app.session.focused(),
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
        app.kernel.contents().query(
            editor_cid(),
            ContentQuery::TextRows(RowRange { start: 0, end: 1 }),
        ),
        ContentData::TextRows(vec!["".to_string()]),
    );
    assert_eq!(
        app.kernel.contents().query(
            other_cid,
            ContentQuery::TextRows(RowRange { start: 0, end: 1 }),
        ),
        ContentData::TextRows(vec!["Z".to_string()]),
    );
    assert_eq!(
        app.session
            .views()
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
        .contents_mut()
        .insert(other_cid, Content::Buffer(Buffer::new()));

    app.execute_command(DispatchCommand::ViewContent {
        command: ContentCommand::Edit(EditCommand::InsertText("Z".to_string())),
        view: view_id(&app, app.session.focused()),
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
    assert_eq!(app.session.scene().size.width, 100);
    assert_eq!(app.session.scene().size.height, 40);
    assert_eq!(app.session.scene_revision(), Revision(1));
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
            .contents()
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
    app.session
        .replace_dispatcher_for_test(Dispatcher::new(global));
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
        view: view_id(&app, app.session.focused()),
        content: editor_cid(),
    })
    .unwrap();
    assert!(document_status(&app, editor_cid()).modified);
    app.kernel.track_pending_save_for_test(
        editor_cid(),
        1,
        crate::core::transaction::TextStateId(1),
        None,
    );

    app.handle_app_message(AppMessage::SaveCompleted {
        content: editor_cid(),
        revision: 1,
        state: crate::core::transaction::TextStateId(1),
        result: Ok(()),
    })
    .unwrap();

    assert!(!app.kernel.has_pending_save(editor_cid()));
    let status = document_status(&app, editor_cid());
    assert!(!status.modified);
    assert_eq!(status.message, StatusMessage::Saved);
}

#[test]
fn save_completed_err_marks_buffer_save_failed() {
    let mut app = make_app(vec![], None);
    app.kernel.track_pending_save_for_test(
        editor_cid(),
        0,
        crate::core::transaction::TextStateId(0),
        None,
    );

    app.handle_app_message(AppMessage::SaveCompleted {
        content: editor_cid(),
        revision: 0,
        state: crate::core::transaction::TextStateId(0),
        result: Err(io::Error::other("boom")),
    })
    .unwrap();

    assert!(!app.kernel.has_pending_save(editor_cid()));
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
        view: view_id(&app, app.session.focused()),
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
        view: view_id(&app, app.session.focused()),
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
        view: view_id(&app, app.session.focused()),
        content: editor_cid(),
    })
    .unwrap();
    app.execute_command(DispatchCommand::Content {
        command: ContentCommand::Save,
        content: editor_cid(),
    })
    .unwrap();
    assert!(app.kernel.has_pending_save(editor_cid()));

    app.shutdown_tasks().await.unwrap();

    assert!(!app.kernel.has_pending_save(editor_cid()));
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
        .contents_mut()
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
    assert!(!app.kernel.has_pending_save(editor_cid()));
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
    let head = view_at(&app, app.session.focused())
        .selections()
        .unwrap()
        .primary()
        .head();
    assert_eq!(head.char_index, 3);
    assert_eq!(
        view_at(&app, app.session.focused())
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
            FrontendEvent::Key(KeyEvent::char('h')),                   // collapsed 左移 → head=1
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    app.run().await.unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["abc"]); // Escape/h 不改文本
    let head = view_at(&app, app.session.focused())
        .selections()
        .unwrap()
        .primary()
        .head();
    assert_eq!(text_point(&app, editor_cid(), head).col, 1);
    assert_eq!(
        view_at(&app, app.session.focused())
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
    let selection = view_at(&app, app.session.focused())
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
async fn vim_line_visual_ctrl_d_deletes_frontend_sized_line_range() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('V')),
            FrontendEvent::Key(KeyEvent::ctrl('d')),
            FrontendEvent::Key(KeyEvent::char('d')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    let focused_view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ViewContent {
        command: ContentCommand::Edit(EditCommand::InsertText("one\ntwo\nthree\nfour".to_string())),
        view: focused_view,
        content: editor_cid(),
    })
    .unwrap();

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["four"]);
    assert_eq!(app.frontend.viewport_commands.len(), 1);
    assert_eq!(
        app.frontend.viewport_commands[0],
        (
            focused_view,
            ViewportCommand::new(
                crate::protocol::viewport::ViewportMoveDirection::Down,
                crate::protocol::viewport::ViewportMoveAmount::HalfPage,
                ViewportCursorBehavior::Extend,
            ),
        )
    );
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
    let head = view_at(&app, app.session.focused())
        .selections()
        .unwrap()
        .primary()
        .head();
    assert_eq!(head.char_index, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_insert_accepts_unicode_text() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('中')),
            FrontendEvent::Key(KeyEvent::char('文')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["中文"]);
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
    let left = app.session.focused();
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
    let right_revision = app.session.views()[&right_view].revision();
    match app.session.view_mut(left_view).unwrap().state_mut() {
        crate::core::content_view_state::ContentViewState::Buffer(state) => {
            *state.selections_mut() = Selections::single(Selection {
                anchor: TextOffset::origin(),
                head: TextOffset { char_index: 3 },
            });
        }
        _ => unreachable!(),
    }
    match app.session.view_mut(right_view).unwrap().state_mut() {
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
        app.session.views()[&right_view]
            .selections()
            .unwrap()
            .primary()
            .head(),
        TextOffset::origin()
    );
    assert!(app.session.views()[&right_view].revision() > right_revision);
}

#[test]
fn shared_view_positions_follow_text_change_affinity() {
    let mut app = make_app(vec![], None);
    let left = app.session.focused();
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
        let ContentViewState::Buffer(state) = app.session.view_mut(view).unwrap().state_mut()
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
        app.session.views()[&right_view]
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
    let left = app.session.focused();
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
    let ContentViewState::Buffer(state) = app.session.view_mut(right_view).unwrap().state_mut()
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
            app.session.views()[&view]
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
            app.session.views()[&view]
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
    let next = app.session.next_view_id_for_test();

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
    assert_eq!(app.session.next_view_id_for_test(), next);
    assert!(
        app.replace_space_content(SpaceId(999), editor_cid(), true)
            .is_err()
    );
    assert_eq!(app.session.next_view_id_for_test(), next);
}

#[test]
fn no_op_edit_does_not_advance_content_or_view_revision() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    let view_revision = app.session.views()[&view].revision();
    let content_revision = app.kernel.contents().revision(editor_cid()).unwrap();

    app.execute_command(DispatchCommand::ViewContent {
        command: ContentCommand::Edit(EditCommand::MoveLeftBy(1)),
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(app.session.views()[&view].revision(), view_revision);
    assert_eq!(
        app.kernel.contents().revision(editor_cid()),
        Some(content_revision)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn frontend_error_still_waits_for_pending_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("save-on-error.txt");
    std::fs::write(&path, "old").unwrap();
    let mut app = make_app(vec![], path.to_str());
    let view = view_id(&app, app.session.focused());
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
    assert!(!app.kernel.has_pending_saves());
}

#[tokio::test(flavor = "multi_thread")]
async fn render_error_still_waits_for_pending_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("save-on-render-error.txt");
    std::fs::write(&path, "old").unwrap();
    let mut app = make_app(vec![], path.to_str());
    let view = view_id(&app, app.session.focused());
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
    assert!(!app.kernel.has_pending_saves());
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
