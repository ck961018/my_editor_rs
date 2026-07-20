//! TypeScript runtime owned by the application layer.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Once};

use deno_ast::{
    EmitOptions, MediaType, ModuleSpecifier, ParseParams, TranspileModuleOptions, TranspileOptions,
    parse_module, parse_program,
};

use crate::app::command::{Command, ModeCommand, ModeValue};
use crate::app::mode::{
    CursorDomain, Mode, ModeContentContext, ModeError, ModeJobRequest, ModeJobResult, ModeResult,
    ModeState, ModeViewContext, ModeViewPolicy,
};
use crate::app::mode_name::{ModeActionName, ModeName};
use crate::core::keymap::Keymap;
use crate::protocol::content_query::{
    Color, ContentData, ContentQuery, CursorStyle, Face, FaceName, NamedTextDecoration, RowRange,
    SelectionShape,
};
use crate::protocol::key_event::{ArrowKey, KeyCode, KeyEvent};

mod primitives;
mod worker;

use primitives::PrimitiveRuntime;
use worker::ScriptWorker;

static V8_INIT: Once = Once::new();

include!(concat!(env!("OUT_DIR"), "/plugin_assets.rs"));

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
    plugin_root: Rc<RefCell<Option<String>>>,
    primitives: Rc<RefCell<PrimitiveRuntime>>,
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
        let plugin_root = Rc::new(RefCell::new(None));
        let primitives = PrimitiveRuntime::new();
        isolate.set_slot(modules.clone());
        isolate.set_slot(definitions.clone());
        isolate.set_slot(plugin_root.clone());
        isolate.set_slot(primitives.clone());

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
            plugin_root,
            primitives,
        }
    }

    pub(crate) fn execute_typescript(
        &mut self,
        specifier: &str,
        source: &str,
    ) -> Result<(), ScriptError> {
        self.evaluate_typescript(specifier, source).map(|_| ())
    }

    fn execute_embedded_plugin(&mut self, path: &str, source: &str) -> Result<(), ScriptError> {
        let root = path
            .rsplit_once('/')
            .map(|(root, _)| format!("{root}/"))
            .unwrap_or_default();
        self.plugin_root.replace(Some(root));
        let result = self.execute_typescript(&format!("file:///runtime/plugins/{path}"), source);
        self.plugin_root.replace(None);
        result
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
        arguments: &ModeValue,
        content_state: &mut ScriptModeState,
        view_state: &mut ScriptModeState,
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
        let arguments = json_to_v8(scope, &mode_value_to_json(arguments))?;
        set_value(scope, argument, "arguments", arguments);
        let content_value = json_to_v8(scope, &content_state.data)?;
        let view_value = json_to_v8(scope, &view_state.data)?;
        set_value(scope, argument, "contentState", content_value);
        set_value(scope, argument, "viewState", view_value);
        let primitive_id = self.primitives.borrow_mut().begin(context)?;
        primitives::install(scope, argument, primitive_id);
        let callback = v8::Local::new(scope, callback);
        let receiver = v8::undefined(scope).into();
        let callback_result = callback.call(scope, receiver, &[argument.into()]);
        let operations = self.primitives.borrow_mut().finish(primitive_id)?;
        let result = callback_result
            .ok_or_else(|| current_exception(scope, "script mode action", "execute"))?;
        let content_decorations = parse_decorations_property(
            scope,
            result,
            "contentDecorations",
            context.text_snapshot(),
            context.content_revision(),
        )?;
        let view_decorations = parse_decorations_property(
            scope,
            result,
            "viewDecorations",
            context.text_snapshot(),
            context.content_revision(),
        )?;
        let result = parse_action_result(scope, result, operations)?;
        let next_content = property(scope, argument, "contentState")
            .ok_or_else(|| ScriptError::new("script removed context.contentState"))?;
        let next_view = property(scope, argument, "viewState")
            .ok_or_else(|| ScriptError::new("script removed context.viewState"))?;
        let next_content = v8_to_json(scope, next_content, "contentState")?;
        let next_view = v8_to_json(scope, next_view, "viewState")?;
        view_policy_from_json(&next_view)?;
        scope.perform_microtask_checkpoint();
        content_state.data = next_content;
        view_state.data = next_view;
        if let Some(decorations) = content_decorations {
            content_state.decorations = DecorationSet::new(decorations);
        }
        if let Some(decorations) = view_decorations {
            view_state.decorations = DecorationSet::new(decorations);
        }
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

    fn create_content_state(
        &mut self,
        callback: Option<&v8::Global<v8::Function>>,
        context: &ModeContentContext<'_>,
    ) -> Result<serde_json::Value, ScriptError> {
        let Some(callback) = callback.cloned() else {
            return Ok(serde_json::Value::Null);
        };
        let v8_context = self.context.clone();
        v8::scope_with_context!(scope, &mut self.isolate, v8_context);
        v8::tc_scope!(let scope, scope);
        let argument = content_context_object(scope, context, true)?;
        let callback = v8::Local::new(scope, callback);
        let receiver = v8::undefined(scope).into();
        let value = callback
            .call(scope, receiver, &[argument.into()])
            .ok_or_else(|| current_exception(scope, "script content state factory", "execute"))?;
        v8_to_json(scope, value, "mode content state")
    }

    fn content_changed(
        &mut self,
        callback: &v8::Global<v8::Function>,
        context: &ModeContentContext<'_>,
        state: &mut ScriptModeState,
        change: &crate::core::content::ContentChange,
    ) -> Result<(), ScriptError> {
        let v8_context = self.context.clone();
        v8::scope_with_context!(scope, &mut self.isolate, v8_context);
        v8::tc_scope!(let scope, scope);
        let argument = content_context_object(scope, context, false)?;
        let content_state = json_to_v8(scope, &state.data)?;
        set_value(scope, argument, "contentState", content_state);
        let change_value = content_change_to_v8(scope, change)?;
        set_value(scope, argument, "change", change_value);
        let callback = v8::Local::new(scope, callback.clone());
        let receiver = v8::undefined(scope).into();
        callback
            .call(scope, receiver, &[argument.into()])
            .ok_or_else(|| current_exception(scope, "script content changed", "execute"))?;
        let next = property(scope, argument, "contentState")
            .ok_or_else(|| ScriptError::new("script removed context.contentState"))?;
        state.data = v8_to_json(scope, next, "contentState")?;
        scope.perform_microtask_checkpoint();
        Ok(())
    }

    fn take_content_job(
        &mut self,
        callback: &v8::Global<v8::Function>,
        context: &ModeContentContext<'_>,
        state: &mut ScriptModeState,
    ) -> Result<Option<ScriptJob>, ScriptError> {
        let v8_context = self.context.clone();
        v8::scope_with_context!(scope, &mut self.isolate, v8_context);
        v8::tc_scope!(let scope, scope);
        let argument = content_context_object(scope, context, false)?;
        let content_state = json_to_v8(scope, &state.data)?;
        set_value(scope, argument, "contentState", content_state);
        let callback = v8::Local::new(scope, callback);
        let receiver = v8::undefined(scope).into();
        let value = callback
            .call(scope, receiver, &[argument.into()])
            .ok_or_else(|| current_exception(scope, "script content job", "execute"))?;
        let next = property(scope, argument, "contentState")
            .ok_or_else(|| ScriptError::new("script removed context.contentState"))?;
        state.data = v8_to_json(scope, next, "contentState")?;
        scope.perform_microtask_checkpoint();
        if value.is_null_or_undefined() {
            return Ok(None);
        }
        let value = v8_to_json(scope, value, "content job")?;
        let mut job = ScriptJob::from_json(value)?;
        if job.include_text {
            job.text_snapshot = Some(
                context
                    .text_snapshot()
                    .ok_or_else(|| ScriptError::new("content job text requires text content"))?,
            );
        }
        Ok(Some(job))
    }

    fn apply_content_job(
        &mut self,
        callback: &v8::Global<v8::Function>,
        context: &ModeContentContext<'_>,
        state: &mut ScriptModeState,
        version: u64,
        result: &serde_json::Value,
    ) -> Result<bool, ScriptError> {
        let v8_context = self.context.clone();
        v8::scope_with_context!(scope, &mut self.isolate, v8_context);
        v8::tc_scope!(let scope, scope);
        let argument = content_context_object(scope, context, false)?;
        let content_state = json_to_v8(scope, &state.data)?;
        set_value(scope, argument, "contentState", content_state);
        set_number(scope, argument, "jobVersion", version as f64);
        let result_value = json_to_v8(scope, result)?;
        set_value(scope, argument, "arguments", result_value);
        let callback = v8::Local::new(scope, callback);
        let receiver = v8::undefined(scope).into();
        let value = callback
            .call(scope, receiver, &[argument.into()])
            .ok_or_else(|| current_exception(scope, "script content applyJob", "execute"))?;
        let decorations = parse_decorations_property(
            scope,
            value,
            "contentDecorations",
            context.text_snapshot(),
            context.content_revision(),
        )?;
        let next = property(scope, argument, "contentState")
            .ok_or_else(|| ScriptError::new("script removed context.contentState"))?;
        let next = v8_to_json(scope, next, "contentState")?;
        let changed = next != state.data || decorations.is_some();
        state.data = next;
        if let Some(decorations) = decorations {
            state.decorations = DecorationSet::new(decorations);
        }
        scope.perform_microtask_checkpoint();
        Ok(changed)
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
    input_action: Option<usize>,
    faces: Vec<(FaceName, Face)>,
    before: Option<ModeName>,
    create_content: Option<v8::Global<v8::Function>>,
    content_changed: Option<v8::Global<v8::Function>>,
    content_job: Option<v8::Global<v8::Function>>,
    content_apply_job: Option<v8::Global<v8::Function>>,
    create_view: Option<v8::Global<v8::Function>>,
    worker: Option<ScriptWorker>,
}

#[derive(Clone)]
struct ScriptModeState {
    data: serde_json::Value,
    decorations: DecorationSet,
}

#[derive(Clone, Default)]
struct DecorationSet {
    values: Arc<Vec<NamedTextDecoration>>,
    prefix_max_end: Arc<Vec<usize>>,
}

impl DecorationSet {
    fn new(values: Vec<NamedTextDecoration>) -> Self {
        let mut max_end = 0;
        let prefix_max_end = values
            .iter()
            .map(|decoration| {
                max_end = max_end.max(decoration.end.char_index);
                max_end
            })
            .collect();
        Self {
            values: Arc::new(values),
            prefix_max_end: Arc::new(prefix_max_end),
        }
    }

    fn iter(&self) -> impl Iterator<Item = &NamedTextDecoration> {
        self.values.iter()
    }

    fn visible(
        &self,
        snapshot: &crate::core::text_snapshot::TextSnapshot,
        rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        let range = snapshot.char_range_for_rows(rows.start, rows.end);
        if range.is_empty() {
            return Vec::new();
        }
        let end = self
            .values
            .partition_point(|decoration| decoration.start.char_index < range.end);
        let start = self.prefix_max_end[..end].partition_point(|end| *end <= range.start);
        self.values[start..end]
            .iter()
            .filter(|decoration| decoration.end.char_index > range.start)
            .cloned()
            .collect()
    }
}

struct ScriptJob {
    slot: String,
    version: u64,
    message: serde_json::Value,
    include_text: bool,
    text_snapshot: Option<crate::core::text_snapshot::TextSnapshot>,
}

enum ScriptJobOutput {
    Response(serde_json::Value),
    CallbackError(String),
}

impl ScriptJob {
    fn from_json(value: serde_json::Value) -> Result<Self, ScriptError> {
        let object = value
            .as_object()
            .ok_or_else(|| ScriptError::new("content job must be an object"))?;
        let slot = object
            .get("slot")
            .and_then(serde_json::Value::as_str)
            .filter(|slot| !slot.is_empty())
            .ok_or_else(|| ScriptError::new("content job.slot must be a non-empty string"))?
            .to_owned();
        let version = object
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                ScriptError::new("content job.version must be a non-negative integer")
            })?;
        let message = object
            .get("message")
            .cloned()
            .ok_or_else(|| ScriptError::new("content job.message is required"))?;
        let include_text = object
            .get("includeText")
            .map(|value| {
                value
                    .as_bool()
                    .ok_or_else(|| ScriptError::new("content job.includeText must be a boolean"))
            })
            .transpose()?
            .unwrap_or(false);
        if include_text {
            let message = message.as_object().ok_or_else(|| {
                ScriptError::new("content job.message must be an object when includeText is true")
            })?;
            if message.contains_key("text") {
                return Err(ScriptError::new(
                    "content job.message.text is reserved when includeText is true",
                ));
            }
        }
        Ok(Self {
            slot,
            version,
            message,
            include_text,
            text_snapshot: None,
        })
    }
}

