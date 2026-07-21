use crate::action::{TransactionIntent, ViewAction};
use crate::command::{AppCommand, ModeCommand, ModeInputCommand};
use vell_core::action::ContentAction;
use vell_core::command::EditCommand;
use vell_protocol::ids::{ContentId, ViewId};
use vell_protocol::revision::Revision;
use vell_protocol::selection::Selections;
use vell_protocol::viewport::ViewportCommand;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentTarget {
    Current,
    #[allow(dead_code, reason = "explicit cross-content requests are reserved")]
    Id(ContentId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewTarget {
    Current,
    #[allow(dead_code, reason = "explicit cross-view requests are reserved")]
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
    Content {
        target: ContentTarget,
        operation: ContentOperation,
    },
    View {
        target: ViewTarget,
        operation: ViewOperation,
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
    App(AppOperation),
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
