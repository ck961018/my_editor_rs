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