impl ScriptModeState {
    fn new(data: serde_json::Value) -> Self {
        Self {
            data,
            decorations: DecorationSet::default(),
        }
    }
}

pub(crate) struct ScriptMode {
    host: Rc<RefCell<ScriptHost>>,
    mode_index: usize,
    name: ModeName,
    actions: Vec<ModeActionName>,
    keymap: Keymap<Command>,
    input_action: Option<ModeActionName>,
    faces: Vec<(FaceName, Face)>,
    before: Option<ModeName>,
    create_content: Option<v8::Global<v8::Function>>,
    content_changed: Option<v8::Global<v8::Function>>,
    content_job: Option<v8::Global<v8::Function>>,
    content_apply_job: Option<v8::Global<v8::Function>>,
    create_view: Option<v8::Global<v8::Function>>,
    worker: Option<ScriptWorker>,
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
        let actions: Vec<_> = definition
            .actions
            .iter()
            .map(|action| action.name.clone())
            .collect();
        let input_action = definition.input_action.map(|index| actions[index].clone());
        Self {
            host,
            mode_index,
            name: definition.name,
            actions,
            keymap,
            input_action,
            faces: definition.faces,
            before: definition.before,
            create_content: definition.create_content,
            content_changed: definition.content_changed,
            content_job: definition.content_job,
            content_apply_job: definition.content_apply_job,
            create_view: definition.create_view,
            worker: definition.worker,
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

    fn faces(&self) -> Vec<(FaceName, Face)> {
        self.faces.clone()
    }

    fn create_content_state(
        &self,
        context: &ModeContentContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        self.host
            .borrow_mut()
            .create_content_state(self.create_content.as_ref(), context)
            .map(|state| Box::new(ScriptModeState::new(state)) as Box<dyn ModeState>)
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
        let content_state = &script_state(content_state, &self.name)?.data;
        let state = self
            .host
            .borrow_mut()
            .create_state(self.create_view.as_ref(), Some(content_state))
            .map_err(|error| ModeError::CallbackFailed {
                mode: self.name.clone(),
                message: error.to_string(),
            })?;
        view_policy_from_json(&state).map_err(|error| ModeError::CallbackFailed {
            mode: self.name.clone(),
            message: error.to_string(),
        })?;
        Ok(Box::new(ScriptModeState::new(state)))
    }

    fn input_keymap<'a>(
        &'a self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> &'a Keymap<Command> {
        &self.keymap
    }

    fn input_typing(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> Option<Command> {
        let action = self.input_action.clone()?;
        Some(Command::Mode(
            ModeCommand::new(self.name.clone(), action).with_arguments(key_event_arguments(key)),
        ))
    }

    fn view_policy(
        &self,
        _content_state: &dyn ModeState,
        view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> ModeViewPolicy {
        script_state(view_state, &self.name)
            .ok()
            .and_then(|state| view_policy_from_json(&state.data).ok())
            .unwrap_or_default()
    }

    fn on_content_changed(
        &self,
        state: &mut dyn ModeState,
        context: &ModeContentContext<'_>,
        change: &crate::core::content::ContentChange,
    ) -> Result<(), ModeError> {
        let state = script_state_mut(state, &self.name)?;
        let crate::core::content::ContentChange::Text(text_change) = change;
        let mapped = state
            .decorations
            .iter()
            .filter_map(|decoration| {
                let start = text_change.map_position(
                    decoration.start.char_index,
                    crate::core::transaction::Affinity::After,
                );
                let end = text_change.map_position(
                    decoration.end.char_index,
                    crate::core::transaction::Affinity::Before,
                );
                (start < end).then(|| NamedTextDecoration {
                    start: crate::protocol::selection::TextOffset { char_index: start },
                    end: crate::protocol::selection::TextOffset { char_index: end },
                    face: decoration.face.clone(),
                })
            })
            .collect();
        state.decorations = DecorationSet::new(mapped);
        if let Some(callback) = self.content_changed.as_ref() {
            self.host
                .borrow_mut()
                .content_changed(callback, context, state, change)
                .map_err(|error| ModeError::CallbackFailed {
                    mode: self.name.clone(),
                    message: error.to_string(),
                })?;
        }
        Ok(())
    }

    fn take_background_job(
        &self,
        state: &mut dyn ModeState,
        context: &ModeContentContext<'_>,
    ) -> Option<ModeJobRequest> {
        let (Some(callback), Some(worker)) = (self.content_job.as_ref(), self.worker.as_ref())
        else {
            return None;
        };
        let state = match script_state_mut(state, &self.name) {
            Ok(state) => state,
            Err(error) => {
                return Some(failed_script_job(error.to_string()));
            }
        };
        let job = match self
            .host
            .borrow_mut()
            .take_content_job(callback, context, state)
        {
            Ok(Some(job)) => job,
            Ok(None) => return None,
            Err(error) => return Some(failed_script_job(error.to_string())),
        };
        let worker = worker.clone();
        let ScriptJob {
            slot,
            version,
            mut message,
            text_snapshot,
            ..
        } = job;
        Some(ModeJobRequest::new(slot, version, move |cancellation| {
            if let Some(snapshot) = text_snapshot {
                message
                    .as_object_mut()
                    .expect("includeText message was validated")
                    .insert(
                        "text".to_owned(),
                        serde_json::Value::String(snapshot.to_owned_string()),
                    );
            }
            worker.request(message, cancellation).map(|result| {
                Box::new(ScriptJobOutput::Response(result)) as Box<dyn std::any::Any + Send>
            })
        }))
    }

    fn apply_background_job(
        &self,
        state: &mut dyn ModeState,
        context: &ModeContentContext<'_>,
        version: u64,
        result: ModeJobResult,
    ) -> Result<bool, ModeError> {
        let Some(callback) = self.content_apply_job.as_ref() else {
            return Ok(false);
        };
        let Ok(result) = result else {
            return Ok(false);
        };
        let result =
            result
                .downcast::<ScriptJobOutput>()
                .map_err(|_| ModeError::CallbackFailed {
                    mode: self.name.clone(),
                    message: "script worker returned an invalid host value".to_owned(),
                })?;
        let result = match *result {
            ScriptJobOutput::Response(result) => result,
            ScriptJobOutput::CallbackError(message) => {
                return Err(ModeError::CallbackFailed {
                    mode: self.name.clone(),
                    message,
                });
            }
        };
        let state = script_state_mut(state, &self.name)?;
        self.host
            .borrow_mut()
            .apply_content_job(callback, context, state, version, &result)
            .map_err(|error| ModeError::CallbackFailed {
                mode: self.name.clone(),
                message: error.to_string(),
            })
    }

    fn content_decorations(
        &self,
        content_state: &dyn ModeState,
        context: &ModeContentContext<'_>,
        visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        let Some(snapshot) = context.text_snapshot() else {
            return Vec::new();
        };
        script_state(content_state, &self.name)
            .map(|state| state.decorations.visible(&snapshot, visible_rows))
            .unwrap_or_default()
    }

    fn view_decorations(
        &self,
        _content_state: &dyn ModeState,
        view_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
        visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        let Some(snapshot) = context.text_snapshot() else {
            return Vec::new();
        };
        script_state(view_state, &self.name)
            .map(|state| state.decorations.visible(&snapshot, visible_rows))
            .unwrap_or_default()
    }

    fn execute_view_with_arguments(
        &self,
        content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
        action: &ModeActionName,
        arguments: &ModeValue,
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
                arguments,
                content_state,
                view_state,
            )
            .map_err(|error| ModeError::CallbackFailed {
                mode: self.name.clone(),
                message: error.to_string(),
            })
    }
}

fn failed_script_job(message: String) -> ModeJobRequest {
    ModeJobRequest::new("script-error", 0, move |_| {
        Ok(Box::new(ScriptJobOutput::CallbackError(message)))
    })
}

fn script_state<'state>(
    state: &'state dyn ModeState,
    mode: &ModeName,
) -> Result<&'state ScriptModeState, ModeError> {
    state
        .as_any()
        .downcast_ref::<ScriptModeState>()
        .ok_or_else(|| ModeError::CallbackFailed {
            mode: mode.clone(),
            message: "script content state has an invalid host type".to_owned(),
        })
}

