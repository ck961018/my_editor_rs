use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::ptr;
use std::rc::Rc;

use vell_mode::command_registry::{
    CommandAdapter, CommandCompletion, CommandContinuation, CommandContinuationResult,
    CommandEntry, CommandError, CommandHost, CommandId, CommandInvocation, CommandPending,
    CommandResult, CommandTaskCompletion, CommandTaskId, CommandValue,
};

use super::{
    ScriptError, ScriptHost, ScriptInvocationKind, call_script_callback, current_exception,
    json_to_v8, perform_microtask_checkpoint, throw_script_error, v8_to_json,
};

const RESERVED_ROOTS: &[&str] = &["$commandLine", "$script", "register", "shortcut"];

#[derive(Clone, Debug)]
pub(super) struct ScriptSourceSpan {
    pub(super) identity: String,
    pub(super) line: usize,
    pub(super) column: usize,
}

#[derive(Clone, Debug)]
pub(super) struct ScriptCommandRegistration {
    pub(super) id: CommandId,
    pub(super) source: Option<ScriptSourceSpan>,
}

#[derive(Clone, Default)]
pub(super) struct ScriptCommands {
    definitions: BTreeMap<CommandId, v8::Global<v8::Function>>,
    shortcuts: BTreeMap<String, v8::Global<v8::Function>>,
    changes: Vec<ScriptCommandRegistration>,
    fallback_roots: BTreeMap<String, v8::Global<v8::Value>>,
    source_maps: BTreeMap<String, deno_ast::swc::sourcemap::SourceMap>,
}

