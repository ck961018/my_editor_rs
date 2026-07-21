use std::collections::VecDeque;
use std::fmt;

use crate::app::action::{TransactionIntent, ViewAction};
use crate::app::command::{
    AppCommand, ContentCommand, ModeCommand, ModeInputCommand, TransactionCommand,
};
use crate::app::dispatcher::DispatchCommand;
use crate::app::mode::ModeId;
use crate::core::action::ContentAction;
use crate::core::command::EditCommand;
use crate::protocol::ids::{ContentId, ViewId};
use crate::protocol::revision::Revision;
use crate::protocol::selection::Selections;
use crate::protocol::viewport::ViewportCommand;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationOriginScope {
    App,
    Content,
    View,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperationOrigin {
    pub scope: OperationOriginScope,
    pub view: Option<ViewId>,
    pub content: Option<ContentId>,
    pub mode: Option<ModeId>,
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedOperation {
    Content {
        content: ContentId,
        operation: ContentOperation,
    },
    View {
        view: ViewId,
        content: ContentId,
        operation: ViewOperation,
    },
    History {
        content: ContentId,
        owner: Option<ViewId>,
        operation: TransactionIntent,
    },
    Mode {
        mode: ModeId,
        scope: ResolvedModeScope,
        invocation: ModeInvocation,
    },
    ModeInput {
        mode: ModeId,
        view: ViewId,
        content: ContentId,
        input: ModeInputCommand,
    },
    App(AppOperation),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolvedModeScope {
    Content {
        content: ContentId,
        source_view: Option<ViewId>,
    },
    View {
        view: ViewId,
        content: ContentId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedOperation {
    pub request: OperationRequest,
    pub origin: OperationOrigin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationError(String);

impl OperationOrigin {
    pub fn app() -> Self {
        Self {
            scope: OperationOriginScope::App,
            view: None,
            content: None,
            mode: None,
        }
    }

    pub fn content(content: ContentId, source_view: Option<ViewId>) -> Self {
        Self {
            scope: OperationOriginScope::Content,
            view: source_view,
            content: Some(content),
            mode: None,
        }
    }

    pub fn view(view: ViewId, content: ContentId) -> Self {
        Self {
            scope: OperationOriginScope::View,
            view: Some(view),
            content: Some(content),
            mode: None,
        }
    }
}

impl OperationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for OperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for OperationError {}

pub fn adapt_dispatch_command(
    command: DispatchCommand,
) -> Result<Vec<QueuedOperation>, OperationError> {
    let (origin, requests) = match command {
        DispatchCommand::App(command) => (
            OperationOrigin::app(),
            vec![OperationRequest::App(AppOperation::Command(command))],
        ),
        DispatchCommand::Content { command, content } => {
            let origin = OperationOrigin::content(content, None);
            let requests = adapt_content_command(command, false)?;
            (origin, requests)
        }
        DispatchCommand::ContentWithView {
            command,
            view,
            content,
        } => {
            let origin = OperationOrigin::view(view, content);
            let requests = adapt_content_command(command, true)?;
            (origin, requests)
        }
        DispatchCommand::Mode {
            command,
            view,
            content,
        } => (
            OperationOrigin::view(view, content),
            vec![OperationRequest::Mode {
                target: ModeTarget::CurrentView,
                invocation: ModeInvocation {
                    command,
                    nested: false,
                    flow: ModeFlowPropagation::Propagate,
                },
            }],
        ),
        DispatchCommand::ModeInput {
            input,
            view,
            content,
        } => (
            OperationOrigin::view(view, content),
            vec![OperationRequest::ModeInput {
                target: ViewTarget::Current,
                input,
            }],
        ),
        DispatchCommand::Viewport {
            command,
            view,
            content,
        } => (
            OperationOrigin::view(view, content),
            vec![OperationRequest::View {
                target: ViewTarget::Current,
                operation: ViewOperation::Viewport(command),
            }],
        ),
        DispatchCommand::ModeContentOperations {
            operations,
            content,
        } => (OperationOrigin::content(content, None), operations),
        DispatchCommand::ModeOperations {
            operations,
            view,
            content,
        } => (OperationOrigin::view(view, content), operations),
        DispatchCommand::Noop => (OperationOrigin::app(), Vec::new()),
    };
    Ok(requests
        .into_iter()
        .map(|request| QueuedOperation { request, origin })
        .collect())
}

pub fn prepend_operations(
    queue: &mut VecDeque<QueuedOperation>,
    origin: OperationOrigin,
    requests: Vec<OperationRequest>,
) {
    for request in requests.into_iter().rev() {
        queue.push_front(QueuedOperation { request, origin });
    }
}

fn adapt_content_command(
    command: ContentCommand,
    with_view: bool,
) -> Result<Vec<OperationRequest>, OperationError> {
    match command {
        ContentCommand::Edit(command) if with_view => Ok(vec![OperationRequest::View {
            target: ViewTarget::Current,
            operation: ViewOperation::Edit(command),
        }]),
        ContentCommand::Transaction(command) if with_view => {
            Ok(vec![history_request(transaction_intent(command))])
        }
        ContentCommand::Undo if with_view => Ok(vec![history_request(TransactionIntent::Undo)]),
        ContentCommand::Redo if with_view => Ok(vec![history_request(TransactionIntent::Redo)]),
        ContentCommand::Sequence(commands) if with_view => {
            let mut requests = Vec::new();
            for command in commands.into_commands() {
                requests.extend(adapt_content_command(command, true)?);
            }
            Ok(requests)
        }
        ContentCommand::Save if !with_view => Ok(vec![OperationRequest::Content {
            target: ContentTarget::Current,
            operation: ContentOperation::Save,
        }]),
        _ => Err(OperationError::new(
            "content command is incompatible with its execution origin",
        )),
    }
}

fn history_request(operation: TransactionIntent) -> OperationRequest {
    OperationRequest::History {
        target: ContentTarget::Current,
        operation,
    }
}

fn transaction_intent(command: TransactionCommand) -> TransactionIntent {
    match command {
        TransactionCommand::Begin => TransactionIntent::Begin,
        TransactionCommand::Commit => TransactionIntent::Commit,
        TransactionCommand::Rollback => TransactionIntent::Rollback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_adapter_preserves_order_and_one_origin() {
        let command = DispatchCommand::ContentWithView {
            command: ContentCommand::try_sequence(vec![
                ContentCommand::Transaction(TransactionCommand::Begin),
                ContentCommand::Edit(EditCommand::MoveLeftBy(1)),
                ContentCommand::Undo,
            ])
            .unwrap(),
            view: ViewId(7),
            content: ContentId(9),
        };

        let operations = adapt_dispatch_command(command).unwrap();

        assert_eq!(operations.len(), 3);
        assert!(matches!(
            operations[0].request,
            OperationRequest::History {
                operation: TransactionIntent::Begin,
                ..
            }
        ));
        assert!(matches!(
            operations[1].request,
            OperationRequest::View {
                operation: ViewOperation::Edit(EditCommand::MoveLeftBy(1)),
                ..
            }
        ));
        assert!(matches!(
            operations[2].request,
            OperationRequest::History {
                operation: TransactionIntent::Undo,
                ..
            }
        ));
        assert!(operations.iter().all(|operation| {
            operation.origin == OperationOrigin::view(ViewId(7), ContentId(9))
        }));
    }

    #[test]
    fn noop_dispatch_has_no_operations() {
        assert!(
            adapt_dispatch_command(DispatchCommand::Noop)
                .unwrap()
                .is_empty()
        );
    }
}