fn script_state_mut<'state>(
    state: &'state mut dyn ModeState,
    mode: &ModeName,
) -> Result<&'state mut ScriptModeState, ModeError> {
    state
        .as_any_mut()
        .downcast_mut::<ScriptModeState>()
        .ok_or_else(|| ModeError::CallbackFailed {
            mode: mode.clone(),
            message: "script mode state has an invalid host type".to_owned(),
        })
}

pub(crate) fn load_default_plugins() -> Result<Rc<RefCell<ScriptHost>>, ScriptError> {
    let mut host = ScriptHost::new();
    let mut plugins = default_plugin_entries()?;
    plugins.sort_by_key(|plugin| plugin.0);
    for (_, path, source) in plugins {
        host.execute_embedded_plugin(path, source)?;
    }
    Ok(Rc::new(RefCell::new(host)))
}

fn default_plugin_entries() -> Result<Vec<(i64, &'static str, &'static str)>, ScriptError> {
    DEFAULT_PLUGIN_ASSETS
        .iter()
        .filter(|(path, _)| path.ends_with("/plugin.json"))
        .map(|(manifest_path, bytes)| {
            let manifest = std::str::from_utf8(bytes).map_err(|error| {
                ScriptError::new(format!("invalid UTF-8 in {manifest_path}: {error}"))
            })?;
            let manifest: serde_json::Value = serde_json::from_str(manifest).map_err(|error| {
                ScriptError::new(format!("invalid plugin manifest {manifest_path}: {error}"))
            })?;
            let entry = manifest
                .get("entry")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    ScriptError::new(format!("plugin manifest {manifest_path} has no entry"))
                })?;
            if entry.contains('/') || entry.contains('\\') || entry == "." || entry == ".." {
                return Err(ScriptError::new(format!(
                    "plugin manifest {manifest_path} has an invalid entry"
                )));
            }
            let directory = manifest_path
                .strip_suffix("plugin.json")
                .expect("filtered plugin manifest suffix");
            let entry_path = format!("{directory}{entry}");
            let (_, source) = DEFAULT_PLUGIN_ASSETS
                .iter()
                .find(|(path, _)| *path == entry_path)
                .ok_or_else(|| {
                    ScriptError::new(format!(
                        "plugin entry {entry_path} from {manifest_path} does not exist"
                    ))
                })?;
            let source = std::str::from_utf8(source).map_err(|error| {
                ScriptError::new(format!("invalid UTF-8 in {entry_path}: {error}"))
            })?;
            let order = manifest
                .get("order")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default();
            let path = DEFAULT_PLUGIN_ASSETS
                .iter()
                .find_map(|(path, _)| (*path == entry_path).then_some(*path))
                .expect("plugin entry was resolved");
            Ok((order, path, source))
        })
        .collect()
}