impl ScriptCommands {
    pub(super) fn install_api(&self, scope: &mut v8::PinScope<'_, '_>) {
        let editor = editor_object(scope).expect("editor API is installed first");
        let commands = v8::Object::new(scope);
        let null = v8::null(scope);
        commands
            .set_prototype(scope, null.into())
            .expect("command namespace prototype");
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

    pub(super) fn record_source_map(
        &mut self,
        identity: String,
        source_map: &str,
    ) -> Result<(), ScriptError> {
        let source_map = deno_ast::swc::sourcemap::SourceMap::from_slice(source_map.as_bytes())
            .map_err(|error| ScriptError::new(format!("invalid script source map: {error}")))?;
        self.source_maps.insert(identity, source_map);
        Ok(())
    }

    fn original_position(
        &self,
        identity: &str,
        line: usize,
        column: usize,
    ) -> Option<(usize, usize)> {
        let line = u32::try_from(line.checked_sub(1)?).ok()?;
        let column = u32::try_from(column.checked_sub(1)?).ok()?;
        let token = self.source_maps.get(identity)?.lookup_token(line, column)?;
        Some((
            usize::try_from(token.get_src_line()).ok()?.checked_add(1)?,
            usize::try_from(token.get_src_col()).ok()?.checked_add(1)?,
        ))
    }

    fn register(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        id: CommandId,
        callback: v8::Local<v8::Function>,
        source: Option<ScriptSourceSpan>,
    ) -> Result<(), ScriptError> {
        ensure_available_root(&id)?;
        self.install_value(scope, &id, callback.into())?;
        self.definitions
            .insert(id.clone(), v8::Global::new(scope, callback));
        self.changes.push(ScriptCommandRegistration { id, source });
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
                if parent.has_own_property(scope, key.into()) == Some(true)
                    && let Some(previous) = parent.get(scope, key.into())
                {
                    preserve_command_children(scope, previous, value)?;
                }
                if parent.create_data_property(scope, key.into(), value) != Some(true) {
                    return Err(ScriptError::new(format!(
                        "failed to install command '{id}'"
                    )));
                }
                break;
            }
            let child = if parent.has_own_property(scope, key.into()) == Some(true) {
                parent
                    .get(scope, key.into())
                    .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
            } else {
                None
            };
            let child = match child {
                Some(child) => child,
                None => {
                    let child = v8::Object::new(scope);
                    let null = v8::null(scope);
                    if child.set_prototype(scope, null.into()) != Some(true)
                        || parent.create_data_property(scope, key.into(), child.into())
                            != Some(true)
                    {
                        return Err(ScriptError::new(format!(
                            "failed to install command namespace '{segment}'"
                        )));
                    }
                    child
                }
            };
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

    pub(super) fn shortcut(&self, name: &str) -> Option<v8::Global<v8::Function>> {
        self.shortcuts.get(name).cloned()
    }

    pub(super) fn change_count(&self) -> usize {
        self.changes.len()
    }

    pub(super) fn changes_since(&self, count: usize) -> Vec<ScriptCommandRegistration> {
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
    pending_tasks: Vec<ScriptTaskResolver>,
}

pub(super) struct ScriptTaskResolver {
    task: CommandTaskId,
    resolver: v8::Global<v8::PromiseResolver>,
}

impl<'a> ScopedHost<'a> {
    pub(super) fn new(host: &'a mut dyn CommandHost) -> Self {
        Self {
            host,
            pending_tasks: Vec::new(),
        }
    }

    pub(super) fn take_pending_tasks(&mut self) -> Vec<ScriptTaskResolver> {
        std::mem::take(&mut self.pending_tasks)
    }
}

pub(super) struct ActiveHostGuard<'a> {
    bridge: Rc<ActiveCommandHost>,
    _host: PhantomData<&'a mut ()>,
}

impl ActiveCommandHost {
    pub(super) fn activate<'a>(
        self: &Rc<Self>,
        host: &'a mut ScopedHost<'_>,
    ) -> Result<ActiveHostGuard<'a>, ScriptError> {
        if !self.pointer.get().is_null() {
            return Err(ScriptError::new("a command host is already active"));
        }
        self.pointer.set(host as *mut ScopedHost<'_> as *mut ());
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

    fn track_task(
        &self,
        task: CommandTaskId,
        resolver: v8::Global<v8::PromiseResolver>,
    ) -> Result<(), CommandError> {
        let pointer = self.pointer.get();
        if pointer.is_null() {
            return Err(CommandError::Failed(
                "native command wrapper is outside an active command callback".to_owned(),
            ));
        }
        // SAFETY: the active guard owns the stack host for the duration of all
        // synchronous V8 callbacks, as documented by `activate`.
        let host = unsafe { &mut *(pointer.cast::<ScopedHost<'static>>()) };
        if host
            .pending_tasks
            .iter()
            .any(|pending| pending.task == task)
        {
            return Err(CommandError::Failed(format!(
                "command task {} was returned more than once",
                task.get()
            )));
        }
        host.pending_tasks
            .push(ScriptTaskResolver { task, resolver });
        Ok(())
    }
}

impl Drop for ActiveHostGuard<'_> {
    fn drop(&mut self) {
        self.bridge.pointer.set(ptr::null_mut());
        self.bridge.invoking.set(false);
    }
}

pub(super) enum ScriptExecution {
    Ready(CommandValue),
    Pending(ScriptPromiseState),
}

pub(super) struct ScriptPromiseState {
    promise: v8::Global<v8::Promise>,
    tasks: BTreeMap<CommandTaskId, v8::Global<v8::PromiseResolver>>,
}

impl ScriptExecution {
    pub(super) fn into_completion(self, host: &Rc<RefCell<ScriptHost>>) -> CommandResult {
        match self {
            Self::Ready(value) => Ok(CommandCompletion::Ready(value)),
            Self::Pending(state) => {
                let tasks = state.tasks.keys().copied().collect();
                let continuation = ScriptPromiseContinuation {
                    host: Rc::clone(host),
                    state: RefCell::new(Some(state)),
                };
                CommandPending::continuation(tasks, continuation).map(CommandCompletion::Pending)
            }
        }
    }
}

struct ScriptPromiseContinuation {
    host: Rc<RefCell<ScriptHost>>,
    state: RefCell<Option<ScriptPromiseState>>,
}

impl CommandContinuation for ScriptPromiseContinuation {
    fn resume(
        &self,
        host: &mut dyn CommandHost,
        completion: CommandTaskCompletion,
    ) -> Result<CommandContinuationResult, CommandError> {
        let change_count = change_count(&self.host);
        let state = self.state.borrow_mut().take().ok_or_else(|| {
            CommandError::Failed("command continuation is not pending".to_owned())
        })?;
        let execution = {
            self.host
                .try_borrow_mut()
                .map_err(|_| {
                    CommandError::Failed("script command runtime is reentrant".to_owned())
                })?
                .resume_command_promise(state, host, completion)
                .map_err(|error| CommandError::Failed(error.to_string()))
        };
        publish_changes(&self.host, host, change_count);
        let execution = execution?;
        match execution {
            ScriptExecution::Ready(value) => Ok(CommandContinuationResult::Ready(value)),
            ScriptExecution::Pending(state) => {
                let tasks = state.tasks.keys().copied().collect();
                self.state.replace(Some(state));
                Ok(CommandContinuationResult::Pending(tasks))
            }
        }
    }

    fn cancel(&self, _reason: CommandError) {
        self.state.borrow_mut().take();
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
        let execution = self
            .host
            .try_borrow_mut()
            .map_err(|_| CommandError::Failed("script command runtime is reentrant".to_owned()))?
            .execute_command(&self.id, host, arguments)
            .map_err(|error| CommandError::Failed(error.to_string()));
        let result = execution.and_then(|execution| execution.into_completion(&self.host));
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
    script.borrow_mut().publish_command_types(&changes);
    for registration in changes {
        host.register_command(ScriptCommandAdapter::entry(script, registration.id));
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
    entries.push(super::command_line::entry(host));
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
        let source = current_registration_source(scope, &registry);
        registry
            .borrow_mut()
            .register(scope, id, callback, source)?;
        Ok(callback)
    })();
    match parsed {
        Ok(callback) => return_value.set(callback.into()),
        Err(error) => throw_script_error(scope, &error.to_string()),
    }
}

fn current_registration_source(
    scope: &mut v8::PinScope<'_, '_>,
    registry: &Rc<RefCell<ScriptCommands>>,
) -> Option<ScriptSourceSpan> {
    let stack = v8::StackTrace::current_stack_trace(scope, 1)?;
    let frame = stack.get_frame(scope, 0)?;
    let identity = frame
        .get_script_name_or_source_url(scope)?
        .to_rust_string_lossy(scope);
    let line = frame.get_line_number();
    let column = frame.get_column();
    let (line, column) = registry
        .borrow()
        .original_position(&identity, line, column)
        .unwrap_or((line, column));
    (!identity.is_empty() && line > 0 && column > 0).then_some(ScriptSourceSpan {
        identity,
        line,
        column,
    })
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
        let id = CommandId::new(id)
            .map_err(|error| CommandError::InvalidArguments(error.to_string()))?;
        let values = (0..arguments.length())
            .map(|index| v8_to_json(scope, arguments.get(index), "command argument"))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| CommandError::InvalidArguments(error.to_string()))?;
        let bridge = scope
            .get_slot::<Rc<ActiveCommandHost>>()
            .cloned()
            .ok_or_else(|| CommandError::Failed("command host bridge is unavailable".to_owned()))?;
        bridge.invoke(CommandInvocation::new(id, values))
    })();
    match result {
        Ok(CommandCompletion::Ready(value)) => match json_to_v8(scope, &value) {
            Ok(value) => return_value.set(value),
            Err(error) => throw_script_error(scope, &error.to_string()),
        },
        Ok(CommandCompletion::Pending(pending)) => {
            let result: Result<v8::Local<v8::Promise>, ScriptError> = (|| {
                let task = pending.direct_task().ok_or_else(|| {
                    ScriptError::new("native wrapper received a nested command continuation")
                })?;
                let resolver = v8::PromiseResolver::new(scope)
                    .ok_or_else(|| ScriptError::new("failed to create command promise"))?;
                let promise = resolver.get_promise(scope);
                let bridge = scope
                    .get_slot::<Rc<ActiveCommandHost>>()
                    .cloned()
                    .ok_or_else(|| ScriptError::new("command host bridge is unavailable"))?;
                bridge
                    .track_task(task, v8::Global::new(scope, resolver))
                    .map_err(|error| ScriptError::new(error.to_string()))?;
                Ok(promise)
            })();
            match result {
                Ok(promise) => return_value.set(promise.into()),
                Err(error) => throw_script_error(scope, &error.to_string()),
            }
        }
        Err(CommandError::AsyncFailed(message)) => {
            let result: Result<v8::Local<v8::Promise>, ScriptError> = (|| {
                let resolver = v8::PromiseResolver::new(scope)
                    .ok_or_else(|| ScriptError::new("failed to create command promise"))?;
                let promise = resolver.get_promise(scope);
                let message = v8::String::new(scope, &message)
                    .ok_or_else(|| ScriptError::new("command rejection message is too large"))?;
                let exception = v8::Exception::error(scope, message);
                if resolver.reject(scope, exception) != Some(true) {
                    return Err(ScriptError::new("failed to reject command promise"));
                }
                Ok(promise)
            })();
            match result {
                Ok(promise) => return_value.set(promise.into()),
                Err(error) => throw_script_error(scope, &error.to_string()),
            }
        }
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
    ) -> Result<ScriptExecution, ScriptError> {
        let callback = self
            .commands
            .borrow()
            .callback(id)
            .ok_or_else(|| ScriptError::new(format!("unknown script command '{id}'")))?;
        let context = self.context.clone();
        let bridge = self.command_host.clone();
        let mut scoped_host = ScopedHost::new(host);
        let active_host = bridge.activate(&mut scoped_host)?;
        let result = self.invoke(ScriptInvocationKind::Action, |isolate| {
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
            perform_microtask_checkpoint(scope);
            poll_script_value(scope, value)
        });
        drop(active_host);
        finish_script_execution(result?, scoped_host.take_pending_tasks())
    }

    pub(super) fn execute_shortcut(
        &mut self,
        name: &str,
        argument: Option<&str>,
        host: &mut dyn CommandHost,
    ) -> Result<ScriptExecution, ScriptError> {
        let callback = self
            .commands
            .borrow()
            .shortcut(name)
            .ok_or_else(|| ScriptError::new(format!("unknown shortcut '{name}'")))?;
        let context = self.context.clone();
        let bridge = self.command_host.clone();
        let mut scoped_host = ScopedHost::new(host);
        let active_host = bridge.activate(&mut scoped_host)?;
        let result = self.invoke(ScriptInvocationKind::Action, |isolate| {
            v8::scope_with_context!(scope, isolate, context);
            v8::tc_scope!(let scope, scope);
            let callback = v8::Local::new(scope, callback);
            let argument = argument
                .map(|argument| {
                    v8::String::new(scope, argument)
                        .map(|argument| vec![argument.into()])
                        .ok_or_else(|| ScriptError::new("shortcut argument is too large"))
                })
                .transpose()?
                .unwrap_or_default();
            let receiver = v8::undefined(scope).into();
            let value = call_script_callback(scope, callback, receiver, &argument)
                .ok_or_else(|| current_exception(scope, name, "execute shortcut"))?;
            perform_microtask_checkpoint(scope);
            poll_script_value(scope, value)
        });
        drop(active_host);
        finish_script_execution(result?, scoped_host.take_pending_tasks())
    }

    fn resume_command_promise(
        &mut self,
        mut state: ScriptPromiseState,
        host: &mut dyn CommandHost,
        completion: CommandTaskCompletion,
    ) -> Result<ScriptExecution, ScriptError> {
        let task = completion.task();
        let resolver = state.tasks.remove(&task).ok_or_else(|| {
            ScriptError::new(format!("command is not waiting for task {}", task.get()))
        })?;
        let completion = completion.into_result();
        let context = self.context.clone();
        let promise = state.promise.clone();
        let bridge = self.command_host.clone();
        let mut scoped_host = ScopedHost::new(host);
        let active_host = bridge.activate(&mut scoped_host)?;
        let result = self.invoke(ScriptInvocationKind::Action, |isolate| {
            v8::scope_with_context!(scope, isolate, context);
            v8::tc_scope!(let scope, scope);
            let resolver = v8::Local::new(scope, resolver);
            match completion {
                Ok(value) => {
                    let value = json_to_v8(scope, &value)?;
                    if resolver.resolve(scope, value) != Some(true) {
                        return Err(ScriptError::new("failed to resolve command promise"));
                    }
                }
                Err(error) => {
                    let message = v8::String::new(scope, &error.to_string()).ok_or_else(|| {
                        ScriptError::new("command rejection message is too large")
                    })?;
                    let exception = v8::Exception::error(scope, message);
                    if resolver.reject(scope, exception) != Some(true) {
                        return Err(ScriptError::new("failed to reject command promise"));
                    }
                }
            }
            perform_microtask_checkpoint(scope);
            let promise = v8::Local::new(scope, promise);
            poll_promise(scope, promise)
        });
        drop(active_host);
        let result = result?;
        if let ScriptValue::Pending(promise) = result {
            state.promise = promise;
            for pending in scoped_host.take_pending_tasks() {
                if state.tasks.insert(pending.task, pending.resolver).is_some() {
                    return Err(ScriptError::new(format!(
                        "command task {} was returned more than once",
                        pending.task.get()
                    )));
                }
            }
            if state.tasks.is_empty() {
                return Err(ScriptError::new(
                    "pending command has no host task to resume it",
                ));
            }
            Ok(ScriptExecution::Pending(state))
        } else {
            finish_script_execution(result, Vec::new())
        }
    }
}

