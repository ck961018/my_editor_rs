use crate::core::buffer::Buffer;
use crate::core::command::EditCommand;
use crate::protocol::selection::Selections;

/// 执行文本编辑命令。全局命令由 App 分流；多 selection 在 Buffer 内统一处理。
pub(crate) fn apply_edit(command: EditCommand, buffer: &mut Buffer, selections: &mut Selections) {
    match command {
        // Left/Right：非空 selection 收缩到 min/max，collapsed 时移动 head。
        EditCommand::MoveLeftBy(n) => {
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
        EditCommand::MoveRightBy(n) => {
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
        EditCommand::MoveWithinLineLeftBy(n) => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index < sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_within_line_left(sel, n);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveWithinLineRightBy(n) => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index > sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_within_line_right(sel, n);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        // Up/Down：移动 head 后 collapse，取消 selection 并继续垂直移动。
        EditCommand::MoveUpBy(n) => {
            for sel in selections.all_mut() {
                buffer.move_head_up(sel, n);
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveDownBy(n) => {
            for sel in selections.all_mut() {
                buffer.move_head_down(sel, n);
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveToLine { line_index } => {
            for sel in selections.all_mut() {
                buffer.move_head_to_line(sel, line_index);
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveToChar {
            target,
            direction,
            occurrence,
        } => {
            for sel in selections.all_mut() {
                if buffer.move_head_to_char(sel, target, direction, occurrence) {
                    Buffer::collapse_to_head(sel);
                }
            }
        }
        EditCommand::MoveWordForward => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index < sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_word_forward(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveWordBackward => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index > sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_word_backward(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveWordEnd => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index < sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_word_end(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveToLineStart => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index < sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_to_line_start(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveToFirstNonBlank => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index < sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_to_first_non_blank(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveToLineEnd => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index > sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_to_line_end(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveAfterLineEnd => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index > sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_after_line_end(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveToLastLine => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index > sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_to_last_line(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveToPrevParagraph => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index < sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_to_prev_paragraph(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveToNextParagraph => {
            for sel in selections.all_mut() {
                if sel.anchor != sel.head {
                    sel.head = if sel.anchor.char_index > sel.head.char_index {
                        sel.anchor
                    } else {
                        sel.head
                    };
                } else {
                    buffer.move_head_to_next_paragraph(sel);
                }
                Buffer::collapse_to_head(sel);
            }
        }
        EditCommand::MoveBy { chars, lines } => {
            for sel in selections.all_mut() {
                buffer.move_head_by(sel, chars, lines);
                Buffer::collapse_to_head(sel);
            }
        }
        // Extend：只移动 head，不改变 anchor，也不 collapse。
        EditCommand::ExtendLeftBy(n) => {
            for sel in selections.all_mut() {
                buffer.move_head_left(sel, n);
            }
        }
        EditCommand::ExtendRightBy(n) => {
            for sel in selections.all_mut() {
                buffer.move_head_right(sel, n);
            }
        }
        EditCommand::ExtendUpBy(n) => {
            for sel in selections.all_mut() {
                buffer.move_head_up(sel, n);
            }
        }
        EditCommand::ExtendDownBy(n) => {
            for sel in selections.all_mut() {
                buffer.move_head_down(sel, n);
            }
        }
        // Escape：collapse 到 head，并仅保留 primary selection。
        EditCommand::CollapseSelections => {
            for sel in selections.all_mut() {
                Buffer::collapse_to_head(sel);
            }
            selections.retain_primary();
        }
        EditCommand::MoveTo { char_idx, line_idx } => {
            buffer.set_head(selections.primary_mut(), char_idx, line_idx);
            Buffer::collapse_to_head(selections.primary_mut());
            selections.retain_primary();
        }
        EditCommand::InsertText(text) => buffer.insert_at_selections(selections, &text),
        EditCommand::Delete(n) => buffer.delete_at_selections(selections, n),
        EditCommand::DeleteLines { lines } => buffer.delete_lines_at_selections(selections, lines),
        EditCommand::DeleteWordBackward => buffer.delete_word_backward_at_selections(selections),
        EditCommand::DeleteToLineStart => {
            buffer.delete_to_line_start_at_selections(selections);
        }
        EditCommand::DeleteToLineEnd => {
            buffer.delete_to_line_end_at_selections(selections);
        }
        EditCommand::JoinLines => {
            buffer.join_lines_at_selections(selections);
        }
        EditCommand::ToggleCase => {
            buffer.toggle_case_at_selections(selections);
        }
        EditCommand::InsertNewLineBelow => {
            buffer.insert_new_line_below_at_selections(selections);
        }
        EditCommand::InsertNewLineAbove => {
            buffer.insert_new_line_above_at_selections(selections);
        }
        EditCommand::DeleteLineContent => {
            buffer.delete_line_content_at_selections(selections);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::buffer::Buffer;
    use crate::core::command::EditCommand;
    use crate::protocol::selection::{Selection, Selections, TextOffset};

    fn single_sel(at: TextOffset) -> Selections {
        Selections::single(Selection::collapsed(at))
    }

    #[test]
    fn insert_text_changes_buffer_and_selection() {
        let mut buf = Buffer::new();
        let mut s = single_sel(TextOffset::origin());
        apply_edit(EditCommand::InsertText("hi".to_string()), &mut buf, &mut s);
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
            let mut c = TextOffset::origin();
            c.char_index = 2;
            buf.clamp_offset(&mut c);
            single_sel(c)
        };
        apply_edit(EditCommand::Delete(-1), &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "a");
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn delete_word_backward_dispatches_to_buffer() {
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(&mut single_sel(TextOffset::origin()), "alpha beta");
        let mut selections = single_sel({
            let mut cursor = TextOffset::origin();
            cursor.char_index = 10;
            buffer.clamp_offset(&mut cursor);
            cursor
        });

        apply_edit(
            EditCommand::DeleteWordBackward,
            &mut buffer,
            &mut selections,
        );

        assert_eq!(buffer.slice().to_string(), "alpha ");
        assert_eq!(selections.primary().head().char_index, 6);
    }

    #[test]
    fn move_right_advances_head() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        let mut s = single_sel(TextOffset::origin());
        apply_edit(EditCommand::MoveRightBy(1), &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_to_retains_primary_clears_secondaries() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        let mut s = Selections::from_parts(
            vec![
                Selection::collapsed(TextOffset::origin()),
                Selection::collapsed(TextOffset::origin()),
            ],
            0,
        );
        apply_edit(
            EditCommand::MoveTo {
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
        let mut a = TextOffset::origin();
        a.char_index = anchor_idx;
        buf.clamp_offset(&mut a);
        let mut h = a;
        h.char_index = head_idx;
        buf.clamp_offset(&mut h);
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
        apply_edit(EditCommand::MoveLeftBy(1), &mut buf, &mut s);
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
        apply_edit(EditCommand::MoveLeftBy(1), &mut buf, &mut s);
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
        apply_edit(EditCommand::MoveRightBy(1), &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 3);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_left_on_collapsed_moves_head() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        buf.insert_char(1, 'b');
        let mut s = non_empty_sel(2, 2, &buf);
        apply_edit(EditCommand::MoveLeftBy(1), &mut buf, &mut s);
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
        apply_edit(EditCommand::ExtendLeftBy(1), &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.primary().anchor.char_index, 2);
        assert!(s.primary().anchor != s.primary().head());
    }

    #[test]
    fn collapse_selections_collapses_and_retains_primary() {
        let mut buf = Buffer::new();
        buf.insert_char(0, 'a');
        let mut s = non_empty_sel(0, 1, &buf);
        apply_edit(EditCommand::CollapseSelections, &mut buf, &mut s);
        assert_eq!(s.primary().anchor, s.primary().head());
        assert_eq!(s.primary().head().char_index, 1);
        assert_eq!(s.all().count(), 1);
    }

    #[test]
    fn insert_on_non_empty_replaces_range() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(
            &mut Selections::single(Selection::collapsed(TextOffset::origin())),
            "hello",
        );
        let mut s = non_empty_sel(1, 4, &buf);
        apply_edit(EditCommand::InsertText("XY".to_string()), &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "hXYo");
        assert_eq!(s.primary().head().char_index, 3);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_word_forward_advances_head() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(TextOffset::origin()), "foo bar");
        let mut s = single_sel({
            let mut c = TextOffset::origin();
            c.char_index = 0;
            buf.clamp_offset(&mut c);
            c
        });
        apply_edit(EditCommand::MoveWordForward, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 4);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_word_backward_advances_head() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(TextOffset::origin()), "foo bar");
        let mut s = single_sel({
            let mut c = TextOffset::origin();
            c.char_index = 7;
            buf.clamp_offset(&mut c);
            c
        });
        apply_edit(EditCommand::MoveWordBackward, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 4);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_word_end_advances_head() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(TextOffset::origin()), "foo.bar");
        let mut s = single_sel({
            let mut c = TextOffset::origin();
            c.char_index = 0;
            buf.clamp_offset(&mut c);
            c
        });
        apply_edit(EditCommand::MoveWordEnd, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 2);
        assert_eq!(s.primary().anchor, s.primary().head());
    }

    #[test]
    fn move_to_line_start_goes_to_column_zero() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(TextOffset::origin()), "  foo\n  bar");
        let mut s = single_sel({
            let mut c = TextOffset::origin();
            c.char_index = 7;
            buf.clamp_offset(&mut c);
            c
        });
        apply_edit(EditCommand::MoveToLineStart, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 6);
    }

    #[test]
    fn move_to_first_non_blank_skips_whitespace() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(TextOffset::origin()), "  foo");
        let mut s = single_sel(TextOffset::origin());
        apply_edit(EditCommand::MoveToFirstNonBlank, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 2);
    }

    #[test]
    fn move_to_line_end_lands_on_last_char() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(TextOffset::origin()), "foo\nbar");
        let mut s = single_sel(TextOffset::origin());
        apply_edit(EditCommand::MoveToLineEnd, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 2);
    }

    #[test]
    fn move_to_last_line_goes_to_last_line() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(TextOffset::origin()), "foo\nbar\nbaz");
        let mut s = single_sel(TextOffset::origin());
        apply_edit(EditCommand::MoveToLastLine, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 8);
    }

    #[test]
    fn move_to_prev_paragraph_jumps_to_empty_line() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(TextOffset::origin()), "foo\n\nbar");
        let mut s = single_sel({
            let mut c = TextOffset::origin();
            c.char_index = 5;
            buf.clamp_offset(&mut c);
            c
        });
        apply_edit(EditCommand::MoveToPrevParagraph, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 4);
    }

    #[test]
    fn move_to_next_paragraph_jumps_to_empty_line() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(TextOffset::origin()), "foo\n\nbar");
        let mut s = single_sel(TextOffset::origin());
        apply_edit(EditCommand::MoveToNextParagraph, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 4);
    }

    #[test]
    fn move_after_line_end_lands_after_last_char() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(TextOffset::origin()), "foo\n");
        let mut s = single_sel(TextOffset::origin());
        apply_edit(EditCommand::MoveAfterLineEnd, &mut buf, &mut s);
        assert_eq!(s.primary().head().char_index, 3);
    }

    #[test]
    fn delete_to_line_start_removes_text() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(TextOffset::origin()), "foo\nbar");
        let mut s = single_sel({
            let mut c = TextOffset::origin();
            c.char_index = 5;
            buf.clamp_offset(&mut c);
            c
        });
        apply_edit(EditCommand::DeleteToLineStart, &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "foo\nar");
        assert_eq!(s.primary().head().char_index, 4);
    }

    #[test]
    fn delete_to_line_end_removes_text() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(TextOffset::origin()), "foo\nbar");
        let mut s = single_sel({
            let mut c = TextOffset::origin();
            c.char_index = 1;
            buf.clamp_offset(&mut c);
            c
        });
        apply_edit(EditCommand::DeleteToLineEnd, &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "f\nbar");
        assert_eq!(s.primary().head().char_index, 1);
    }

    #[test]
    fn join_lines_merges_lines() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(TextOffset::origin()), "foo\nbar");
        let mut s = single_sel(TextOffset::origin());
        apply_edit(EditCommand::JoinLines, &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "foo bar");
    }

    #[test]
    fn toggle_case_flips_char() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(TextOffset::origin()), "aBc");
        let mut s = single_sel(TextOffset::origin());
        apply_edit(EditCommand::ToggleCase, &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "ABc");
        assert_eq!(s.primary().head().char_index, 1);
    }

    #[test]
    fn insert_new_line_below_adds_line() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(TextOffset::origin()), "foo");
        let mut s = single_sel(TextOffset::origin());
        apply_edit(EditCommand::InsertNewLineBelow, &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "foo\n");
        assert_eq!(s.primary().head().char_index, 4);
    }

    #[test]
    fn insert_new_line_above_adds_line() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(TextOffset::origin()), "foo");
        let mut s = single_sel(TextOffset::origin());
        apply_edit(EditCommand::InsertNewLineAbove, &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "\nfoo");
        assert_eq!(s.primary().head().char_index, 0);
    }

    #[test]
    fn delete_line_content_clears_line() {
        let mut buf = Buffer::new();
        buf.insert_at_selections(&mut single_sel(TextOffset::origin()), "foo\nbar");
        let mut s = single_sel({
            let mut c = TextOffset::origin();
            c.char_index = 1;
            buf.clamp_offset(&mut c);
            c
        });
        apply_edit(EditCommand::DeleteLineContent, &mut buf, &mut s);
        assert_eq!(buf.slice().to_string(), "\nbar");
        assert_eq!(s.primary().head().char_index, 0);
    }
}
