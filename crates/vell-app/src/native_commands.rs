use crate::action::TransactionIntent;
use crate::command::AppCommand;
use crate::operation::{
    AppOperation, BufferOperation, ContentOperation, ContentTarget, OperationRequest,
};
use vell_mode::command_registry::{
    CommandEntry, CommandError, CommandHost, CommandId, CommandRegistry, CommandRequest,
    CommandValue,
};
use vell_protocol::ids::ContentId;
use vell_protocol::space::SplitDirection;

pub const NATIVE_COMMAND_IDS: &[&str] = &[
    "newBuffer",
    "switchBuffer",
    "save",
    "undo",
    "redo",
    "quit",
    "forceQuit",
    "closePane",
    "splitHorizontal",
    "splitVertical",
    "focusLeft",
    "focusDown",
    "focusUp",
    "focusRight",
];

pub(super) fn native_command_registry() -> CommandRegistry {
    let mut registry = CommandRegistry::new();
    register_no_args(&mut registry, "newBuffer", CommandRequest::CreateBuffer);
    registry.register(CommandEntry::new(
        command_id("switchBuffer"),
        |host: &mut dyn CommandHost, arguments: Vec<CommandValue>| {
            let content = one_content_id(arguments)?;
            host.request(CommandRequest::Execute(OperationRequest::Buffer(
                BufferOperation::Switch { content },
            )))
        },
    ));
    register_no_args(
        &mut registry,
        "save",
        CommandRequest::ExecuteAsync(OperationRequest::Content {
            target: ContentTarget::Current,
            operation: ContentOperation::Save,
        }),
    );
    register_no_args(&mut registry, "undo", history(TransactionIntent::Undo));
    register_no_args(&mut registry, "redo", history(TransactionIntent::Redo));
    register_no_args(&mut registry, "quit", app(AppCommand::Quit));
    register_no_args(&mut registry, "forceQuit", app(AppCommand::ForceQuit));
    register_no_args(&mut registry, "closePane", app(AppCommand::Close));
    register_no_args(
        &mut registry,
        "splitHorizontal",
        app(AppCommand::Split(SplitDirection::Down)),
    );
    register_no_args(
        &mut registry,
        "splitVertical",
        app(AppCommand::Split(SplitDirection::Right)),
    );
    register_no_args(
        &mut registry,
        "focusLeft",
        app(AppCommand::Focus(SplitDirection::Left)),
    );
    register_no_args(
        &mut registry,
        "focusDown",
        app(AppCommand::Focus(SplitDirection::Down)),
    );
    register_no_args(
        &mut registry,
        "focusUp",
        app(AppCommand::Focus(SplitDirection::Up)),
    );
    register_no_args(
        &mut registry,
        "focusRight",
        app(AppCommand::Focus(SplitDirection::Right)),
    );
    registry
}

pub fn native_command_ids() -> Vec<CommandId> {
    NATIVE_COMMAND_IDS.iter().map(|id| command_id(id)).collect()
}

fn register_no_args(registry: &mut CommandRegistry, id: &str, request: CommandRequest) {
    registry.register(CommandEntry::new(
        command_id(id),
        move |host: &mut dyn CommandHost, arguments: Vec<CommandValue>| {
            if arguments.is_empty() {
                host.request(request.clone())
            } else {
                Err(CommandError::InvalidArguments(
                    "expected no arguments".to_owned(),
                ))
            }
        },
    ));
}

fn execute(operation: OperationRequest) -> CommandRequest {
    CommandRequest::Execute(operation)
}

fn history(operation: TransactionIntent) -> CommandRequest {
    execute(OperationRequest::History {
        target: ContentTarget::Current,
        operation,
    })
}

fn app(command: AppCommand) -> CommandRequest {
    execute(OperationRequest::App(AppOperation::Command(command)))
}

fn command_id(value: &str) -> CommandId {
    CommandId::new(value).expect("native command ids are static and valid")
}

fn one_content_id(arguments: Vec<CommandValue>) -> Result<ContentId, CommandError> {
    let [value] = arguments.as_slice() else {
        return Err(CommandError::InvalidArguments(
            "expected one content id".to_owned(),
        ));
    };
    value.as_u64().map(ContentId).ok_or_else(|| {
        CommandError::InvalidArguments("content id must be a non-negative integer".to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exported_native_ids_match_the_registry() {
        let registry = native_command_registry();
        let registered = registry
            .iter()
            .map(|entry| entry.id().clone())
            .collect::<Vec<_>>();
        let mut exported = native_command_ids();
        exported.sort();

        assert_eq!(registered, exported);
    }

    #[test]
    fn exported_native_ids_match_the_typescript_seed_declarations() {
        let declarations = include_str!("../../../runtime/commands.generated.d.ts");
        let mut declared = declarations
            .lines()
            .skip_while(|line| !line.starts_with("interface EditorCommands"))
            .skip(1)
            .take_while(|line| *line != "}")
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("readonly ")?
                    .split_once(':')
                    .map(|(id, _)| id.to_owned())
            })
            .collect::<Vec<_>>();
        let mut exported = NATIVE_COMMAND_IDS
            .iter()
            .map(|id| (*id).to_owned())
            .collect::<Vec<_>>();
        declared.sort();
        exported.sort();

        assert_eq!(declared, exported);
    }
}