pub(super) enum ScriptValue {
    Ready(CommandValue),
    Pending(v8::Global<v8::Promise>),
}

pub(super) fn poll_script_value(
    scope: &mut v8::PinScope<'_, '_>,
    value: v8::Local<v8::Value>,
) -> Result<ScriptValue, ScriptError> {
    match v8::Local::<v8::Promise>::try_from(value) {
        Ok(promise) => poll_promise(scope, promise),
        Err(_) => Ok(ScriptValue::Ready(CommandValue::Null)),
    }
}

fn poll_promise(
    scope: &mut v8::PinScope<'_, '_>,
    promise: v8::Local<v8::Promise>,
) -> Result<ScriptValue, ScriptError> {
    match promise.state() {
        v8::PromiseState::Pending => Ok(ScriptValue::Pending(v8::Global::new(scope, promise))),
        v8::PromiseState::Fulfilled => {
            command_value(scope, promise.result(scope)).map(ScriptValue::Ready)
        }
        v8::PromiseState::Rejected => {
            promise.mark_as_handled();
            Err(ScriptError::new(
                promise.result(scope).to_rust_string_lossy(scope),
            ))
        }
    }
}

fn command_value(
    scope: &mut v8::PinScope<'_, '_>,
    value: v8::Local<v8::Value>,
) -> Result<CommandValue, ScriptError> {
    if value.is_undefined() {
        Ok(CommandValue::Null)
    } else {
        v8_to_json(scope, value, "command result")
    }
}

