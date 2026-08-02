use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use unicode_id_start::{is_id_continue, is_id_start};

use crate::operation::OperationRequest;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandId(String);

impl CommandId {
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidCommandId> {
        let value = value.into();
        if is_command_id(&value) {
            Ok(Self(value))
        } else {
            Err(InvalidCommandId { value })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl TryFrom<&str> for CommandId {
    type Error = InvalidCommandId;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for CommandId {
    type Error = InvalidCommandId;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidCommandId {
    value: String,
}

impl InvalidCommandId {
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for InvalidCommandId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid command id '{}': expected dot-separated TypeScript identifiers",
            self.value
        )
    }
}

impl std::error::Error for InvalidCommandId {}

fn is_command_id(value: &str) -> bool {
    !value.is_empty() && value.split('.').all(is_identifier)
}

fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters.next().is_some_and(is_identifier_start) && characters.all(is_identifier_continue)
}

fn is_identifier_start(character: char) -> bool {
    character == '$' || character == '_' || is_id_start(character)
}

fn is_identifier_continue(character: char) -> bool {
    character == '$'
        || character == '\u{200c}'
        || character == '\u{200d}'
        || is_id_continue(character)
}

pub type CommandValue = serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandInvocation {
    pub command: CommandId,
    pub arguments: Vec<CommandValue>,
}

impl CommandInvocation {
    pub fn new(command: CommandId, arguments: Vec<CommandValue>) -> Self {
        Self { command, arguments }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandError {
    UnknownCommand(CommandId),
    InvalidArguments(String),
    RecursionLimit { limit: usize },
    Failed(String),
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCommand(command) => write!(formatter, "unknown command '{command}'"),
            Self::InvalidArguments(message) => {
                write!(formatter, "invalid command arguments: {message}")
            }
            Self::RecursionLimit { limit } => {
                write!(formatter, "command recursion limit of {limit} exceeded")
            }
            Self::Failed(message) => message.fmt(formatter),
        }
    }
}

impl std::error::Error for CommandError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandCompletion {
    Ready(CommandValue),
    Pending,
}

impl From<CommandValue> for CommandCompletion {
    fn from(value: CommandValue) -> Self {
        Self::Ready(value)
    }
}

pub type CommandResult = Result<CommandCompletion, CommandError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandQuery {
    CurrentContent,
    CurrentView,
    CurrentText,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandRequest {
    Execute(OperationRequest),
    CreateBuffer,
    Query(CommandQuery),
}

pub trait CommandHost {
    fn invoke_command(&mut self, invocation: CommandInvocation) -> CommandResult;

    fn request(&mut self, request: CommandRequest) -> CommandResult;

    /// Installs a definition immediately. Definition state is intentionally
    /// outside the host mutation journal used by an execution frame.
    fn register_command(&mut self, entry: CommandEntry);
}

pub trait CommandAdapter {
    fn invoke(&self, host: &mut dyn CommandHost, arguments: Vec<CommandValue>) -> CommandResult;
}

impl<F> CommandAdapter for F
where
    F: Fn(&mut dyn CommandHost, Vec<CommandValue>) -> CommandResult,
{
    fn invoke(&self, host: &mut dyn CommandHost, arguments: Vec<CommandValue>) -> CommandResult {
        self(host, arguments)
    }
}

#[derive(Clone)]
pub struct CommandEntry {
    id: CommandId,
    adapter: Rc<dyn CommandAdapter>,
}

impl CommandEntry {
    pub fn new(id: CommandId, adapter: impl CommandAdapter + 'static) -> Self {
        Self {
            id,
            adapter: Rc::new(adapter),
        }
    }

    pub fn id(&self) -> &CommandId {
        &self.id
    }

    pub fn invoke(
        &self,
        host: &mut dyn CommandHost,
        arguments: Vec<CommandValue>,
    ) -> CommandResult {
        self.adapter.invoke(host, arguments)
    }
}

impl fmt::Debug for CommandEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommandEntry")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
pub struct CommandRegistry {
    entries: BTreeMap<CommandId, CommandEntry>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, entry: CommandEntry) -> Option<CommandEntry> {
        self.entries.insert(entry.id.clone(), entry)
    }

    pub fn get(&self, id: &CommandId) -> Option<&CommandEntry> {
        self.entries.get(id)
    }

    /// Returns an owned entry so the registry is not borrowed during execution.
    pub fn resolve(&self, id: &CommandId) -> Option<CommandEntry> {
        self.entries.get(id).cloned()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &CommandEntry> {
        self.entries.values()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn id(value: &str) -> CommandId {
        CommandId::new(value).unwrap()
    }

    #[test]
    fn validates_dot_separated_typescript_identifiers() {
        for value in ["save", "buffer.save", "$tools._save2", "编辑器.保存"] {
            assert_eq!(CommandId::new(value).unwrap().as_str(), value);
        }

        for value in ["", ".save", "save.", "buffer..save", "2save", "save-as"] {
            let error = CommandId::new(value).unwrap_err();
            assert_eq!(error.value(), value);
        }
    }

    #[test]
    fn command_values_cover_json_numbers() {
        assert_eq!(CommandValue::from(1.5).as_f64(), Some(1.5));
    }

    #[test]
    fn invokes_native_adapter_and_returns_owned_value() {
        let mut registry = CommandRegistry::new();
        registry.register(CommandEntry::new(
            id("math.echo"),
            |_host: &mut dyn CommandHost, arguments: Vec<CommandValue>| {
                arguments
                    .into_iter()
                    .next()
                    .map(CommandCompletion::from)
                    .ok_or_else(|| {
                        CommandError::InvalidArguments("expected one argument".to_owned())
                    })
            },
        ));
        let registry = Rc::new(RefCell::new(registry));
        let mut host = TestHost::new(Rc::clone(&registry), 8);

        let result = host
            .invoke_command(CommandInvocation::new(
                id("math.echo"),
                vec![CommandValue::from("hello")],
            ))
            .unwrap();

        assert_eq!(result, CommandCompletion::from(CommandValue::from("hello")));
    }

    #[test]
    fn replacement_updates_existing_id_invocations() {
        use crate::command::Command;
        use vell_core::keymap::Keymap;
        use vell_protocol::key_event::KeyEvent;

        let mut registry = CommandRegistry::new();
        registry.register(constant_entry("sample.command", "old"));
        let invocation = CommandInvocation::new(id("sample.command"), Vec::new());
        let key = KeyEvent::char('x');
        let mut keymap = Keymap::new();
        keymap.bind([key], Command::Registered(invocation));
        assert!(
            registry
                .register(constant_entry("sample.command", "new"))
                .is_some()
        );
        let registry = Rc::new(RefCell::new(registry));
        let mut host = TestHost::new(Rc::clone(&registry), 8);
        let Command::Registered(invocation) = keymap.node(&[key]).unwrap().action().unwrap() else {
            panic!("registered key binding changed command kind");
        };

        assert_eq!(
            host.invoke_command(invocation.clone()).unwrap(),
            CommandCompletion::from(CommandValue::from("new"))
        );
    }

    #[test]
    fn adapter_can_replace_a_command_while_it_is_running() {
        let registry = Rc::new(RefCell::new(CommandRegistry::new()));
        let registry_for_adapter = Rc::clone(&registry);
        registry.borrow_mut().register(CommandEntry::new(
            id("sample.command"),
            move |_host: &mut dyn CommandHost, _arguments: Vec<CommandValue>| {
                registry_for_adapter
                    .borrow_mut()
                    .register(constant_entry("sample.command", "new"));
                Ok(CommandValue::from("old").into())
            },
        ));
        let mut host = TestHost::new(Rc::clone(&registry), 8);
        let invocation = CommandInvocation::new(id("sample.command"), Vec::new());

        assert_eq!(
            host.invoke_command(invocation.clone()).unwrap(),
            CommandCompletion::from(CommandValue::from("old"))
        );
        assert_eq!(
            host.invoke_command(invocation).unwrap(),
            CommandCompletion::from(CommandValue::from("new"))
        );
    }

    #[test]
    fn iterates_in_command_id_order() {
        let mut registry = CommandRegistry::new();
        registry.register(constant_entry("z.last", "z"));
        registry.register(constant_entry("a.first", "a"));

        let ids = registry
            .iter()
            .map(|entry| entry.id().as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, ["a.first", "z.last"]);
    }

    #[test]
    fn reports_unknown_commands_and_argument_errors() {
        let registry = Rc::new(RefCell::new(CommandRegistry::new()));
        let mut host = TestHost::new(Rc::clone(&registry), 8);

        assert_eq!(
            host.invoke_command(CommandInvocation::new(id("missing"), Vec::new())),
            Err(CommandError::UnknownCommand(id("missing")))
        );

        registry.borrow_mut().register(CommandEntry::new(
            id("needs.argument"),
            |_host: &mut dyn CommandHost, arguments: Vec<CommandValue>| {
                if arguments.is_empty() {
                    Err(CommandError::InvalidArguments(
                        "expected one argument".to_owned(),
                    ))
                } else {
                    Ok(CommandValue::Null.into())
                }
            },
        ));

        assert_eq!(
            host.invoke_command(CommandInvocation::new(id("needs.argument"), Vec::new())),
            Err(CommandError::InvalidArguments(
                "expected one argument".to_owned()
            ))
        );
    }

    #[test]
    fn host_enforces_a_nested_invocation_budget() {
        let mut registry = CommandRegistry::new();
        registry.register(CommandEntry::new(
            id("recurse"),
            |host: &mut dyn CommandHost, _arguments: Vec<CommandValue>| {
                host.invoke_command(CommandInvocation::new(id("recurse"), Vec::new()))
            },
        ));
        let registry = Rc::new(RefCell::new(registry));
        let mut host = TestHost::new(Rc::clone(&registry), 3);

        assert_eq!(
            host.invoke_command(CommandInvocation::new(id("recurse"), Vec::new())),
            Err(CommandError::RecursionLimit { limit: 3 })
        );
        assert_eq!(host.depth, 0);
    }

    fn constant_entry(command: &str, value: &'static str) -> CommandEntry {
        CommandEntry::new(
            id(command),
            move |_host: &mut dyn CommandHost, _arguments: Vec<CommandValue>| {
                Ok(CommandValue::from(value).into())
            },
        )
    }

    struct TestHost {
        registry: Rc<RefCell<CommandRegistry>>,
        depth: usize,
        limit: usize,
    }

    impl TestHost {
        fn new(registry: Rc<RefCell<CommandRegistry>>, limit: usize) -> Self {
            Self {
                registry,
                depth: 0,
                limit,
            }
        }
    }

    impl CommandHost for TestHost {
        fn invoke_command(&mut self, invocation: CommandInvocation) -> CommandResult {
            if self.depth == self.limit {
                return Err(CommandError::RecursionLimit { limit: self.limit });
            }

            self.depth += 1;
            let registry = Rc::clone(&self.registry);
            let entry = registry.borrow().resolve(&invocation.command);
            let result = entry
                .ok_or(CommandError::UnknownCommand(invocation.command))
                .and_then(|entry| entry.invoke(self, invocation.arguments));
            self.depth -= 1;
            result
        }

        fn request(&mut self, _request: CommandRequest) -> CommandResult {
            Err(CommandError::Failed(
                "test host does not support requests".to_owned(),
            ))
        }

        fn register_command(&mut self, entry: CommandEntry) {
            self.registry.borrow_mut().register(entry);
        }
    }
}
