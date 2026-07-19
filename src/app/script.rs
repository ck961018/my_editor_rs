//! TypeScript runtime owned by the application layer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Once;

use deno_ast::{
    EmitOptions, MediaType, ModuleSpecifier, ParseParams, TranspileModuleOptions, TranspileOptions,
    parse_module, parse_program,
};

use crate::app::command::{Command, ModeCommand};
use crate::app::mode::{
    InputFlow, Mode, ModeContentContext, ModeError, ModeResult, ModeState, ModeViewContext,
};
use crate::app::mode_name::{ModeActionName, ModeName};
use crate::core::action::ContentAction;
use crate::core::command::EditCommand;
use crate::core::keymap::Keymap;
use crate::core::transaction::{TextChangeSet, TextEdit};
use crate::protocol::key_event::{ArrowKey, KeyCode, KeyEvent};

static V8_INIT: Once = Once::new();

#[derive(Debug)]
pub(crate) struct ScriptError {
    message: String,
}

impl ScriptError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ScriptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ScriptError {}

/// The single long-lived V8 isolate used by script modes.
#[allow(dead_code)]
pub(crate) struct ScriptHost {
    isolate: v8::OwnedIsolate,
    context: v8::Global<v8::Context>,
    modules: Rc<RefCell<ModuleMap>>,
    definitions: Rc<RefCell<Vec<ScriptModeDefinition>>>,
}

#[allow(dead_code)]
impl ScriptHost {
    pub(crate) fn new() -> Self {
        initialize_v8();

        let mut isolate = v8::Isolate::new(v8::CreateParams::default());
        isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
        isolate.set_capture_stack_trace_for_uncaught_exceptions(true, 10);
        let modules = Rc::new(RefCell::new(ModuleMap::default()));
        let definitions = Rc::new(RefCell::new(Vec::new()));
        isolate.set_slot(modules.clone());
        isolate.set_slot(definitions.clone());

        let context = {
            v8::scope!(scope, &mut isolate);
            let context = v8::Context::new(scope, Default::default());
            v8::Global::new(scope, context)
        };
        {
            v8::scope_with_context!(scope, &mut isolate, context.clone());
            install_editor_api(scope);
        }

        Self {
            isolate,
            context,
            modules,
            definitions,
        }
    }

    pub(crate) fn execute_typescript(
        &mut self,
        specifier: &str,
        source: &str,
    ) -> Result<(), ScriptError> {
        self.evaluate_typescript(specifier, source).map(|_| ())
    }

