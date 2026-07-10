//! 前端 pull 后端内容的契约。同进程同步调用，返回 owned 数据。

use crate::protocol::ids::{ContentId, SpaceId};
use crate::protocol::selection::Selections;
use crate::protocol::status::StatusMessage;

/// 行范围 [start, end)，前端按可见行拉取。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowRange {
    pub start: usize,
    pub end: usize,
}

/// 状态栏显示数据（owned）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusBarData {
    pub file_name: Option<String>,
    pub modified: bool,
    pub message: StatusMessage,
}

/// 前端查询后端内容的契约。同进程同步调用。
/// 返回 Vec 长度 = min(range.len(), line_count - start)；超出末尾的行不返回。
pub trait ContentQuery {
    fn lines(&self, cid: ContentId, range: RowRange) -> Vec<String>;
    fn status_bar(&self, cid: ContentId) -> StatusBarData;
    fn selections(&self, sid: SpaceId) -> Selections;
    fn line_count(&self, cid: ContentId) -> usize;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn row_range_constructs() {
        let r = RowRange { start: 1, end: 5 };
        assert_eq!(r.start, 1);
        assert_eq!(r.end, 5);
    }
    #[test]
    fn status_bar_data_eq() {
        let a = StatusBarData {
            file_name: None,
            modified: false,
            message: StatusMessage::None,
        };
        assert_eq!(a, a.clone());
    }
}