pub(crate) fn load_user_config() -> Result<Rc<RefCell<ScriptHost>>, ScriptError> {
    let host = load_default_plugins()?;
    let explicit = std::env::var_os("MY_EDITOR_CONFIG").map(PathBuf::from);
    let path = explicit.clone().or_else(default_config_path);
    let Some(path) = path else {
        return Ok(host);
    };
    if explicit.is_none() && !path.is_file() {
        return Ok(host);
    }

    host.borrow_mut().execute_module(&path)?;
    Ok(host)
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
    let input_action = optional_string(scope, object, "input")?
        .map(|name| {
            actions
                .iter()
                .position(|action| action.name.as_str() == name)
                .ok_or_else(|| ScriptError::new(format!("unknown input action '{name}'")))
        })
        .transpose()?;
    let faces = parse_faces(scope, object)?;
    let create_content = optional_factory(scope, object, "content")?;
    let content_changed = optional_section_callback(scope, object, "content", "changed")?;
    let content_job = optional_section_callback(scope, object, "content", "job")?;
    let content_apply_job = optional_section_callback(scope, object, "content", "applyJob")?;
    let create_view = optional_factory(scope, object, "view")?;
    let worker = optional_string(scope, object, "worker")?
        .map(|entry| {
            let root = scope
                .get_slot::<Rc<RefCell<Option<String>>>>()
                .and_then(|root| root.borrow().clone())
                .ok_or_else(|| {
                    ScriptError::new(
                        "mode workers currently require an embedded plugin resource root",
                    )
                })?;
            ScriptWorker::start(root, entry)
        })
        .transpose()?;
    if content_job.is_some() != worker.is_some() || content_apply_job.is_some() != worker.is_some()
    {
        return Err(ScriptError::new(
            "mode worker, content.job, and content.applyJob must be defined together",
        ));
    }
    Ok(ScriptModeDefinition {
        name: ModeName::new(name),
        actions,
        bindings,
        input_action,
        faces,
        before,
        create_content,
        content_changed,
        content_job,
        content_apply_job,
        create_view,
        worker,
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

fn optional_section_callback(
    scope: &mut v8::PinScope,
    definition: v8::Local<v8::Object>,
    section_name: &str,
    callback_name: &str,
) -> Result<Option<v8::Global<v8::Function>>, ScriptError> {
    let Some(value) = property(scope, definition, section_name) else {
        return Ok(None);
    };
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    let section = v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| ScriptError::new(format!("mode {section_name} must be an object")))?;
    let Some(callback) = property(scope, section, callback_name) else {
        return Ok(None);
    };
    if callback.is_null_or_undefined() {
        return Ok(None);
    }
    let callback = v8::Local::<v8::Function>::try_from(callback).map_err(|_| {
        ScriptError::new(format!(
            "mode {section_name}.{callback_name} must be a function"
        ))
    })?;
    Ok(Some(v8::Global::new(scope, callback)))
}

fn parse_faces(
    scope: &mut v8::PinScope,
    definition: v8::Local<v8::Object>,
) -> Result<Vec<(FaceName, Face)>, ScriptError> {
    let Some(value) = property(scope, definition, "faces") else {
        return Ok(Vec::new());
    };
    if value.is_null_or_undefined() {
        return Ok(Vec::new());
    }
    let faces = v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| ScriptError::new("mode faces must be an object"))?;
    let names = faces
        .get_own_property_names(scope, Default::default())
        .ok_or_else(|| ScriptError::new("failed to enumerate mode faces"))?;
    let mut parsed = Vec::with_capacity(names.length() as usize);
    for index in 0..names.length() {
        let name = names
            .get_index(scope, index)
            .ok_or_else(|| ScriptError::new("failed to read face name"))?;
        let face = faces
            .get(scope, name)
            .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
            .ok_or_else(|| ScriptError::new("mode face must be an object"))?;
        parsed.push((
            FaceName::new(name.to_rust_string_lossy(scope)),
            Face {
                foreground: parse_color(scope, face, "foreground")?,
                background: parse_color(scope, face, "background")?,
                bold: optional_bool(scope, face, "bold")?,
                italic: optional_bool(scope, face, "italic")?,
                underline: optional_bool(scope, face, "underline")?,
            },
        ));
    }
    Ok(parsed)
}

fn parse_color(
    scope: &mut v8::PinScope,
    face: v8::Local<v8::Object>,
    name: &str,
) -> Result<Option<Color>, ScriptError> {
    let Some(value) = property(scope, face, name) else {
        return Ok(None);
    };
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    if let Some(ansi) = value
        .integer_value(scope)
        .and_then(|value| u8::try_from(value).ok())
    {
        return Ok(Some(Color::Ansi(ansi)));
    }
    if value.is_string() {
        let value = value.to_rust_string_lossy(scope);
        if value.len() == 7 && value.starts_with('#') {
            let red = u8::from_str_radix(&value[1..3], 16).ok();
            let green = u8::from_str_radix(&value[3..5], 16).ok();
            let blue = u8::from_str_radix(&value[5..7], 16).ok();
            if let (Some(red), Some(green), Some(blue)) = (red, green, blue) {
                return Ok(Some(Color::Rgb { red, green, blue }));
            }
        }
    }
    Err(ScriptError::new(format!(
        "face {name} must be an ANSI index or #RRGGBB color"
    )))
}

fn optional_bool(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    name: &str,
) -> Result<Option<bool>, ScriptError> {
    let Some(value) = property(scope, object, name) else {
        return Ok(None);
    };
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    if !value.is_boolean() {
        return Err(ScriptError::new(format!("face {name} must be a boolean")));
    }
    Ok(Some(value.boolean_value(scope)))
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

fn key_event_arguments(key: KeyEvent) -> ModeValue {
    let mut value = BTreeMap::new();
    match key.code {
        KeyCode::Char(character) => {
            value.insert("code".to_owned(), ModeValue::String("character".to_owned()));
            value.insert(
                "character".to_owned(),
                ModeValue::String(character.to_string()),
            );
        }
        KeyCode::Arrow(direction) => {
            value.insert("code".to_owned(), ModeValue::String("arrow".to_owned()));
            value.insert(
                "direction".to_owned(),
                ModeValue::String(
                    match direction {
                        ArrowKey::Up => "up",
                        ArrowKey::Down => "down",
                        ArrowKey::Left => "left",
                        ArrowKey::Right => "right",
                    }
                    .to_owned(),
                ),
            );
        }
        KeyCode::Backspace => {
            value.insert("code".to_owned(), ModeValue::String("backspace".to_owned()));
        }
        KeyCode::Enter => {
            value.insert("code".to_owned(), ModeValue::String("enter".to_owned()));
        }
        KeyCode::Escape => {
            value.insert("code".to_owned(), ModeValue::String("escape".to_owned()));
        }
        KeyCode::Function(number) => {
            value.insert("code".to_owned(), ModeValue::String("function".to_owned()));
            value.insert("number".to_owned(), ModeValue::Integer(i64::from(number)));
        }
        KeyCode::Unknown => {
            value.insert("code".to_owned(), ModeValue::String("unknown".to_owned()));
        }
    }
    value.insert(
        "modifiers".to_owned(),
        ModeValue::Map(BTreeMap::from([
            ("alt".to_owned(), ModeValue::Bool(key.modifiers.alt)),
            ("ctrl".to_owned(), ModeValue::Bool(key.modifiers.ctrl)),
            ("shift".to_owned(), ModeValue::Bool(key.modifiers.shift)),
        ])),
    );
    ModeValue::Map(value)
}

fn mode_value_to_json(value: &ModeValue) -> serde_json::Value {
    match value {
        ModeValue::Null => serde_json::Value::Null,
        ModeValue::Bool(value) => serde_json::Value::Bool(*value),
        ModeValue::Integer(value) => serde_json::Value::Number((*value).into()),
        ModeValue::String(value) => serde_json::Value::String(value.clone()),
        ModeValue::List(values) => {
            serde_json::Value::Array(values.iter().map(mode_value_to_json).collect())
        }
        ModeValue::Map(values) => serde_json::Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), mode_value_to_json(value)))
                .collect(),
        ),
    }
}

