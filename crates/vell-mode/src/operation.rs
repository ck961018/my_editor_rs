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
    Buffer { source: BufferViewSource },
    Diff { left: ContentId, right: ContentId },
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
