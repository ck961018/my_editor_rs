use crate::core::buffer::Buffer;
use crate::core::command::TextCommand;
use crate::protocol::selection::Selections;

/// 执行文本编辑命令。全局/多光标变体不进此处（App 分流）。
pub fn execute_text_command(
    command: TextCommand,
    buffer: &mut Buffer,
    selections: &mut Selections,
) {
    match command {
        // Left/Right 有端点语义：非空收缩到 min/max（不额外移），空则移动 head
        TextCommand::MoveLeftBy(n) => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index < sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_left(sel, n);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        TextCommand::MoveRightBy(n) => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index > sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_right(sel, n);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        // Up/Down 无端点语义：统一 move_head + collapse（取消并继续上下移）
        TextCommand::MoveUpBy(n) => {
            for sel in selections.all_mut() {
                buffer.move_head_up(sel, n);
                Buffer::collapse_to_head(sel);
            }
        }
        TextCommand::MoveDownBy(n) => {
            for sel in selections.all_mut() {
                buffer.move_head_down(sel, n);
                Buffer::collapse_to_head(sel);
            }
        }
        TextCommand::MoveBy { chars, lines } => {
            for sel in selections.all_mut() {
                buffer.move_head_by(sel, chars, lines);
                Buffer::collapse_to_head(sel);
            }
        }
        // Extend：只动 head 不碰 anchor，不 collapse（选区变非空）
        TextCommand::ExtendLeftBy(n) => {
            for sel in selections.all_mut() {
                buffer.move_head_left(sel, n);
            }
        }
        TextCommand::ExtendRightBy(n) => {
            for sel in selections.all_mut() {
                buffer.move_head_right(sel, n);
            }
        }
        TextCommand::ExtendUpBy(n) => {
            for sel in selections.all_mut() {
                buffer.move_head_up(sel, n);
            }
        }
        TextCommand::ExtendDownBy(n) => {
            for sel in selections.all_mut() {
                buffer.move_head_down(sel, n);
            }
        }
        // Escape：collapse to head + 仅留 primary
        TextCommand::CollapseSelections => {
            for sel in selections.all_mut() {
                Buffer::collapse_to_head(sel);
            }
            selections.retain_primary();
        }
        TextCommand::MoveTo { char_idx, line_idx } => {
            buffer.set_head(selections.primary_mut(), char_idx, line_idx);
            Buffer::collapse_to_head(selections.primary_mut());
            selections.retain_primary();
        }
        TextCommand::InsertText(text) => buffer.insert_at_selections(selections, &text),
        TextCommand::Delete(n) => buffer.delete_at_selections(selections, n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::buffer::Buffer;
    use crate::core::command::TextCommand;
    use crate::protocol::selection::{CursorPos, Selection, Selections};

    fn single_sel(at: CursorPos) -> Selections {
        Selections::single(Selection::collapsed(at))
    }

    #[test]
    fn insert_text_changes_buffer_and_selection() {
        let mut buf = Buffer::new();
        let mut s = single_sel(CursorPos::origin());
        execute_text_command(TextCommand::InsertText("hi".to_string()), &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "hi");
        assert_eq!(s.primary().head().char_index, 2);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn delete_left_removes_char() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        buf.insert_char(1, 'b');
        let mut s = {
            let mut c = CursorPos::origin();
            c.char_index = 2;
            buf.recompute_cursor(&mut c);
            single_sel(c)
        };
        execute_text_command(TextCommand::Delete(-1), &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "a");
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_right_advances_head() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        let mut s = single_sel(CursorPos::origin());
        execute_text_command(TextCommand::MoveRightBy(1), &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_to_retains_primary_clears_secondaries() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        let mut s = Selections::from_parts(
            vec![
                Selection::collapsed(CursorPos::origin()),
                Selection::collapsed(CursorPos::origin()),
            ],
            0,
        );
        execute_text_command(
            TextCommand::MoveTo {
                char_idx: 0,
                line_idx: 0,
            },
            &mut buf,
            &mut s,
        );
        assert_eq!(s.all().count(), 1);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    fn non_empty_sel(anchor_idx: usize, head_idx: usize, buf: &Buffer) -> Selections {
        let mut a = CursorPos::origin();
        a.char_index = anchor_idx;
        buf.recompute_cursor(&mut a);
        let mut h = a;
        h.char_index = head_idx;
        buf.recompute_cursor(&mut h);
        let sel = Selection { anchor: a, head: h };
        Selections::single(sel)
    }

    #[test]
    fn move_left_on_non_empty_shrinks_to_min() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        buf.insert_char(1, 'b');
        buf.insert_char(2, 'c');
        let mut s = non_empty_sel(1, 3, &buf);
        execute_text_command(TextCommand::MoveLeftBy(1), &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_left_on_backward_selection_shrinks_to_min() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        buf.insert_char(1, 'b');
        buf.insert_char(2, 'c');
        let mut s = non_empty_sel(3, 1, &buf);
        execute_text_command(TextCommand::MoveLeftBy(1), &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_right_on_non_empty_shrinks_to_max() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        buf.insert_char(1, 'b');
        buf.insert_char(2, 'c');
        let mut s = non_empty_sel(1, 3, &buf);
        execute_text_command(TextCommand::MoveRightBy(1), &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 3);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_left_on_collapsed_moves_head() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        buf.insert_char(1, 'b');
        let mut s = non_empty_sel(2, 2, &buf);
        execute_text_command(TextCommand::MoveLeftBy(1), &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn extend_left_moves_head_keeps_anchor() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        buf.insert_char(1, 'b');
        buf.insert_char(2, 'c');
        let mut s = non_empty_sel(2, 2, &buf);
        execute_text_command(TextCommand::ExtendLeftBy(1), &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.primary().anchor.char_index, 2);
        assert!(s.primary().anchor != s.primary().head());
    }

    #[test]
    fn collapse_selections_collapses_and_retains_primary() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        let mut s = non_empty_sel(0, 1, &buf);
        execute_text_command(TextCommand::CollapseSelections, &mut buf, &mut s);
        assert_eq!(s.primary().anchor, s.primary().head());
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.all().count(), 1);
    }

    #[test]
    fn insert_on_non_empty_replaces_range() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(
            &mut Selections::single(Selection::collapsed(CursorPos::origin())),
            "hello",
        );
        let mut s = non_empty_sel(1, 4, &buf);
        execute_text_command(TextCommand::InsertText("XY".to_string()), &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "hXYo");
        assert_eq!(s.primary().head().char_index, 3);
        assert_eq!(s.primary().anchor, s.primary().head());
    }
}
