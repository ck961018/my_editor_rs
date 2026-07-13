//! 前端 pull 后端内容的契约。同进程同步调用，返回 owned 数据。

use crate::protocol::ids::{ContentId, ViewId};
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
pub struct TextPresentation {
    pub selections: Selections,
    pub cursor_style: CursorStyle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewPresentation {
    Text(TextPresentation),
    StatusBar,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewData {
    pub content: ContentId,
    pub presentation: ViewPresentation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentQuery {
    TextRows(RowRange),
    #[allow(dead_code)]
    DocumentStatus,
    StatusBarData,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentData {
    TextRows(Vec<String>),
    DocumentStatus(DocumentStatus),
    StatusBarData(StatusBarData),
    #[allow(dead_code)]
    Unsupported,
}

/// 前端通过消息拉取后端内容的只读契约。
pub trait RenderQuery {
    fn content(&self, id: ContentId, query: ContentQuery) -> ContentData;
    fn view(&self, id: ViewId) -> ViewData;
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
    fn view_data_has_explicit_text_presentation() {
        let selections = Selections::single(Selection::collapsed(CursorPos::origin()));
        let data = ViewData {
            content: ContentId(7),
            presentation: ViewPresentation::Text(TextPresentation {
                selections: selections.clone(),
                cursor_style: CursorStyle::Block,
            }),
        };
        assert_eq!(data.content, ContentId(7));
        assert_eq!(
            data.presentation,
            ViewPresentation::Text(TextPresentation {
                selections,
                cursor_style: CursorStyle::Block,
            })
        );
    }

    #[test]
    fn status_bar_presentation_has_no_text_state() {
        let data = ViewData {
            content: ContentId(8),
            presentation: ViewPresentation::StatusBar,
        };

        assert_eq!(data.presentation, ViewPresentation::StatusBar);
    }
}
