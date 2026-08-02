use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::ptr;
use std::rc::Rc;

use vell_mode::command_registry::{
    CommandAdapter, CommandCompletion, CommandEntry, CommandError, CommandHost, CommandId,
    CommandInvocation, CommandResult, CommandValue,
};

use super::{
    ScriptError, ScriptHost, ScriptInvocationKind, call_script_callback, current_exception,
    json_to_v8, perform_microtask_checkpoint, throw_script_error, v8_to_json,
};

const RESERVED_ROOTS: &[&str] = &["$script", "register", "shortcut"];

#[derive(Clone, Default)]
pub(super) struct ScriptCommands {
    definitions: BTreeMap<CommandId, v8::Global<v8::Function>>,
    shortcuts: BTreeMap<String, v8::Global<v8::Function>>,
    changes: Vec<CommandId>,
    fallback_roots: BTreeMap<String, v8::Global<v8::Value>>,
}

impl ScriptCommands {
    pub(super) fn install_api(&self, scope: &mut v8::PinScope<'_, '_>) {
        let editor = editor_object(scope).expect("editor API is installed first");
        let commands = v8::Object::new(scope);
        let register = v8::FunctionTemplate::new(scope, register_command)
            .get_function(scope)
            .expect("command registration function");
        set(scope, commands, "register", register.into());
        let shortcut = v8::FunctionTemplate::new(scope, register_shortcut)
            .get_function(scope)
            .expect("shortcut registration function");
        set(scope, commands, "shortcut", shortcut.into());
        set(scope, editor, "commands", commands.into());
    }

    fn register(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        id: CommandId,
        callback: v8::Local<v8::Function>,
    ) -> Result<(), ScriptError> {
        ensure_available_root(&id)?;
        self.install_value(scope, &id, callback.into())?;
        self.definitions
            .insert(id.clone(), v8::Global::new(scope, callback));
        self.changes.push(id);
        Ok(())
    }

    fn register_shortcut(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        name: String,
        callback: v8::Local<v8::Function>,
    ) -> Result<(), ScriptError> {
        if name.is_empty() || name.chars().any(char::is_whitespace) {
            return Err(ScriptError::new(
                "shortcut name must be non-empty and contain no whitespace",
            ));
        }
        self.shortcuts
            .insert(name, v8::Global::new(scope, callback));
        Ok(())
    }

    pub(super) fn install_native(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        id: &CommandId,
    ) -> Result<(), ScriptError> {
        ensure_available_root(id)?;
        if self.definitions.contains_key(id) {
            return Ok(());
        }
        let data = v8::String::new(scope, id.as_str())
            .ok_or_else(|| ScriptError::new("command id is too large for V8"))?;
        let function = v8::FunctionTemplate::builder(invoke_native_command)
            .data(data.into())
            .build(scope)
            .get_function(scope)
            .ok_or_else(|| ScriptError::new("failed to create native command wrapper"))?;
        self.install_value(scope, id, function.into())
    }

    fn install_value(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        id: &CommandId,
        value: v8::Local<v8::Value>,
    ) -> Result<(), ScriptError> {
        let commands = commands_object(scope)?;
        let mut segments = id.as_str().split('.').peekable();
        let root = segments
            .peek()
            .expect("validated command ids have at least one segment")
            .to_string();
        let mut parent = commands;
        while let Some(segment) = segments.next() {
            let key = v8::String::new(scope, segment)
                .ok_or_else(|| ScriptError::new("command id is too large for V8"))?;
            if segments.peek().is_none() {
                if let Some(previous) = parent.get(scope, key.into()) {
                    preserve_command_children(scope, previous, value)?;
                }
                if parent.set(scope, key.into(), value) != Some(true) {
                    return Err(ScriptError::new(format!(
                        "failed to install command '{id}'"
                    )));
                }
                break;
            }
            let child = parent
                .get(scope, key.into())
                .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
                .unwrap_or_else(|| {
                    let child = v8::Object::new(scope);
                    parent.set(scope, key.into(), child.into());
                    child
                });
            parent = child;
        }

        self.update_global_fallback(scope, &root, commands)
    }

