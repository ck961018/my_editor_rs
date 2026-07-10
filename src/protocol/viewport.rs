/// 视口滚动位置。尺寸不存（从 layout 给的 rect 拿），消除「预留状态栏行」越权。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub top_row: usize,
    pub left_col: usize,
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

    #[allow(dead_code)] // v0.2 预留滚动 API
    pub fn scroll_down(&mut self, n: usize, view_height: usize) {
        self.top_row = self.top_row.saturating_add(n);
        let _ = view_height; // v0.2 预留：未来按 view_height 钳位
    }

    #[allow(dead_code)] // v0.2 预留滚动 API
    pub fn scroll_up(&mut self, n: usize) {
        self.top_row = self.top_row.saturating_sub(n);
    }

    /// 按 lines 滚动（负向上、正向下）。v0.2 不绑键，预留 executor 路径。
    #[allow(dead_code)] // v0.2 预留滚动 API
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
    fn origin_is_zero() {
        let v = Viewport::origin();
        assert_eq!((v.top_row, v.left_col), (0, 0));
    }

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
}