    pub(crate) fn execute_module(&mut self, entry: &Path) -> Result<(), ScriptError> {
        let entry = entry
            .canonicalize()
            .map_err(|error| ScriptError::new(format!("failed to open script: {error}")))?;
        let root = entry
            .parent()
            .ok_or_else(|| ScriptError::new("script entry has no parent directory"))?
            .to_owned();
        self.modules.borrow_mut().reset(root);

        let modules = self.modules.clone();
        let context = self.context.clone();
        v8::scope_with_context!(scope, &mut self.isolate, context);
        v8::tc_scope!(let scope, scope);

        let module = load_module_tree(scope, &entry, &modules)?;
        match module.instantiate_module(scope, resolve_module) {
            Some(true) => {}
            _ => {
                return Err(current_exception(
                    scope,
                    &entry.display().to_string(),
                    "link",
                ));
            }
        }
        if module.evaluate(scope).is_none() {
            return Err(current_exception(
                scope,
                &entry.display().to_string(),
                "execute",
            ));
        }
        scope.perform_microtask_checkpoint();
        match module.get_status() {
            v8::ModuleStatus::Evaluated => {}
            v8::ModuleStatus::Errored => {
                let message = module.get_exception().to_rust_string_lossy(scope);
                return Err(ScriptError::new(format!(
                    "failed to execute {}: {message}",
                    entry.display()
                )));
            }
            _ => {
                return Err(ScriptError::new(format!(
                    "script did not finish synchronously: {}",
                    entry.display()
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn script_modes(host: &Rc<RefCell<Self>>) -> Vec<ScriptMode> {
        let definitions = host.borrow().definitions.borrow().clone();
        definitions
            .into_iter()
            .enumerate()
            .map(|(index, definition)| ScriptMode::new(host.clone(), index, definition))
            .collect()
    }

    fn execute_action(
        &mut self,
        mode_index: usize,
        action_index: usize,
        context: &ModeViewContext<'_>,
        content_state: &mut serde_json::Value,
        view_state: &mut serde_json::Value,
    ) -> Result<ModeResult, ScriptError> {
        let callback = self
            .definitions
            .borrow()
            .get(mode_index)
            .and_then(|mode| mode.actions.get(action_index))
            .map(|action| action.callback.clone())
            .ok_or_else(|| ScriptError::new("script action is no longer registered"))?;
        let v8_context = self.context.clone();
        v8::scope_with_context!(scope, &mut self.isolate, v8_context);
        v8::tc_scope!(let scope, scope);

        let argument = v8::Object::new(scope);
        set_number(scope, argument, "contentId", context.content_id().0 as f64);
        set_number(scope, argument, "viewId", context.view_id().0 as f64);
        if let Some(revision) = context.content_revision() {
            set_number(scope, argument, "revision", revision.0 as f64);
        }
        let content_value = json_to_v8(scope, content_state)?;
        let view_value = json_to_v8(scope, view_state)?;
        set_value(scope, argument, "contentState", content_value);
        set_value(scope, argument, "viewState", view_value);
        let callback = v8::Local::new(scope, callback);
        let receiver = v8::undefined(scope).into();
        let result = callback
            .call(scope, receiver, &[argument.into()])
            .ok_or_else(|| current_exception(scope, "script mode action", "execute"))?;
        let result = parse_action_result(scope, result, context)?;
        let next_content = property(scope, argument, "contentState")
            .ok_or_else(|| ScriptError::new("script removed context.contentState"))?;
        let next_view = property(scope, argument, "viewState")
            .ok_or_else(|| ScriptError::new("script removed context.viewState"))?;
        let next_content = v8_to_json(scope, next_content, "contentState")?;
        let next_view = v8_to_json(scope, next_view, "viewState")?;
        scope.perform_microtask_checkpoint();
        *content_state = next_content;
        *view_state = next_view;
        Ok(result)
    }

    fn create_state(
        &mut self,
        callback: Option<&v8::Global<v8::Function>>,
        parent: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, ScriptError> {
        let Some(callback) = callback.cloned() else {
            return Ok(serde_json::Value::Null);
        };
        let context = self.context.clone();
        v8::scope_with_context!(scope, &mut self.isolate, context);
        v8::tc_scope!(let scope, scope);
        let callback = v8::Local::new(scope, callback);
        let receiver = v8::undefined(scope).into();
        let arguments = parent
            .map(|value| json_to_v8(scope, value))
            .transpose()?
            .into_iter()
            .collect::<Vec<_>>();
        let value = callback
            .call(scope, receiver, &arguments)
            .ok_or_else(|| current_exception(scope, "script mode state factory", "execute"))?;
        v8_to_json(scope, value, "mode state")
    }

    fn evaluate_typescript(
        &mut self,
        specifier: &str,
        source: &str,
    ) -> Result<String, ScriptError> {
        let javascript = transpile_typescript(specifier, source)?;
        let context = self.context.clone();
        v8::scope_with_context!(scope, &mut self.isolate, context);
        v8::tc_scope!(let scope, scope);

        let source = v8::String::new(scope, &javascript)
            .ok_or_else(|| ScriptError::new("script source is too large for V8"))?;
        let script = match v8::Script::compile(scope, source, None) {
            Some(script) => script,
            None => return Err(current_exception(scope, specifier, "compile")),
        };
        let value = match script.run(scope) {
            Some(value) => value,
            None => return Err(current_exception(scope, specifier, "execute")),
        };

        scope.perform_microtask_checkpoint();
        Ok(value.to_rust_string_lossy(scope))
    }
}

#[derive(Clone)]
struct ScriptActionDefinition {
    name: ModeActionName,
    callback: v8::Global<v8::Function>,
}

#[derive(Clone)]
struct ScriptModeDefinition {
    name: ModeName,
    actions: Vec<ScriptActionDefinition>,
    bindings: Vec<(KeyEvent, usize)>,
    before: Option<ModeName>,
    create_content: Option<v8::Global<v8::Function>>,
    create_view: Option<v8::Global<v8::Function>>,
}

pub(crate) struct ScriptMode {
    host: Rc<RefCell<ScriptHost>>,
    mode_index: usize,
    name: ModeName,
    actions: Vec<ModeActionName>,
    keymap: Keymap<Command>,
    before: Option<ModeName>,
    create_content: Option<v8::Global<v8::Function>>,
    create_view: Option<v8::Global<v8::Function>>,
}

impl ScriptMode {
    fn new(
        host: Rc<RefCell<ScriptHost>>,
        mode_index: usize,
        definition: ScriptModeDefinition,
    ) -> Self {
        let mut keymap = Keymap::new();
        for (key, action_index) in &definition.bindings {
            let action = definition.actions[*action_index].name.clone();
            keymap.bind(
                *key,
                Command::Mode(ModeCommand::new(definition.name.clone(), action)),
            );
        }
        Self {
            host,
            mode_index,
            name: definition.name,
            actions: definition
                .actions
                .into_iter()
                .map(|action| action.name)
                .collect(),
            keymap,
            before: definition.before,
            create_content: definition.create_content,
            create_view: definition.create_view,
        }
    }

    pub(crate) fn before(&self) -> Option<&ModeName> {
        self.before.as_ref()
    }
}

impl Mode for ScriptMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &self.actions
    }

    fn new_content_state(&self) -> Box<dyn ModeState> {
        Box::new(serde_json::Value::Null)
    }

    fn new_view_state(&self) -> Box<dyn ModeState> {
        Box::new(serde_json::Value::Null)
    }

    fn create_content_state(
        &self,
        _context: &ModeContentContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        self.host
            .borrow_mut()
            .create_state(self.create_content.as_ref(), None)
            .map(|state| Box::new(state) as Box<dyn ModeState>)
            .map_err(|error| ModeError::CallbackFailed {
                mode: self.name.clone(),
                message: error.to_string(),
            })
    }

    fn create_view_state(
        &self,
        content_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        let content_state = script_state(content_state, &self.name)?;
        self.host
            .borrow_mut()
            .create_state(self.create_view.as_ref(), Some(content_state))
            .map(|state| Box::new(state) as Box<dyn ModeState>)
            .map_err(|error| ModeError::CallbackFailed {
                mode: self.name.clone(),
                message: error.to_string(),
            })
    }

    fn input_keymap<'a>(
        &'a self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> &'a Keymap<Command> {
        &self.keymap
    }

    fn execute_view_with_content(
        &self,
        content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
        action: &ModeActionName,
    ) -> Result<ModeResult, ModeError> {
        let action_index = self
            .actions
            .iter()
            .position(|candidate| candidate == action)
            .ok_or_else(|| ModeError::UnknownAction {
                mode: self.name.clone(),
                action: action.clone(),
            })?;
        let content_state = script_state_mut(content_state, &self.name)?;
        let view_state = script_state_mut(view_state, &self.name)?;
        self.host
            .borrow_mut()
            .execute_action(
                self.mode_index,
                action_index,
                context,
                content_state,
                view_state,
            )
            .map_err(|error| ModeError::CallbackFailed {
                mode: self.name.clone(),
                message: error.to_string(),
            })
    }
}

fn script_state<'state>(
    state: &'state dyn ModeState,
    mode: &ModeName,
) -> Result<&'state serde_json::Value, ModeError> {
    state
        .as_any()
        .downcast_ref::<serde_json::Value>()
        .ok_or_else(|| ModeError::CallbackFailed {
            mode: mode.clone(),
            message: "script content state has an invalid host type".to_owned(),
        })
}

fn script_state_mut<'state>(
    state: &'state mut dyn ModeState,
    mode: &ModeName,
) -> Result<&'state mut serde_json::Value, ModeError> {
    state
        .as_any_mut()
        .downcast_mut::<serde_json::Value>()
        .ok_or_else(|| ModeError::CallbackFailed {
            mode: mode.clone(),
            message: "script mode state has an invalid host type".to_owned(),
        })
}

pub(crate) fn load_user_config() -> Result<Option<Rc<RefCell<ScriptHost>>>, ScriptError> {
    let explicit = std::env::var_os("MY_EDITOR_CONFIG").map(PathBuf::from);
    let path = explicit.clone().or_else(default_config_path);
    let Some(path) = path else {
        return Ok(None);
    };
    if explicit.is_none() && !path.is_file() {
        return Ok(None);
    }

    let mut host = ScriptHost::new();
    host.execute_module(&path)?;
    Ok(Some(Rc::new(RefCell::new(host))))
}

fn default_config_path() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));

    base.map(|base| base.join("my_editor_rs").join("config.ts"))
}

