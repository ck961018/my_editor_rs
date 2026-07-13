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

/// 文档状态显示数据（owned）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentStatus {
    pub file_name: Option<String>,
    pub modified: bool,
    pub message: StatusMessage,
}

pub type StatusBarData = DocumentStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorStyle {
    Default,
    Block,
    Bar,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewData {
    pub selections: Option<Selections>,
    pub cursor_style: CursorStyle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentQuery {
    TextRows(RowRange),
    TextLineCount,
    #[allow(dead_code)]
    DocumentStatus,
    StatusBarData,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentData {
    TextRows(Vec<String>),
    TextLineCount(usize),
    DocumentStatus(DocumentStatus),
    StatusBarData(StatusBarData),
    #[allow(dead_code)]
    Unsupported,
}

/// 前端通过消息拉取后端内容的只读契约。
pub trait RenderQuery {
    fn content(&self, id: ContentId, query: ContentQuery) -> ContentData;
    fn view(&self, id: SpaceId) -> ViewData;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::selection::{CursorPos, Selection};
    #[test]
    fn row_range_constructs() {
        let r = RowRange { start: 1, end: 5 };
        assert_eq!(r.start, 1);
        assert_eq!(r.end, 5);
    }
    #[test]
    fn status_bar_data_eq() {
        let a = DocumentStatus {
            file_name: None,
            modified: false,
            message: StatusMessage::None,
        };
        assert_eq!(a, a.clone());
    }

    #[test]
    fn content_query_and_data_preserve_owned_status() {
        let status = DocumentStatus {
            file_name: Some("note.txt".to_string()),
            modified: true,
            message: StatusMessage::Saved,
        };
        let data = ContentData::DocumentStatus(status.clone());

        assert_eq!(data, ContentData::DocumentStatus(status));
        assert_eq!(ContentData::Unsupported, ContentData::Unsupported);
    }

    #[test]
    fn view_data_contains_selections_and_cursor_style() {
        let data = ViewData {
            selections: Some(Selections::single(
                Selection::collapsed(CursorPos::origin()),
            )),
            cursor_style: CursorStyle::Block,
        };
        assert_eq!(data.cursor_style, CursorStyle::Block);
        assert_eq!(
            data.selections.unwrap().primary().head(),
            CursorPos::origin()
        );
    }
}