fn parse_action_result(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
    operations: Vec<crate::app::operation::OperationRequest>,
) -> Result<ModeResult, ScriptError> {
    if value.is_null_or_undefined() {
        return Ok(ModeResult::operations(operations));
    }
    if value.is_boolean() {
        return Ok(if value.boolean_value(scope) {
            ModeResult::continue_with(operations)
        } else {
            ModeResult::operations(operations)
        });
    }
    let object = v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| ScriptError::new("script action must return a flow value or object"))?;
    let continue_input = property(scope, object, "continue")
        .filter(|value| !value.is_null_or_undefined())
        .map(|value| {
            value
                .is_boolean()
                .then(|| value.boolean_value(scope))
                .ok_or_else(|| ScriptError::new("action continue must be a boolean"))
        })
        .transpose()?
        .unwrap_or(false);
    Ok(if continue_input {
        ModeResult::continue_with(operations)
    } else {
        ModeResult::operations(operations)
    })
}

fn parse_decorations_property(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
    name: &str,
    snapshot: Option<crate::core::text_snapshot::TextSnapshot>,
    current_revision: Option<crate::protocol::revision::Revision>,
) -> Result<Option<Vec<NamedTextDecoration>>, ScriptError> {
    if value.is_null_or_undefined() || value.is_boolean() {
        return Ok(None);
    }
    let result = v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| ScriptError::new("script action must return an object or undefined"))?;
    let Some(value) = property(scope, result, name) else {
        return Ok(None);
    };
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    let snapshot =
        snapshot.ok_or_else(|| ScriptError::new("decorations require editable text content"))?;
    let snapshot_value = v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| ScriptError::new(format!("{name} must be an object")))?;
    let revision = required_usize(scope, snapshot_value, "revision")? as u64;
    let current_revision =
        current_revision.ok_or_else(|| ScriptError::new("decorations require a text revision"))?;
    if revision != current_revision.0 {
        return Err(ScriptError::new(format!(
            "stale decoration revision: expected {}, got {revision}",
            current_revision.0
        )));
    }
    let spans = property(scope, snapshot_value, "spans")
        .and_then(|value| v8::Local::<v8::Array>::try_from(value).ok())
        .ok_or_else(|| ScriptError::new(format!("{name}.spans must be an array")))?;
    let mut decorations = Vec::with_capacity(spans.length() as usize);
    for index in 0..spans.length() {
        let span = spans
            .get_index(scope, index)
            .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
            .ok_or_else(|| ScriptError::new(format!("decoration {index} must be an object")))?;
        let range = required_object(scope, span, "range")?;
        let start_value = required_object(scope, range, "start")?;
        let start = parse_position(scope, start_value, &snapshot)?;
        let end_value = required_object(scope, range, "end")?;
        let end = parse_position(scope, end_value, &snapshot)?;
        if start >= end {
            return Err(ScriptError::new(format!(
                "decoration {index} must have a non-empty ordered range"
            )));
        }
        decorations.push(NamedTextDecoration {
            start: crate::protocol::selection::TextOffset { char_index: start },
            end: crate::protocol::selection::TextOffset { char_index: end },
            face: FaceName::new(required_string(scope, span, "face")?),
        });
    }
    decorations.sort_by_key(|decoration| (decoration.start.char_index, decoration.end.char_index));
    Ok(Some(decorations))
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