fn initialize_v8() {
    V8_INIT.call_once(|| {
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
}

fn install_editor_api(scope: &mut v8::PinScope<'_, '_>) {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let editor = v8::Object::new(scope);
    let modes = v8::Object::new(scope);
    let define_name = v8::String::new(scope, "define").unwrap();
    let define = v8::FunctionTemplate::new(scope, define_mode)
        .get_function(scope)
        .unwrap();
    modes.set(scope, define_name.into(), define.into());
    set_object(scope, editor, "modes", modes);
    set_object(scope, global, "editor", editor);
}

fn define_mode(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let result = parse_mode_definition(scope, arguments.get(0));
    match result {
        Ok(definition) => {
            let Some(definitions) = scope
                .get_slot::<Rc<RefCell<Vec<ScriptModeDefinition>>>>()
                .cloned()
            else {
                throw_script_error(scope, "script definition registry is unavailable");
                return;
            };
            if definitions
                .borrow()
                .iter()
                .any(|mode| mode.name == definition.name)
            {
                throw_script_error(
                    scope,
                    &format!("duplicate script mode '{}'", definition.name.as_str()),
                );
                return;
            }
            definitions.borrow_mut().push(definition);
            return_value.set_undefined();
        }
        Err(error) => throw_script_error(scope, &error.to_string()),
    }
}

fn parse_mode_definition(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
) -> Result<ScriptModeDefinition, ScriptError> {
    let object = v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| ScriptError::new("editor.modes.define expects an object"))?;
    let name = required_string(scope, object, "name")?;
    let actions_object = required_object(scope, object, "actions")?;
    let action_keys = actions_object
        .get_own_property_names(scope, Default::default())
        .ok_or_else(|| ScriptError::new("failed to enumerate mode actions"))?;
    let mut actions = Vec::new();
    for index in 0..action_keys.length() {
        let key = action_keys
            .get_index(scope, index)
            .ok_or_else(|| ScriptError::new("failed to read action name"))?;
        let action_name = key.to_rust_string_lossy(scope);
        let callback = actions_object
            .get(scope, key)
            .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
            .ok_or_else(|| {
                ScriptError::new(format!("mode action '{action_name}' must be a function"))
            })?;
        actions.push(ScriptActionDefinition {
            name: ModeActionName::new(action_name),
            callback: v8::Global::new(scope, callback),
        });
    }
    if actions.is_empty() {
        return Err(ScriptError::new(
            "a script mode must define at least one action",
        ));
    }

    let mut bindings = Vec::new();
    if let Some(keys_value) =
        property(scope, object, "keys").filter(|value| !value.is_null_or_undefined())
    {
        let keys = v8::Local::<v8::Object>::try_from(keys_value)
            .map_err(|_| ScriptError::new("mode keys must be an object"))?;
        let binding_keys = keys
            .get_own_property_names(scope, Default::default())
            .ok_or_else(|| ScriptError::new("failed to enumerate mode keys"))?;
        for index in 0..binding_keys.length() {
            let key_value = binding_keys
                .get_index(scope, index)
                .ok_or_else(|| ScriptError::new("failed to read key binding"))?;
            let key_name = key_value.to_rust_string_lossy(scope);
            let action_name = keys
                .get(scope, key_value)
                .filter(|value| value.is_string())
                .map(|value| value.to_rust_string_lossy(scope))
                .ok_or_else(|| ScriptError::new("key binding action must be a string"))?;
            let action_index = actions
                .iter()
                .position(|action| action.name.as_str() == action_name)
                .ok_or_else(|| {
                    ScriptError::new(format!("unknown action '{action_name}' in key bindings"))
                })?;
            bindings.push((parse_key(&key_name)?, action_index));
        }
    }

    let before = optional_string(scope, object, "before")?.map(ModeName::new);
    let create_content = optional_factory(scope, object, "content")?;
    let create_view = optional_factory(scope, object, "view")?;
    Ok(ScriptModeDefinition {
        name: ModeName::new(name),
        actions,
        bindings,
        before,
        create_content,
        create_view,
    })
}

fn optional_factory(
    scope: &mut v8::PinScope,
    definition: v8::Local<v8::Object>,
    name: &str,
) -> Result<Option<v8::Global<v8::Function>>, ScriptError> {
    let Some(value) = property(scope, definition, name) else {
        return Ok(None);
    };
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    let section = v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| ScriptError::new(format!("mode {name} must be an object")))?;
    let Some(create) = property(scope, section, "create") else {
        return Ok(None);
    };
    let create = v8::Local::<v8::Function>::try_from(create)
        .map_err(|_| ScriptError::new(format!("mode {name}.create must be a function")))?;
    Ok(Some(v8::Global::new(scope, create)))
}

