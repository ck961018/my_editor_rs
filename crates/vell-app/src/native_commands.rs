use crate::action::TransactionIntent;
use crate::command::AppCommand;
use crate::operation::{
    AppOperation, BufferViewSource, ContentLifecycleOperation, ContentTarget, ModeFlowPropagation,
    ModeInvocation, ModeTarget, OperationRequest, ViewLifecycleOperation, ViewSpec,
};
use vell_mode::command::{ModeCommand, ModeValue};
use vell_mode::command_registry::{
    CommandEntry, CommandError, CommandHost, CommandId, CommandRegistry, CommandRequest,
    CommandValue,
};
use vell_mode::mode_name::{ModeActionName, ModeName};
use vell_protocol::ids::{ContentId, ViewId};
use vell_protocol::space::SplitDirection;
use vell_protocol::view::{BindingKey, RIGHT_BINDING};

pub const NATIVE_COMMAND_IDS: &[&str] = &[
    "content.create",
    "content.open",
    "content.list",
    "content.close",
    "content.save",
    "content.saveAs",
    "content.reload",
    "view.focus",
    "view.switch",
    "diff.setRightContent",
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
    "invokeMode",
];

pub(super) fn native_command_registry() -> CommandRegistry {
    let mut registry = CommandRegistry::new();
    register_no_args(
        &mut registry,
        "content.create",
        CommandRequest::CreateContent,
    );
    register_no_args(
        &mut registry,
        "content.list",
        content_lifecycle(ContentLifecycleOperation::List),
    );
    registry.register(CommandEntry::new(
        command_id("content.open"),
        |host: &mut dyn CommandHost, arguments: Vec<CommandValue>| {
            let path = one_string(arguments, "path")?;
            host.request(content_lifecycle(ContentLifecycleOperation::Open { path }))
        },
    ));
    registry.register(content_target_command("content.close", |target, force| {
        content_lifecycle(ContentLifecycleOperation::Close { target, force })
    }));
    registry.register(content_target_command("content.save", |target, force| {
        CommandRequest::ExecuteAsync(OperationRequest::ContentLifecycle(
            ContentLifecycleOperation::Save { target, force },
        ))
    }));
    registry.register(CommandEntry::new(
        command_id("content.saveAs"),
        |host: &mut dyn CommandHost, arguments: Vec<CommandValue>| {
            let (path, force) = path_and_force(arguments)?;
            host.request(content_lifecycle(ContentLifecycleOperation::SaveAs {
                target: ContentTarget::Current,
                path,
                force,
            }))
        },
    ));
    registry.register(content_target_command("content.reload", |target, force| {
        content_lifecycle(ContentLifecycleOperation::Reload { target, force })
    }));
    registry.register(CommandEntry::new(
        command_id("view.focus"),
        |host: &mut dyn CommandHost, arguments: Vec<CommandValue>| {
            let view = one_view_id(arguments)?;
            host.request(execute(OperationRequest::ViewLifecycle(
                ViewLifecycleOperation::Focus { view },
            )))
        },
    ));
    registry.register(CommandEntry::new(
        command_id("view.switch"),
        |host: &mut dyn CommandHost, arguments: Vec<CommandValue>| {
            let spec = one_view_spec(arguments)?;
            host.request(execute(OperationRequest::ViewLifecycle(
                ViewLifecycleOperation::Switch { spec },
            )))
        },
    ));
    registry.register(CommandEntry::new(
        command_id("diff.setRightContent"),
        |host: &mut dyn CommandHost, arguments: Vec<CommandValue>| {
            let content = one_content_id(arguments)?;
            host.request(execute(OperationRequest::ViewBinding {
                target: crate::operation::ViewTarget::Switchable,
                operation: crate::operation::ViewBindingOperation::Rebind {
                    binding: BindingKey::new(RIGHT_BINDING),
                    content: ContentTarget::Id(content),
                },
            }))
        },
    ));
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
    registry.register(CommandEntry::new(
        command_id("invokeMode"),
        |host: &mut dyn CommandHost, arguments: Vec<CommandValue>| {
            let (qualified, arguments) = match arguments.as_slice() {
                [qualified] => (qualified, ModeValue::Null),
                [qualified, arguments] => (qualified, command_value_to_mode(arguments)?),
                _ => {
                    return Err(CommandError::InvalidArguments(
                        "expected a qualified mode command and optional arguments".to_owned(),
                    ));
                }
            };
            let qualified = qualified.as_str().ok_or_else(|| {
                CommandError::InvalidArguments("mode command must be a string".to_owned())
            })?;
            let (mode, action) = qualified.rsplit_once('.').ok_or_else(|| {
                CommandError::InvalidArguments(
                    "mode command must use the qualified name 'mode.command'".to_owned(),
                )
            })?;
            if mode.is_empty() || action.is_empty() {
                return Err(CommandError::InvalidArguments(
                    "mode command must use the qualified name 'mode.command'".to_owned(),
                ));
            }
            host.request(CommandRequest::Execute(OperationRequest::Mode {
                target: ModeTarget::CurrentView,
                invocation: ModeInvocation {
                    command: ModeCommand::new(ModeName::new(mode), ModeActionName::new(action))
                        .with_arguments(arguments),
                    nested: true,
                    flow: ModeFlowPropagation::Isolate,
                },
            }))
        },
    ));
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

fn content_lifecycle(operation: ContentLifecycleOperation) -> CommandRequest {
    execute(OperationRequest::ContentLifecycle(operation))
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

fn one_view_id(arguments: Vec<CommandValue>) -> Result<ViewId, CommandError> {
    let [value] = arguments.as_slice() else {
        return Err(CommandError::InvalidArguments(
            "expected one view id".to_owned(),
        ));
    };
    value.as_u64().map(ViewId).ok_or_else(|| {
        CommandError::InvalidArguments("view id must be a non-negative integer".to_owned())
    })
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

fn one_string(arguments: Vec<CommandValue>, name: &str) -> Result<String, CommandError> {
    let [value] = arguments.as_slice() else {
        return Err(CommandError::InvalidArguments(format!(
            "expected one {name}"
        )));
    };
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| CommandError::InvalidArguments(format!("{name} must be a non-empty string")))
}

fn content_target_command(
    id: &str,
    request: impl Fn(ContentTarget, bool) -> CommandRequest + 'static,
) -> CommandEntry {
    CommandEntry::new(
        command_id(id),
        move |host: &mut dyn CommandHost, arguments: Vec<CommandValue>| {
            let (target, force) = content_target_and_force(arguments)?;
            host.request(request(target, force))
        },
    )
}

fn content_target_and_force(
    arguments: Vec<CommandValue>,
) -> Result<(ContentTarget, bool), CommandError> {
    let (target, force) = match arguments.as_slice() {
        [] => (ContentTarget::Current, false),
        [target] => (content_target(target)?, false),
        [target, force] => (
            content_target(target)?,
            force.as_bool().ok_or_else(|| {
                CommandError::InvalidArguments("force must be a boolean".to_owned())
            })?,
        ),
        _ => {
            return Err(CommandError::InvalidArguments(
                "expected an optional content id and force flag".to_owned(),
            ));
        }
    };
    Ok((target, force))
}

fn content_target(value: &CommandValue) -> Result<ContentTarget, CommandError> {
    if value.is_null() {
        return Ok(ContentTarget::Current);
    }
    value
        .as_u64()
        .map(ContentId)
        .map(ContentTarget::Id)
        .ok_or_else(|| {
            CommandError::InvalidArguments(
                "content id must be null or a non-negative integer".to_owned(),
            )
        })
}

fn path_and_force(arguments: Vec<CommandValue>) -> Result<(String, bool), CommandError> {
    let [path, rest @ ..] = arguments.as_slice() else {
        return Err(CommandError::InvalidArguments(
            "expected a path and optional force flag".to_owned(),
        ));
    };
    let path = path
        .as_str()
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            CommandError::InvalidArguments("path must be a non-empty string".to_owned())
        })?;
    let force = match rest {
        [] => false,
        [force] => force
            .as_bool()
            .ok_or_else(|| CommandError::InvalidArguments("force must be a boolean".to_owned()))?,
        _ => {
            return Err(CommandError::InvalidArguments(
                "expected a path and optional force flag".to_owned(),
            ));
        }
    };
    Ok((path, force))
}