    fn update_global_fallback(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        root: &str,
        commands: v8::Local<v8::Object>,
    ) -> Result<(), ScriptError> {
        let key = v8::String::new(scope, root)
            .ok_or_else(|| ScriptError::new("command id is too large for V8"))?;
        let root_value = commands
            .get(scope, key.into())
            .ok_or_else(|| ScriptError::new("failed to read command namespace"))?;
        let global = scope.get_current_context().global(scope);
        let current = global
            .get(scope, key.into())
            .ok_or_else(|| ScriptError::new("failed to inspect global command fallback"))?;
        let owned_fallback = self.fallback_roots.get(root).cloned();
        let owns_fallback = owned_fallback
            .map(|previous| current.strict_equals(v8::Local::new(scope, previous)))
            .unwrap_or(false);
        if current.is_undefined() || owns_fallback {
            if global.set(scope, key.into(), root_value) != Some(true) {
                return Err(ScriptError::new(
                    "failed to install global command fallback",
                ));
            }
            self.fallback_roots
                .insert(root.to_owned(), v8::Global::new(scope, root_value));
        }
        Ok(())
    }

    fn callback(&self, id: &CommandId) -> Option<v8::Global<v8::Function>> {
        self.definitions.get(id).cloned()
    }

    fn change_count(&self) -> usize {
        self.changes.len()
    }

    fn changes_since(&self, count: usize) -> Vec<CommandId> {
        self.changes[count..].to_vec()
    }

    fn ids(&self) -> Vec<CommandId> {
        self.definitions.keys().cloned().collect()
    }
}

pub(super) struct ActiveCommandHost {
    pointer: Cell<*mut ()>,
    invoking: Cell<bool>,
}

impl Default for ActiveCommandHost {
    fn default() -> Self {
        Self {
            pointer: Cell::new(ptr::null_mut()),
            invoking: Cell::new(false),
        }
    }
}

pub(super) struct ScopedHost<'a> {
    pub(super) host: &'a mut dyn CommandHost,
}

pub(super) struct ActiveHostGuard<'a> {
    bridge: Rc<ActiveCommandHost>,
    _host: PhantomData<&'a mut ScopedHost<'a>>,
}

impl ActiveCommandHost {
    pub(super) fn activate<'a>(
        self: &Rc<Self>,
        host: &'a mut ScopedHost<'a>,
    ) -> Result<ActiveHostGuard<'a>, ScriptError> {
        if !self.pointer.get().is_null() {
            return Err(ScriptError::new("a command host is already active"));
        }
        self.pointer.set(host as *mut ScopedHost<'a> as *mut ());
        Ok(ActiveHostGuard {
            bridge: Rc::clone(self),
            _host: PhantomData,
        })
    }

    fn invoke(&self, invocation: CommandInvocation) -> CommandResult {
        let pointer = self.pointer.get();
        if pointer.is_null() {
            return Err(CommandError::Failed(
                "native command wrapper is outside an active command callback".to_owned(),
            ));
        }
        if self.invoking.replace(true) {
            return Err(CommandError::Failed(
                "native command wrapper is already executing".to_owned(),
            ));
        }
        struct InvokingGuard<'a>(&'a Cell<bool>);
        impl Drop for InvokingGuard<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        let _guard = InvokingGuard(&self.invoking);
        // SAFETY: `activate` stores a pointer to a stack value and its guard
        // clears the pointer before that value can be dropped. V8 callbacks
        // run synchronously on the isolate thread. Lifetimes have no runtime
        // representation, so the erased wrapper has the same layout here.
        let host = unsafe { &mut *(pointer.cast::<ScopedHost<'static>>()) };
        host.host.invoke_command(invocation)
    }
}

impl Drop for ActiveHostGuard<'_> {
    fn drop(&mut self) {
        self.bridge.pointer.set(ptr::null_mut());
        self.bridge.invoking.set(false);
    }
}

#[derive(Clone)]
struct ScriptCommandAdapter {
    host: Rc<RefCell<ScriptHost>>,
    id: CommandId,
}

impl ScriptCommandAdapter {
    pub(super) fn entry(host: &Rc<RefCell<ScriptHost>>, id: CommandId) -> CommandEntry {
        CommandEntry::new(
            id.clone(),
            Self {
                host: Rc::clone(host),
                id,
            },
        )
    }
}