fn parse_key(key: &str) -> Result<KeyEvent, ScriptError> {
    let mut characters = key.chars();
    if let (Some(character), None) = (characters.next(), characters.next()) {
        return Ok(KeyEvent::char(character));
    }
    let code = match key {
        "Escape" => KeyCode::Escape,
        "Enter" => KeyCode::Enter,
        "Backspace" => KeyCode::Backspace,
        "ArrowUp" => KeyCode::Arrow(ArrowKey::Up),
        "ArrowDown" => KeyCode::Arrow(ArrowKey::Down),
        "ArrowLeft" => KeyCode::Arrow(ArrowKey::Left),
        "ArrowRight" => KeyCode::Arrow(ArrowKey::Right),
        _ => return Err(ScriptError::new(format!("unsupported key binding: {key}"))),
    };
    Ok(KeyEvent::plain(code))
}

fn parse_action_result(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
    context: &ModeViewContext<'_>,
) -> Result<ModeResult, ScriptError> {
    if value.is_null_or_undefined() {
        return Ok(ModeResult::none());
    }
    let object = v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| ScriptError::new("script action must return an object or undefined"))?;
    let flow = match optional_string(scope, object, "flow")?.as_deref() {
        None | Some("stop") => InputFlow::Stop,
        Some("continue") => InputFlow::Continue,
        Some(other) => return Err(ScriptError::new(format!("invalid input flow: {other}"))),
    };
    let mut effects = Vec::new();
    let insert_text = optional_string(scope, object, "insertText")?;
    let content_edits =
        property(scope, object, "contentEdits").filter(|value| !value.is_null_or_undefined());
    if insert_text.is_some() && content_edits.is_some() {
        return Err(ScriptError::new(
            "insertText and contentEdits cannot be returned together",
        ));
    }
    if let Some(text) = insert_text {
        effects.push(crate::app::mode::ModeEffect::DeferredEdit(
            EditCommand::InsertText(text),
        ));
    }
    if let Some(batch) = content_edits {
        effects.push(crate::app::mode::ModeEffect::Content(parse_content_edits(
            scope, batch, context,
        )?));
    }
    Ok(match flow {
        InputFlow::Continue => ModeResult::continue_with(effects),
        InputFlow::Stop => ModeResult::operations(effects),
    })
}

