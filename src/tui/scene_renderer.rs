//! 前端核心：layout（TaffyEngine）+ viewport 跟随 + pull 可见行 + paint 到 Canvas。
//! TuiFrontend 经此渲染；单元测试用 StubQuery + Output<Vec<u8>> 断言 VT 字节。

use std::collections::{HashMap, HashSet};
use std::io;

use crate::protocol::content_query::{
    ContentData, ContentQuery, RenderQuery, RowRange, SelectionShape, TextPresentation, ViewData,
    ViewPresentation,
};
use crate::protocol::ids::{SpaceId, ViewId};
use crate::protocol::revision::Revision;
use crate::protocol::scene::Scene;
use crate::protocol::selection::{TextOffset, TextPoint};
use crate::protocol::viewport::{
    Viewport, ViewportCommand, ViewportMoveAmount, ViewportMoveDirection,
};
use crate::terminal::output::Canvas;
use crate::tui::resolved::{RenderItem, ResolvedScene};
use crate::tui::status_line::status_line;
use crate::tui::taffy_engine::TaffyEngine;
use crate::tui::text_cells::{
    display_width_before_col, line_content, sanitize_terminal_text, take_display_width,
    terminal_char, terminal_char_width,
};

pub struct SceneRenderer {
    engine: TaffyEngine,
    viewports: HashMap<ViewId, Viewport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DisplayPoint {
    row: usize,
    col: usize,
}

impl SceneRenderer {
    pub fn new() -> Self {
        Self {
            engine: TaffyEngine::new(),
            viewports: HashMap::new(),
        }
    }

    pub fn render(
        &mut self,
        scene: &Scene,
        scene_revision: Revision,
        query: &dyn RenderQuery,
        focused: SpaceId,
        canvas: &mut dyn Canvas,
    ) -> io::Result<()> {
        let resolved: &ResolvedScene = self.engine.layout(scene, scene_revision);
        let live_views: HashSet<ViewId> = resolved.items.iter().map(|item| item.view_id).collect();
        self.viewports.retain(|view, _| live_views.contains(view));
        let views: HashMap<ViewId, ViewData> = resolved
            .items
            .iter()
            .map(|item| (item.view_id, query.view(item.view_id)))
            .collect();
        canvas.hide_cursor()?;
        // 焦点 viewport 跟随
        let focused_item = resolved.items.iter().find(|item| item.space_id == focused);
        let focused_view = views
            .get(&focused_item.expect("focused item exists").view_id)
            .expect("focused view has render data");
        let focused_text = match &focused_view.presentation {
            ViewPresentation::Text(text) => Some(text),
            ViewPresentation::StatusBar => None,
        };
        let focused_head = focused_text.map(|text| {
            text_point(
                query,
                focused_view.content,
                text.selections.primary().head(),
            )
        });
        let focused_display_col = focused_head.map(|head| {
            let line = text_row(query, focused_view.content, head.row);
            display_width_before_col(&line, head.col)
        });
        if let (Some(item), Some(focused_head), Some(focused_display_col)) =
            (focused_item, focused_head, focused_display_col)
        {
            let viewport = self
                .viewports
                .entry(item.view_id)
                .or_insert_with(Viewport::origin);
            follow_viewport(
                viewport,
                focused_head.row,
                focused_display_col,
                item.rect.width as usize,
                item.rect.height as usize,
            );
        }
        // 逐 Content item paint
        for item in &resolved.items {
            paint_item(
                item,
                query,
                views
                    .get(&item.view_id)
                    .expect("resolved item has view data"),
                &self.viewports,
                canvas,
            )?;
        }
        // 焦点光标定位
        if let (Some(item), Some(focused_head)) = (
            focused_item.filter(|item| item.rect.width > 0 && item.rect.height > 0),
            focused_head,
        ) {
            let vp = self
                .viewports
                .get(&item.view_id)
                .copied()
                .unwrap_or_else(Viewport::origin);
            let display = display_point(
                focused_head.row,
                focused_display_col.expect("text cursor has a display column"),
                item,
                vp,
            );
            canvas.move_cursor(display.row, display.col)?;
            canvas.set_cursor_style(
                focused_text
                    .expect("text cursor has text presentation")
                    .cursor_style,
            )?;
            canvas.show_cursor()?;
        }
        canvas.flush()
    }