impl CommandAdapter for ScriptCommandAdapter {
    fn invoke(&self, host: &mut dyn CommandHost, arguments: Vec<CommandValue>) -> CommandResult {
        let change_count = self.host.borrow().commands.borrow().change_count();
        let result = self
            .host
            .try_borrow_mut()
            .map_err(|_| CommandError::Failed("script command runtime is reentrant".to_owned()))?
            .execute_command(&self.id, host, arguments)
            .map_err(|error| CommandError::Failed(error.to_string()));
        publish_changes(&self.host, host, change_count);
        result
    }
}

pub(super) fn publish_changes(
    script: &Rc<RefCell<ScriptHost>>,
    host: &mut dyn CommandHost,
    change_count: usize,
) {
    let changes = script
        .borrow()
        .commands
        .borrow()
        .changes_since(change_count);
    for id in changes {
        host.register_command(ScriptCommandAdapter::entry(script, id));
    }
}

pub(super) fn change_count(script: &Rc<RefCell<ScriptHost>>) -> usize {
    script.borrow().commands.borrow().change_count()
}

pub(crate) fn entries(host: &Rc<RefCell<ScriptHost>>) -> Vec<CommandEntry> {
    let ids = host.borrow().commands.borrow().ids();
    let mut entries = ids
        .into_iter()
        .map(|id| ScriptCommandAdapter::entry(host, id))
        .collect::<Vec<_>>();
    entries.push(super::global_script::entry(host));
    entries
}

fn register_command(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let parsed = (|| {
        let (id, callback) = match arguments.length() {
            1 => {
                let callback = v8::Local::<v8::Function>::try_from(arguments.get(0))
                    .map_err(|_| ScriptError::new("commands.register expects a function"))?;
                let name = callback.get_name(scope).to_rust_string_lossy(scope);
                if name.is_empty() {
                    return Err(ScriptError::new(
                        "commands.register(function) requires a named function",
                    ));
                }
                (name, callback)
            }
            2 => {
                if !arguments.get(0).is_string() {
                    return Err(ScriptError::new("command id must be a string"));
                }
                let id = arguments.get(0).to_rust_string_lossy(scope);
                let callback = v8::Local::<v8::Function>::try_from(arguments.get(1))
                    .map_err(|_| ScriptError::new("command callback must be a function"))?;
                (id, callback)
            }
            _ => {
                return Err(ScriptError::new(
                    "commands.register expects a function or an id and function",
                ));
            }
        };
        let id = CommandId::new(id).map_err(|error| ScriptError::new(error.to_string()))?;
        ensure_available_root(&id)?;
        let registry = scope
            .get_slot::<Rc<RefCell<ScriptCommands>>>()
            .cloned()
            .ok_or_else(|| ScriptError::new("command registry is unavailable"))?;
        registry.borrow_mut().register(scope, id, callback)
    })();
    match parsed {
        Ok(()) => return_value.set_undefined(),
        Err(error) => throw_script_error(scope, &error.to_string()),
    }
}

fn register_shortcut(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let result = (|| {
        if arguments.length() != 2 || !arguments.get(0).is_string() {
            return Err(ScriptError::new(
                "commands.shortcut expects a name and callback",
            ));
        }
        let name = arguments.get(0).to_rust_string_lossy(scope);
        let callback = v8::Local::<v8::Function>::try_from(arguments.get(1))
            .map_err(|_| ScriptError::new("shortcut callback must be a function"))?;
        let registry = scope
            .get_slot::<Rc<RefCell<ScriptCommands>>>()
            .cloned()
            .ok_or_else(|| ScriptError::new("command registry is unavailable"))?;
        registry
            .borrow_mut()
            .register_shortcut(scope, name, callback)
    })();
    match result {
        Ok(()) => return_value.set_undefined(),
        Err(error) => throw_script_error(scope, &error.to_string()),
    }
}