pub(super) fn finish_script_execution(
    value: ScriptValue,
    tasks: Vec<ScriptTaskResolver>,
) -> Result<ScriptExecution, ScriptError> {
    match value {
        ScriptValue::Ready(value) => Ok(ScriptExecution::Ready(value)),
        ScriptValue::Pending(promise) => {
            let mut task_map = BTreeMap::new();
            for pending in tasks {
                if task_map.insert(pending.task, pending.resolver).is_some() {
                    return Err(ScriptError::new(format!(
                        "command task {} was returned more than once",
                        pending.task.get()
                    )));
                }
            }
            if task_map.is_empty() {
                return Err(ScriptError::new(
                    "pending command has no host task to resume it",
                ));
            }
            Ok(ScriptExecution::Pending(ScriptPromiseState {
                promise,
                tasks: task_map,
            }))
        }
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
        let name = v8::Local::<v8::Name>::try_from(key)
            .map_err(|_| ScriptError::new("invalid command namespace key"))?;
        let value = previous
            .get(scope, name.into())
            .ok_or_else(|| ScriptError::new("failed to read command namespace child"))?;
        if replacement.create_data_property(scope, name, value) != Some(true) {
            return Err(ScriptError::new(
                "failed to preserve nested command namespace",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
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
            Ok(CommandCompletion::Ready(CommandValue::from(1)))
        );
        assert_eq!(&*calls.borrow(), &["ordinary", "native", "direct"]);
    }

    #[test]
    fn resumed_continuation_keeps_the_watchdog_and_host_recovers() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let (_script, mut host) = configured_host(
            r#"
editor.commands.register("test.hangAfterWait", async () => {
  await wait();
  while (true) {}
});
editor.commands.register("test.afterTimeout", () => 42);
"#,
            &["wait"],
            calls,
        );
        let task = CommandTaskId::new(91);
        host.registry.borrow_mut().register(CommandEntry::new(
            id("wait"),
            move |_host: &mut dyn CommandHost, _arguments: Vec<CommandValue>| {
                Ok(CommandCompletion::Pending(CommandPending::task(task)))
            },
        ));
        let CommandCompletion::Pending(pending) = invoke(&mut host, "test.hangAfterWait").unwrap()
        else {
            panic!("async command did not suspend");
        };

        let error = pending
            .resume(
                &mut host,
                CommandTaskCompletion::new(task, Ok(CommandValue::Null)),
            )
            .unwrap_err();

        assert!(error.to_string().contains("timeout"), "{error}");
        assert_eq!(
            invoke(&mut host, "test.afterTimeout"),
            Ok(CommandCompletion::Ready(CommandValue::Null))
        );
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
    fn registered_handlers_publish_inferred_command_types() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let (script, _host) = configured_host(
            r#"
function increment(value: number, by?: number): number {
  return value + (by ?? 1);
}
editor.commands.register(increment);
editor.commands.register(
  "format.count",
  async (prefix: string, count: number): Promise<string> => prefix + count,
);
"#,
            &[],
            calls,
        );

        let declarations = script
            .borrow()
            .type_environment
            .generated_declarations()
            .to_owned();
        assert!(declarations.contains("increment"), "{declarations}");
        assert!(declarations.contains("value: number"), "{declarations}");
        assert!(declarations.contains("by?: number"), "{declarations}");
        assert!(declarations.contains("Promise<string>"), "{declarations}");

        let diagnostics = script
            .borrow_mut()
            .type_environment
            .diagnostics(
                "file:///probe.ts",
                r#"
editor.commands.increment("bad");
increment("bad");
editor.commands.format.count(1, 2);
editor.commands.format();
format();
"#,
            )
            .unwrap();
        assert_eq!(diagnostics.len(), 5, "{diagnostics:#?}");
    }

    #[test]
    fn replacement_atomically_updates_namespace_and_bare_global_types() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let (script, _host) = configured_host(
            r#"editor.commands.register("replaceable", (value: number) => value);"#,
            &[],
            calls,
        );
        script
            .borrow_mut()
            .execute_typescript(
                "file:///replacement.ts",
                r#"editor.commands.register("replaceable", (value: string) => value);"#,
            )
            .unwrap();

        let diagnostics = script
            .borrow_mut()
            .type_environment
            .diagnostics(
                "file:///replacement-probe.ts",
                r#"
editor.commands.replaceable(1);
replaceable(1);
editor.commands.replaceable("ok");
replaceable("ok");
"#,
            )
            .unwrap();
        assert_eq!(diagnostics.len(), 2, "{diagnostics:#?}");

        script
            .borrow_mut()
            .execute_typescript(
                "file:///native-replacement.ts",
                r#"editor.commands.register("save", (path: string) => path);"#,
            )
            .unwrap();
        let diagnostics = script
            .borrow_mut()
            .type_environment
            .diagnostics(
                "file:///native-replacement-probe.ts",
                r#"
editor.commands.save();
save();
editor.commands.save("file.txt");
save("file.txt");
"#,
            )
            .unwrap();
        assert_eq!(diagnostics.len(), 2, "{diagnostics:#?}");
    }

    #[test]
    fn module_sources_contribute_imported_handler_types() {
        let directory = tempfile::tempdir().unwrap();
        let dependency = directory.path().join("handler.ts");
        let entry = directory.path().join("commands.ts");
        fs::write(
            &dependency,
            "export function convert(value: number): string { return String(value); }",
        )
        .unwrap();
        fs::write(
            &entry,
            r#"
import { convert } from "./handler.ts";
editor.commands.register("module.convert", convert);
"#,
        )
        .unwrap();
        let mut script = ScriptHost::new();
        script.execute_module(&entry).unwrap();

        let declarations = script.type_environment.generated_declarations().to_owned();
        assert!(declarations.contains("value: number"), "{declarations}");
        assert!(declarations.contains("string"), "{declarations}");
        let diagnostics = script
            .type_diagnostics(
                "file:///module-probe.ts",
                "convert(1); editor.commands.module.convert(1);",
            )
            .unwrap();
        assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    }

    #[test]
    fn dynamic_registration_locations_use_the_safe_unknown_signature() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let (script, _host) = configured_host(
            r#"
const dynamicId = "dynamic.command";
editor.commands.register(dynamicId, (value: number) => value);
eval('editor.commands.register("from.eval", (value) => value)');
editor.commands.register("class.member", (value: number) => value);
editor.commands.register("__proto__.member", (value: number) => value);
if (editor.commands.__proto__.member(42) !== 42) throw new Error("namespace");
"#,
            &[],
            calls,
        );

        let declarations = script
            .borrow()
            .type_environment
            .generated_declarations()
            .to_owned();
        assert!(
            declarations.matches("unknown[]) => unknown").count() >= 2,
            "{declarations}"
        );
        assert!(
            declarations.contains("readonly \"class\""),
            "{declarations}"
        );
        assert!(
            !declarations.contains("declare const class:"),
            "{declarations}"
        );
        assert!(
            declarations.contains("readonly \"__proto__\""),
            "{declarations}"
        );
    }

    #[test]
    fn compiler_fault_does_not_break_runtime_registration() {
        let mut script = ScriptHost::new();
        script.type_environment.fault_for_test();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let (script, mut host) = configure_script(
            script,
            r#"editor.commands.register("still.runs", () => 42);"#,
            &[],
            calls,
        );

        assert!(invoke(&mut host, "still.runs").is_ok());
        assert!(script.borrow().type_environment.fault().is_some());
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
    fn registration_returns_the_same_callable() {
        let mut host = ScriptHost::new();
        host.execute_typescript(
            "file:///register-return.ts",
            r#"
function named(value: number) { return value + 1; }
const namedResult = editor.commands.register(named);
const explicit = (value: string) => value.length;
const explicitResult = editor.commands.register("explicit", explicit);
if (namedResult !== named || explicitResult !== explicit) {
  throw new Error("register replaced the callable");
}
"#,
        )
        .unwrap();
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
