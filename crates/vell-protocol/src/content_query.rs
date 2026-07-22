//! 前端 pull 后端内容的契约。同进程同步调用，返回 owned 数据。

use std::fmt;

use crate::ids::{ContentId, ViewId};
use crate::selection::{Selections, TextOffset, TextPoint};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FaceName(String);

impl FaceName {
    #[allow(dead_code, reason = "dynamic modes and themes create named faces")]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ThemeName(String);

impl ThemeName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
}

#[allow(dead_code, reason = "dynamic faces provide terminal and RGB colors")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Ansi(u8),
    Rgb { red: u8, green: u8, blue: u8 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FaceValue<T> {
    #[default]
    Unspecified,
    Value(T),
    Reset,
}

impl<T: Clone> FaceValue<T> {
    pub fn overlay(&mut self, patch: &Self) {
        if !matches!(patch, Self::Unspecified) {
            *self = patch.clone();
        }
    }
}

impl<T: PartialEq> PartialEq<Option<T>> for FaceValue<T> {
    fn eq(&self, other: &Option<T>) -> bool {
        match (self, other) {
            (Self::Value(left), Some(right)) => left == right,
            (Self::Unspecified | Self::Reset, None) => true,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FacePatch {
    pub foreground: FaceValue<Color>,
    pub background: FaceValue<Color>,
    pub bold: FaceValue<bool>,
    pub italic: FaceValue<bool>,
    pub underline: FaceValue<bool>,
}

impl FacePatch {
    pub fn overlay(&mut self, patch: &Self) {
        self.foreground.overlay(&patch.foreground);
        self.background.overlay(&patch.background);
        self.bold.overlay(&patch.bold);
        self.italic.overlay(&patch.italic);
        self.underline.overlay(&patch.underline);
    }

    pub fn resolve(&self, root: &PaintFace) -> PaintFace {
        let mut resolved = root.clone();
        resolved.apply_patch(self, root);
        resolved
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaintFace {
    pub foreground: Option<Color>,
    pub background: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

impl PaintFace {
    pub fn apply_patch(&mut self, patch: &FacePatch, root: &Self) {
        apply_value(&mut self.foreground, &patch.foreground, &root.foreground);
        apply_value(&mut self.background, &patch.background, &root.background);
        apply_value(&mut self.bold, &patch.bold, &root.bold);
        apply_value(&mut self.italic, &patch.italic, &root.italic);
        apply_value(&mut self.underline, &patch.underline, &root.underline);
    }
}

fn apply_value<T: Clone>(target: &mut T, patch: &FaceValue<T>, root: &T) {
    match patch {
        FaceValue::Unspecified => {}
        FaceValue::Value(value) => *target = value.clone(),
        FaceValue::Reset => *target = root.clone(),
    }
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

impl From<&Face> for FacePatch {
    fn from(face: &Face) -> Self {
        Self {
            foreground: face
                .foreground
                .map_or(FaceValue::Unspecified, FaceValue::Value),
            background: face
                .background
                .map_or(FaceValue::Unspecified, FaceValue::Value),
            bold: face.bold.map_or(FaceValue::Unspecified, FaceValue::Value),
            italic: face
                .italic
                .map_or(FaceValue::Unspecified, FaceValue::Value),
            underline: face
                .underline
                .map_or(FaceValue::Unspecified, FaceValue::Value),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaceDefinition {
    pub name: FaceName,
    pub inherits: Vec<FaceName>,
    pub fallback: FacePatch,
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
    pub face: FacePatch,
}

/// 行范围 [start, end)，前端按可见行拉取。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferBackingState {
    Untitled,
    Unmaterialized,
    Materialized,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirtyState {
    Clean,
    Modified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveState {
    Idle,
    Saved,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextMetrics {
    pub line_count: usize,
    pub char_count: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusBarSegment {
    pub text: String,
    pub face: FacePatch,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusBarPresentation {
    pub base_face: PaintFace,
    pub left: Vec<StatusBarSegment>,
    pub center: Vec<StatusBarSegment>,
    pub right: Vec<StatusBarSegment>,
}

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
    pub base_face: PaintFace,
    pub selections: Selections,
    pub cursor_style: CursorStyle,
    pub selection_shape: SelectionShape,
    pub selection_face: FacePatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewPresentation {
    Text(TextPresentation),
    StatusBar(StatusBarPresentation),
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
    ResourceName,
    ResourcePath,
    BackingState,
    DirtyState,
    SaveState,
    TextMetrics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentQueryKind {
    TextRows,
    TextPoints,
    ResourceName,
    ResourcePath,
    BackingState,
    DirtyState,
    SaveState,
    TextMetrics,
}

impl ContentQuery {
    pub fn kind(&self) -> ContentQueryKind {
        match self {
            Self::TextRows(_) => ContentQueryKind::TextRows,
            Self::TextPoints(_) => ContentQueryKind::TextPoints,
            Self::ResourceName => ContentQueryKind::ResourceName,
            Self::ResourcePath => ContentQueryKind::ResourcePath,
            Self::BackingState => ContentQueryKind::BackingState,
            Self::DirtyState => ContentQueryKind::DirtyState,
            Self::SaveState => ContentQueryKind::SaveState,
            Self::TextMetrics => ContentQueryKind::TextMetrics,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentData {
    TextRows(Vec<String>),
    TextPoints(Vec<TextPoint>),
    ResourceName(Option<String>),
    ResourcePath(Option<String>),
    BackingState(BufferBackingState),
    DirtyState(DirtyState),
    SaveState(SaveState),
    TextMetrics(TextMetrics),
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
    IncompatibleContentViewState {
        view: ViewId,
        content: ContentId,
    },
}

impl fmt::Display for RenderQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingView(view) => write!(formatter, "view {} does not exist", view.0),
            Self::MissingContent(content) => {
                write!(formatter, "content {} does not exist", content.0)
            }
            Self::UnsupportedContentQuery { content, query } => {
                write!(
                    formatter,
                    "content {} does not support {query:?}",
                    content.0
                )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_composition_distinguishes_false_from_unspecified() {
        let mut face = FacePatch {
            italic: FaceValue::Value(true),
            ..FacePatch::default()
        };
        face.overlay(&FacePatch::default());
        assert_eq!(face.italic, FaceValue::Value(true));
        face.overlay(&FacePatch {
            italic: FaceValue::Value(false),
            ..FacePatch::default()
        });
        assert_eq!(face.italic, FaceValue::Value(false));
    }

    #[test]
    fn reset_restores_the_presentation_root() {
        let root = PaintFace {
            foreground: Some(Color::Ansi(7)),
            background: Some(Color::Ansi(0)),
            bold: false,
            italic: true,
            underline: false,
        };
        let mut painted = PaintFace {
            foreground: Some(Color::Ansi(1)),
            background: Some(Color::Ansi(4)),
            bold: true,
            italic: false,
            underline: true,
        };
        painted.apply_patch(
            &FacePatch {
                foreground: FaceValue::Reset,
                italic: FaceValue::Reset,
                ..FacePatch::default()
            },
            &root,
        );
        assert_eq!(painted.foreground, root.foreground);
        assert_eq!(painted.italic, root.italic);
        assert_eq!(painted.background, Some(Color::Ansi(4)));
        assert!(painted.bold);
    }
}