fn one_view_spec(arguments: Vec<CommandValue>) -> Result<ViewSpec, CommandError> {
    let [value] = arguments.as_slice() else {
        return Err(CommandError::InvalidArguments(
            "expected one view spec".to_owned(),
        ));
    };
    let object = value
        .as_object()
        .ok_or_else(|| CommandError::InvalidArguments("view spec must be an object".to_owned()))?;
    if object.get("type").and_then(CommandValue::as_str) == Some("core.diff") {
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "type" | "left" | "right"))
        {
            return Err(CommandError::InvalidArguments(
                "diff view spec contains an unknown field".to_owned(),
            ));
        }
        let content = |name: &str| {
            object
                .get(name)
                .and_then(CommandValue::as_u64)
                .map(ContentId)
                .ok_or_else(|| {
                    CommandError::InvalidArguments(format!(
                        "diff view spec {name} must be a non-negative content id"
                    ))
                })
        };
        return Ok(ViewSpec::diff(content("left")?, content("right")?));
    }
    if object.get("type").and_then(CommandValue::as_str) != Some("core.buffer") {
        return Err(CommandError::InvalidArguments(
            "view spec type must be 'core.buffer' or 'core.diff'".to_owned(),
        ));
    }
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "type" | "content" | "create" | "path"))
    {
        return Err(CommandError::InvalidArguments(
            "buffer view spec contains an unknown field".to_owned(),
        ));
    }
    let sources = [
        object.contains_key("content"),
        object.contains_key("create"),
        object.contains_key("path"),
    ];
    if sources.into_iter().filter(|present| *present).count() != 1 {
        return Err(CommandError::InvalidArguments(
            "buffer view spec requires exactly one of content, create, or path".to_owned(),
        ));
    }
    let source = if let Some(content) = object.get("content") {
        let content = content.as_u64().map(ContentId).ok_or_else(|| {
            CommandError::InvalidArguments(
                "view spec content must be a non-negative integer".to_owned(),
            )
        })?;
        BufferViewSource::Content(content)
    } else if let Some(create) = object.get("create") {
        if create.as_bool() != Some(true) {
            return Err(CommandError::InvalidArguments(
                "view spec create must be true".to_owned(),
            ));
        }
        BufferViewSource::Create
    } else {
        let path = object
            .get("path")
            .and_then(CommandValue::as_str)
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                CommandError::InvalidArguments(
                    "view spec path must be a non-empty string".to_owned(),
                )
            })?;
        BufferViewSource::Open { path }
    };
    Ok(ViewSpec::Buffer { source })
}