    pub fn execute_viewport_command(
        &mut self,
        scene: &Scene,
        scene_revision: Revision,
        view: ViewId,
        command: ViewportCommand,
    ) -> usize {
        let resolved = self.engine.layout(scene, scene_revision);
        let Some(item) = resolved.items.iter().find(|item| item.view_id == view) else {
            return 0;
        };
        let height = item.rect.height.max(0) as usize;
        if height == 0 {
            return 0;
        }
        let lines = match command.amount {
            ViewportMoveAmount::HalfPage => (height / 2).max(1),
            ViewportMoveAmount::FullPage => height,
        };
        let viewport = self.viewports.entry(view).or_insert_with(Viewport::origin);
        match command.direction {
            ViewportMoveDirection::Up => viewport.scroll_up(lines),
            ViewportMoveDirection::Down => viewport.scroll_down(lines),
        }
        lines
    }
}

fn text_row(
    query: &dyn RenderQuery,
    content: crate::protocol::ids::ContentId,
    row: usize,
) -> String {
    let ContentData::TextRows(lines) = query.content(
        content,
        ContentQuery::TextRows(RowRange {
            start: row,
            end: row.saturating_add(1),
        }),
    ) else {
        panic!("text presentation must answer TextRows")
    };
    lines.into_iter().next().unwrap_or_default()
}

impl Default for SceneRenderer {
    fn default() -> Self {
        Self::new()
    }
}

fn text_point(
    query: &dyn RenderQuery,
    content: crate::protocol::ids::ContentId,
    offset: TextOffset,
) -> TextPoint {
    let ContentData::TextPoints(mut points) =
        query.content(content, ContentQuery::TextPoints(vec![offset]))
    else {
        panic!("text presentation must answer TextPoints")
    };
    assert_eq!(points.len(), 1, "one offset must produce one text point");
    points.remove(0)
}

fn follow_viewport(
    viewport: &mut Viewport,
    cursor_row: usize,
    cursor_col: usize,
    width: usize,
    height: usize,
) {
    viewport.ensure_cursor_visible(cursor_row, height);

    if width == 0 || cursor_col < viewport.left_col {
        viewport.left_col = cursor_col;
    } else if cursor_col >= viewport.left_col.saturating_add(width) {
        viewport.left_col = cursor_col - width + 1;
    }
}

fn display_point(
    row: usize,
    cell_col: usize,
    item: &RenderItem,
    viewport: Viewport,
) -> DisplayPoint {
    DisplayPoint {
        row: row.saturating_sub(viewport.top_row) + item.rect.y as usize,
        col: cell_col.saturating_sub(viewport.left_col) + item.rect.x as usize,
    }
}

fn paint_item(
    item: &RenderItem,
    query: &dyn RenderQuery,
    view: &ViewData,
    viewports: &HashMap<ViewId, Viewport>,
    canvas: &mut dyn Canvas,
) -> io::Result<()> {
    match &view.presentation {
        ViewPresentation::Text(text) => {
            paint_text_item(item, query, view.content, text, viewports, canvas)
        }
        ViewPresentation::StatusBar => paint_status_bar(item, query, view.content, canvas),
    }
}

fn paint_text_item(
    item: &RenderItem,
    query: &dyn RenderQuery,
    content: crate::protocol::ids::ContentId,
    text: &TextPresentation,
    viewports: &HashMap<ViewId, Viewport>,
    canvas: &mut dyn Canvas,
) -> io::Result<()> {
    let vid = item.view_id;
    let vp = viewports
        .get(&vid)
        .copied()
        .unwrap_or_else(Viewport::origin);
    let height = item.rect.height as usize;
    let width = item.rect.width as usize;
    let start = vp.top_row;
    let ContentData::TextRows(lines) = query.content(
        content,
        ContentQuery::TextRows(RowRange {
            start,
            end: start + height,
        }),
    ) else {
        panic!("text presentation must answer TextRows")
    };
    let primary = text.selections.primary();
    let selection_offsets = match text.selection_shape {
        SelectionShape::Character => (primary.anchor != primary.head).then_some({
            if primary.anchor.char_index <= primary.head.char_index {
                (primary.anchor, primary.head)
            } else {
                (primary.head, primary.anchor)
            }
        }),
        SelectionShape::Line => Some((primary.anchor, primary.head)),
    };
    let selection = selection_offsets.map(|(start, end)| {
        let ContentData::TextPoints(points) =
            query.content(content, ContentQuery::TextPoints(vec![start, end]))
        else {
            panic!("text presentation must answer TextPoints")
        };
        assert_eq!(points.len(), 2, "two offsets must produce two text points");
        (points[0], points[1])
    });
    for (row, line) in lines.iter().enumerate() {
        let buf_row = start + row;
        let screen_row = (item.rect.y + row as i32) as usize;
        let linewise_highlight = text.selection_shape == SelectionShape::Line
            && selection.is_some_and(|(anchor, head)| {
                let first = anchor.row.min(head.row);
                let last = anchor.row.max(head.row);
                buf_row >= first && buf_row <= last
            });
        clear_item_row_with_highlight(
            canvas,
            screen_row,
            item.rect.x as usize,
            width,
            linewise_highlight,
        )?;
        let hi = if linewise_highlight {
            Some((0, usize::MAX))
        } else if text.selection_shape == SelectionShape::Character {
            selection.and_then(|(sel_start, sel_end)| {
                (buf_row >= sel_start.row && buf_row <= sel_end.row).then(|| {
                    let start = if buf_row == sel_start.row {
                        sel_start.col
                    } else {
                        0
                    };
                    let end = if buf_row == sel_end.row {
                        sel_end.col
                    } else {
                        usize::MAX
                    };
                    (start, end)
                })
            })
        } else {
            None
        };
        paint_line_with_highlight(canvas, line, vp.left_col, width, hi)?;
    }
    for row in lines.len()..height {
        let screen_row = (item.rect.y + row as i32) as usize;
        clear_item_row(canvas, screen_row, item.rect.x as usize, width)?;
    }
    Ok(())
}

fn paint_status_bar(
    item: &RenderItem,
    query: &dyn RenderQuery,
    content: crate::protocol::ids::ContentId,
    canvas: &mut dyn Canvas,
) -> io::Result<()> {
    let ContentData::StatusBarData(data) = query.content(content, ContentQuery::StatusBarData)
    else {
        panic!("status bar presentation must answer StatusBarData")
    };
    let width = item.rect.width.max(0) as usize;
    clear_item_row(canvas, item.rect.y as usize, item.rect.x as usize, width)?;
    let line = sanitize_terminal_text(&status_line(
        data.file_name.as_deref(),
        data.modified,
        &data.message,
    ));
    canvas.write_str(&take_display_width(&line, width))
}

fn clear_item_row(canvas: &mut dyn Canvas, row: usize, col: usize, width: usize) -> io::Result<()> {
    clear_item_row_with_highlight(canvas, row, col, width, false)
}

fn clear_item_row_with_highlight(
    canvas: &mut dyn Canvas,
    row: usize,
    col: usize,
    width: usize,
    highlighted: bool,
) -> io::Result<()> {
    canvas.move_cursor(row, col)?;
    if width > 0 {
        if highlighted {
            canvas.set_reverse(true)?;
        }
        canvas.write_str(&" ".repeat(width))?;
        if highlighted {
            canvas.set_reverse(false)?;
        }
        canvas.move_cursor(row, col)?;
    }
    Ok(())
}

/// Paint the visible display-cell interval `[left_col, left_col + width)` of one logical row.
/// A trailing logical newline is discarded. `hi`, when present, remains an absolute logical-char
/// range; each complete visible character is highlighted according to its logical column.
fn paint_line_with_highlight(
    canvas: &mut dyn Canvas,
    line: &str,
    left_col: usize,
    width: usize,
    hi: Option<(usize, usize)>,
) -> io::Result<()> {
    if width == 0 {
        return Ok(());
    }
    let content = line_content(line);
    let visible_end = left_col.saturating_add(width);
    let mut cell_col: usize = 0;
    let mut reverse_on = false;
    let mut previous_char_was_visible = false;

    for (logical_col, source) in content.chars().enumerate() {
        let ch = terminal_char(source);
        let char_width = terminal_char_width(ch);
        let char_start = cell_col;
        let char_end = char_start.saturating_add(char_width);
        cell_col = char_end;

        if char_width == 0 {
            if previous_char_was_visible {
                let mut encoded = [0; 4];
                canvas.write_str(ch.encode_utf8(&mut encoded))?;
            }
            continue;
        }
        previous_char_was_visible = false;
        if char_end <= left_col {
            continue;
        }
        if char_start < left_col {
            if reverse_on {
                canvas.set_reverse(false)?;
                reverse_on = false;
            }
            canvas.write_str(&" ".repeat(char_end.min(visible_end) - left_col))?;
            continue;
        }
        if char_start >= visible_end || char_end > visible_end {
            break;
        }

        let highlighted = hi.is_some_and(|(start, end)| logical_col >= start && logical_col < end);
        if highlighted != reverse_on {
            canvas.set_reverse(highlighted)?;
            reverse_on = highlighted;
        }
        let mut encoded = [0; 4];
        canvas.write_str(ch.encode_utf8(&mut encoded))?;
        previous_char_was_visible = true;
    }
    if reverse_on {
        canvas.set_reverse(false)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::content_query::{
        ContentData, ContentQuery, CursorStyle, RenderQuery, StatusBarData, TextPresentation,
        ViewData, ViewPresentation,
    };
    use crate::protocol::ids::{ContentId, ViewId};
    use crate::protocol::selection::{Selection, Selections, TextOffset};
    use crate::protocol::status::StatusMessage;
    use crate::terminal::output::Output;
    use crate::tui::test_scene::{editor_scene, split_editor_scene};
    use std::collections::HashMap;

    fn points_for_lines(lines: &[String], offsets: Vec<TextOffset>) -> Vec<TextPoint> {
        let text = lines
            .iter()
            .map(|line| line.strip_suffix('\n').unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");
        let len = text.chars().count();
        offsets
            .into_iter()
            .map(|offset| {
                let mut point = TextPoint { row: 0, col: 0 };
                for ch in text.chars().take(offset.char_index.min(len)) {
                    if ch == '\n' {
                        point.row += 1;
                        point.col = 0;
                    } else {
                        point.col += 1;
                    }
                }
                point
            })
            .collect()
    }

    fn text_view(
        content: ContentId,
        selections: Selections,
        cursor_style: CursorStyle,
    ) -> ViewData {
        text_view_with_shape(content, selections, cursor_style, SelectionShape::Character)
    }

    fn text_view_with_shape(
        content: ContentId,
        selections: Selections,
        cursor_style: CursorStyle,
        selection_shape: SelectionShape,
    ) -> ViewData {
        ViewData {
            content,
            presentation: ViewPresentation::Text(TextPresentation {
                selections,
                cursor_style,
                selection_shape,
            }),
        }
    }

    fn status_view(content: ContentId) -> ViewData {
        ViewData {
            content,
            presentation: ViewPresentation::StatusBar,
        }
    }

    struct StubQuery {
        editor_cid: ContentId,
        lines: Vec<String>,
        selections: Selections,
    }
    impl RenderQuery for StubQuery {
        fn content(&self, cid: ContentId, query: ContentQuery) -> ContentData {
            let status = StatusBarData {
                file_name: Some("f.txt".to_string()),
                modified: false,
                message: StatusMessage::None,
            };
            match query {
                ContentQuery::TextRows(range) => {
                    assert_eq!(cid, self.editor_cid, "only editor content has lines");
                    ContentData::TextRows(
                        self.lines
                            .iter()
                            .skip(range.start)
                            .take(range.end.saturating_sub(range.start))
                            .cloned()
                            .collect(),
                    )
                }
                ContentQuery::TextPoints(offsets) => {
                    assert_eq!(cid, self.editor_cid, "only text content maps offsets");
                    ContentData::TextPoints(points_for_lines(&self.lines, offsets))
                }
                ContentQuery::DocumentStatus => ContentData::DocumentStatus(status),
                ContentQuery::StatusBarData => {
                    assert_eq!(
                        cid,
                        ContentId(1),
                        "only status presentation asks status data"
                    );
                    ContentData::StatusBarData(status)
                }
            }
        }
        fn view(&self, view: ViewId) -> ViewData {
            if view == ViewId(1) {
                status_view(ContentId(1))
            } else {
                text_view(
                    self.editor_cid,
                    self.selections.clone(),
                    CursorStyle::Default,
                )
            }
        }
    }

    struct MultiSpaceQuery {
        lines: Vec<String>,
        selections: HashMap<ViewId, ViewData>,
    }

    impl RenderQuery for MultiSpaceQuery {
        fn content(&self, cid: ContentId, query: ContentQuery) -> ContentData {
            let status = StatusBarData {
                file_name: None,
                modified: false,
                message: StatusMessage::None,
            };
            match query {
                ContentQuery::TextRows(range) => {
                    assert_eq!(cid, ContentId(0));
                    ContentData::TextRows(
                        self.lines
                            .iter()
                            .skip(range.start)
                            .take(range.end.saturating_sub(range.start))
                            .cloned()
                            .collect(),
                    )
                }
                ContentQuery::TextPoints(offsets) => {
                    assert_eq!(cid, ContentId(0), "only text content maps offsets");
                    ContentData::TextPoints(points_for_lines(&self.lines, offsets))
                }
                ContentQuery::DocumentStatus => ContentData::DocumentStatus(status),
                ContentQuery::StatusBarData => {
                    assert_eq!(
                        cid,
                        ContentId(1),
                        "only status presentation asks status data"
                    );
                    ContentData::StatusBarData(status)
                }
            }
        }

        fn view(&self, view: ViewId) -> ViewData {
            self.selections
                .get(&view)
                .cloned()
                .unwrap_or_else(|| status_view(ContentId(1)))
        }
    }

    #[test]
    fn shared_content_spaces_use_their_own_selections() {
        let (scene, left, _right) = split_editor_scene(20, 2, ViewId(0), ViewId(1), ViewId(2));
        let query = MultiSpaceQuery {
            lines: vec!["abcd".to_string()],
            selections: HashMap::from([
                (
                    ViewId(0),
                    text_view(
                        ContentId(0),
                        Selections::single(Selection {
                            anchor: TextOffset { char_index: 0 },
                            head: TextOffset { char_index: 1 },
                        }),
                        CursorStyle::Default,
                    ),
                ),
                (
                    ViewId(2),
                    text_view(
                        ContentId(0),
                        Selections::single(Selection {
                            anchor: TextOffset { char_index: 2 },
                            head: TextOffset { char_index: 3 },
                        }),
                        CursorStyle::Default,
                    ),
                ),
            ]),
        };
        let mut renderer = SceneRenderer::new();
        let mut out = Output::new(Vec::new());
        renderer
            .render(
                &scene,
                Revision(0),
                &query,
                left,
                &mut out as &mut dyn Canvas,
            )
            .unwrap();
        let output = String::from_utf8(out.into_inner()).unwrap();

        assert!(output.contains("\x1b[7ma\x1b[27mbcd"), "left: {output}");
        assert!(output.contains("ab\x1b[7mc\x1b[27md"), "right: {output}");
        assert!(
            !output.contains("\x1b[2K"),
            "pane painting must not clear the full terminal row: {output}"
        );
    }

    #[test]
    fn moving_a_view_to_another_space_preserves_its_viewport() {
        let (scene, _left, right) = split_editor_scene(20, 2, ViewId(3), ViewId(1), ViewId(0));
        let saved_viewport = Viewport {
            top_row: 1,
            left_col: 0,
        };
        let mut renderer = SceneRenderer::new();
        renderer.viewports.insert(ViewId(0), saved_viewport);

        let query = MultiSpaceQuery {
            lines: vec!["line0".to_string(), "line1".to_string()],
            selections: HashMap::from([
                (
                    ViewId(0),
                    text_view(
                        ContentId(0),
                        Selections::single(Selection::collapsed(TextOffset { char_index: 6 })),
                        CursorStyle::Default,
                    ),
                ),
                (
                    ViewId(3),
                    text_view(
                        ContentId(0),
                        Selections::single(Selection::collapsed(TextOffset::origin())),
                        CursorStyle::Default,
                    ),
                ),
            ]),
        };
        let mut out = Output::new(Vec::new());

        renderer
            .render(
                &scene,
                Revision(0),
                &query,
                right,
                &mut out as &mut dyn Canvas,
            )
            .unwrap();

        assert_eq!(renderer.viewports.get(&ViewId(0)), Some(&saved_viewport));
        assert!(
            String::from_utf8(out.into_inner())
                .unwrap()
                .contains("line1")
        );
    }

    #[test]
    fn rendering_drops_viewports_for_views_removed_from_the_scene() {
        let (scene, editor) = editor_scene(20, 2, ViewId(2), ViewId(1));
        let mut renderer = SceneRenderer::new();
        renderer.viewports.insert(
            ViewId(0),
            Viewport {
                top_row: 7,
                left_col: 3,
            },
        );
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["line0".to_string()],
            selections: Selections::single(Selection::collapsed(TextOffset::origin())),
        };
        let mut out = Output::new(Vec::new());

        renderer
            .render(
                &scene,
                Revision(0),
                &query,
                editor,
                &mut out as &mut dyn Canvas,
            )
            .unwrap();

        assert!(!renderer.viewports.contains_key(&ViewId(0)));
        assert!(renderer.viewports.contains_key(&ViewId(2)));
    }

    #[test]
    fn focused_view_controls_terminal_cursor_style() {
        let (scene, left, right) = split_editor_scene(20, 2, ViewId(0), ViewId(1), ViewId(2));
        let query = MultiSpaceQuery {
            lines: vec!["abcd".to_string()],
            selections: HashMap::from([
                (
                    ViewId(0),
                    text_view(
                        ContentId(0),
                        Selections::single(Selection::collapsed(TextOffset::origin())),
                        CursorStyle::Default,
                    ),
                ),
                (
                    ViewId(2),
                    text_view(
                        ContentId(0),
                        Selections::single(Selection::collapsed(TextOffset::origin())),
                        CursorStyle::Block,
                    ),
                ),
            ]),
        };
        let mut renderer = SceneRenderer::new();

        let mut right_out = Output::new(Vec::new());
        renderer
            .render(
                &scene,
                Revision(0),
                &query,
                right,
                &mut right_out as &mut dyn Canvas,
            )
            .unwrap();
        let right_output = String::from_utf8(right_out.into_inner()).unwrap();
        assert!(right_output.contains("\x1b[2 q"), "right: {right_output}");

        let mut left_out = Output::new(Vec::new());
        renderer
            .render(
                &scene,
                Revision(0),
                &query,
                left,
                &mut left_out as &mut dyn Canvas,
            )
            .unwrap();
        let left_output = String::from_utf8(left_out.into_inner()).unwrap();
        assert!(left_output.contains("\x1b[0 q"), "left: {left_output}");
        assert!(!left_output.contains("\x1b[2 q"), "left: {left_output}");
    }

    #[test]
    fn renders_editor_lines_and_status() {
        let (scene, ed) = editor_scene(40, 5, ViewId(0), ViewId(1));
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["hello".to_string(), "world".to_string()],
            selections: Selections::single(Selection::collapsed(TextOffset::origin())),
        };
        let mut r = SceneRenderer::new();
        let mut out = Output::new(Vec::new());
        r.render(&scene, Revision(0), &query, ed, &mut out as &mut dyn Canvas)
            .unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("hello"), "{s}");
        assert!(s.contains("f.txt"), "{s}");
    }

