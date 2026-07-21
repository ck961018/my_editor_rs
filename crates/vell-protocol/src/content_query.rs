//! 前端 pull 后端内容的契约。同进程同步调用，返回 owned 数据。

use std::fmt;

use crate::ids::{ContentId, ViewId};
use crate::selection::{Selections, TextOffset, TextPoint};
use crate::status::StatusMessage;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FaceName(String);

impl FaceName {
    #[allow(dead_code, reason = "dynamic modes and themes create named faces")]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }
}

#[allow(dead_code, reason = "dynamic faces provide terminal and RGB colors")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Ansi(u8),
    Rgb { red: u8, green: u8, blue: u8 },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Face {
    pub foreground: Option<Color>,
    pub background: Option<Color>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
}

impl Face {
    pub fn overlay(&mut self, patch: &Self) {
        if patch.foreground.is_some() {
            self.foreground = patch.foreground;
        }
        if patch.background.is_some() {
            self.background = patch.background;
        }
        if patch.bold.is_some() {
            self.bold = patch.bold;
        }
        if patch.italic.is_some() {
            self.italic = patch.italic;
        }
        if patch.underline.is_some() {
            self.underline = patch.underline;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedTextDecoration {
    pub start: TextOffset,
    pub end: TextOffset,
    pub face: FaceName,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextDecoration {
    pub start: TextOffset,
    pub end: TextOffset,
    pub face: Face,
}

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
    CharacterInclusive,
    Line,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextPresentation {
    pub selections: Selections,
    pub cursor_style: CursorStyle,
    pub selection_shape: SelectionShape,
    pub selection_face: Face,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentQuery {
    TextRows(RowRange),
    TextPoints(Vec<TextOffset>),
    DocumentStatus,
    StatusBarData,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentQueryKind {
    TextRows,
    TextPoints,
    DocumentStatus,
    StatusBarData,
}

impl ContentQuery {
    pub fn kind(&self) -> ContentQueryKind {
        match self {
            Self::TextRows(_) => ContentQueryKind::TextRows,
            Self::TextPoints(_) => ContentQueryKind::TextPoints,
            Self::DocumentStatus => ContentQueryKind::DocumentStatus,
            Self::StatusBarData => ContentQueryKind::StatusBarData,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentData {
    TextRows(Vec<String>),
    TextPoints(Vec<TextPoint>),
    DocumentStatus(DocumentStatus),
    StatusBarData(StatusBarData),
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderQueryError {
    MissingView(ViewId),
    MissingContent(ContentId),
    UnsupportedContentQuery {
        content: ContentId,
        query: ContentQueryKind,
    },
    InvalidContentData {
        content: ContentId,
        query: ContentQueryKind,
    },
    IncompatibleContentViewState { view: ViewId, content: ContentId },
}

impl fmt::Display for RenderQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingView(view) => write!(formatter, "view {} does not exist", view.0),
            Self::MissingContent(content) => {
                write!(formatter, "content {} does not exist", content.0)
            }
            Self::UnsupportedContentQuery { content, query } => {
                write!(formatter, "content {} does not support {query:?}", content.0)
            }
            Self::InvalidContentData { content, query } => write!(
                formatter,
                "content {} returned invalid data for {query:?}",
                content.0
            ),
            Self::IncompatibleContentViewState { view, content } => write!(
                formatter,
                "view {} has state incompatible with content {}",
                view.0, content.0
            ),
        }
    }
}

impl std::error::Error for RenderQueryError {}

/// 前端通过消息拉取后端内容的只读契约。
pub trait RenderQuery {
    fn content(&self, id: ContentId, query: ContentQuery) -> Result<ContentData, RenderQueryError>;
    fn view(&self, id: ViewId) -> Result<ViewData, RenderQueryError>;
    fn decorations(
        &self,
        _view: ViewId,
        _visible_rows: RowRange,
    ) -> Result<Vec<TextDecoration>, RenderQueryError> {
        Ok(Vec::new())
    }
}
