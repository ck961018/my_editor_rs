//! 前端核心：layout（TaffyEngine）+ viewport 跟随 + pull 可见行 + paint 到 Canvas。
//! TuiFrontend 经此渲染；单元测试用 StubQuery + Output<Vec<u8>> 断言 VT 字节。

use std::collections::HashMap;
use std::io;

use crate::protocol::content_query::{
    ContentData, ContentQuery, RenderQuery, RowRange, TextPresentation, ViewData, ViewPresentation,
};
use crate::protocol::ids::{SpaceId, ViewId};
use crate::protocol::scene::Scene;
use crate::protocol::selection::{TextOffset, TextPoint};
use crate::protocol::status::StatusMessage;
use crate::protocol::viewport::Viewport;
use crate::terminal::output::Canvas;
use crate::tui::resolved::{RenderItem, ResolvedScene};
use crate::tui::taffy_engine::TaffyEngine;

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
        query: &dyn RenderQuery,
        focused: SpaceId,
        canvas: &mut dyn Canvas,
    ) -> io::Result<()> {
        let resolved: ResolvedScene = self.engine.layout(scene);
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
        if let (Some(item), Some(focused_head)) = (focused_item, focused_head) {
            let viewport = self
                .viewports
                .entry(item.view_id)
                .or_insert_with(Viewport::origin);
            follow_viewport(
                viewport,
                focused_head,
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
            let display = display_point(focused_head, item, vp);
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

fn follow_viewport(viewport: &mut Viewport, head: TextPoint, width: usize, height: usize) {
    viewport.ensure_cursor_visible(head.row, height);

    if width == 0 || head.col < viewport.left_col {
        viewport.left_col = head.col;
    } else if head.col >= viewport.left_col.saturating_add(width) {
        viewport.left_col = head.col - width + 1;
    }
}

fn display_point(point: TextPoint, item: &RenderItem, viewport: Viewport) -> DisplayPoint {
    DisplayPoint {
        row: point.row.saturating_sub(viewport.top_row) + item.rect.y as usize,
        col: point.col.saturating_sub(viewport.left_col) + item.rect.x as usize,
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
    let selection_offsets = (primary.anchor != primary.head).then_some({
        if primary.anchor.char_index <= primary.head.char_index {
            (primary.anchor, primary.head)
        } else {
            (primary.head, primary.anchor)
        }
    });
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
        canvas.move_cursor(screen_row, item.rect.x as usize)?;
        canvas.clear_line()?;
        let hi = selection.and_then(|(sel_start, sel_end)| {
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
        });
        paint_line_with_highlight(canvas, line, vp.left_col, width, hi)?;
    }
    for row in lines.len()..height {
        let screen_row = (item.rect.y + row as i32) as usize;
        canvas.move_cursor(screen_row, item.rect.x as usize)?;
        canvas.clear_line()?;
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
    canvas.move_cursor(item.rect.y as usize, item.rect.x as usize)?;
    canvas.clear_line()?;
    canvas.write_str(&status_line(
        data.file_name.as_deref(),
        data.modified,
        &data.message,
    ))
}

/// Paint the visible character interval `[left_col, left_col + width)` of one logical row.
/// A trailing logical newline is discarded. `hi`, when present, is an absolute logical-column
/// range and is clipped to the visible interval before reverse highlighting is emitted.
fn paint_line_with_highlight(
    canvas: &mut dyn Canvas,
    line: &str,
    left_col: usize,
    width: usize,
    hi: Option<(usize, usize)>,
) -> io::Result<()> {
    let content = line.strip_suffix('\n').unwrap_or(line);
    // char 边界（byte offset, char），用于按列切 byte 范围
    let bounds: Vec<(usize, char)> = content.char_indices().collect();
    let content_len = bounds.len();
    let visible_start = left_col.min(content_len);
    let visible_end = left_col.saturating_add(width).min(content_len);
    let write_segment =
        |canvas: &mut dyn Canvas, from: usize, to: usize, reverse: bool| -> io::Result<()> {
            if to <= from {
                return Ok(());
            }
            let start_byte = bounds[from].0;
            let end_byte = if to == content_len {
                content.len()
            } else {
                bounds[to].0
            };
            if reverse {
                canvas.set_reverse(true)?;
            }
            canvas.write_str(&content[start_byte..end_byte])?;
            if reverse {
                canvas.set_reverse(false)?;
            }
            Ok(())
        };
    let clipped_hi = hi.and_then(|(start, end)| {
        let start = start.max(visible_start);
        let end = end.min(visible_end);
        (start < end).then_some((start, end))
    });
    match clipped_hi {
        None => write_segment(canvas, visible_start, visible_end, false),
        Some((start, end)) => {
            write_segment(canvas, visible_start, start, false)?;
            write_segment(canvas, start, end, true)?;
            write_segment(canvas, end, visible_end, false)
        }
    }
}

fn status_line(file_name: Option<&str>, modified: bool, message: &StatusMessage) -> String {
    let name = file_name.unwrap_or("[No Name]");
    let modified = if modified { "[+]" } else { "" };
    let msg = match message {
        StatusMessage::None => "",
        StatusMessage::Saved => "Saved",
        StatusMessage::SaveFailed => "SaveFailed",
        StatusMessage::NewFile => "NewFile",
        StatusMessage::OpenFailed => "OpenFailed",
    };
    format!("{name} {modified}  {msg}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::content_query::{
        ContentData, ContentQuery, CursorStyle, RenderQuery, StatusBarData, TextPresentation,
        ViewData, ViewPresentation,
    };
    use crate::protocol::ids::{ContentId, ViewId};
    use crate::protocol::scene::{SceneBuilder, build_editor_scene};
    use crate::protocol::selection::{Selection, Selections, TextOffset};
    use crate::protocol::space::SplitDirection;
    use crate::protocol::status::StatusMessage;
    use crate::terminal::output::Output;
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
        ViewData {
            content,
            presentation: ViewPresentation::Text(TextPresentation {
                selections,
                cursor_style,
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
        let mut builder = SceneBuilder::new();
        let (mut scene, left) =
            build_editor_scene(&mut builder, 20, 2, ViewId(0), ViewId(1)).unwrap();
        let _right = builder
            .split(&mut scene, left, ViewId(2), true, SplitDirection::Right)
            .unwrap()
            .new_space;
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
            .render(&scene, &query, left, &mut out as &mut dyn Canvas)
            .unwrap();
        let output = String::from_utf8(out.into_inner()).unwrap();

        assert!(output.contains("\x1b[7ma\x1b[27mbcd"), "left: {output}");
        assert!(output.contains("ab\x1b[7mc\x1b[27md"), "right: {output}");
    }

    #[test]
    fn moving_a_view_to_another_space_preserves_its_viewport() {
        let mut builder = SceneBuilder::new();
        let (mut scene, left) =
            build_editor_scene(&mut builder, 20, 2, ViewId(0), ViewId(1)).unwrap();
        let right = builder
            .split(&mut scene, left, ViewId(2), true, SplitDirection::Right)
            .unwrap()
            .new_space;
        let saved_viewport = Viewport {
            top_row: 1,
            left_col: 0,
        };
        let mut renderer = SceneRenderer::new();
        renderer.viewports.insert(ViewId(0), saved_viewport);

        builder
            .replace_view(&mut scene, left, ViewId(3), true)
            .unwrap();
        builder
            .replace_view(&mut scene, right, ViewId(0), true)
            .unwrap();
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
            .render(&scene, &query, right, &mut out as &mut dyn Canvas)
            .unwrap();

        assert_eq!(renderer.viewports.get(&ViewId(0)), Some(&saved_viewport));
        assert!(
            String::from_utf8(out.into_inner())
                .unwrap()
                .contains("line1")
        );
    }

    #[test]
    fn focused_view_controls_terminal_cursor_style() {
        let mut builder = SceneBuilder::new();
        let (mut scene, left) =
            build_editor_scene(&mut builder, 20, 2, ViewId(0), ViewId(1)).unwrap();
        let right = builder
            .split(&mut scene, left, ViewId(2), true, SplitDirection::Right)
            .unwrap()
            .new_space;
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
            .render(&scene, &query, right, &mut right_out as &mut dyn Canvas)
            .unwrap();
        let right_output = String::from_utf8(right_out.into_inner()).unwrap();
        assert!(right_output.contains("\x1b[2 q"), "right: {right_output}");

        let mut left_out = Output::new(Vec::new());
        renderer
            .render(&scene, &query, left, &mut left_out as &mut dyn Canvas)
            .unwrap();
        let left_output = String::from_utf8(left_out.into_inner()).unwrap();
        assert!(left_output.contains("\x1b[0 q"), "left: {left_output}");
        assert!(!left_output.contains("\x1b[2 q"), "left: {left_output}");
    }

    #[test]
    fn renders_editor_lines_and_status() {
        let mut builder = SceneBuilder::new();
        let (scene, ed) = build_editor_scene(&mut builder, 40, 5, ViewId(0), ViewId(1)).unwrap();
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["hello".to_string(), "world".to_string()],
            selections: Selections::single(Selection::collapsed(TextOffset::origin())),
        };
        let mut r = SceneRenderer::new();
        let mut out = Output::new(Vec::new());
        r.render(&scene, &query, ed, &mut out as &mut dyn Canvas)
            .unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("hello"), "{s}");
        assert!(s.contains("f.txt"), "{s}");
    }

    #[test]
    fn viewport_follows_cursor_below() {
        let mut builder = SceneBuilder::new();
        let (scene, ed) = build_editor_scene(&mut builder, 40, 5, ViewId(0), ViewId(1)).unwrap();
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
        r.render(&scene, &query, ed, &mut out as &mut dyn Canvas)
            .unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("line25"), "{s}");
        assert!(!s.contains("line0"), "{s}");
    }

    #[test]
    fn renders_non_empty_selection_with_reverse() {
        let mut builder = SceneBuilder::new();
        let (scene, ed) = build_editor_scene(&mut builder, 40, 5, ViewId(0), ViewId(1)).unwrap();
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
        r.render(&scene, &query, ed, &mut out as &mut dyn Canvas)
            .unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(s.contains("\x1b[7m"), "should contain reverse-on: {s}");
        assert!(s.contains("\x1b[27m"), "should contain reverse-off: {s}");
    }

    #[test]
    fn renders_collapsed_selection_without_reverse() {
        let mut builder = SceneBuilder::new();
        let (scene, ed) = build_editor_scene(&mut builder, 40, 5, ViewId(0), ViewId(1)).unwrap();
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["hello".to_string()],
            selections: Selections::single(Selection::collapsed(TextOffset::origin())),
        };
        let mut r = SceneRenderer::new();
        let mut out = Output::new(Vec::new());
        r.render(&scene, &query, ed, &mut out as &mut dyn Canvas)
            .unwrap();
        let s = String::from_utf8(out.into_inner()).unwrap();
        assert!(!s.contains("\x1b[7m"), "collapsed should not reverse: {s}");
    }

    #[test]
    fn renders_multiline_selection_reverse_spans_lines() {
        let mut builder = SceneBuilder::new();
        let (scene, ed) = build_editor_scene(&mut builder, 40, 5, ViewId(0), ViewId(1)).unwrap();
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
        r.render(&scene, &query, ed, &mut out as &mut dyn Canvas)
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
        let mut builder = SceneBuilder::new();
        let (scene, ed) = build_editor_scene(&mut builder, 40, 5, ViewId(0), ViewId(1)).unwrap();
        let lines: Vec<String> = (0..30).map(|i| format!("line{i}")).collect();
        // 第一次：cursor row 25 → viewport top_row=21
        let q1 = StubQuery {
            editor_cid: ContentId(0),
            lines: lines.clone(),
            selections: Selections::single(Selection::collapsed(TextOffset { char_index: 0 })),
        };
        let mut r = SceneRenderer::new();
        let mut out = Output::new(Vec::new());
        r.render(&scene, &q1, ed, &mut out as &mut dyn Canvas)
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
        r.render(&scene, &q2, ed, &mut out2 as &mut dyn Canvas)
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
        let mut builder = SceneBuilder::new();
        let (scene, editor) = build_editor_scene(&mut builder, 5, 2, ViewId(0), ViewId(1)).unwrap();
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["abcdefgh".to_string()],
            selections: Selections::single(Selection::collapsed(TextOffset { char_index: 7 })),
        };
        let mut renderer = SceneRenderer::new();
        let mut out = Output::new(Vec::new());

        renderer
            .render(&scene, &query, editor, &mut out as &mut dyn Canvas)
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
    fn horizontal_viewport_moves_back_when_cursor_returns_left() {
        let mut builder = SceneBuilder::new();
        let (scene, editor) = build_editor_scene(&mut builder, 5, 2, ViewId(0), ViewId(1)).unwrap();
        let mut renderer = SceneRenderer::new();
        let right_query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["abcdefgh".to_string()],
            selections: Selections::single(Selection::collapsed(TextOffset { char_index: 7 })),
        };
        let mut first = Output::new(Vec::new());
        renderer
            .render(&scene, &right_query, editor, &mut first as &mut dyn Canvas)
            .unwrap();

        let left_query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["abcdefgh".to_string()],
            selections: Selections::single(Selection::collapsed(TextOffset { char_index: 1 })),
        };
        let mut second = Output::new(Vec::new());
        renderer
            .render(&scene, &left_query, editor, &mut second as &mut dyn Canvas)
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
        let mut builder = SceneBuilder::new();
        let (scene, editor) = build_editor_scene(&mut builder, 5, 2, ViewId(0), ViewId(1)).unwrap();
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["abcdefgh\n".to_string()],
            selections: Selections::single(Selection::collapsed(TextOffset::origin())),
        };
        let mut renderer = SceneRenderer::new();
        let mut out = Output::new(Vec::new());

        renderer
            .render(&scene, &query, editor, &mut out as &mut dyn Canvas)
            .unwrap();

        let output = String::from_utf8(out.into_inner()).unwrap();
        assert!(output.contains("abcde"), "output: {output}");
        assert!(!output.contains("abcdef"), "output: {output}");
        assert!(!output.contains('\n'), "output: {output:?}");
    }

    #[test]
    fn selection_highlight_is_clipped_to_horizontal_viewport() {
        let mut builder = SceneBuilder::new();
        let (scene, editor) = build_editor_scene(&mut builder, 5, 2, ViewId(0), ViewId(1)).unwrap();
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
            .render(&scene, &query, editor, &mut out as &mut dyn Canvas)
            .unwrap();

        let output = String::from_utf8(out.into_inner()).unwrap();
        assert!(output.contains("\x1b[7mdefg\x1b[27mh"), "output: {output}");
        assert!(!output.contains("\x1b[7mabc"), "output: {output}");
    }
}