    #[test]
    fn viewport_follows_cursor_below() {
        let (scene, ed) = editor_scene(40, 5, ViewId(0), ViewId(1));
        let lines: Vec<String> = (0..30).map(|i| format!("line{i}")).collect();
        let row_25_offset = lines
            .iter()
            .take(25)
            .map(|line| line.chars().count() + 1)
            .sum();
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines,
            selections: Selections::single(Selection::collapsed(TextOffset {
                char_index: row_25_offset,
            })),
        };
        let mut r = SceneRenderer::new();
        let mut out = Output::new(Vec::new());
        r.render(&scene, Revision(0), &query, ed, &mut out as &mut dyn Canvas)
            .unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("line25"), "{s}");
        assert!(!s.contains("line0"), "{s}");
    }

    #[test]
    fn viewport_commands_resolve_half_and_full_page_from_layout_height() {
        let (scene, _editor) = editor_scene(40, 5, ViewId(0), ViewId(1));
        let mut renderer = SceneRenderer::new();
        renderer.viewports.insert(
            ViewId(0),
            Viewport {
                top_row: 10,
                left_col: 0,
            },
        );

        let half = renderer.execute_viewport_command(
            &scene,
            Revision(0),
            ViewId(0),
            ViewportCommand::new(
                ViewportMoveDirection::Up,
                ViewportMoveAmount::HalfPage,
                crate::protocol::viewport::ViewportCursorBehavior::Move,
            ),
        );
        let full = renderer.execute_viewport_command(
            &scene,
            Revision(0),
            ViewId(0),
            ViewportCommand::new(
                ViewportMoveDirection::Down,
                ViewportMoveAmount::FullPage,
                crate::protocol::viewport::ViewportCursorBehavior::Move,
            ),
        );

        assert_eq!(half, 2);
        assert_eq!(full, 4);
        assert_eq!(renderer.viewports[&ViewId(0)].top_row, 12);
    }

    #[test]
    fn renders_non_empty_selection_with_reverse() {
        let (scene, ed) = editor_scene(40, 5, ViewId(0), ViewId(1));
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["hello".to_string()],
            selections: Selections::single(Selection {
                anchor: TextOffset { char_index: 1 },
                head: TextOffset { char_index: 4 },
            }),
        };
        let mut r = SceneRenderer::new();
        let mut out = Output::new(Vec::new());
        r.render(&scene, Revision(0), &query, ed, &mut out as &mut dyn Canvas)
            .unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("\x1b[7m"), "should contain reverse-on: {s}");
        assert!(s.contains("\x1b[27m"), "should contain reverse-off: {s}");
    }

    #[test]
    fn renders_collapsed_selection_without_reverse() {
        let (scene, ed) = editor_scene(40, 5, ViewId(0), ViewId(1));
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["hello".to_string()],
            selections: Selections::single(Selection::collapsed(TextOffset::origin())),
        };
        let mut r = SceneRenderer::new();
        let mut out = Output::new(Vec::new());
        r.render(&scene, Revision(0), &query, ed, &mut out as &mut dyn Canvas)
            .unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(!s.contains("\x1b[7m"), "collapsed should not reverse: {s}");
    }

    #[test]
    fn line_selection_highlights_collapsed_cursor_row_across_pane_width() {
        let (scene, editor) = editor_scene(8, 2, ViewId(0), ViewId(1));
        let query = MultiSpaceQuery {
            lines: vec!["hello".to_string()],
            selections: HashMap::from([(
                ViewId(0),
                text_view_with_shape(
                    ContentId(0),
                    Selections::single(Selection::collapsed(TextOffset::origin())),
                    CursorStyle::Block,
                    SelectionShape::Line,
                ),
            )]),
        };
        let mut renderer = SceneRenderer::new();
        let mut out = Output::new(Vec::new());

        renderer
            .render(
                &scene,
                Revision(0),
                &query,
                editor,
                &mut out as &mut dyn Canvas,
            )
            .unwrap();

        let output = String::from_utf8(out.into_inner()).unwrap();
        assert!(
            output.contains("\x1b[7m        \x1b[27m"),
            "output: {output}"
        );
        assert!(output.contains("\x1b[7mhello\x1b[27m"), "output: {output}");
    }

    #[test]
    fn renders_multiline_selection_reverse_spans_lines() {
        let (scene, ed) = editor_scene(40, 5, ViewId(0), ViewId(1));
        // "hello\nworld"：row0 col2 = idx2；row1 col2 = idx8
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["hello".to_string(), "world".to_string()],
            selections: Selections::single(Selection {
                anchor: TextOffset { char_index: 2 },
                head: TextOffset { char_index: 8 },
            }),
        };
        let mut r = SceneRenderer::new();
        let mut out = Output::new(Vec::new());
        r.render(&scene, Revision(0), &query, ed, &mut out as &mut dyn Canvas)
            .unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        let count = s.matches("\x1b[7m").count();
        assert!(
            count >= 2,
            "multiline should have >=2 reverse segments, got {count}: {s}"
        );
    }

    #[test]
    fn selection_clipped_to_viewport_does_not_draw_invisible_rows() {
        let (scene, ed) = editor_scene(40, 5, ViewId(0), ViewId(1));
        let lines: Vec<String> = (0..30).map(|i| format!("line{i}")).collect();
        // 第一次：cursor row 25 → viewport top_row=21
        let q1 = StubQuery {
            editor_cid: ContentId(0),
            lines: lines.clone(),
            selections: Selections::single(Selection::collapsed(TextOffset { char_index: 0 })),
        };
        let mut r = SceneRenderer::new();
        let mut out = Output::new(Vec::new());
        r.render(&scene, Revision(0), &q1, ed, &mut out as &mut dyn Canvas)
            .unwrap();
        // 第二次：selection 跨 row 0-25，head 在 row 25 维持 viewport（top_row=21）
        // line0..line29 每行 5 chars + \n = 6 chars；row25 col0 → char_index=150
        let q2 = StubQuery {
            editor_cid: ContentId(0),
            lines,
            selections: Selections::single(Selection {
                anchor: TextOffset { char_index: 1 },
                head: TextOffset { char_index: 150 },
            }),
        };
        let mut out2 = Output::new(Vec::new());
        r.render(&scene, Revision(0), &q2, ed, &mut out2 as &mut dyn Canvas)
            .unwrap();
        let s = String::from_utf8(out2.into_inner()).unwrap();
        assert!(
            !s.contains("line0"),
            "invisible row should not be drawn: {s}"
        );
        assert!(
            s.contains("\x1b[7m"),
            "visible middle rows should reverse: {s}"
        );
    }

    #[test]
    fn viewport_follows_cursor_right_and_clips_long_line() {
        let (scene, editor) = editor_scene(5, 2, ViewId(0), ViewId(1));
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["abcdefgh".to_string()],
            selections: Selections::single(Selection::collapsed(TextOffset { char_index: 7 })),
        };
        let mut renderer = SceneRenderer::new();
        let mut out = Output::new(Vec::new());

        renderer
            .render(
                &scene,
                Revision(0),
                &query,
                editor,
                &mut out as &mut dyn Canvas,
            )
            .unwrap();

        let output = String::from_utf8(out.into_inner()).unwrap();
        assert!(output.contains("defgh"), "output: {output}");
        assert!(!output.contains("abc"), "output: {output}");
        assert!(
            output.contains("1;5H"),
            "cursor should be at column 4: {output}"
        );
    }

    #[test]
    fn wide_unicode_uses_terminal_cell_columns() {
        let (scene, editor) = editor_scene(5, 2, ViewId(0), ViewId(1));
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["中文a".to_string()],
            selections: Selections::single(Selection::collapsed(TextOffset { char_index: 2 })),
        };
        let mut renderer = SceneRenderer::new();
        let mut out = Output::new(Vec::new());

        renderer
            .render(
                &scene,
                Revision(0),
                &query,
                editor,
                &mut out as &mut dyn Canvas,
            )
            .unwrap();

        let output = String::from_utf8(out.into_inner()).unwrap();
        assert!(output.contains("中文a"), "output: {output}");
        assert!(
            output.contains("1;5H"),
            "cursor after two wide characters should be at cell 4: {output}"
        );
    }

    #[test]
    fn wide_unicode_clip_preserves_terminal_cell_alignment() {
        let (scene, editor) = editor_scene(4, 2, ViewId(0), ViewId(1));
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["a中文".to_string()],
            selections: Selections::single(Selection::collapsed(TextOffset { char_index: 3 })),
        };
        let mut renderer = SceneRenderer::new();
        let mut out = Output::new(Vec::new());

        renderer
            .render(
                &scene,
                Revision(0),
                &query,
                editor,
                &mut out as &mut dyn Canvas,
            )
            .unwrap();

        let output = String::from_utf8(out.into_inner()).unwrap();
        assert!(
            output.contains(" 文"),
            "a clipped half of a wide character should remain one blank cell: {output}"
        );
        assert!(
            output.contains("1;4H"),
            "cursor should stay aligned after clipping a wide character: {output}"
        );
    }

    #[test]
    fn selection_highlight_uses_logical_columns_with_wide_unicode() {
        let mut out = Output::new(Vec::new());

        paint_line_with_highlight(&mut out, "中文a", 0, 5, Some((1, 2))).unwrap();

        let output = String::from_utf8(out.into_inner()).unwrap();
        assert_eq!(output, "中\x1b[7m文\x1b[27ma");
    }

    #[test]
    fn horizontal_viewport_moves_back_when_cursor_returns_left() {
        let (scene, editor) = editor_scene(5, 2, ViewId(0), ViewId(1));
        let mut renderer = SceneRenderer::new();
        let right_query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["abcdefgh".to_string()],
            selections: Selections::single(Selection::collapsed(TextOffset { char_index: 7 })),
        };
        let mut first = Output::new(Vec::new());
        renderer
            .render(
                &scene,
                Revision(0),
                &right_query,
                editor,
                &mut first as &mut dyn Canvas,
            )
            .unwrap();

        let left_query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["abcdefgh".to_string()],
            selections: Selections::single(Selection::collapsed(TextOffset { char_index: 1 })),
        };
        let mut second = Output::new(Vec::new());
        renderer
            .render(
                &scene,
                Revision(0),
                &left_query,
                editor,
                &mut second as &mut dyn Canvas,
            )
            .unwrap();

        let output = String::from_utf8(second.into_inner()).unwrap();
        assert!(output.contains("bcdef"), "output: {output}");
        assert!(!output.contains("abcdef"), "output: {output}");
        assert!(
            output.contains("1;1H"),
            "cursor should be at column 0: {output}"
        );
    }

    #[test]
    fn long_row_is_clipped_without_emitting_its_newline() {
        let (scene, editor) = editor_scene(5, 2, ViewId(0), ViewId(1));
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["abcdefgh\n".to_string()],
            selections: Selections::single(Selection::collapsed(TextOffset::origin())),
        };
        let mut renderer = SceneRenderer::new();
        let mut out = Output::new(Vec::new());

        renderer
            .render(
                &scene,
                Revision(0),
                &query,
                editor,
                &mut out as &mut dyn Canvas,
            )
            .unwrap();

        let output = String::from_utf8(out.into_inner()).unwrap();
        assert!(output.contains("abcde"), "output: {output}");
        assert!(!output.contains("abcdef"), "output: {output}");
        assert!(!output.contains('\n'), "output: {output:?}");
    }

    #[test]
    fn selection_highlight_is_clipped_to_horizontal_viewport() {
        let (scene, editor) = editor_scene(5, 2, ViewId(0), ViewId(1));
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["abcdefgh".to_string()],
            selections: Selections::single(Selection {
                anchor: TextOffset { char_index: 1 },
                head: TextOffset { char_index: 7 },
            }),
        };
        let mut renderer = SceneRenderer::new();
        let mut out = Output::new(Vec::new());

        renderer
            .render(
                &scene,
                Revision(0),
                &query,
                editor,
                &mut out as &mut dyn Canvas,
            )
            .unwrap();

        let output = String::from_utf8(out.into_inner()).unwrap();
        assert!(output.contains("\x1b[7mdefg\x1b[27mh"), "output: {output}");
        assert!(!output.contains("\x1b[7mabc"), "output: {output}");
    }

    #[test]
    fn document_control_characters_are_sanitized_before_terminal_output() {
        let mut out = Output::new(Vec::new());

        paint_line_with_highlight(&mut out, "safe\x1b]52;payload\u{0007}", 0, 40, None).unwrap();

        let output = String::from_utf8(out.into_inner()).unwrap();
        assert_eq!(output, "safe�]52;payload�");
        assert!(!output.contains('\x1b'));
        assert!(!output.contains('\u{0007}'));
    }

    #[test]
    fn status_bar_output_is_clipped_to_its_rect_width() {
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec![String::new()],
            selections: Selections::single(Selection::collapsed(TextOffset::origin())),
        };
        let item = RenderItem {
            space_id: SpaceId(1),
            view_id: ViewId(1),
            rect: crate::protocol::geometry::Rect {
                x: 0,
                y: 0,
                width: 3,
                height: 1,
            },
            clip: None,
            layer: crate::protocol::space::Layer::Base,
            z_index: 0,
            order: 0,
        };
        let mut out = Output::new(Vec::new());

        paint_status_bar(&item, &query, ContentId(1), &mut out).unwrap();

        let output = String::from_utf8(out.into_inner()).unwrap();
        assert!(output.ends_with("f.t"), "output: {output}");
        assert!(!output.contains("f.txt"), "output: {output}");
    }
}