fn parse_content_edits(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
    context: &ModeViewContext<'_>,
) -> Result<ContentAction, ScriptError> {
    let batch = v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| ScriptError::new("contentEdits must be an object"))?;
    let revision = required_usize(scope, batch, "revision")?;
    let current_revision = context
        .content_revision()
        .ok_or_else(|| ScriptError::new("attached content has no text revision"))?;
    if revision as u64 != current_revision.0 {
        return Err(ScriptError::new(format!(
            "stale content revision: expected {}, got {revision}",
            current_revision.0
        )));
    }
    let snapshot = context
        .text_snapshot()
        .ok_or_else(|| ScriptError::new("attached content is not editable text"))?;
    let edits = property(scope, batch, "edits")
        .and_then(|value| v8::Local::<v8::Array>::try_from(value).ok())
        .ok_or_else(|| ScriptError::new("contentEdits.edits must be an array"))?;
    let mut parsed_edits = Vec::with_capacity(edits.length() as usize);
    for index in 0..edits.length() {
        let edit = edits
            .get_index(scope, index)
            .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
            .ok_or_else(|| ScriptError::new(format!("content edit {index} must be an object")))?;
        let range = required_object(scope, edit, "range")?;
        let start = required_object(scope, range, "start")?;
        let end = required_object(scope, range, "end")?;
        let start = parse_position(scope, start, &snapshot)?;
        let end = parse_position(scope, end, &snapshot)?;
        let text = required_string(scope, edit, "text")?;
        parsed_edits.push(TextEdit::new(start..end, text));
    }
    let change = TextChangeSet::from_edits(snapshot.len_chars(), parsed_edits)
        .map_err(|error| ScriptError::new(format!("invalid content edits: {error:?}")))?;
    Ok(ContentAction::Text(change))
}

fn parse_position(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Object>,
    snapshot: &crate::core::text_snapshot::TextSnapshot,
) -> Result<usize, ScriptError> {
    let line = required_usize(scope, value, "line")?;
    let character = required_usize(scope, value, "character")?;
    snapshot
        .utf16_position_to_char(line, character)
        .ok_or_else(|| ScriptError::new(format!("invalid UTF-16 position {line}:{character}")))
}

fn property<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    object: v8::Local<v8::Object>,
    name: &str,
) -> Option<v8::Local<'scope, v8::Value>> {
    let name = v8::String::new(scope, name)?;
    object.get(scope, name.into())
}

fn required_object<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    object: v8::Local<v8::Object>,
    name: &str,
) -> Result<v8::Local<'scope, v8::Object>, ScriptError> {
    property(scope, object, name)
        .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
        .ok_or_else(|| ScriptError::new(format!("mode {name} must be an object")))
}

fn required_string(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    name: &str,
) -> Result<String, ScriptError> {
    optional_string(scope, object, name)?
        .ok_or_else(|| ScriptError::new(format!("mode {name} must be a string")))
}

fn optional_string(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    name: &str,
) -> Result<Option<String>, ScriptError> {
    let Some(value) = property(scope, object, name) else {
        return Ok(None);
    };
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    if !value.is_string() {
        return Err(ScriptError::new(format!("mode {name} must be a string")));
    }
    Ok(Some(value.to_rust_string_lossy(scope)))
}

fn required_usize(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    name: &str,
) -> Result<usize, ScriptError> {
    let value = property(scope, object, name)
        .and_then(|value| value.integer_value(scope))
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| ScriptError::new(format!("{name} must be a non-negative integer")))?;
    if value as u64 > 9_007_199_254_740_991_u64 {
        return Err(ScriptError::new(format!(
            "{name} exceeds JavaScript's safe integer range"
        )));
    }
    Ok(value)
}