fn command_value_to_mode(value: &CommandValue) -> Result<ModeValue, CommandError> {
    match value {
        CommandValue::Null => Ok(ModeValue::Null),
        CommandValue::Bool(value) => Ok(ModeValue::Bool(*value)),
        CommandValue::Number(value) => value.as_i64().map(ModeValue::Integer).ok_or_else(|| {
            CommandError::InvalidArguments("mode arguments require integer numbers".to_owned())
        }),
        CommandValue::String(value) => Ok(ModeValue::String(value.clone())),
        CommandValue::Array(values) => values
            .iter()
            .map(command_value_to_mode)
            .collect::<Result<Vec<_>, _>>()
            .map(ModeValue::List),
        CommandValue::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), command_value_to_mode(value)?)))
            .collect::<Result<_, _>>()
            .map(ModeValue::Map),
    }
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
    fn legacy_buffer_command_surface_is_not_registered() {
        let registry = native_command_registry();

        for legacy in ["newBuffer", "switchBuffer", "save"] {
            let id = CommandId::new(legacy).unwrap();
            assert!(
                registry.get(&id).is_none(),
                "legacy command {legacy} leaked"
            );
        }
    }

    #[test]
    fn view_switch_accepts_closed_buffer_and_diff_specs() {
        assert_eq!(
            one_view_spec(vec![serde_json::json!({
                "type": "core.buffer",
                "content": 7,
            })]),
            Ok(ViewSpec::buffer(ContentId(7)))
        );
        assert_eq!(
            one_view_spec(vec![serde_json::json!({
                "type": "core.buffer",
                "create": true,
            })]),
            Ok(ViewSpec::Buffer {
                source: BufferViewSource::Create,
            })
        );
        assert!(one_view_spec(vec![serde_json::json!(7)]).is_err());
        assert!(
            one_view_spec(vec![serde_json::json!({
                "type": "core.buffer",
                "content": 7,
                "create": true,
            })])
            .is_err()
        );
        assert_eq!(
            one_view_spec(vec![serde_json::json!({
                "type": "core.diff",
                "left": 7,
                "right": 8,
            })]),
            Ok(ViewSpec::diff(ContentId(7), ContentId(8)))
        );
        assert!(
            one_view_spec(vec![serde_json::json!({
                "type": "core.diff",
                "left": 7,
            })])
            .is_err()
        );
        assert!(
            one_view_spec(vec![serde_json::json!({
                "type": "core.diff",
                "left": 7,
                "right": 8,
                "document": 9,
            })])
            .is_err()
        );
    }

    #[test]
    fn exported_native_ids_match_the_typescript_seed_declarations() {
        let declarations = include_str!("../../../runtime/commands.generated.d.ts");
        let mut declared = declarations
            .lines()
            .skip_while(|line| !line.starts_with("interface EditorCommandSeeds"))
            .skip(1)
            .take_while(|line| *line != "}")
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("readonly ")?
                    .split_once(':')
                    .map(|(id, _)| id.trim_matches('"').to_owned())
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