fn json_to_mode_value(value: &serde_json::Value) -> Result<ModeValue, ScriptError> {
    Ok(match value {
        serde_json::Value::Null => ModeValue::Null,
        serde_json::Value::Bool(value) => ModeValue::Bool(*value),
        serde_json::Value::Number(value) => ModeValue::Integer(
            value
                .as_i64()
                .ok_or_else(|| ScriptError::new("mode arguments must use integer numbers"))?,
        ),
        serde_json::Value::String(value) => ModeValue::String(value.clone()),
        serde_json::Value::Array(values) => ModeValue::List(
            values
                .iter()
                .map(json_to_mode_value)
                .collect::<Result<_, _>>()?,
        ),
        serde_json::Value::Object(values) => ModeValue::Map(
            values
                .iter()
                .map(|(key, value)| Ok((key.clone(), json_to_mode_value(value)?)))
                .collect::<Result<_, ScriptError>>()?,
        ),
    })
}

fn view_policy_from_json(state: &serde_json::Value) -> Result<ModeViewPolicy, ScriptError> {
    let Some(value) = state.get("viewPolicy") else {
        return Ok(ModeViewPolicy::default());
    };
    if value.is_null() {
        return Ok(ModeViewPolicy::default());
    }
    let object = value
        .as_object()
        .ok_or_else(|| ScriptError::new("viewState.viewPolicy must be an object"))?;
    let string = |name: &str| -> Result<Option<&str>, ScriptError> {
        object
            .get(name)
            .map(|value| {
                value.as_str().ok_or_else(|| {
                    ScriptError::new(format!("viewState.viewPolicy.{name} must be a string"))
                })
            })
            .transpose()
    };
    let cursor_style = match string("cursorStyle")? {
        None => None,
        Some("default") => Some(CursorStyle::Default),
        Some("block") => Some(CursorStyle::Block),
        Some("bar") => Some(CursorStyle::Bar),
        Some(value) => return Err(ScriptError::new(format!("invalid cursor style: {value}"))),
    };
    let cursor_domain = match string("cursorDomain")? {
        None => None,
        Some("insertion-point") => Some(CursorDomain::InsertionPoint),
        Some("character") => Some(CursorDomain::Character),
        Some(value) => return Err(ScriptError::new(format!("invalid cursor domain: {value}"))),
    };
    let selection_shape = match string("selectionShape")? {
        None => None,
        Some("character") => Some(SelectionShape::Character),
        Some("character-inclusive") => Some(SelectionShape::CharacterInclusive),
        Some("line") => Some(SelectionShape::Line),
        Some(value) => {
            return Err(ScriptError::new(format!(
                "invalid selection shape: {value}"
            )));
        }
    };
    Ok(ModeViewPolicy {
        cursor_style,
        cursor_domain,
        selection_shape,
        selection_face: string("selectionFace")?.map(FaceName::new),
    })
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

fn set_string(scope: &mut v8::PinScope, object: v8::Local<v8::Object>, name: &str, value: &str) {
    let key = v8::String::new(scope, name).unwrap();
    let value = v8::String::new(scope, value).unwrap();
    object.set(scope, key.into(), value.into());
}

fn content_context_object<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    context: &ModeContentContext<'_>,
    include_text: bool,
) -> Result<v8::Local<'scope, v8::Object>, ScriptError> {
    let argument = v8::Object::new(scope);
    set_number(scope, argument, "contentId", context.content_id().0 as f64);
    if let Some(revision) = context.content_revision() {
        set_number(scope, argument, "revision", revision.0 as f64);
    }
    if include_text && let Some(snapshot) = context.text_snapshot() {
        set_string(scope, argument, "text", &snapshot.to_owned_string());
    }
    if let ContentData::DocumentStatus(status) = context.query_content(ContentQuery::DocumentStatus)
    {
        let document = v8::Object::new(scope);
        if let Some(file_name) = status.file_name {
            set_string(scope, document, "fileName", &file_name);
        }
        let key = v8::String::new(scope, "modified").unwrap();
        let modified = v8::Boolean::new(scope, status.modified);
        document.set(scope, key.into(), modified.into());
        set_object(scope, argument, "document", document);
    }
    Ok(argument)
}

