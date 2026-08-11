use std::collections::BTreeMap;
use std::fmt;

use crate::action::{TransactionIntent, ViewAction};
use crate::command::{AppCommand, ModeCommand, ModeInputCommand};
use vell_core::action::ContentAction;
use vell_core::clipboard::{ClipboardKind, PastePlacement};
use vell_core::command::EditCommand;
use vell_core::search::{CaseSensitivity, SearchOptions, SearchPattern};
use vell_protocol::content_query::{FaceExpr, FaceName, FaceRemapToken};
use vell_protocol::ids::{ContentId, ViewId};
use vell_protocol::revision::Revision;
use vell_protocol::selection::Selections;
use vell_protocol::view::BindingKey;
use vell_protocol::viewport::ViewportCommand;

/// Maximum number of operations one app execution frame will evaluate.
///
/// This lives in the shared extension contract so operation producers and the
/// app executor cannot silently drift to different limits.
pub const MAX_OPERATIONS_PER_FRAME: usize = 256;

/// Maximum operations a single mode callback may append to its invoking
/// operation. Nested callbacks still share the enclosing frame budget.
pub const MAX_MODE_CALLBACK_OPERATIONS: usize = MAX_OPERATIONS_PER_FRAME - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentTarget {
    Current,
    Id(ContentId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewTarget {
    Current,
    Switchable,
    Id(ViewId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeTarget {
    #[allow(
        dead_code,
        reason = "content-scoped nested modes are an extension contract"
    )]
    CurrentContent,
    CurrentView,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperationRequest {
    ExecuteCommandLine(ExecuteCommandLine),
    Content {
        target: ContentTarget,
        operation: ContentOperation,
    },
    View {
        target: ViewTarget,
        operation: ViewOperation,
    },
    ViewBinding {
        target: ViewTarget,
        operation: ViewBindingOperation,
    },
    History {
        target: ContentTarget,
        operation: TransactionIntent,
    },
    Mode {
        target: ModeTarget,
        invocation: ModeInvocation,
    },
    ModeInput {
        target: ViewTarget,
        input: ModeInputCommand,
    },
    Face(FaceOperation),
    Clipboard {
        target: ViewTarget,
        operation: ClipboardOperation,
    },
    Search {
        target: ViewTarget,
        operation: SearchOperation,
    },
    ContentLifecycle(ContentLifecycleOperation),
    ViewLifecycle(ViewLifecycleOperation),
    App(AppOperation),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecuteCommandLine {
    pub source: String,
}

impl ExecuteCommandLine {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardSource {
    Internal,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardDestination {
    Internal,
    InternalAndSystem,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardOperation {
    Copy {
        kind: ClipboardKind,
        destination: ClipboardDestination,
    },
    CopyForEdit {
        command: EditCommand,
        kind: ClipboardKind,
        destination: ClipboardDestination,
    },
    Cut {
        kind: ClipboardKind,
        destination: ClipboardDestination,
    },
    Paste {
        source: ClipboardSource,
        placement: PastePlacement,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchOperation {
    Find {
        expected_revision: Revision,
        start: usize,
        pattern: SearchPattern,
        options: SearchOptions,
    },
    ReplaceNext {
        expected_revision: Revision,
        start: usize,
        pattern: SearchPattern,
        replacement: String,
        options: SearchOptions,
    },
    ReplaceAll {
        expected_revision: Revision,
        pattern: SearchPattern,
        replacement: String,
        case: CaseSensitivity,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaceRemapTarget {
    Session,
    CurrentContent,
    CurrentView,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FaceOperation {
    SetBase {
        target: FaceRemapTarget,
        face: FaceName,
        expressions: Option<Vec<FaceExpr>>,
    },
    AddRelative {
        target: FaceRemapTarget,
        face: FaceName,
        token: FaceRemapToken,
        expressions: Vec<FaceExpr>,
    },
    RemoveRelative {
        token: FaceRemapToken,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentLifecycleOperation {
    Create,
    Open {
        path: String,
    },
    List,
    Close {
        target: ContentTarget,
        force: bool,
    },
    Save {
        target: ContentTarget,
        force: bool,
    },
    SaveAs {
        target: ContentTarget,
        path: String,
        force: bool,
    },
    Reload {
        target: ContentTarget,
        force: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BufferViewSource {
    Content(ContentId),
    Create,
    Open { path: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewSpec {
    Buffer {
        source: BufferViewSource,
    },
    Diff {
        left: ContentId,
        right: ContentId,
    },
    Defined {
        definition: vell_protocol::view::ViewDefinitionId,
        bindings: BTreeMap<BindingKey, ContentId>,
    },
}

impl ViewSpec {
    pub fn buffer(content: ContentId) -> Self {
        Self::Buffer {
            source: BufferViewSource::Content(content),
        }
    }

    pub fn diff(left: ContentId, right: ContentId) -> Self {
        Self::Diff { left, right }
    }

    pub fn defined(
        definition: vell_protocol::view::ViewDefinitionId,
        bindings: impl IntoIterator<Item = (BindingKey, ContentId)>,
    ) -> Self {
        Self::Defined {
            definition,
            bindings: bindings.into_iter().collect(),
        }
    }

    pub fn from_json(value: &serde_json::Value) -> Result<Self, ViewSpecParseError> {
        let object = value
            .as_object()
            .ok_or_else(|| ViewSpecParseError::new("view spec must be an object"))?;
        match object.get("type").and_then(serde_json::Value::as_str) {
            Some("core.buffer") => parse_buffer_view_spec(object),
            Some("core.diff") => parse_diff_view_spec(object),
            Some("defined") => parse_defined_view_spec(object),
            _ => Err(ViewSpecParseError::new(
                "view spec type must be 'core.buffer', 'core.diff', or 'defined'",
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewSpecParseError {
    message: String,
}

impl ViewSpecParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ViewSpecParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ViewSpecParseError {}

fn parse_buffer_view_spec(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<ViewSpec, ViewSpecParseError> {
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "type" | "content" | "create" | "path"))
    {
        return Err(ViewSpecParseError::new(
            "buffer view spec contains an unknown field",
        ));
    }
    let sources = [
        object.contains_key("content"),
        object.contains_key("create"),
        object.contains_key("path"),
    ];
    if sources.into_iter().filter(|present| *present).count() != 1 {
        return Err(ViewSpecParseError::new(
            "buffer view spec requires exactly one of content, create, or path",
        ));
    }
    let source = if let Some(content) = object.get("content") {
        let content = content.as_u64().map(ContentId).ok_or_else(|| {
            ViewSpecParseError::new("buffer view spec content must be a non-negative content id")
        })?;
        BufferViewSource::Content(content)
    } else if let Some(create) = object.get("create") {
        if create.as_bool() != Some(true) {
            return Err(ViewSpecParseError::new(
                "buffer view spec create must be true",
            ));
        }
        BufferViewSource::Create
    } else {
        let path = object
            .get("path")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                ViewSpecParseError::new("buffer view spec path must be a non-empty string")
            })?;
        BufferViewSource::Open { path }
    };
    Ok(ViewSpec::Buffer { source })
}

fn parse_diff_view_spec(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<ViewSpec, ViewSpecParseError> {
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "type" | "left" | "right"))
    {
        return Err(ViewSpecParseError::new(
            "diff view spec contains an unknown field",
        ));
    }
    let content = |name: &str| {
        object
            .get(name)
            .and_then(serde_json::Value::as_u64)
            .map(ContentId)
            .ok_or_else(|| {
                ViewSpecParseError::new(format!(
                    "diff view spec {name} must be a non-negative content id"
                ))
            })
    };
    Ok(ViewSpec::diff(content("left")?, content("right")?))
}

fn parse_defined_view_spec(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<ViewSpec, ViewSpecParseError> {
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "type" | "definition" | "bindings"))
    {
        return Err(ViewSpecParseError::new(
            "defined view spec contains an unknown field",
        ));
    }
    let definition = object
        .get("definition")
        .and_then(serde_json::Value::as_str)
        .filter(|definition| !definition.is_empty())
        .map(vell_protocol::view::ViewDefinitionId::new)
        .ok_or_else(|| {
            ViewSpecParseError::new("defined view spec definition must be a non-empty string")
        })?;
    let bindings = object
        .get("bindings")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| ViewSpecParseError::new("defined view spec bindings must be an object"))?
        .iter()
        .map(|(binding, content)| {
            let content = content.as_u64().map(ContentId).ok_or_else(|| {
                ViewSpecParseError::new(format!(
                    "defined view spec binding '{binding}' must be a non-negative content id"
                ))
            })?;
            Ok((BindingKey::new(binding), content))
        })
        .collect::<Result<Vec<_>, ViewSpecParseError>>()?;
    Ok(ViewSpec::defined(definition, bindings))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewLifecycleOperation {
    Focus { view: ViewId },
    Switch { spec: ViewSpec },
}

/// View-specific behavior uses this primitive to preserve the View while
/// changing one role declared by its definition. It is not a generic user
/// command and must not be confused with `view.switch`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewBindingOperation {
    Rebind {
        binding: BindingKey,
        content: ContentTarget,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentOperation {
    #[allow(dead_code, reason = "content-scoped modes emit typed content actions")]
    Apply(ContentAction),
    Save,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewOperation {
    Edit(EditCommand),
    #[allow(dead_code, reason = "preplanned edits are an extension contract")]
    ApplyPlan(ViewEditPlan),
    ApplyContent(ContentAction),
    #[allow(dead_code, reason = "modes can emit selection-only view actions")]
    Apply(ViewAction),
    Viewport(ViewportCommand),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeInvocation {
    pub command: ModeCommand,
    pub nested: bool,
    pub flow: ModeFlowPropagation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeFlowPropagation {
    Propagate,
    Isolate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppOperation {
    Command(AppCommand),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewEditPlan {
    pub expected: ViewPrecondition,
    pub content: Option<ContentAction>,
    pub view: Option<ViewAction>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewPrecondition {
    Selections(Selections),
    #[allow(dead_code, reason = "revision preconditions are reserved for plugins")]
    Revision(Revision),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_spec_json_parser_owns_all_public_variants() {
        assert_eq!(
            ViewSpec::from_json(&serde_json::json!({
                "type": "core.buffer",
                "path": "notes.md",
            })),
            Ok(ViewSpec::Buffer {
                source: BufferViewSource::Open {
                    path: "notes.md".to_owned(),
                },
            })
        );
        assert_eq!(
            ViewSpec::from_json(&serde_json::json!({
                "type": "core.diff",
                "left": 7,
                "right": 8,
            })),
            Ok(ViewSpec::diff(ContentId(7), ContentId(8)))
        );
        assert_eq!(
            ViewSpec::from_json(&serde_json::json!({
                "type": "defined",
                "definition": "example.diff",
                "bindings": { "left": 7, "right": 8 },
            })),
            Ok(ViewSpec::defined(
                vell_protocol::view::ViewDefinitionId::new("example.diff"),
                [
                    (BindingKey::new("left"), ContentId(7)),
                    (BindingKey::new("right"), ContentId(8)),
                ],
            ))
        );
    }

    #[test]
    fn view_spec_json_parser_has_one_strict_error_contract() {
        let invalid = [
            (
                serde_json::json!({
                    "type": "core.buffer",
                    "content": -1,
                }),
                "buffer view spec content must be a non-negative content id",
            ),
            (
                serde_json::json!({
                    "type": "core.diff",
                    "left": 1,
                    "right": 2,
                    "document": 3,
                }),
                "diff view spec contains an unknown field",
            ),
            (
                serde_json::json!({
                    "type": "defined",
                    "definition": "example.diff",
                    "bindings": { "left": -1 },
                }),
                "defined view spec binding 'left' must be a non-negative content id",
            ),
        ];

        for (value, expected) in invalid {
            assert_eq!(
                ViewSpec::from_json(&value).unwrap_err().to_string(),
                expected
            );
        }
    }
}