fn invoke_native_command(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let result = (|| {
        let id = arguments.data().to_rust_string_lossy(scope);
        let id = CommandId::new(id).map_err(|error| ScriptError::new(error.to_string()))?;
        let values = (0..arguments.length())
            .map(|index| v8_to_json(scope, arguments.get(index), "command argument"))
            .collect::<Result<Vec<_>, _>>()?;
        let bridge = scope
            .get_slot::<Rc<ActiveCommandHost>>()
            .cloned()
            .ok_or_else(|| ScriptError::new("command host bridge is unavailable"))?;
        bridge
            .invoke(CommandInvocation::new(id, values))
            .map_err(|error| ScriptError::new(error.to_string()))
    })();
    match result {
        Ok(CommandCompletion::Ready(value)) => match json_to_v8(scope, &value) {
            Ok(value) => return_value.set(value),
            Err(error) => throw_script_error(scope, &error.to_string()),
        },
        Ok(CommandCompletion::Pending) => return_value.set_undefined(),
        Err(error) => throw_script_error(scope, &error.to_string()),
    }
}

impl ScriptHost {
    pub(crate) fn command_entries(host: &Rc<RefCell<Self>>) -> Vec<CommandEntry> {
        entries(host)
    }

    pub(crate) fn install_native_commands(&mut self, ids: &[CommandId]) -> Result<(), ScriptError> {
        let context = self.context.clone();
        let commands = self.commands.clone();
        self.invoke(ScriptInvocationKind::Action, |isolate| {
            v8::scope_with_context!(scope, isolate, context);
            for id in ids {
                commands.borrow_mut().install_native(scope, id)?;
            }
            Ok(())
        })
    }

    fn execute_command(
        &mut self,
        id: &CommandId,
        host: &mut dyn CommandHost,
        arguments: Vec<CommandValue>,
    ) -> Result<CommandCompletion, ScriptError> {
        let callback = self
            .commands
            .borrow()
            .callback(id)
            .ok_or_else(|| ScriptError::new(format!("unknown script command '{id}'")))?;
        let context = self.context.clone();
        let bridge = self.command_host.clone();
        let mut scoped_host = ScopedHost { host };
        let _active_host = bridge.activate(&mut scoped_host)?;
        self.invoke(ScriptInvocationKind::Action, |isolate| {
            v8::scope_with_context!(scope, isolate, context);
            v8::tc_scope!(let scope, scope);
            let callback = v8::Local::new(scope, callback);
            let arguments = arguments
                .iter()
                .map(|value| json_to_v8(scope, value))
                .collect::<Result<Vec<_>, _>>()?;
            let receiver = v8::undefined(scope).into();
            let value = call_script_callback(scope, callback, receiver, &arguments)
                .ok_or_else(|| current_exception(scope, id.as_str(), "execute"))?;
            let pending = value.is_promise();
            perform_microtask_checkpoint(scope);
            Ok(if pending {
                CommandCompletion::Pending
            } else {
                CommandCompletion::Ready(CommandValue::Null)
            })
        })
    }
}

fn ensure_available_root(id: &CommandId) -> Result<(), ScriptError> {
    let root = id
        .as_str()
        .split('.')
        .next()
        .expect("validated command ids have a root");
    if RESERVED_ROOTS.contains(&root) {
        Err(ScriptError::new(format!(
            "command id '{id}' uses reserved root '{root}'"
        )))
    } else {
        Ok(())
    }
}

fn editor_object<'s>(
    scope: &mut v8::PinScope<'s, '_>,
) -> Result<v8::Local<'s, v8::Object>, ScriptError> {
    let global = scope.get_current_context().global(scope);
    object_property(scope, global, "editor")
}

fn commands_object<'s>(
    scope: &mut v8::PinScope<'s, '_>,
) -> Result<v8::Local<'s, v8::Object>, ScriptError> {
    let editor = editor_object(scope)?;
    object_property(scope, editor, "commands")
}

fn object_property<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<v8::Object>,
    name: &str,
) -> Result<v8::Local<'s, v8::Object>, ScriptError> {
    let key = v8::String::new(scope, name)
        .ok_or_else(|| ScriptError::new("property name is too large for V8"))?;
    object
        .get(scope, key.into())
        .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
        .ok_or_else(|| ScriptError::new(format!("editor.{name} is unavailable")))
}

fn set(
    scope: &mut v8::PinScope<'_, '_>,
    object: v8::Local<v8::Object>,
    name: &str,
    value: v8::Local<v8::Value>,
) {
    let key = v8::String::new(scope, name).expect("static property name");
    assert_eq!(object.set(scope, key.into(), value), Some(true));
}