fn json_to_v8<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    value: &serde_json::Value,
) -> Result<v8::Local<'scope, v8::Value>, ScriptError> {
    let json = serde_json::to_string(value)
        .map_err(|error| ScriptError::new(format!("failed to encode mode state: {error}")))?;
    let json = v8::String::new(scope, &json)
        .ok_or_else(|| ScriptError::new("mode state is too large for V8"))?;
    v8::json::parse(scope, json).ok_or_else(|| ScriptError::new("failed to decode mode state"))
}

fn v8_to_json(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
    name: &str,
) -> Result<serde_json::Value, ScriptError> {
    let json = v8::json::stringify(scope, value)
        .ok_or_else(|| ScriptError::new(format!("{name} must contain only structured data")))?
        .to_rust_string_lossy(scope);
    serde_json::from_str(&json)
        .map_err(|error| ScriptError::new(format!("invalid {name}: {error}")))
}

fn set_number(scope: &mut v8::PinScope, object: v8::Local<v8::Object>, name: &str, value: f64) {
    let key = v8::String::new(scope, name).unwrap();
    let value = v8::Number::new(scope, value);
    object.set(scope, key.into(), value.into());
}

fn set_object(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    name: &str,
    value: v8::Local<v8::Object>,
) {
    let key = v8::String::new(scope, name).unwrap();
    object.set(scope, key.into(), value.into());
}

fn set_value(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    name: &str,
    value: v8::Local<v8::Value>,
) {
    let key = v8::String::new(scope, name).unwrap();
    object.set(scope, key.into(), value);
}

fn throw_script_error(scope: &mut v8::PinScope, message: &str) {
    if let Some(message) = v8::String::new(scope, message) {
        scope.throw_exception(message.into());
    }
}

fn transpile_typescript(specifier: &str, source: &str) -> Result<String, ScriptError> {
    let specifier = ModuleSpecifier::parse(specifier)
        .map_err(|error| ScriptError::new(format!("invalid script specifier: {error}")))?;
    let parsed = parse_program(ParseParams {
        specifier,
        text: source.into(),
        media_type: MediaType::TypeScript,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })
    .map_err(|error| ScriptError::new(error.to_string()))?;
    let emitted = parsed
        .transpile(
            &TranspileOptions::default(),
            &TranspileModuleOptions::default(),
            &EmitOptions::default(),
        )
        .map_err(|error| ScriptError::new(error.to_string()))?
        .into_source();
    Ok(emitted.text)
}

fn transpile_module(path: &Path, source: &str) -> Result<String, ScriptError> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("js") => return Ok(source.to_owned()),
        Some("ts") => {}
        _ => {
            return Err(ScriptError::new(format!(
                "unsupported script extension: {}",
                path.display()
            )));
        }
    }

    let specifier = ModuleSpecifier::from_file_path(path)
        .map_err(|_| ScriptError::new(format!("invalid script path: {}", path.display())))?;
    let parsed = parse_module(ParseParams {
        specifier,
        text: source.into(),
        media_type: MediaType::TypeScript,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })
    .map_err(|error| ScriptError::new(error.to_string()))?;
    let emitted = parsed
        .transpile(
            &TranspileOptions::default(),
            &TranspileModuleOptions::default(),
            &EmitOptions::default(),
        )
        .map_err(|error| ScriptError::new(error.to_string()))?
        .into_source();
    Ok(emitted.text)
}

#[derive(Default)]
struct ModuleMap {
    root: PathBuf,
    by_path: HashMap<PathBuf, v8::Global<v8::Module>>,
    by_id: HashMap<i32, Vec<(PathBuf, v8::Global<v8::Module>)>>,
}

impl ModuleMap {
    fn reset(&mut self, root: PathBuf) {
        self.root = root;
        self.by_path.clear();
        self.by_id.clear();
    }

    fn insert(&mut self, path: PathBuf, module: v8::Global<v8::Module>, id: i32) {
        self.by_path.insert(path.clone(), module.clone());
        self.by_id.entry(id).or_default().push((path, module));
    }

    fn path_for(&self, id: i32, module: &v8::Global<v8::Module>) -> Option<&PathBuf> {
        self.by_id
            .get(&id)?
            .iter()
            .find(|(_, candidate)| candidate == module)
            .map(|(path, _)| path)
    }
}

