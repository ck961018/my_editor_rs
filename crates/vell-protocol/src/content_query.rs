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

pub fn is_host_face_name(name: &FaceName) -> bool {
    ["ui", "syntax", "diagnostic", "diff"]
        .iter()
        .any(|namespace| {
            name.as_str() == *namespace
                || name
                    .as_str()
                    .strip_prefix(namespace)
                    .is_some_and(|suffix| suffix.starts_with('.'))
        })
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorDepth {
    TrueColor,
    Ansi256,
    Ansi16,
    Monochrome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayProfile {
    pub color_depth: ColorDepth,
    pub appearance: Option<Appearance>,
    pub supports_italic: bool,
    pub supports_underline: bool,
    pub supports_extended_underline: bool,
    pub supports_undercurl: bool,
    pub supports_strikethrough: bool,
    pub supports_dim: bool,
}

impl Default for DisplayProfile {
    fn default() -> Self {
        Self {
            color_depth: ColorDepth::TrueColor,
            appearance: None,
            supports_italic: true,
            supports_underline: true,
            supports_extended_underline: true,
            supports_undercurl: true,
            supports_strikethrough: true,
            supports_dim: true,
        }
    }
}

#[allow(dead_code, reason = "dynamic faces provide terminal and RGB colors")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Ansi(u8),
    Ansi16(u8),
    Rgb { red: u8, green: u8, blue: u8 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UnderlineStyle {
    #[default]
    Line,
    Double,
    Curl,
    Dotted,
    Dashed,
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
    pub dim: FaceValue<bool>,
    pub italic: FaceValue<bool>,
    pub underline: FaceValue<bool>,
    pub underline_style: FaceValue<UnderlineStyle>,
    pub strikethrough: FaceValue<bool>,
}

impl FacePatch {
    pub fn overlay(&mut self, patch: &Self) {
        self.foreground.overlay(&patch.foreground);
        self.background.overlay(&patch.background);
        self.bold.overlay(&patch.bold);
        self.dim.overlay(&patch.dim);
        self.italic.overlay(&patch.italic);
        self.underline.overlay(&patch.underline);
        self.underline_style.overlay(&patch.underline_style);
        if matches!(patch.underline_style, FaceValue::Value(_))
            && matches!(patch.underline, FaceValue::Unspecified)
        {
            self.underline = FaceValue::Value(true);
        }
        self.strikethrough.overlay(&patch.strikethrough);
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
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub underline_style: UnderlineStyle,
    pub strikethrough: bool,
}

impl PaintFace {
    pub fn apply_patch(&mut self, patch: &FacePatch, root: &Self) {
        apply_value(&mut self.foreground, &patch.foreground, &root.foreground);
        apply_value(&mut self.background, &patch.background, &root.background);
        apply_value(&mut self.bold, &patch.bold, &root.bold);
        apply_value(&mut self.dim, &patch.dim, &root.dim);
        apply_value(&mut self.italic, &patch.italic, &root.italic);
        apply_value(&mut self.underline, &patch.underline, &root.underline);
        apply_value(
            &mut self.underline_style,
            &patch.underline_style,
            &root.underline_style,
        );
        if matches!(patch.underline_style, FaceValue::Value(_))
            && matches!(patch.underline, FaceValue::Unspecified)
        {
            self.underline = true;
        }
        apply_value(
            &mut self.strikethrough,
            &patch.strikethrough,
            &root.strikethrough,
        );
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
    pub dim: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub underline_style: Option<UnderlineStyle>,
    pub strikethrough: Option<bool>,
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
        if patch.dim.is_some() {
            self.dim = patch.dim;
        }
        if patch.italic.is_some() {
            self.italic = patch.italic;
        }
        if patch.underline.is_some() {
            self.underline = patch.underline;
        }
        if patch.underline_style.is_some() {
            self.underline_style = patch.underline_style;
        }
        if patch.strikethrough.is_some() {
            self.strikethrough = patch.strikethrough;
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
            dim: face.dim.map_or(FaceValue::Unspecified, FaceValue::Value),
            italic: face
                .italic
                .map_or(FaceValue::Unspecified, FaceValue::Value),
            underline: face
                .underline
                .map_or(FaceValue::Unspecified, FaceValue::Value),
            underline_style: face
                .underline_style
                .map_or(FaceValue::Unspecified, FaceValue::Value),
            strikethrough: face
                .strikethrough
                .map_or(FaceValue::Unspecified, FaceValue::Value),
        }
    }
}

impl DisplayProfile {
    pub fn adapt_patch(&self, patch: &mut FacePatch) {
        adapt_color(&mut patch.foreground, self.color_depth);
        adapt_color(&mut patch.background, self.color_depth);
        if !self.supports_italic {
            patch.italic = FaceValue::Unspecified;
        }
        if !self.supports_underline {
            patch.underline = FaceValue::Unspecified;
            patch.underline_style = FaceValue::Unspecified;
        } else if let FaceValue::Value(style) = patch.underline_style {
            let supported = match style {
                UnderlineStyle::Line => style,
                UnderlineStyle::Curl if self.supports_undercurl => style,
                UnderlineStyle::Double | UnderlineStyle::Dotted | UnderlineStyle::Dashed
                    if self.supports_extended_underline => style,
                UnderlineStyle::Double
                | UnderlineStyle::Curl
                | UnderlineStyle::Dotted
                | UnderlineStyle::Dashed => UnderlineStyle::Line,
            };
            patch.underline_style = FaceValue::Value(supported);
        }
        if !self.supports_strikethrough {
            patch.strikethrough = FaceValue::Unspecified;
        }
        if !self.supports_dim {
            patch.dim = FaceValue::Unspecified;
        }
    }
}

fn adapt_color(value: &mut FaceValue<Color>, depth: ColorDepth) {
    let FaceValue::Value(color) = value else {
        if depth == ColorDepth::Monochrome {
            *value = FaceValue::Unspecified;
        }
        return;
    };
    *value = match depth {
        ColorDepth::TrueColor => FaceValue::Value(*color),
        ColorDepth::Ansi256 => FaceValue::Value(Color::Ansi(match color {
            Color::Ansi(value) => *value,
            Color::Ansi16(value) => *value,
            Color::Rgb { red, green, blue } => nearest_ansi(*red, *green, *blue, 256),
        })),
        ColorDepth::Ansi16 => {
            let (red, green, blue) = color_rgb(*color);
            FaceValue::Value(Color::Ansi16(nearest_ansi(red, green, blue, 16)))
        }
        ColorDepth::Monochrome => FaceValue::Unspecified,
    };
}

fn color_rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Ansi(value) => ansi_rgb(value),
        Color::Ansi16(value) => ansi_rgb(value.min(15)),
        Color::Rgb { red, green, blue } => (red, green, blue),
    }
}

fn nearest_ansi(red: u8, green: u8, blue: u8, count: u16) -> u8 {
    (0..count)
        .min_by_key(|candidate| {
            let (candidate_red, candidate_green, candidate_blue) = ansi_rgb(*candidate as u8);
            let red = i32::from(red) - i32::from(candidate_red);
            let green = i32::from(green) - i32::from(candidate_green);
            let blue = i32::from(blue) - i32::from(candidate_blue);
            red * red + green * green + blue * blue
        })
        .expect("ANSI palette is non-empty") as u8
}

fn ansi_rgb(value: u8) -> (u8, u8, u8) {
    const ANSI16: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (128, 0, 0),
        (0, 128, 0),
        (128, 128, 0),
        (0, 0, 128),
        (128, 0, 128),
        (0, 128, 128),
        (192, 192, 192),
        (128, 128, 128),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (0, 0, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    match value {
        0..=15 => ANSI16[usize::from(value)],
        16..=231 => {
            const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let index = value - 16;
            (
                LEVELS[usize::from(index / 36)],
                LEVELS[usize::from(index / 6 % 6)],
                LEVELS[usize::from(index % 6)],
            )
        }
        232..=255 => {
            let level = 8 + (value - 232) * 10;
            (level, level, level)
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
pub struct FaceOverride {
    pub face: FaceName,
    pub theme: Option<ThemeName>,
    pub patch: FacePatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FaceExpr {
    Named(FaceName),
    Patch(FacePatch),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaceRemapScope {
    Session,
    Content(ContentId),
    View(ViewId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FaceRemapToken(pub u64);

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
            dim: false,
            italic: true,
            underline: false,
            underline_style: UnderlineStyle::Line,
            strikethrough: false,
        };
        let mut painted = PaintFace {
            foreground: Some(Color::Ansi(1)),
            background: Some(Color::Ansi(4)),
            bold: true,
            dim: false,
            italic: false,
            underline: true,
            underline_style: UnderlineStyle::Double,
            strikethrough: false,
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

    #[test]
    fn host_face_names_include_namespace_roots_but_not_similar_prefixes() {
        assert!(is_host_face_name(&FaceName::new("ui")));
        assert!(is_host_face_name(&FaceName::new("syntax.keyword")));
        assert!(!is_host_face_name(&FaceName::new("ui-plugin.face")));
        assert!(!is_host_face_name(&FaceName::new("plugin.example.ui")));
    }

    #[test]
    fn display_profile_quantizes_color_and_degrades_unsupported_attributes() {
        let mut patch = FacePatch {
            foreground: FaceValue::Value(Color::Rgb {
                red: 95,
                green: 135,
                blue: 175,
            }),
            italic: FaceValue::Value(true),
            underline: FaceValue::Value(true),
            underline_style: FaceValue::Value(UnderlineStyle::Curl),
            strikethrough: FaceValue::Value(true),
            ..FacePatch::default()
        };
        DisplayProfile {
            color_depth: ColorDepth::Ansi256,
            appearance: None,
            supports_italic: false,
            supports_underline: true,
            supports_extended_underline: false,
            supports_undercurl: false,
            supports_strikethrough: false,
            supports_dim: true,
        }
        .adapt_patch(&mut patch);

        assert_eq!(patch.foreground, FaceValue::Value(Color::Ansi(67)));
        assert_eq!(patch.italic, FaceValue::Unspecified);
        assert_eq!(
            patch.underline_style,
            FaceValue::Value(UnderlineStyle::Line)
        );
        assert_eq!(patch.strikethrough, FaceValue::Unspecified);
    }

    #[test]
    fn monochrome_profile_removes_color_to_preserve_reverse_selection_fallback() {
        let mut patch = FacePatch {
            foreground: FaceValue::Value(Color::Ansi(1)),
            background: FaceValue::Reset,
            bold: FaceValue::Value(true),
            ..FacePatch::default()
        };
        DisplayProfile {
            color_depth: ColorDepth::Monochrome,
            appearance: None,
            supports_italic: false,
            supports_underline: false,
            supports_extended_underline: false,
            supports_undercurl: false,
            supports_strikethrough: false,
            supports_dim: false,
        }
        .adapt_patch(&mut patch);

        assert_eq!(patch.foreground, FaceValue::Unspecified);
        assert_eq!(patch.background, FaceValue::Unspecified);
        assert_eq!(patch.bold, FaceValue::Value(true));
    }
}
