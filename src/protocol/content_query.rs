//! 前端 pull 后端内容的契约。同进程同步调用，返回 owned 数据。

use crate::protocol::ids::{ContentId, ViewId};
use crate::protocol::selection::{Selections, TextOffset, TextPoint};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionShape {
    Character,
    Line,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextPresentation {
    pub selections: Selections,
    pub cursor_style: CursorStyle,
    pub selection_shape: SelectionShape,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewPresentation {
    Text(TextPresentation),
    StatusBar,
}

/// Content 声明的呈现类别；ViewPresentation 在此基础上附加会话状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentPresentation {
    Text,
    StatusBar,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewData {
    pub content: ContentId,
    pub presentation: ViewPresentation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentQuery {
    TextRows(RowRange),
    TextPoints(Vec<TextOffset>),
    DocumentStatus,
    StatusBarData,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentData {
    TextRows(Vec<String>),
    TextPoints(Vec<TextPoint>),
    DocumentStatus(DocumentStatus),
    StatusBarData(StatusBarData),
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
    use crate::protocol::selection::{Selection, TextOffset};
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
    fn text_points_query_owns_offsets_and_points() {
        let offsets = vec![TextOffset::origin(), TextOffset { char_index: 3 }];

        assert_eq!(
            ContentQuery::TextPoints(offsets),
            ContentQuery::TextPoints(vec![TextOffset::origin(), TextOffset { char_index: 3 }])
        );
        assert_eq!(
            ContentData::TextPoints(vec![TextPoint { row: 1, col: 2 }]),
            ContentData::TextPoints(vec![TextPoint { row: 1, col: 2 }])
        );
    }

    #[test]
    fn view_data_has_explicit_text_presentation() {
        let selections = Selections::single(Selection::collapsed(TextOffset::origin()));
        let data = ViewData {
            content: ContentId(7),
            presentation: ViewPresentation::Text(TextPresentation {
                selections: selections.clone(),
                cursor_style: CursorStyle::Block,
                selection_shape: SelectionShape::Character,
            }),
        };
        assert_eq!(data.content, ContentId(7));
        assert_eq!(
            data.presentation,
            ViewPresentation::Text(TextPresentation {
                selections,
                cursor_style: CursorStyle::Block,
                selection_shape: SelectionShape::Character,
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