fn preserve_command_children(
    scope: &mut v8::PinScope<'_, '_>,
    previous: v8::Local<v8::Value>,
    replacement: v8::Local<v8::Value>,
) -> Result<(), ScriptError> {
    let (Ok(previous), Ok(replacement)) = (
        v8::Local::<v8::Object>::try_from(previous),
        v8::Local::<v8::Object>::try_from(replacement),
    ) else {
        return Ok(());
    };
    let names = previous
        .get_own_property_names(scope, v8::GetPropertyNamesArgs::default())
        .ok_or_else(|| ScriptError::new("failed to inspect command namespace"))?;
    for index in 0..names.length() {
        let key = names
            .get_index(scope, index)
            .ok_or_else(|| ScriptError::new("failed to inspect command namespace key"))?;
        let value = previous
            .get(scope, key)
            .ok_or_else(|| ScriptError::new("failed to read command namespace child"))?;
        if replacement.set(scope, key, value) != Some(true) {
            return Err(ScriptError::new(
                "failed to preserve nested command namespace",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use vell_mode::command_registry::{CommandRegistry, CommandRequest};

    use super::*;

    struct TestHost {
        registry: Rc<RefCell<CommandRegistry>>,
    }

    impl CommandHost for TestHost {
        fn invoke_command(&mut self, invocation: CommandInvocation) -> CommandResult {
            let entry = self
                .registry
                .borrow()
                .resolve(&invocation.command)
                .ok_or_else(|| CommandError::UnknownCommand(invocation.command.clone()))?;
            entry.invoke(self, invocation.arguments)
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

    fn id(value: &str) -> CommandId {
        CommandId::new(value).unwrap()
    }

    fn invoke(host: &mut TestHost, command: &str) -> CommandResult {
        host.invoke_command(CommandInvocation::new(id(command), Vec::new()))
    }

    fn configured_host(
        source: &str,
        native: &[&str],
        calls: Rc<RefCell<Vec<String>>>,
    ) -> (Rc<RefCell<ScriptHost>>, TestHost) {
        let script =
            ScriptHost::with_timeouts(Duration::from_millis(50), Duration::from_millis(100));
        configure_script(script, source, native, calls)
    }

    fn configure_script(
        mut script: ScriptHost,
        source: &str,
        native: &[&str],
        calls: Rc<RefCell<Vec<String>>>,
    ) -> (Rc<RefCell<ScriptHost>>, TestHost) {
        script
            .execute_typescript("file:///commands.ts", source)
            .unwrap();
        let native_ids = native.iter().map(|name| id(name)).collect::<Vec<_>>();
        script.install_native_commands(&native_ids).unwrap();
        let script = Rc::new(RefCell::new(script));
        let registry = Rc::new(RefCell::new(CommandRegistry::new()));
        for command in entries(&script) {
            registry.borrow_mut().register(command);
        }
        for name in native {
            let calls = Rc::clone(&calls);
            let command = id(name);
            let default_label = (*name).to_owned();
            registry.borrow_mut().register(CommandEntry::new(
                command,
                move |_host: &mut dyn CommandHost, arguments: Vec<CommandValue>| {
                    let label = arguments
                        .first()
                        .and_then(CommandValue::as_str)
                        .unwrap_or(&default_label)
                        .to_owned();
                    calls.borrow_mut().push(label);
                    Ok(CommandValue::Null.into())
                },
            ));
        }
        (script, TestHost { registry })
    }

    #[test]
    fn namespace_calls_preserve_javascript_values_and_lexical_shadowing() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let (_script, mut host) = configured_host(
            r#"
function save() { editor.commands.capture("ordinary"); }
editor.commands.register("test.inner", (object, closure, promise) => {
  if (object.answer === 42 && closure() === 7 && promise instanceof Promise) {
    editor.commands.capture("direct");
  }
  return promise;
});
editor.commands.register("test.run", () => {
  save();
  editor.commands.save("native");
  return editor.commands.test.inner({ answer: 42 }, () => 7, Promise.resolve(1));
});
"#,
            &["capture", "save"],
            Rc::clone(&calls),
        );

        assert_eq!(
            invoke(&mut host, "test.run"),
            Ok(CommandCompletion::Pending)
        );
        assert_eq!(&*calls.borrow(), &["ordinary", "native", "direct"]);
    }

    #[test]
    fn dynamic_redefinition_updates_namespace_fallback_and_host_registry() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let (_script, mut host) = configured_host(
            r#"
editor.commands.register("increment", () => editor.commands.capture("old"));
editor.commands.register("test.redefine", () => {
  editor.commands.register("increment", () => editor.commands.capture("new"));
  throw new Error("definition survives");
});
editor.commands.register("test.callBare", () => increment());
"#,
            &["capture"],
            Rc::clone(&calls),
        );

        assert!(invoke(&mut host, "test.redefine").is_err());
        assert!(invoke(&mut host, "test.callBare").is_ok());
        assert!(invoke(&mut host, "increment").is_ok());
        assert_eq!(&*calls.borrow(), &["new", "new"]);
    }

    #[test]
    fn a_callable_namespace_keeps_commands_registered_below_it() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let (_script, mut host) = configured_host(
            r#"
editor.commands.register("tree.leaf", () => editor.commands.capture("leaf"));
editor.commands.register("tree", () => editor.commands.capture("root"));
editor.commands.register("test.run", () => {
  tree();
  tree.leaf();
});
"#,
            &["capture"],
            Rc::clone(&calls),
        );

        assert!(invoke(&mut host, "test.run").is_ok());
        assert_eq!(&*calls.borrow(), &["root", "leaf"]);
    }

    #[test]
    fn shortcut_registration_keeps_private_callback_identity() {
        let mut host = ScriptHost::new();
        host.execute_typescript(
            "file:///shortcuts.ts",
            r#"
editor.commands.shortcut("write", () => "first");
editor.commands.shortcut("write", () => "replacement");
"#,
        )
        .unwrap();

        let commands = host.commands.borrow();
        assert_eq!(commands.shortcuts.len(), 1);
        assert!(commands.shortcuts.contains_key("write"));
    }

    #[test]
    fn scoped_native_wrapper_is_cleared_after_exception() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let (script, mut host) = configured_host(
            r#"
editor.commands.register("test.fail", () => {
  globalThis.savedCapture = editor.commands.capture;
  editor.commands.capture("inside");
  throw new Error("boom");
});
"#,
            &["capture"],
            Rc::clone(&calls),
        );

        assert!(invoke(&mut host, "test.fail").is_err());
        let error = script
            .borrow_mut()
            .evaluate_script("savedCapture('outside')")
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("outside an active command callback"),
            "{error}"
        );
        assert_eq!(&*calls.borrow(), &["inside"]);
    }

    #[test]
    fn scoped_native_wrapper_is_cleared_after_timeout() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let (script, mut host) = configured_host(
            r#"
editor.commands.register("test.timeout", () => {
  globalThis.savedCapture = editor.commands.capture;
  while (true) {}
});
"#,
            &["capture"],
            calls,
        );

        assert!(invoke(&mut host, "test.timeout").is_err());
        let error = script
            .borrow_mut()
            .evaluate_script("savedCapture('outside')")
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("outside an active command callback"),
            "{error}"
        );
    }

    #[test]
    fn scoped_native_wrapper_is_cleared_after_heap_termination() {
        let budget = super::super::ScriptExecutionBudget {
            callback_timeout: Duration::from_secs(5),
            startup_timeout: Duration::from_secs(5),
        };
        let script = ScriptHost::with_budget_and_heap(budget, 16 * 1024 * 1024);
        let calls = Rc::new(RefCell::new(Vec::new()));
        let (script, mut host) = configure_script(
            script,
            r#"
editor.commands.register("test.heap", () => {
  globalThis.savedCapture = editor.commands.capture;
  const retained = [];
  while (true) retained.push(new Array(100_000).fill(42));
});
"#,
            &["capture"],
            calls,
        );

        let error = invoke(&mut host, "test.heap").unwrap_err().to_string();
        assert!(error.contains("heap limit exceeded"), "{error}");
        let error = script
            .borrow_mut()
            .evaluate_script("savedCapture('outside')")
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("outside an active command callback"),
            "{error}"
        );
    }
}