fn content_change_to_v8<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    change: &crate::core::content::ContentChange,
) -> Result<v8::Local<'scope, v8::Value>, ScriptError> {
    let crate::core::content::ContentChange::Text(change) = change;
    let edits = change
        .to_edits()
        .map_err(|error| ScriptError::new(format!("invalid content change: {error:?}")))?;
    let values = v8::Array::new(scope, i32::try_from(edits.len()).unwrap_or(i32::MAX));
    for (index, edit) in edits.into_iter().enumerate() {
        let value = v8::Object::new(scope);
        set_number(scope, value, "startCharacter", edit.range.start as f64);
        set_number(scope, value, "endCharacter", edit.range.end as f64);
        set_string(scope, value, "text", &edit.insert);
        values.set_index(
            scope,
            u32::try_from(index).expect("edit index overflow"),
            value.into(),
        );
    }
    Ok(values.into())
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
    use crate::app::mode::InputFlow;
    use crate::app::view::View;
    use crate::core::action::ContentAction;
    use crate::core::buffer::Buffer;
    use crate::core::command::EditCommand;
    use crate::core::content::Content;
    use crate::core::content_store::ContentStore;
    use crate::protocol::ids::{ContentId, ViewId};

    #[test]
    fn decoration_set_returns_only_spans_intersecting_visible_rows() {
        let snapshot = crate::core::text_snapshot::TextSnapshot::new(&ropey::Rope::from_str(
            &"a\n".repeat(100),
        ));
        let face = FaceName::new("syntax.test");
        let decorations = DecorationSet::new(vec![
            NamedTextDecoration {
                start: crate::protocol::selection::TextOffset { char_index: 0 },
                end: crate::protocol::selection::TextOffset { char_index: 150 },
                face: face.clone(),
            },
            NamedTextDecoration {
                start: crate::protocol::selection::TextOffset { char_index: 10 },
                end: crate::protocol::selection::TextOffset { char_index: 20 },
                face: face.clone(),
            },
            NamedTextDecoration {
                start: crate::protocol::selection::TextOffset { char_index: 100 },
                end: crate::protocol::selection::TextOffset { char_index: 101 },
                face,
            },
        ]);

        let visible = decorations.visible(&snapshot, RowRange { start: 50, end: 51 });

        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].start.char_index, 0);
        assert_eq!(visible[1].start.char_index, 100);
    }

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
    fn registers_script_mode_that_calls_a_native_primitive() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.ts");
        fs::write(
            &config,
            r#"
editor.modes.define({
  name: "pairs",
  before: "base-mode",
  content: { create: () => ({ calls: 0 }) },
  view: { create: (content: { calls: number }) => ({ initial: content.calls }) },
  actions: {
    quote(context) {
      context.contentState.calls++;
      context.viewState.initial++;
      context.text.insert("\"\"");
      return context.handled();
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
        assert_eq!(mode.before().unwrap().as_str(), "base-mode");

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
            .execute_view_with_arguments(
                content_state.as_mut(),
                view_state.as_mut(),
                &context,
                &ModeActionName::new("quote"),
                &ModeValue::Null,
            )
            .unwrap();
        let (flow, operations) = result.into_parts();

        assert_eq!(flow, InputFlow::Stop);
        assert_eq!(
            &content_state
                .as_any()
                .downcast_ref::<ScriptModeState>()
                .unwrap()
                .data,
            &serde_json::json!({ "calls": 1 })
        );
        assert_eq!(
            &view_state
                .as_any()
                .downcast_ref::<ScriptModeState>()
                .unwrap()
                .data,
            &serde_json::json!({ "initial": 1 })
        );
        assert!(matches!(
            operations.as_slice(),
            [crate::app::operation::OperationRequest::View {
                operation: crate::app::operation::ViewOperation::Edit(
                    EditCommand::InsertText(text)
                ),
                ..
            }] if text == "\"\""
        ));
    }

    #[test]
    fn script_action_faults_do_not_publish_mutated_mode_state() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.ts");
        fs::write(
            &config,
            r#"
editor.modes.define({
  name: "faulty-state",
  content: { create: () => ({ calls: 0 }) },
  view: {
    create: () => ({
      calls: 0,
      viewPolicy: { cursorStyle: "block" },
    }),
  },
  actions: {
    throwing(context) {
      context.contentState.calls++;
      context.viewState.calls++;
      throw new Error("action exploded");
    },
    invalid(context) {
      context.contentState.calls++;
      context.viewState.calls++;
      context.viewState.viewPolicy.cursorStyle = 42;
      return context.handled();
    },
  },
});
"#,
        )
        .unwrap();

        let mut host = ScriptHost::new();
        host.execute_module(&config).unwrap();
        let host = Rc::new(RefCell::new(host));
        let mode = ScriptHost::script_modes(&host).pop().unwrap();
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

        let throwing = mode
            .execute_view_with_arguments(
                content_state.as_mut(),
                view_state.as_mut(),
                &context,
                &ModeActionName::new("throwing"),
                &ModeValue::Null,
            )
            .unwrap_err()
            .to_string();
        assert!(throwing.contains("action exploded"), "{throwing}");
        assert_eq!(
            script_state(content_state.as_ref(), mode.name())
                .unwrap()
                .data,
            serde_json::json!({ "calls": 0 })
        );
        assert_eq!(
            script_state(view_state.as_ref(), mode.name()).unwrap().data,
            serde_json::json!({
                "calls": 0,
                "viewPolicy": { "cursorStyle": "block" },
            })
        );

        let invalid = mode
            .execute_view_with_arguments(
                content_state.as_mut(),
                view_state.as_mut(),
                &context,
                &ModeActionName::new("invalid"),
                &ModeValue::Null,
            )
            .unwrap_err()
            .to_string();
        assert!(
            invalid.contains("cursorStyle must be a string"),
            "{invalid}"
        );
        assert_eq!(
            script_state(content_state.as_ref(), mode.name())
                .unwrap()
                .data,
            serde_json::json!({ "calls": 0 })
        );
        assert_eq!(
            script_state(view_state.as_ref(), mode.name()).unwrap().data,
            serde_json::json!({
                "calls": 0,
                "viewPolicy": { "cursorStyle": "block" },
            })
        );
    }

    #[test]
    fn default_plugins_follow_manifest_order() {
        let host = load_default_plugins().unwrap();
        let host = host.borrow();
        let definitions = host.definitions.borrow();

        assert_eq!(
            definitions
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>(),
            vec!["vim", "syntax-highlighting"]
        );
    }

    #[test]
    fn native_apply_edits_converts_utf16_positions_to_content_action() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.ts");
        fs::write(
            &config,
            r#"
editor.modes.define({
  name: "unicode-edit",
  actions: {
    replace(context) {
      context.text.applyEdits([{
            range: {
              start: { line: 0, character: 1 },
              end: { line: 0, character: 3 },
            },
            text: "中",
      }]);
      return context.handled();
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
        let content_context = ModeContentContext::new(content_id, &contents);
        let mut content_state = mode.create_content_state(&content_context).unwrap();
        let mut view_state = mode
            .create_view_state(content_state.as_ref(), &context)
            .unwrap();
        let (_, operations) = mode
            .execute_view_with_arguments(
                content_state.as_mut(),
                view_state.as_mut(),
                &context,
                &ModeActionName::new("replace"),
                &ModeValue::Null,
            )
            .unwrap()
            .into_parts();
        let crate::app::operation::OperationRequest::View {
            operation:
                crate::app::operation::ViewOperation::ApplyContent(ContentAction::Text(change)),
            ..
        } = &operations[0]
        else {
            panic!("script action should return a text content effect");
        };

        assert_eq!(before.apply(change).unwrap().to_owned_string(), "a中b");
    }

    #[test]
    fn rejects_primitives_from_a_retained_action_context() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.ts");
        fs::write(
            &config,
            r#"
let retained;
editor.modes.define({
  name: "retained-context",
  actions: {
    retain(context) {
      retained = context;
      return context.handled();
    },
    reuse(context) {
      retained.cursor.moveLeft();
      return context.handled();
    },
  },
});
"#,
        )
        .unwrap();

        let mut host = ScriptHost::new();
        host.execute_module(&config).unwrap();
        let host = Rc::new(RefCell::new(host));
        let mode = ScriptHost::script_modes(&host).pop().unwrap();
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

        mode.execute_view_with_arguments(
            content_state.as_mut(),
            view_state.as_mut(),
            &context,
            &ModeActionName::new("retain"),
            &ModeValue::Null,
        )
        .unwrap();
        let error = mode
            .execute_view_with_arguments(
                content_state.as_mut(),
                view_state.as_mut(),
                &context,
                &ModeActionName::new("reuse"),
                &ModeValue::Null,
            )
            .unwrap_err()
            .to_string();

        assert!(error.contains("current action"), "{error}");
    }
}