fn load_module_tree<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    path: &Path,
    modules: &Rc<RefCell<ModuleMap>>,
) -> Result<v8::Local<'scope, v8::Module>, ScriptError> {
    if let Some(module) = modules.borrow().by_path.get(path).cloned() {
        return Ok(v8::Local::new(scope, module));
    }

    let source = fs::read_to_string(path)
        .map_err(|error| ScriptError::new(format!("failed to read {}: {error}", path.display())))?;
    let source = transpile_module(path, &source)?;
    let source = v8::String::new(scope, &source)
        .ok_or_else(|| ScriptError::new(format!("script is too large: {}", path.display())))?;
    let origin = module_origin(scope, path);
    let mut compiler_source = v8::script_compiler::Source::new(source, Some(&origin));
    let module = v8::script_compiler::compile_module(scope, &mut compiler_source)
        .ok_or_else(|| ScriptError::new(format!("failed to compile {}", path.display())))?;

    modules.borrow_mut().insert(
        path.to_owned(),
        v8::Global::new(scope, module),
        module.get_identity_hash().get(),
    );

    let requests = module.get_module_requests();
    for index in 0..requests.length() {
        let request = requests
            .get(scope, index)
            .and_then(|request| v8::Local::<v8::ModuleRequest>::try_from(request).ok())
            .ok_or_else(|| ScriptError::new("V8 returned an invalid module request"))?;
        let specifier = request.get_specifier().to_rust_string_lossy(scope);
        let dependency = resolve_path(path, &specifier, &modules.borrow().root)?;
        load_module_tree(scope, &dependency, modules)?;
    }

    Ok(module)
}

fn resolve_path(referrer: &Path, specifier: &str, root: &Path) -> Result<PathBuf, ScriptError> {
    let requested = Path::new(specifier);
    if !requested.is_absolute() && !specifier.starts_with("./") && !specifier.starts_with("../") {
        return Err(ScriptError::new(format!(
            "bare and URL imports are not supported: {specifier}"
        )));
    }
    let candidate = if requested.is_absolute() {
        requested.to_owned()
    } else {
        referrer.parent().unwrap_or(root).join(requested)
    };
    let candidate = candidate
        .canonicalize()
        .map_err(|error| ScriptError::new(format!("failed to resolve {specifier}: {error}")))?;
    if !candidate.starts_with(root) {
        return Err(ScriptError::new(format!(
            "script import escapes the config directory: {specifier}"
        )));
    }
    Ok(candidate)
}

fn module_origin<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    path: &Path,
) -> v8::ScriptOrigin<'scope> {
    let name = v8::String::new(scope, &path.display().to_string()).unwrap();
    let source_map = v8::undefined(scope);
    v8::ScriptOrigin::new(
        scope,
        name.into(),
        0,
        0,
        false,
        0,
        Some(source_map.into()),
        false,
        false,
        true,
        None,
    )
}

#[allow(clippy::unnecessary_wraps)]
fn resolve_module<'scope>(
    context: v8::Local<'scope, v8::Context>,
    specifier: v8::Local<'scope, v8::String>,
    _attributes: v8::Local<'scope, v8::FixedArray>,
    referrer: v8::Local<'scope, v8::Module>,
) -> Option<v8::Local<'scope, v8::Module>> {
    v8::callback_scope!(unsafe scope, context);
    let modules = scope.get_slot::<Rc<RefCell<ModuleMap>>>()?.clone();
    let referrer_global = v8::Global::new(scope, referrer);
    let map = modules.borrow();
    let referrer_path = map.path_for(referrer.get_identity_hash().get(), &referrer_global)?;
    let specifier = specifier.to_rust_string_lossy(scope);
    let path = match resolve_path(referrer_path, &specifier, &map.root) {
        Ok(path) => path,
        Err(error) => {
            let message = v8::String::new(scope, &error.to_string())?;
            scope.throw_exception(message.into());
            return None;
        }
    };
    map.by_path
        .get(&path)
        .cloned()
        .map(|module| v8::Local::new(scope, module))
}

