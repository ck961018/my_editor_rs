/// 视口滚动位置。top_row 是逻辑行，left_col 是终端显示 cell 列。
/// 尺寸不存（从 layout 给的 rect 拿），消除「预留状态栏行」越权。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub top_row: usize,
    pub left_col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewportMoveAmount {
    HalfPage,
    FullPage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewportMoveDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewportCursorBehavior {
    Move,
    Extend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewportAlignment {
    Top,
    Center,
    Bottom,
}

impl ViewportAlignment {
    pub const fn row_offset(self, height: usize) -> usize {
        match self {
            Self::Top => 0,
            Self::Center => height.saturating_sub(1) / 2,
            Self::Bottom => height.saturating_sub(1),
        }
    }
}

/// 由后端上送给前端的视口请求。前端根据实际布局高度解析滚动量或对齐位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewportCommand {
    Scroll {
        direction: ViewportMoveDirection,
        amount: ViewportMoveAmount,
        cursor_behavior: ViewportCursorBehavior,
    },
    Align {
        alignment: ViewportAlignment,
    },
}

/// 前端基于 pane 高度解析后的、可延迟提交的 viewport mutation。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedViewportCommand {
    Scroll {
        direction: ViewportMoveDirection,
        lines: usize,
    },
    SetTopRow {
        top_row: usize,
    },
}

impl ViewportCommand {
    pub const fn new(
        direction: ViewportMoveDirection,
        amount: ViewportMoveAmount,
        cursor_behavior: ViewportCursorBehavior,
    ) -> Self {
        Self::Scroll {
            direction,
            amount,
            cursor_behavior,
        }
    }

    pub const fn align(alignment: ViewportAlignment) -> Self {
        Self::Align { alignment }
    }
}

impl Viewport {
    pub const fn origin() -> Self {
        Self {
            top_row: 0,
            left_col: 0,
        }
    }

    /// 调整 top_row 使 cursor_row 在 [top_row, top_row+view_height) 内。
    pub fn ensure_cursor_visible(&mut self, cursor_row: usize, view_height: usize) {
        if view_height == 0 {
            self.top_row = cursor_row;
            return;
        }
        if cursor_row < self.top_row {
            self.top_row = cursor_row;
        } else if cursor_row >= self.top_row + view_height {
            self.top_row = cursor_row - view_height + 1;
        }
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.top_row = self.top_row.saturating_add(n);
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.top_row = self.top_row.saturating_sub(n);
    }

    pub fn set_top_row(&mut self, top_row: usize) {
        self.top_row = top_row;
    }

    /// 按 lines 滚动（负向上、正向下）。
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "signed scrolling is retained as a viewport executor seam"
        )
    )]
    pub fn scroll_by(&mut self, lines: isize) {
        if lines >= 0 {
            self.top_row = self.top_row.saturating_add(lines as usize);
        } else {
            self.top_row = self.top_row.saturating_sub((-lines) as usize);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_down_when_cursor_below() {
        let mut v = Viewport::origin();
        v.ensure_cursor_visible(25, 23);
        assert_eq!(v.top_row, 3);
    }

    #[test]
    fn scroll_up_when_cursor_above() {
        let mut v = Viewport {
            top_row: 10,
            left_col: 0,
        };
        v.ensure_cursor_visible(5, 23);
        assert_eq!(v.top_row, 5);
    }

    #[test]
    fn no_scroll_when_visible() {
        let mut v = Viewport {
            top_row: 5,
            left_col: 0,
        };
        v.ensure_cursor_visible(10, 23);
        assert_eq!(v.top_row, 5);
    }

    #[test]
    fn zero_height_sets_top_to_cursor() {
        let mut v = Viewport::origin();
        v.ensure_cursor_visible(7, 0);
        assert_eq!(v.top_row, 7);
    }

    #[test]
    fn scroll_by_positive_down() {
        let mut v = Viewport::origin();
        v.scroll_by(3);
        assert_eq!(v.top_row, 3);
    }
    #[test]
    fn scroll_by_negative_up() {
        let mut v = Viewport {
            top_row: 10,
            left_col: 0,
        };
        v.scroll_by(-4);
        assert_eq!(v.top_row, 6);
    }

    #[test]
    fn viewport_command_preserves_frontend_owned_amount() {
        let command = ViewportCommand::new(
            ViewportMoveDirection::Down,
            ViewportMoveAmount::HalfPage,
            ViewportCursorBehavior::Extend,
        );

        assert_eq!(
            command,
            ViewportCommand::Scroll {
                direction: ViewportMoveDirection::Down,
                amount: ViewportMoveAmount::HalfPage,
                cursor_behavior: ViewportCursorBehavior::Extend,
            }
        );
    }

    #[test]
    fn viewport_alignment_is_distinct_from_cursor_moving_scroll() {
        let command = ViewportCommand::align(ViewportAlignment::Center);

        assert_eq!(
            command,
            ViewportCommand::Align {
                alignment: ViewportAlignment::Center,
            }
        );
    }

    #[test]
    fn center_alignment_uses_the_upper_middle_row() {
        assert_eq!(ViewportAlignment::Center.row_offset(0), 0);
        assert_eq!(ViewportAlignment::Center.row_offset(4), 1);
        assert_eq!(ViewportAlignment::Center.row_offset(5), 2);
    }
}
