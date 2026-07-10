//! 前端核心：layout（TaffyEngine）+ viewport 跟随 + pull 可见行 + paint 到 Canvas。
//! TuiFrontend 经此渲染；单元测试用 StubQuery + Output<Vec<u8>> 断言 VT 字节。

use std::collections::HashMap;
use std::io;

use crate::protocol::content_query::{ContentQuery, RowRange};
use crate::protocol::ids::SpaceId;
use crate::protocol::scene::Scene;
use crate::protocol::status::StatusMessage;
use crate::protocol::viewport::Viewport;
use crate::terminal::output::Canvas;
use crate::tui::resolved::{RenderItem, ResolvedScene};
use crate::tui::taffy_engine::TaffyEngine;

pub struct SceneRenderer {
    engine: TaffyEngine,
    viewports: HashMap<SpaceId, Viewport>,
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
        query: &dyn ContentQuery,
        focused: SpaceId,
        canvas: &mut dyn Canvas,
    ) -> io::Result<()> {
        let resolved: ResolvedScene = self.engine.layout(scene);
        canvas.hide_cursor()?;
        // 焦点 viewport 跟随
        let focused_item = resolved.items.iter().find(|item| item.space_id == focused);
        let focused_head = query.selections(focused).primary().head();
        if let Some(item) = focused_item {
            let viewport = self
                .viewports
                .entry(focused)
                .or_insert_with(Viewport::origin);
            viewport.ensure_cursor_visible(focused_head.row, item.rect.height as usize);
        }
        // 逐 Content item paint
        for item in &resolved.items {
            paint_item(item, query, &self.viewports, canvas)?;
        }
        // 焦点光标定位
        if let Some(item) = focused_item {
            let vp = self
                .viewports
                .get(&focused)
                .copied()
                .unwrap_or_else(Viewport::origin);
            let screen_row = focused_head.row.saturating_sub(vp.top_row) + item.rect.y as usize;
            let screen_col = focused_head.col.saturating_sub(vp.left_col) + item.rect.x as usize;
            canvas.move_cursor(screen_row, screen_col)?;
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

fn paint_item(
    item: &RenderItem,
    query: &dyn ContentQuery,
    viewports: &HashMap<SpaceId, Viewport>,
    canvas: &mut dyn Canvas,
) -> io::Result<()> {
    let sid = item.space_id;
    let vp = viewports
        .get(&sid)
        .copied()
        .unwrap_or_else(Viewport::origin);
    let line_count = query.line_count(item.content_id);
    if line_count > 0 {
        // editor：拉可见行
        let height = item.rect.height as usize;
        let start = vp.top_row;
        let lines = query.lines(
            item.content_id,
            RowRange {
                start,
                end: start + height,
            },
        );
        // 选区高亮：primary 非空时算 [start,end] 端点（按 char_index 排序）
        let sels = query.selections(sid);
        let prim = sels.primary();
        let non_empty = prim.anchor != prim.head;
        let (sel_start, sel_end) = if non_empty {
            if prim.anchor.char_index <= prim.head.char_index {
                (prim.anchor, prim.head)
            } else {
                (prim.head, prim.anchor)
            }
        } else {
            (prim.anchor, prim.head) // collapsed：不会触发高亮
        };
        for (row, line) in lines.iter().enumerate() {
            let buf_row = start + row;
            let screen_row = (item.rect.y + row as i32) as usize;
            canvas.move_cursor(screen_row, item.rect.x as usize)?;
            canvas.clear_line()?;
            let hi = if non_empty && buf_row >= sel_start.row && buf_row <= sel_end.row {
                let hs = if buf_row == sel_start.row {
                    sel_start.col
                } else {
                    0
                };
                let he = if buf_row == sel_end.row {
                    sel_end.col
                } else {
                    usize::MAX
                };
                Some((hs, he))
            } else {
                None
            };
            paint_line_with_highlight(canvas, line, hi)?;
        }
        for row in lines.len()..height {
            let screen_row = (item.rect.y + row as i32) as usize;
            canvas.move_cursor(screen_row, item.rect.x as usize)?;
            canvas.clear_line()?;
        }
    } else {
        // status_bar
        let data = query.status_bar(item.content_id);
        let screen_row = item.rect.y as usize;
        canvas.move_cursor(screen_row, item.rect.x as usize)?;
        canvas.clear_line()?;
        canvas.write_str(&status_line(
            data.file_name.as_deref(),
            data.modified,
            &data.message,
        ))?;
    }
    Ok(())
}

/// 画一行文本，可选反白高亮区间 [hi_start_col, hi_end_col)（按 char 列，end 用 usize::MAX 表示到行尾）。
/// hi=None 时整行正常画。行尾换行符（若有）始终正常画，不参与反白。
fn paint_line_with_highlight(
    canvas: &mut dyn Canvas,
    line: &str,
    hi: Option<(usize, usize)>,
) -> io::Result<()> {
    let (content, tail) = match line.strip_suffix('\n') {
        Some(c) => (c, "\n"),
        None => (line, ""),
    };
    // char 边界（byte offset, char），用于按列切 byte 范围
    let bounds: Vec<(usize, char)> = content.char_indices().collect();
    let content_len = bounds.len();
    let write_seg =
        |canvas: &mut dyn Canvas, from: usize, to: usize, reverse: bool| -> io::Result<()> {
            if to <= from {
                return Ok(());
            }
            let from = from.min(content_len);
            let to = to.min(content_len);
            if to <= from {
                return Ok(());
            }
            let start_byte = bounds[from].0;
            let end_byte = if to >= content_len {
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
    match hi {
        None => {
            canvas.write_str(content)?;
        }
        Some((hs, he)) => {
            write_seg(canvas, 0, hs, false)?;
            write_seg(canvas, hs, he, true)?;
            write_seg(canvas, he, content_len, false)?;
        }
    }
    canvas.write_str(tail)?;
    Ok(())
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
    use crate::protocol::content_query::{ContentQuery, RowRange, StatusBarData};
    use crate::protocol::geometry::Size;
    use crate::protocol::ids::{ContentId, SpaceId};
    use crate::protocol::scene::{SceneBuilder, build_editor_scene};
    use crate::protocol::selection::{CursorPos, Selection, Selections};
    use crate::protocol::space::{Align, Arrangement, Axis};
    use crate::protocol::status::StatusMessage;
    use crate::terminal::output::Output;
    use std::collections::HashMap;

    struct StubQuery {
        editor_cid: ContentId,
        lines: Vec<String>,
        selections: Selections,
    }
    impl ContentQuery for StubQuery {
        fn lines(&self, cid: ContentId, range: RowRange) -> Vec<String> {
            assert_eq!(cid, self.editor_cid, "only editor content has lines");
            self.lines
                .iter()
                .skip(range.start)
                .take(range.end.saturating_sub(range.start))
                .cloned()
                .collect()
        }
        fn status_bar(&self, _cid: ContentId) -> StatusBarData {
            StatusBarData {
                file_name: Some("f.txt".to_string()),
                modified: false,
                message: StatusMessage::None,
            }
        }
        fn selections(&self, _sid: SpaceId) -> Selections {
            self.selections.clone()
        }
        fn line_count(&self, cid: ContentId) -> usize {
            if cid == self.editor_cid {
                self.lines.len()
            } else {
                0
            }
        }
    }

    struct MultiSpaceQuery {
        lines: Vec<String>,
        selections: HashMap<SpaceId, Selections>,
    }

    impl ContentQuery for MultiSpaceQuery {
        fn lines(&self, cid: ContentId, range: RowRange) -> Vec<String> {
            assert_eq!(cid, ContentId(0));
            self.lines
                .iter()
                .skip(range.start)
                .take(range.end.saturating_sub(range.start))
                .cloned()
                .collect()
        }

        fn status_bar(&self, _cid: ContentId) -> StatusBarData {
            StatusBarData {
                file_name: None,
                modified: false,
                message: StatusMessage::None,
            }
        }

        fn selections(&self, sid: SpaceId) -> Selections {
            self.selections[&sid].clone()
        }

        fn line_count(&self, cid: ContentId) -> usize {
            if cid == ContentId(0) {
                self.lines.len()
            } else {
                0
            }
        }
    }

    #[test]
    fn shared_content_spaces_use_their_own_selections() {
        let mut builder = SceneBuilder::new();
        let left = builder.content_grow(ContentId(0), 1);
        let right = builder.content_grow(ContentId(0), 1);
        let root = builder.container_grow(
            Arrangement::Flex {
                direction: Axis::Horizontal,
                gap: 0,
                align: Align::Stretch,
            },
            vec![left, right],
            1,
        );
        let scene = builder
            .snapshot(
                root,
                Size {
                    width: 20,
                    height: 1,
                },
            )
            .unwrap();
        let query = MultiSpaceQuery {
            lines: vec!["abcd".to_string()],
            selections: HashMap::from([
                (
                    left,
                    Selections::single(Selection {
                        anchor: CursorPos {
                            char_index: 0,
                            row: 0,
                            col: 0,
                        },
                        head: CursorPos {
                            char_index: 1,
                            row: 0,
                            col: 1,
                        },
                    }),
                ),
                (
                    right,
                    Selections::single(Selection {
                        anchor: CursorPos {
                            char_index: 2,
                            row: 0,
                            col: 2,
                        },
                        head: CursorPos {
                            char_index: 3,
                            row: 0,
                            col: 3,
                        },
                    }),
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
    fn renders_editor_lines_and_status() {
        let mut builder = SceneBuilder::new();
        let (scene, ed) =
            build_editor_scene(&mut builder, 40, 5, ContentId(0), ContentId(1)).unwrap();
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["hello".to_string(), "world".to_string()],
            selections: Selections::single(Selection::collapsed(CursorPos::origin())),
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
        let (scene, ed) =
            build_editor_scene(&mut builder, 40, 5, ContentId(0), ContentId(1)).unwrap();
        let lines: Vec<String> = (0..30).map(|i| format!("line{i}")).collect();
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines,
            selections: Selections::single(Selection::collapsed(CursorPos {
                char_index: 0,
                row: 25,
                col: 0,
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
        let (scene, ed) =
            build_editor_scene(&mut builder, 40, 5, ContentId(0), ContentId(1)).unwrap();
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["hello".to_string()],
            selections: Selections::single(Selection {
                anchor: CursorPos {
                    char_index: 1,
                    row: 0,
                    col: 1,
                },
                head: CursorPos {
                    char_index: 4,
                    row: 0,
                    col: 4,
                },
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
        let (scene, ed) =
            build_editor_scene(&mut builder, 40, 5, ContentId(0), ContentId(1)).unwrap();
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["hello".to_string()],
            selections: Selections::single(Selection::collapsed(CursorPos::origin())),
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
        let (scene, ed) =
            build_editor_scene(&mut builder, 40, 5, ContentId(0), ContentId(1)).unwrap();
        // "hello\nworld"：row0 col2 = idx2；row1 col2 = idx8
        let query = StubQuery {
            editor_cid: ContentId(0),
            lines: vec!["hello".to_string(), "world".to_string()],
            selections: Selections::single(Selection {
                anchor: CursorPos {
                    char_index: 2,
                    row: 0,
                    col: 2,
                },
                head: CursorPos {
                    char_index: 8,
                    row: 1,
                    col: 2,
                },
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
        let (scene, ed) =
            build_editor_scene(&mut builder, 40, 5, ContentId(0), ContentId(1)).unwrap();
        let lines: Vec<String> = (0..30).map(|i| format!("line{i}")).collect();
        // 第一次：cursor row 25 → viewport top_row=21
        let q1 = StubQuery {
            editor_cid: ContentId(0),
            lines: lines.clone(),
            selections: Selections::single(Selection::collapsed(CursorPos {
                char_index: 0,
                row: 25,
                col: 0,
            })),
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
                anchor: CursorPos {
                    char_index: 1,
                    row: 0,
                    col: 1,
                },
                head: CursorPos {
                    char_index: 150,
                    row: 25,
                    col: 0,
                },
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
}