fn current_exception(
    scope: &mut v8::PinnedRef<'_, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>,
    specifier: &str,
    phase: &str,
) -> ScriptError {
    let message = scope
        .exception()
        .map(|exception| exception.to_rust_string_lossy(scope))
        .unwrap_or_else(|| "unknown V8 exception".to_owned());
    ScriptError::new(format!("failed to {phase} {specifier}: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::view::View;
    use crate::core::buffer::Buffer;
    use crate::core::content::Content;
    use crate::core::content_store::ContentStore;
    use crate::protocol::ids::{ContentId, ViewId};

    #[test]
    fn transpiles_and_executes_typescript() {
        let mut host = ScriptHost::new();
        let result = host
            .evaluate_typescript("file:///config.ts", "const value: number = 41; value + 1;")
            .unwrap();

        assert_eq!(result, "42");
    }

    #[test]
    fn reports_typescript_parse_errors() {
        let error = transpile_typescript("file:///config.ts", "const value: = 1;")
            .unwrap_err()
            .to_string();

        assert!(error.contains("Expected"));
    }

    #[test]
    fn loads_local_typescript_module_graph() {
        let directory = tempfile::tempdir().unwrap();
        let helper = directory.path().join("helper.ts");
        let config = directory.path().join("config.ts");
        fs::write(&helper, "export const answer: number = 42;").unwrap();
        fs::write(
            &config,
            "import { answer } from './helper.ts'; globalThis.__answer = answer;",
        )
        .unwrap();

        let mut host = ScriptHost::new();
        host.execute_module(&config).unwrap();
        let value = host
            .evaluate_typescript("file:///probe.ts", "globalThis.__answer;")
            .unwrap();

        assert_eq!(value, "42");
    }

    #[test]
    fn registers_script_mode_that_returns_an_edit_effect() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.ts");
        fs::write(
            &config,
            r#"
editor.modes.define({
  name: "pairs",
  before: "vim",
  content: { create: () => ({ calls: 0 }) },
  view: { create: (content: { calls: number }) => ({ initial: content.calls }) },
  actions: {
    quote(context: {
      contentState: { calls: number },
      viewState: { initial: number },
    }) {
      context.contentState.calls++;
      context.viewState.initial++;
      return { flow: "stop", insertText: "\"\"" };
    },
  },
  keys: { "\"": "quote" },
});
"#,
        )
        .unwrap();

        let mut host = ScriptHost::new();
        host.execute_module(&config).unwrap();
        let host = Rc::new(RefCell::new(host));
        let mut modes = ScriptHost::script_modes(&host);
        let mode = modes.pop().unwrap();
        assert_eq!(mode.name().as_str(), "pairs");
        assert_eq!(mode.before().unwrap().as_str(), "vim");

        let content_id = ContentId(0);
        let mut contents = ContentStore::default();
        contents
            .insert(content_id, Content::Buffer(Buffer::new()))
            .unwrap();
        let view = View::new(content_id, contents.create_view_state(content_id).unwrap());
        let context = ModeViewContext::new(ViewId(0), &view, &contents);
        let content_context = ModeContentContext::new(content_id, &contents);
        let mut content_state = mode.create_content_state(&content_context).unwrap();
        let mut view_state = mode
            .create_view_state(content_state.as_ref(), &context)
            .unwrap();
        let result = mode
            .execute_view_with_content(
                content_state.as_mut(),
                view_state.as_mut(),
                &context,
                &ModeActionName::new("quote"),
            )
            .unwrap();
        let (flow, effects) = result.into_parts();

        assert_eq!(flow, InputFlow::Stop);
        assert_eq!(
            content_state.as_any().downcast_ref::<serde_json::Value>(),
            Some(&serde_json::json!({ "calls": 1 }))
        );
        assert_eq!(
            view_state.as_any().downcast_ref::<serde_json::Value>(),
            Some(&serde_json::json!({ "initial": 1 }))
        );
        assert!(matches!(
            effects.as_slice(),
            [crate::app::mode::ModeEffect::DeferredEdit(
                EditCommand::InsertText(text)
            )] if text == "\"\""
        ));
    }

    #[test]
    fn converts_utf16_content_edit_batch_to_content_action() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.ts");
        fs::write(
            &config,
            r#"
editor.modes.define({
  name: "unicode-edit",
  actions: {
    replace(context: { revision: number }) {
      return {
        contentEdits: {
          revision: context.revision,
          edits: [{
            range: {
              start: { line: 0, character: 1 },
              end: { line: 0, character: 3 },
            },
            text: "中",
          }],
        },
      };
    },
  },
});
"#,
        )
        .unwrap();
        let text_path = directory.path().join("text.txt");
        fs::write(&text_path, "a😀b").unwrap();

        let mut host = ScriptHost::new();
        host.execute_module(&config).unwrap();
        let host = Rc::new(RefCell::new(host));
        let mode = ScriptHost::script_modes(&host).pop().unwrap();
        let mut buffer = Buffer::new();
        buffer.open_path(text_path.to_str().unwrap()).unwrap();
        let content_id = ContentId(0);
        let mut contents = ContentStore::default();
        contents
            .insert(content_id, Content::Buffer(buffer))
            .unwrap();
        let view = View::new(content_id, contents.create_view_state(content_id).unwrap());
        let context = ModeViewContext::new(ViewId(0), &view, &contents);
        let before = context.text_snapshot().unwrap();
        let mut content_state = mode.new_content_state();
        let mut view_state = mode.new_view_state();
        let (_, effects) = mode
            .execute_view_with_content(
                content_state.as_mut(),
                view_state.as_mut(),
                &context,
                &ModeActionName::new("replace"),
            )
            .unwrap()
            .into_parts();
        let crate::app::mode::ModeEffect::Content(ContentAction::Text(change)) = &effects[0] else {
            panic!("script action should return a text content effect");
        };

        assert_eq!(before.apply(change).unwrap().to_owned_string(), "a中b");
    }
}
