//! TypeScript runtime owned by the application layer.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Once};
use std::time::Duration;

use crate::api::{LoadedEditorConfiguration, LoadedScriptModes, ScriptDiagnostic};
use vell_core::content::ContentKind;
use vell_mode::command::ModeValue;
use vell_mode::mode_name::{ModeActionName, ModeName};
use vell_mode::operation::MAX_MODE_CALLBACK_OPERATIONS;
use vell_mode::{
    Mode, ModeContentContext, ModeError, ModeJobRequest, ModeResult, ModeState, ModeViewContext,
};
use vell_protocol::content_query::{
    Color, Face, FaceName, FaceOverride, FacePatch, FaceValue, NamedTextDecoration, RowRange,
    ThemeName,
};
use vell_protocol::key_event::{ArrowKey, KeyCode, KeyEvent};

mod bridge;
mod host;
mod invocation;
mod mode_adapter;
mod module;
mod primitives;
mod schema;
mod worker;

use bridge::{
    content_change_to_v8, content_context_object, json_to_mode_value, json_to_v8, optional_string,
    parse_position, property, required_object, required_string, required_usize, set_number,
    set_object, set_resource_facts, set_save_state, set_value, throw_script_error, v8_to_json,
    view_policy_from_json,
};
pub use host::ScriptHost;
use invocation::{
    HeapLimitState, InvocationWatchdog, ScriptExecutionBudget, ScriptInvocationKind,
    WatchdogOutcome, call_script_callback, install_heap_limit, perform_microtask_checkpoint,
    recover_heap_limit,
};
use mode_adapter::ScriptMode;
use module::{
    ModuleMap, current_exception, load_module_tree, resolve_module, transpile_typescript,
};
use primitives::PrimitiveRuntime;
use schema::install_editor_api;
use worker::ScriptWorker;

static V8_INIT: Once = Once::new();
const V2_INPUT_ACTION: &str = "$input";
const SCRIPT_CALLBACK_TIMEOUT: Duration = Duration::from_secs(2);
const SCRIPT_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_SCRIPT_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const MAX_MODULE_GRAPH_BYTES: usize = 16 * 1024 * 1024;
const MAX_SCRIPT_JSON_BYTES: usize = 4 * 1024 * 1024;
const MAX_SCRIPT_INPUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_SCRIPT_OPERATIONS: usize = MAX_MODE_CALLBACK_OPERATIONS;
const MAX_SCRIPT_DECORATIONS: usize = 100_000;
const SCRIPT_HEAP_LIMIT_BYTES: usize = 128 * 1024 * 1024;
const SCRIPT_HEAP_RECOVERY_BYTES: usize = 16 * 1024 * 1024;

include!(concat!(env!("OUT_DIR"), "/plugin_assets.rs"));

#[derive(Debug)]
pub struct ScriptError {
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

fn ensure_size(label: &str, actual: usize, limit: usize) -> Result<(), ScriptError> {
    if actual > limit {
        return Err(ScriptError::new(format!(
            "script limit exceeded for {label}: {actual} bytes exceeds {limit}"
        )));
    }
    Ok(())
}

fn ensure_count(label: &str, actual: usize, limit: usize) -> Result<(), ScriptError> {
    if actual > limit {
        return Err(ScriptError::new(format!(
            "script limit exceeded for {label}: {actual} exceeds {limit}"
        )));
    }
    Ok(())
}

fn ensure_file_size(path: &Path, label: &str, limit: usize) -> Result<(), ScriptError> {
    let bytes = fs::metadata(path)
        .map_err(|error| {
            ScriptError::new(format!("failed to inspect {}: {error}", path.display()))
        })?
        .len();
    if bytes > limit as u64 {
        return Err(ScriptError::new(format!(
            "script limit exceeded for {label}: {bytes} bytes exceeds {limit}"
        )));
    }
    Ok(())
}

#[derive(Clone)]
struct ScriptActionDefinition {
    name: ModeActionName,
    callback: v8::Global<v8::Function>,
}

#[derive(Clone)]
struct ScriptAnalysisDefinition {
    slot: String,
    input: v8::Global<v8::Function>,
    apply: v8::Global<v8::Function>,
    worker: ScriptWorker,
    snapshot_text: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScriptApiVersion {
    V1,
    V2,
}

#[derive(Clone, Default)]
struct ScriptDiagnostics {
    messages: Vec<ScriptDiagnostic>,
    v1_deprecation_reported: bool,
}

#[derive(Clone, Default)]
struct ScriptConfigurationDraft {
    theme: Option<ThemeName>,
    face_overrides: Vec<FaceOverride>,
}

impl ScriptApiVersion {
    fn content_state_name(self) -> &'static str {
        match self {
            Self::V1 => "contentState",
            Self::V2 => "state",
        }
    }
}

#[derive(Clone)]
struct ScriptAdapterDefinition {
    actions: Vec<ScriptActionDefinition>,
    bindings: Vec<(KeyEvent, usize)>,
    input_action: Option<usize>,
    input: Option<v8::Global<v8::Function>>,
    create_content: Option<v8::Global<v8::Function>>,
    content_changed: Option<v8::Global<v8::Function>>,
    content_job: Option<v8::Global<v8::Function>>,
    content_apply_job: Option<v8::Global<v8::Function>>,
    create_view: Option<v8::Global<v8::Function>>,
    worker: Option<ScriptWorker>,
    analyses: Vec<ScriptAnalysisDefinition>,
}

#[derive(Clone, Default)]
struct ScriptAdapterDefinitions {
    buffer: Option<ScriptAdapterDefinition>,
    status_bar: Option<ScriptAdapterDefinition>,
}

#[derive(Clone)]
struct ScriptModeDefinition {
    name: ModeName,
    version: ScriptApiVersion,
    faces: Vec<(FaceName, Face)>,
    before: Option<ModeName>,
    adapters: ScriptAdapterDefinitions,
}

#[derive(Clone)]
struct ScriptModeState {
    data: serde_json::Value,
    decorations: DecorationSet,
    analysis_decorations: HashMap<String, DecorationSet>,
    analysis_schedules: HashMap<String, ScriptAnalysisSchedule>,
    next_analysis_version: u64,
    analysis_input_epoch: u64,
}

#[derive(Clone)]
struct ScriptAnalysisSchedule {
    version: u64,
    content_revision: u64,
    input_epoch: u64,
    message: Option<serde_json::Value>,
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
        snapshot: &vell_core::text_snapshot::TextSnapshot,
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

fn map_decoration_set(
    decorations: &DecorationSet,
    change: &vell_core::transaction::TextChangeSet,
) -> DecorationSet {
    DecorationSet::new(
        decorations
            .iter()
            .filter_map(|decoration| {
                let start = change.map_position(
                    decoration.start.char_index,
                    vell_core::transaction::Affinity::After,
                );
                let end = change.map_position(
                    decoration.end.char_index,
                    vell_core::transaction::Affinity::Before,
                );
                (start < end).then(|| NamedTextDecoration {
                    start: vell_protocol::selection::TextOffset { char_index: start },
                    end: vell_protocol::selection::TextOffset { char_index: end },
                    face: decoration.face.clone(),
                })
            })
            .collect(),
    )
}

struct ScriptJob {
    slot: String,
    version: u64,
    message: serde_json::Value,
    include_text: bool,
    text_snapshot: Option<vell_core::text_snapshot::TextSnapshot>,
}

struct PreparedAnalysisJob {
    message: Option<serde_json::Value>,
    text_snapshot: Option<vell_core::text_snapshot::TextSnapshot>,
}

enum ScriptJobOutput {
    Response(serde_json::Value),
    Disabled,
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
            analysis_decorations: HashMap::new(),
            analysis_schedules: HashMap::new(),
            next_analysis_version: 0,
            analysis_input_epoch: 0,
        }
    }

    fn publish_external_data(&mut self, data: serde_json::Value) {
        if self.data != data {
            self.analysis_input_epoch = self
                .analysis_input_epoch
                .checked_add(1)
                .expect("script analysis input epoch overflow");
            self.data = data;
        }
    }

    fn mark_analysis_output_change(&mut self) {
        self.analysis_input_epoch = self
            .analysis_input_epoch
            .checked_add(1)
            .expect("script analysis input epoch overflow");
    }

    fn reconcile_analysis_input(
        &mut self,
        slot: &str,
        content_revision: u64,
        message: &Option<serde_json::Value>,
    ) -> bool {
        let Some(schedule) = self.analysis_schedules.get_mut(slot) else {
            return false;
        };
        if schedule.content_revision != content_revision || schedule.message != *message {
            return false;
        }
        schedule.input_epoch = self.analysis_input_epoch;
        true
    }

    fn record_analysis_request(
        &mut self,
        slot: &str,
        content_revision: u64,
        message: Option<serde_json::Value>,
    ) -> u64 {
        let version = self.next_analysis_version;
        self.next_analysis_version = self
            .next_analysis_version
            .checked_add(1)
            .expect("script analysis version overflow");
        self.analysis_schedules.insert(
            slot.to_owned(),
            ScriptAnalysisSchedule {
                version,
                content_revision,
                input_epoch: self.analysis_input_epoch,
                message,
            },
        );
        version
    }

    fn analysis_request_is_current(&self, slot: &str, version: u64, content_revision: u64) -> bool {
        self.analysis_schedules.get(slot).is_some_and(|schedule| {
            schedule.version == version
                && schedule.content_revision == content_revision
                && schedule.input_epoch == self.analysis_input_epoch
        })
    }

    fn accept_analysis_input(
        &mut self,
        slot: &str,
        version: u64,
        content_revision: u64,
        message: Option<serde_json::Value>,
    ) {
        let Some(schedule) = self.analysis_schedules.get_mut(slot) else {
            return;
        };
        if schedule.version == version && schedule.content_revision == content_revision {
            schedule.input_epoch = self.analysis_input_epoch;
            schedule.message = message;
        }
    }
}

fn failed_script_job(message: String) -> ModeJobRequest {
    ModeJobRequest::new("script-error", 0, move |_| {
        Ok(Box::new(ScriptJobOutput::CallbackError(message)))
    })
}

fn disabled_script_job(slot: String, version: u64) -> ModeJobRequest {
    ModeJobRequest::new(slot, version, |_| Ok(Box::new(ScriptJobOutput::Disabled)))
}

fn script_job_request(job: ScriptJob, worker: ScriptWorker) -> ModeJobRequest {
    let ScriptJob {
        slot,
        version,
        mut message,
        text_snapshot,
        ..
    } = job;
    ModeJobRequest::new(slot, version, move |cancellation| {
        if let Some(snapshot) = text_snapshot {
            message
                .as_object_mut()
                .expect("text snapshot analysis message was validated")
                .insert(
                    "text".to_owned(),
                    serde_json::Value::String(snapshot.to_owned_string()),
                );
        }
        worker.request(message, cancellation).map(|result| {
            Box::new(ScriptJobOutput::Response(result)) as Box<dyn std::any::Any + Send>
        })
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

fn load_default_plugins() -> Result<Rc<RefCell<ScriptHost>>, ScriptError> {
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

fn load_user_config() -> Result<Rc<RefCell<ScriptHost>>, ScriptError> {
    let host = load_default_plugins()?;
    let Some(path) = resolve_config_path(
        std::env::var_os("VELL_CONFIG").map(PathBuf::from),
        default_config_root(),
    ) else {
        return Ok(host);
    };

    let _ = load_optional_user_config(&host, &path);
    Ok(host)
}

fn load_optional_user_config(
    host: &Rc<RefCell<ScriptHost>>,
    path: &Path,
) -> Result<(), ScriptError> {
    let result = host.borrow_mut().execute_module(path);
    if let Err(error) = &result {
        eprintln!(
            "warning: failed to load Vell config '{}': {error}",
            path.display()
        );
    }
    for diagnostic in host.borrow_mut().take_diagnostics() {
        eprintln!("warning: {}", diagnostic.message);
    }
    result
}

pub fn load_default_modes() -> Result<Vec<Box<dyn Mode>>, ScriptError> {
    let host = load_default_plugins()?;
    Ok(ScriptHost::script_modes(&host)
        .into_iter()
        .map(|mode| Box::new(mode) as Box<dyn Mode>)
        .collect())
}

pub fn load_user_modes() -> Result<Vec<Box<dyn Mode>>, ScriptError> {
    Ok(load_user_configuration()?.modes)
}

pub fn load_user_configuration() -> Result<LoadedEditorConfiguration, ScriptError> {
    let host = load_user_config()?;
    let modes = ScriptHost::script_modes(&host)
        .into_iter()
        .map(|mode| Box::new(mode) as Box<dyn Mode>)
        .collect();
    let configuration = {
        let host = host.borrow();
        let configuration = host.configuration.borrow().clone();
        configuration
    };
    Ok(LoadedEditorConfiguration {
        modes,
        theme: configuration.theme,
        face_overrides: configuration.face_overrides,
    })
}

pub fn load_typescript_modes(
    specifier: &str,
    source: &str,
) -> Result<LoadedScriptModes, ScriptError> {
    let mut host = ScriptHost::new();
    host.execute_typescript(specifier, source)?;
    let diagnostics = host.take_diagnostics();
    let host = Rc::new(RefCell::new(host));
    let modes = ScriptHost::script_modes(&host)
        .into_iter()
        .map(|mode| Box::new(mode) as Box<dyn Mode>)
        .collect();
    Ok(LoadedScriptModes { modes, diagnostics })
}

fn resolve_config_path(primary: Option<PathBuf>, root: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(path) = primary {
        return Some(path);
    }
    let root = root?;
    let path = root.join("vell").join("config.ts");
    path.is_file().then_some(path)
}

fn default_config_root() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));

    base
}

fn initialize_v8() {
    V8_INIT.call_once(|| {
        // Worker isolates already run off the UI thread. Keeping Wasm compilation
        // there avoids cross-isolate platform tasks delaying cancellation.
        v8::V8::set_flags_from_string("--no-wasm-async-compilation");
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
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
    operations: Vec<vell_mode::operation::OperationRequest>,
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

fn parse_v2_action_result(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
    pass: &v8::Global<v8::Object>,
    operations: Vec<vell_mode::operation::OperationRequest>,
) -> Result<ModeResult, ScriptError> {
    if value.is_undefined() {
        return Ok(ModeResult::operations(operations));
    }
    let pass = v8::Local::new(scope, pass);
    if value.strict_equals(pass.into()) {
        return Ok(ModeResult::continue_with(operations));
    }
    Err(ScriptError::new(
        "v2 command must return undefined or ctx.pass()",
    ))
}

fn parse_decorations_property(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
    name: &str,
    snapshot: Option<vell_core::text_snapshot::TextSnapshot>,
    current_revision: Option<vell_protocol::revision::Revision>,
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
    ensure_count(
        "decorations",
        spans.length() as usize,
        MAX_SCRIPT_DECORATIONS,
    )?;
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
            start: vell_protocol::selection::TextOffset { char_index: start },
            end: vell_protocol::selection::TextOffset { char_index: end },
            face: FaceName::new(required_string(scope, span, "face")?),
        });
    }
    decorations.sort_by_key(|decoration| (decoration.start.char_index, decoration.end.char_index));
    Ok(Some(decorations))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vell_core::action::ContentAction;
    use vell_core::buffer::Buffer;
    use vell_core::command::EditCommand;
    use vell_core::content::{Content, ContentKind};
    use vell_core::content_store::ContentStore;
    use vell_core::content_view_state::ContentViewState;
    use vell_core::status_bar::StatusBar;
    use vell_mode::{InputFlow, ModeJobSlot, ModeRegistry};
    use vell_protocol::ids::{ContentId, ViewId};

    #[test]
    fn decoration_set_returns_only_spans_intersecting_visible_rows() {
        let snapshot = vell_core::text_snapshot::TextSnapshot::from_text(&"a\n".repeat(100));
        let face = FaceName::new("syntax.test");
        let decorations = DecorationSet::new(vec![
            NamedTextDecoration {
                start: vell_protocol::selection::TextOffset { char_index: 0 },
                end: vell_protocol::selection::TextOffset { char_index: 150 },
                face: face.clone(),
            },
            NamedTextDecoration {
                start: vell_protocol::selection::TextOffset { char_index: 10 },
                end: vell_protocol::selection::TextOffset { char_index: 20 },
                face: face.clone(),
            },
            NamedTextDecoration {
                start: vell_protocol::selection::TextOffset { char_index: 100 },
                end: vell_protocol::selection::TextOffset { char_index: 101 },
                face,
            },
        ]);

        let visible = decorations.visible(&snapshot, RowRange { start: 50, end: 51 });

        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].start.char_index, 0);
        assert_eq!(visible[1].start.char_index, 100);
    }

    #[test]
    fn config_resolution_prefers_explicit_vell_path() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let explicit = root.join("explicit.ts");

        assert_eq!(
            resolve_config_path(Some(explicit.clone()), Some(root.to_owned())),
            Some(explicit)
        );

        let default = root.join("vell").join("config.ts");
        std::fs::create_dir_all(default.parent().unwrap()).unwrap();
        std::fs::write(&default, "").unwrap();
        assert_eq!(
            resolve_config_path(None, Some(root.to_owned())),
            Some(default)
        );
    }

    #[test]
    fn editor_visual_configuration_is_typed_and_atomic() {
        let mut host = ScriptHost::new();
        host.execute_typescript(
            "file:///visuals.ts",
            r##"
editor.theme.use("catppuccin-mocha");
editor.faces.override("syntax.comment", {
  foreground: "#010203",
  italic: false,
});
editor.faces.override(
  "ui.editor",
  { background: { reset: true } },
  { theme: "catppuccin-latte" },
);
"##,
        )
        .unwrap();
        let before = host.configuration.borrow().clone();

        let error = host
            .execute_typescript(
                "file:///invalid-color.ts",
                r#"editor.faces.override("syntax.keyword", { foreground: 1.5 });"#,
            )
            .unwrap_err();
        assert!(error.to_string().contains("integer from 0 to 255"));
        assert_eq!(
            host.configuration.borrow().face_overrides,
            before.face_overrides
        );

        let error = host
            .execute_typescript(
                "file:///invalid-visuals.ts",
                r#"
editor.theme.use("catppuccin-latte");
editor.faces.override("syntax.keyword", { bold: true });
throw new Error("rollback");
"#,
            )
            .unwrap_err();

        assert!(error.to_string().contains("rollback"));
        let configuration = host.configuration.borrow();
        assert_eq!(configuration.theme, before.theme);
        assert_eq!(configuration.face_overrides, before.face_overrides);
        assert_eq!(
            configuration.theme,
            Some(ThemeName::new("catppuccin-mocha"))
        );
        assert_eq!(configuration.face_overrides.len(), 2);
        assert_eq!(
            configuration.face_overrides[0].patch.foreground,
            FaceValue::Value(Color::Rgb {
                red: 1,
                green: 2,
                blue: 3,
            })
        );
        assert_eq!(
            configuration.face_overrides[1].patch.background,
            FaceValue::Reset
        );
    }

    #[test]
    fn invalid_optional_config_keeps_existing_modes_and_host_usable() {
        let mut host = ScriptHost::new();
        host.execute_typescript(
            "file:///default.ts",
            r#"
editor.modes.define({
  name: "default-mode",
  on: { buffer: {} },
});
editor.theme.use("terminal-default");
"#,
        )
        .unwrap();
        let host = Rc::new(RefCell::new(host));
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.ts");
        fs::write(
            &config,
            r#"
editor.theme.use("catppuccin-latte");
editor.faces.override("syntax.comment", { italic: false });
throw new Error("invalid user config");
"#,
        )
        .unwrap();

        let error = load_optional_user_config(&host, &config).unwrap_err();

        assert!(error.to_string().contains("invalid user config"));
        assert_eq!(ScriptHost::script_modes(&host).len(), 1);
        assert_eq!(
            host.borrow().configuration.borrow().theme.clone(),
            Some(ThemeName::new("terminal-default"))
        );
        assert!(host.borrow().configuration.borrow().face_overrides.is_empty());
        assert_eq!(
            host.borrow_mut()
                .evaluate_typescript("file:///probe.ts", "40 + 2")
                .unwrap(),
            "42"
        );
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
    fn startup_timeout_interrupts_script_and_host_recovers() {
        let mut host =
            ScriptHost::with_timeouts(Duration::from_millis(50), Duration::from_millis(50));

        let error = host
            .execute_typescript(
                "file:///loop.ts",
                r#"
editor.modes.define({ name: "partial", actions: {} });
while (true) {}
"#,
            )
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("timeout during module evaluation"),
            "{error}"
        );
        assert!(host.definitions.borrow().is_empty());
        assert_eq!(
            host.evaluate_typescript("file:///after-loop.ts", "6 * 7")
                .unwrap(),
            "42"
        );
    }

    #[test]
    fn startup_timeout_interrupts_infinite_microtasks() {
        let mut host =
            ScriptHost::with_timeouts(Duration::from_millis(50), Duration::from_millis(50));

        let error = host
            .evaluate_typescript(
                "file:///microtasks.ts",
                r#"
const spin = () => Promise.resolve().then(spin);
Promise.resolve().then(spin);
"#,
            )
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("timeout during module evaluation"),
            "{error}"
        );
        assert_eq!(
            host.evaluate_typescript("file:///after-microtasks.ts", "21 + 21")
                .unwrap(),
            "42"
        );
    }

    #[test]
    fn heap_limit_interrupts_script_without_terminating_host() {
        let host_budget = ScriptExecutionBudget {
            callback_timeout: Duration::from_secs(5),
            startup_timeout: Duration::from_secs(5),
        };
        let mut host = ScriptHost::with_budget_and_heap(host_budget, 16 * 1024 * 1024);

        let error = host
            .evaluate_typescript(
                "file:///heap.ts",
                r#"
const retained = [];
while (true) retained.push(new Array(100_000).fill(42));
"#,
            )
            .unwrap_err()
            .to_string();

        assert!(error.contains("heap limit exceeded"), "{error}");
        assert_eq!(
            host.evaluate_typescript("file:///after-heap.ts", "40 + 2")
                .unwrap(),
            "42"
        );
    }

    #[test]
    fn reports_typescript_parse_errors() {
        let error = transpile_typescript("file:///config.ts", "const value: = 1;")
            .unwrap_err()
            .to_string();

        assert!(error.contains("Expected"));
    }

    #[test]
    fn rejects_oversized_typescript_before_transpiling() {
        let mut host = ScriptHost::new();
        let source = " ".repeat(MAX_SCRIPT_SOURCE_BYTES + 1);

        let error = host
            .execute_typescript("file:///oversized.ts", &source)
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("limit exceeded for TypeScript source"),
            "{error}"
        );
    }

    #[test]
    fn rejects_module_graphs_over_the_total_source_limit() {
        let mut modules = ModuleMap::default();
        modules.reserve_source(MAX_MODULE_GRAPH_BYTES).unwrap();

        let error = modules.reserve_source(1).unwrap_err().to_string();

        assert!(error.contains("limit exceeded for module graph"), "{error}");
    }

    #[test]
    fn rejects_oversized_module_before_reading_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("oversized.ts");
        fs::write(&path, vec![b' '; MAX_SCRIPT_SOURCE_BYTES + 1]).unwrap();
        let mut host = ScriptHost::new();

        let error = host.execute_module(&path).unwrap_err().to_string();

        assert!(
            error.contains("limit exceeded for module source"),
            "{error}"
        );
    }

    #[test]
    fn rejects_oversized_mode_state_and_host_recovers() {
        let mut host = ScriptHost::with_timeouts(Duration::from_secs(5), Duration::from_secs(5));
        host.execute_typescript(
            "file:///oversized-state.ts",
            &format!(
                r#"
editor.modes.define({{
  name: "oversized-state",
  on: {{ buffer: {{ state: () => "x".repeat({}) }} }},
}});
"#,
                MAX_SCRIPT_JSON_BYTES + 1
            ),
        )
        .unwrap();
        let host = Rc::new(RefCell::new(host));
        let mode = ScriptHost::script_modes(&host).pop().unwrap();
        let content_id = ContentId(0);
        let mut contents = ContentStore::default();
        contents
            .insert(content_id, Content::Buffer(Buffer::new()))
            .unwrap();
        let context = ModeContentContext::new(content_id, &contents);

        let error = match mode.create_content_state(&context) {
            Ok(_) => panic!("oversized state unexpectedly succeeded"),
            Err(error) => error.to_string(),
        };

        assert!(
            error.contains("limit exceeded for mode content state"),
            "{error}"
        );
        assert_eq!(
            host.borrow_mut()
                .evaluate_typescript("file:///after-state.ts", "14 * 3")
                .unwrap(),
            "42"
        );
    }

    #[test]
    fn rejects_oversized_analysis_input_result() {
        let mut host = ScriptHost::with_timeouts(Duration::from_secs(5), Duration::from_secs(5));
        let context = host.context.clone();
        let source = format!("({{ payload: 'x'.repeat({}) }})", MAX_SCRIPT_JSON_BYTES + 1);
        let error = host
            .invoke(ScriptInvocationKind::AnalysisInput, |isolate| {
                v8::scope_with_context!(scope, isolate, context);
                let source = v8::String::new(scope, &source).unwrap();
                let script = v8::Script::compile(scope, source, None).unwrap();
                let value = script.run(scope).unwrap();
                v8_to_json(scope, value, "analysis input")
            })
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("limit exceeded for analysis input"),
            "{error}"
        );
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
    fn module_graph_rejects_imports_outside_the_config_directory() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("config");
        fs::create_dir(&root).unwrap();
        fs::write(parent.path().join("outside.ts"), "export const value = 1;").unwrap();
        let config = root.join("config.ts");
        fs::write(
            &config,
            "import { value } from '../outside.ts'; void value;",
        )
        .unwrap();

        let error = ScriptHost::new()
            .execute_module(&config)
            .unwrap_err()
            .to_string();

        assert!(error.contains("escapes the config directory"), "{error}");
    }

    #[test]
    fn module_graph_rejects_bare_imports() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.ts");
        fs::write(&config, "import 'untrusted-package';").unwrap();

        let error = ScriptHost::new()
            .execute_module(&config)
            .unwrap_err()
            .to_string();

        assert!(error.contains("bare and URL imports"), "{error}");
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
        let registered_mode = ScriptHost::script_modes(&host).pop().unwrap();
        let mut registry = ModeRegistry::new();
        let registered = registry.register(registered_mode).unwrap();
        assert!(registry.adapter(registered, ContentKind::Buffer).is_some());
        assert!(
            registry
                .adapter(registered, ContentKind::StatusBar)
                .is_none()
        );
        let mut modes = ScriptHost::script_modes(&host);
        let mode = modes.pop().unwrap();
        assert_eq!(mode.name().as_str(), "pairs");
        assert_eq!(mode.before().unwrap().as_str(), "base-mode");

        let content_id = ContentId(0);
        let mut contents = ContentStore::default();
        contents
            .insert(content_id, Content::Buffer(Buffer::new()))
            .unwrap();
        let view_state = contents.create_view_state(content_id).unwrap();
        let context = ModeViewContext::new(ViewId(0), content_id, &view_state, &contents).unwrap();
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
            [vell_mode::operation::OperationRequest::View {
                operation: vell_mode::operation::ViewOperation::Edit(
                    EditCommand::InsertText(text)
                ),
                ..
            }] if text == "\"\""
        ));
    }

    #[test]
    fn registers_v2_buffer_commands_with_void_and_qualified_invocation() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.ts");
        fs::write(
            &config,
            r#"
editor.modes.define({
  name: "pairs",
  on: {
    buffer: {
      state: () => ({ enabled: true, calls: 0 }),
      viewState: () => ({ insertedPairs: 0 }),
      commands: {
        quote(ctx) {
          if (!ctx.state.enabled) return ctx.pass();
          ctx.edit.insert("\"\"");
          ctx.cursor.moveLeft();
          ctx.state.calls++;
          ctx.viewState.insertedPairs++;
        },
        delegate(ctx) {
          ctx.commands.invoke("pairs.quote");
        },
        moveWords(ctx) {
          ctx.cursor.moveWordForward(2);
        },
      },
      keys: { "\"": "quote" },
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
        assert!(mode.adapters().contains(ContentKind::Buffer));
        assert!(!mode.adapters().contains(ContentKind::StatusBar));

        let content_id = ContentId(0);
        let mut contents = ContentStore::default();
        contents
            .insert(content_id, Content::Buffer(Buffer::new()))
            .unwrap();
        let view_state = contents.create_view_state(content_id).unwrap();
        let context = ModeViewContext::new(ViewId(0), content_id, &view_state, &contents).unwrap();
        let content_context = ModeContentContext::new(content_id, &contents);
        let mut content_state = mode.create_content_state(&content_context).unwrap();
        let mut view_state = mode
            .create_view_state(content_state.as_ref(), &context)
            .unwrap();

        let quote = mode
            .execute_view_with_arguments(
                content_state.as_mut(),
                view_state.as_mut(),
                &context,
                &ModeActionName::new("quote"),
                &ModeValue::Null,
            )
            .unwrap();
        let (flow, operations) = quote.into_parts();
        assert_eq!(flow, InputFlow::Stop);
        assert_eq!(operations.len(), 2);
        assert_eq!(
            script_state(content_state.as_ref(), mode.name())
                .unwrap()
                .data,
            serde_json::json!({ "enabled": true, "calls": 1 })
        );
        assert_eq!(
            script_state(view_state.as_ref(), mode.name()).unwrap().data,
            serde_json::json!({ "insertedPairs": 1 })
        );

        let delegate = mode
            .execute_view_with_arguments(
                content_state.as_mut(),
                view_state.as_mut(),
                &context,
                &ModeActionName::new("delegate"),
                &ModeValue::Null,
            )
            .unwrap();
        let (_, operations) = delegate.into_parts();
        assert!(matches!(
            operations.as_slice(),
            [vell_mode::operation::OperationRequest::Mode { invocation, .. }]
                if invocation.command.mode.as_str() == "pairs"
                    && invocation.command.action.as_str() == "quote"
        ));

        let move_words = mode
            .execute_view_with_arguments(
                content_state.as_mut(),
                view_state.as_mut(),
                &context,
                &ModeActionName::new("moveWords"),
                &ModeValue::Null,
            )
            .unwrap();
        let (_, operations) = move_words.into_parts();
        assert!(matches!(
            operations.as_slice(),
            [vell_mode::operation::OperationRequest::View {
                operation: vell_mode::operation::ViewOperation::Edit(
                    EditCommand::MoveWordForwardBy(2)
                ),
                ..
            }]
        ));
    }

    #[test]
    fn v2_pass_is_distinct_from_legacy_booleans_and_errors_do_not_publish_state() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.ts");
        fs::write(
            &config,
            r#"
editor.modes.define({
  name: "flow-v2",
  on: {
    buffer: {
      state: () => ({ calls: 0 }),
      commands: {
        pass(ctx) {
          ctx.state.calls++;
          return ctx.pass();
        },
        legacyBoolean(ctx) {
          ctx.state.calls++;
          ctx.edit.insert("x");
          return true;
        },
        returnsNull() {
          return null;
        },
        throws(ctx) {
          ctx.state.calls++;
          ctx.edit.insert("y");
          throw new Error("boom");
        },
      },
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
        let view_state = contents.create_view_state(content_id).unwrap();
        let context = ModeViewContext::new(ViewId(0), content_id, &view_state, &contents).unwrap();
        let content_context = ModeContentContext::new(content_id, &contents);
        let mut content_state = mode.create_content_state(&content_context).unwrap();
        let mut view_state = mode
            .create_view_state(content_state.as_ref(), &context)
            .unwrap();

        let pass = mode
            .execute_view_with_arguments(
                content_state.as_mut(),
                view_state.as_mut(),
                &context,
                &ModeActionName::new("pass"),
                &ModeValue::Null,
            )
            .unwrap();
        assert_eq!(pass.into_parts(), (InputFlow::Continue, Vec::new()));
        assert_eq!(
            script_state(content_state.as_ref(), mode.name())
                .unwrap()
                .data,
            serde_json::json!({ "calls": 1 })
        );

        for (action, message) in [
            ("legacyBoolean", "undefined or ctx.pass()"),
            ("returnsNull", "undefined or ctx.pass()"),
            ("throws", "boom"),
        ] {
            let error = mode
                .execute_view_with_arguments(
                    content_state.as_mut(),
                    view_state.as_mut(),
                    &context,
                    &ModeActionName::new(action),
                    &ModeValue::Null,
                )
                .unwrap_err();
            assert!(error.to_string().contains(message));
            assert_eq!(
                script_state(content_state.as_ref(), mode.name())
                    .unwrap()
                    .data,
                serde_json::json!({ "calls": 1 })
            );
        }
    }

    #[test]
    fn v2_status_bar_adapter_has_no_buffer_primitives() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.ts");
        fs::write(
            &config,
            r#"
editor.modes.define({
  name: "status-probe",
  on: {
    statusBar: {
      state: () => ({ calls: 0 }),
      viewState: () => ({ ready: true }),
      commands: {
        touch(ctx) {
          if ("edit" in ctx || "cursor" in ctx) {
            throw new Error("buffer capability leaked");
          }
          void ctx.targetContentId;
          void ctx.resourceName;
          void ctx.dirty;
          void ctx.saveState;
          ctx.state.calls++;
        },
      },
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
        assert!(!mode.adapters().contains(ContentKind::Buffer));
        assert!(mode.adapters().contains(ContentKind::StatusBar));

        let buffer = ContentId(0);
        let status = ContentId(1);
        let mut contents = ContentStore::default();
        contents
            .insert(buffer, Content::Buffer(Buffer::new()))
            .unwrap();
        contents
            .insert(status, Content::StatusBar(StatusBar::new()))
            .unwrap();
        let unbound = contents.create_view_state(status).unwrap();
        assert!(matches!(
            ModeViewContext::new(ViewId(1), status, &unbound, &contents),
            Err(vell_mode::ModeContextError::UnboundStatusBar { .. })
        ));
        let view_state = ContentViewState::status_bar(ViewId(1), buffer);
        let context = ModeViewContext::new(ViewId(1), status, &view_state, &contents).unwrap();
        let content_context = ModeContentContext::new(status, &contents);
        let mut content_state = mode.create_content_state(&content_context).unwrap();
        let mut view_state = mode
            .create_view_state(content_state.as_ref(), &context)
            .unwrap();
        let result = mode
            .execute_view_with_arguments(
                content_state.as_mut(),
                view_state.as_mut(),
                &context,
                &ModeActionName::new("touch"),
                &ModeValue::Null,
            )
            .unwrap();

        assert_eq!(result.into_parts(), (InputFlow::Stop, Vec::new()));
        assert_eq!(
            script_state(content_state.as_ref(), mode.name())
                .unwrap()
                .data,
            serde_json::json!({ "calls": 1 })
        );
    }

    #[test]
    fn v2_schema_rejects_unknown_adapters_legacy_fields_and_invalid_keys() {
        for (name, body, expected) in [
            (
                "unknown-adapter",
                r#"on: { terminal: { commands: {} } }"#,
                "unknown mode adapter 'terminal'",
            ),
            (
                "mixed-schema",
                r#"on: { buffer: { commands: {} } }, actions: {}"#,
                "cannot combine 'on' with legacy 'actions'",
            ),
            (
                "unknown-command",
                r#"on: { buffer: { commands: {}, keys: { "x": "missing" } } }"#,
                "unknown command 'missing' in key bindings",
            ),
            (
                "invalid-key",
                r#"on: { buffer: { commands: { run() {} }, keys: { "Ctrl+X": "run" } } }"#,
                "unsupported key binding: Ctrl+X",
            ),
            (
                "status-changed",
                r#"on: { statusBar: { changed() {} } }"#,
                "mode statusBar.changed is not supported",
            ),
            (
                "status-worker",
                r#"on: { statusBar: { worker: "worker.ts" } }"#,
                "mode statusBar.worker is not supported",
            ),
            (
                "raw-worker-lifecycle",
                r#"on: { buffer: { job() {} } }"#,
                "mode buffer.job moved to named analysis",
            ),
            (
                "status-analysis",
                r#"on: { statusBar: { analysis: {} } }"#,
                "mode statusBar.analysis is not supported",
            ),
            (
                "incomplete-analysis",
                r#"on: { buffer: { analysis: { syntax: {} } } }"#,
                "mode analysis 'syntax'.worker is required",
            ),
            (
                "invalid-analysis-snapshot",
                concat!(
                    r#"on: { buffer: { analysis: { syntax: { "#,
                    r#"worker: "worker.ts", snapshot: "document", "#,
                    r#"input() {}, apply() {} } } } }"#,
                ),
                "mode analysis 'syntax' has unknown snapshot 'document'",
            ),
            (
                "analysis-host-field",
                concat!(
                    r#"on: { buffer: { analysis: { syntax: { "#,
                    r#"worker: "worker.ts", slot: "parse", "#,
                    r#"input() {}, apply() {} } } } }"#,
                ),
                "mode analysis 'syntax' has unknown field 'slot'",
            ),
            (
                "invalid-input",
                r#"on: { buffer: { input: 42 } }"#,
                "mode input must be a function",
            ),
            (
                "reserved-input",
                r#"on: { buffer: { commands: { "$input"() {} }, input() {} } }"#,
                "mode command '$input' is reserved for raw input",
            ),
            (
                "bound-internal-input",
                r#"on: { buffer: { input() {}, keys: { "x": "$input" } } }"#,
                "unknown command '$input' in key bindings",
            ),
        ] {
            let mut host = ScriptHost::new();
            let source = format!("editor.modes.define({{ name: {name:?}, {body} }});");

            let error = host
                .execute_typescript("file:///v2-invalid.ts", &source)
                .unwrap_err()
                .to_string();

            assert!(error.contains(expected), "{error}");
        }
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
        let view_state = contents.create_view_state(content_id).unwrap();
        let context = ModeViewContext::new(ViewId(0), content_id, &view_state, &contents).unwrap();
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
    fn timed_out_action_discards_state_and_operations_then_recovers() {
        let mut host =
            ScriptHost::with_timeouts(Duration::from_millis(50), Duration::from_millis(100));
        host.execute_typescript(
            "file:///timed-out-action.ts",
            r#"
editor.modes.define({
  name: "timed-out-action",
  content: { create: () => ({ calls: 0 }) },
  view: {
    create: () => ({
      calls: 0,
      viewPolicy: { cursorStyle: "bar" },
    }),
  },
  actions: {
    hang(context) {
      context.contentState.calls++;
      context.viewState.calls++;
      context.viewState.viewPolicy.cursorStyle = "block";
      context.text.insert("discarded");
      while (true) {}
    },
    recover(context) {
      context.contentState.calls++;
      context.viewState.calls++;
      return context.handled();
    },
  },
});
"#,
        )
        .unwrap();
        let host = Rc::new(RefCell::new(host));
        let mode = ScriptHost::script_modes(&host).pop().unwrap();
        let content_id = ContentId(0);
        let mut contents = ContentStore::default();
        contents
            .insert(content_id, Content::Buffer(Buffer::new()))
            .unwrap();
        let view_state = contents.create_view_state(content_id).unwrap();
        let context = ModeViewContext::new(ViewId(0), content_id, &view_state, &contents).unwrap();
        let content_context = ModeContentContext::new(content_id, &contents);
        let mut content_state = mode.create_content_state(&content_context).unwrap();
        let mut view_state = mode
            .create_view_state(content_state.as_ref(), &context)
            .unwrap();

        let error = mode
            .execute_view_with_arguments(
                content_state.as_mut(),
                view_state.as_mut(),
                &context,
                &ModeActionName::new("hang"),
                &ModeValue::Null,
            )
            .unwrap_err()
            .to_string();

        assert!(error.contains("timeout during action"), "{error}");
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
                "viewPolicy": { "cursorStyle": "bar" },
            })
        );

        let result = mode
            .execute_view_with_arguments(
                content_state.as_mut(),
                view_state.as_mut(),
                &context,
                &ModeActionName::new("recover"),
                &ModeValue::Null,
            )
            .unwrap();
        assert!(result.into_parts().1.is_empty());
        assert_eq!(
            script_state(content_state.as_ref(), mode.name())
                .unwrap()
                .data,
            serde_json::json!({ "calls": 1 })
        );
        assert_eq!(
            script_state(view_state.as_ref(), mode.name()).unwrap().data,
            serde_json::json!({
                "calls": 1,
                "viewPolicy": { "cursorStyle": "bar" },
            })
        );
    }

    #[test]
    fn action_output_limits_discard_staged_state_and_operations() {
        let budget = ScriptExecutionBudget {
            callback_timeout: Duration::from_secs(5),
            startup_timeout: Duration::from_secs(5),
        };
        let mut host = ScriptHost::with_budget_and_heap(budget, SCRIPT_HEAP_LIMIT_BYTES);
        host.execute_typescript(
            "file:///output-limits.ts",
            &format!(
                r#"
editor.modes.define({{
  name: "output-limits",
  content: {{ create: () => ({{ calls: 0 }}) }},
  actions: {{
    operations(context) {{
      context.contentState.calls++;
      for (let index = 0; index < {}; index++) context.text.insert("x");
      return context.handled();
    }},
    operationCount(context) {{
      context.contentState.calls++;
      context.cursor.moveWordForward({});
      return context.handled();
    }},
    decorations(context) {{
      context.contentState.calls++;
      return {{
        contentDecorations: {{
          revision: context.revision,
          spans: Array.from({{ length: {} }}, () => ({{
            range: {{
              start: {{ line: 0, character: 0 }},
              end: {{ line: 0, character: 0 }},
            }},
            face: "limit",
          }})),
        }},
      }};
    }},
  }},
}});
"#,
                MAX_SCRIPT_OPERATIONS + 1,
                MAX_SCRIPT_OPERATIONS + 1,
                MAX_SCRIPT_DECORATIONS + 1
            ),
        )
        .unwrap();
        let host = Rc::new(RefCell::new(host));
        let mode = ScriptHost::script_modes(&host).pop().unwrap();
        let content_id = ContentId(0);
        let mut contents = ContentStore::default();
        contents
            .insert(content_id, Content::Buffer(Buffer::new()))
            .unwrap();
        let view_state = contents.create_view_state(content_id).unwrap();
        let context = ModeViewContext::new(ViewId(0), content_id, &view_state, &contents).unwrap();
        let content_context = ModeContentContext::new(content_id, &contents);
        let mut content_state = mode.create_content_state(&content_context).unwrap();
        let mut view_state = mode
            .create_view_state(content_state.as_ref(), &context)
            .unwrap();

        for (action, expected) in [
            ("operations", "limit exceeded for operations"),
            ("operationCount", "limit exceeded for operation count"),
            ("decorations", "limit exceeded for decorations"),
        ] {
            let error = mode
                .execute_view_with_arguments(
                    content_state.as_mut(),
                    view_state.as_mut(),
                    &context,
                    &ModeActionName::new(action),
                    &ModeValue::Null,
                )
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{error}");
            assert_eq!(
                script_state(content_state.as_ref(), mode.name())
                    .unwrap()
                    .data,
                serde_json::json!({ "calls": 0 })
            );
        }
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
        assert!(
            definitions
                .iter()
                .all(|definition| definition.version == ScriptApiVersion::V2)
        );
        let vim = definitions
            .iter()
            .find(|definition| definition.name.as_str() == "vim")
            .unwrap();
        let vim_adapter = vim.adapters.buffer.as_ref().unwrap();
        assert!(vim_adapter.input.is_some());
        assert!(
            vim_adapter
                .actions
                .iter()
                .all(|action| action.name.as_str() != V2_INPUT_ACTION)
        );
        let highlighting = definitions
            .iter()
            .find(|definition| definition.name.as_str() == "syntax-highlighting")
            .unwrap();
        let adapter = highlighting.adapters.buffer.as_ref().unwrap();
        assert!(adapter.worker.is_none());
        assert!(adapter.content_job.is_none());
        assert!(adapter.content_apply_job.is_none());
        assert_eq!(adapter.analyses.len(), 1);
        assert_eq!(adapter.analyses[0].slot, "analysis:syntax");
        assert!(host.diagnostics.borrow().messages.is_empty());
    }

    #[test]
    fn v2_raw_input_is_not_a_registered_mode_command() {
        let host = load_default_plugins().unwrap();
        let vim = ScriptHost::script_modes(&host)
            .into_iter()
            .find(|mode| mode.name().as_str() == "vim")
            .unwrap();
        let mut registry = ModeRegistry::new();
        registry.register(vim).unwrap();

        let error = registry
            .resolve_command_checked(&ModeName::new("vim"), &ModeActionName::new(V2_INPUT_ACTION))
            .unwrap_err();

        assert!(matches!(error, ModeError::UnknownAction { .. }));
    }

    #[test]
    fn v1_schema_reports_one_deprecation_diagnostic_per_host() {
        let mut host = ScriptHost::new();
        host.execute_typescript(
            "file:///legacy.ts",
            r#"
editor.modes.define({ name: "legacy-one", actions: {} });
editor.modes.define({ name: "legacy-two", actions: {} });
"#,
        )
        .unwrap();

        assert_eq!(
            host.take_diagnostics(),
            vec![ScriptDiagnostic::v1_deprecation()]
        );
        assert!(host.take_diagnostics().is_empty());

        host.execute_typescript(
            "file:///legacy-again.ts",
            r#"editor.modes.define({ name: "legacy-three", actions: {} });"#,
        )
        .unwrap();
        assert!(host.take_diagnostics().is_empty());

        host.execute_typescript(
            "file:///modern.ts",
            r#"editor.modes.define({ name: "modern", on: { buffer: {} } });"#,
        )
        .unwrap();
        assert!(host.take_diagnostics().is_empty());
    }

    #[test]
    fn v1_content_context_keeps_the_legacy_document_view() {
        let loaded = load_typescript_modes(
            "file:///legacy-document.ts",
            r#"
editor.modes.define({
  name: "legacy-document",
  content: {
    create(context) {
      return {
        hasDocument: context.document !== undefined,
        modified: context.document?.modified ?? true,
      };
    },
  },
  actions: {},
});
"#,
        )
        .unwrap();
        let mode = loaded.modes.into_iter().next().unwrap();
        let content = ContentId(0);
        let mut contents = ContentStore::default();
        contents
            .insert(content, Content::Buffer(Buffer::new()))
            .unwrap();

        let state = mode
            .create_content_state(&ModeContentContext::new(content, &contents))
            .unwrap();

        assert_eq!(
            script_state(state.as_ref(), mode.name()).unwrap().data,
            serde_json::json!({ "hasDocument": true, "modified": false })
        );
    }

    #[test]
    fn public_contract_executes_the_checked_v1_migration_example() {
        let source = include_str!("../../../../runtime/examples/v1-migration.ts");
        let loaded = load_typescript_modes("file:///v1-migration.ts", source).unwrap();
        let names = loaded
            .modes
            .iter()
            .map(|mode| mode.name().as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["migration-v1", "migration-v2"]);
        assert_eq!(loaded.diagnostics, vec![ScriptDiagnostic::v1_deprecation()]);
        assert_eq!(crate::PLUGIN_API_VERSION, 2);
        assert_eq!(crate::V1_REMOVAL_VERSION, "0.3.0");
        assert!(
            loaded.diagnostics[0]
                .message
                .contains(crate::V1_REMOVAL_VERSION)
        );
        assert!(crate::TYPESCRIPT_DECLARATIONS.contains("interface ModeDefinitionV2"));
        assert!(crate::TYPESCRIPT_DECLARATIONS.contains(&format!(
            "@deprecated Removed in Vell {}",
            crate::V1_REMOVAL_VERSION
        )));
    }

    #[test]
    fn invalid_analysis_input_does_not_publish_mutated_state() {
        let mut host = ScriptHost::new();
        host.execute_embedded_plugin(
            "tree-sitter/invalid-analysis.ts",
            r#"
editor.modes.define({
  name: "invalid-analysis",
  on: {
    buffer: {
      state: () => ({ calls: 0 }),
      analysis: {
        syntax: {
          worker: "worker.ts",
          snapshot: "text",
          input(ctx) {
            ctx.state.calls++;
            return { text: "reserved" };
          },
          apply() {},
        },
      },
    },
  },
});
"#,
        )
        .unwrap();
        let analysis = host.definitions.borrow()[0]
            .adapters
            .buffer
            .as_ref()
            .unwrap()
            .analyses[0]
            .clone();
        let content = ContentId(0);
        let mut contents = ContentStore::default();
        contents
            .insert(content, Content::Buffer(Buffer::new()))
            .unwrap();
        let context = ModeContentContext::new(content, &contents);
        let state = ScriptModeState::new(serde_json::json!({ "calls": 0 }));

        let error = match host.prepare_analysis_job(&analysis, &context, &state) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("invalid analysis input unexpectedly passed validation"),
        };

        assert!(error.contains("input.text is reserved"), "{error}");
        assert_eq!(state.data, serde_json::json!({ "calls": 0 }));
    }

    #[test]
    fn named_analyses_route_slots_and_reject_stale_results() {
        let mut host = ScriptHost::new();
        host.execute_embedded_plugin(
            "tree-sitter/multiple-analyses.ts",
            r#"
editor.modes.define({
  name: "multiple-analyses",
  on: {
    buffer: {
      state: () => ({ applied: [] }),
      analysis: {
        first: {
          worker: "worker.ts",
          input: () => ({}),
          apply(ctx) {
            ctx.state.applied.push("first");
            return { contentDecorations: {
              revision: ctx.revision,
              spans: [{
                range: {
                  start: { line: 0, character: 0 },
                  end: { line: 0, character: 1 },
                },
                face: "first",
              }],
            } };
          },
        },
        second: {
          worker: "worker.ts",
          input: () => ({}),
          apply(ctx) {
            ctx.state.applied.push("second");
            return { contentDecorations: {
              revision: ctx.revision,
              spans: [{
                range: {
                  start: { line: 0, character: 1 },
                  end: { line: 0, character: 2 },
                },
                face: "second",
              }],
            } };
          },
        },
      },
    },
  },
});
"#,
        )
        .unwrap();
        let host = Rc::new(RefCell::new(host));
        let mode = ScriptHost::script_modes(&host).pop().unwrap();
        let content = ContentId(0);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("analysis.txt");
        fs::write(&path, "ab").unwrap();
        let mut buffer = Buffer::new();
        buffer.open_path(path.to_str().unwrap()).unwrap();
        let mut contents = ContentStore::default();
        contents.insert(content, Content::Buffer(buffer)).unwrap();
        let context = ModeContentContext::new(content, &contents);
        let mut state = mode.create_content_state(&context).unwrap();

        let stale = mode
            .apply_background_job(
                state.as_mut(),
                &context,
                &ModeJobSlot::from("analysis:first"),
                1,
                Ok(Box::new(ScriptJobOutput::Response(serde_json::json!({})))),
            )
            .unwrap();
        assert!(!stale);

        let mut initial = mode.take_background_jobs(state.as_mut(), &context);
        assert_eq!(initial.len(), 2);
        let first = initial.remove(0);
        let second = initial.remove(0);
        let (first_slot, first_version, _) = first.into_parts();
        assert_eq!(first_slot.as_str(), "analysis:first");
        assert_eq!(first_version, 0);
        assert!(
            mode.apply_background_job(
                state.as_mut(),
                &context,
                &first_slot,
                first_version,
                Ok(Box::new(ScriptJobOutput::Response(serde_json::json!({})))),
            )
            .unwrap()
        );

        assert!(
            mode.take_background_jobs(state.as_mut(), &context)
                .is_empty()
        );
        let (second_slot, second_version, _) = second.into_parts();
        assert_eq!(second_slot.as_str(), "analysis:second");
        assert_eq!(second_version, 1);
        mode.apply_background_job(
            state.as_mut(),
            &context,
            &second_slot,
            second_version,
            Ok(Box::new(ScriptJobOutput::Response(serde_json::json!({})))),
        )
        .unwrap();
        assert!(
            mode.take_background_jobs(state.as_mut(), &context)
                .is_empty()
        );

        assert_eq!(
            script_state(state.as_ref(), mode.name()).unwrap().data,
            serde_json::json!({ "applied": ["first", "second"] })
        );
        assert_eq!(
            mode.content_decorations(state.as_ref(), &context, RowRange { start: 0, end: 1 })
                .into_iter()
                .map(|decoration| decoration.face)
                .collect::<Vec<_>>(),
            vec![FaceName::new("first"), FaceName::new("second")]
        );
    }

    #[test]
    fn analysis_apply_invalidates_other_slots_by_their_input_message() {
        let mut host = ScriptHost::new();
        host.execute_embedded_plugin(
            "tree-sitter/analysis-dependencies.ts",
            r#"
editor.modes.define({
  name: "analysis-dependencies",
  on: {
    buffer: {
      state: () => ({ theme: "light" }),
      analysis: {
        first: {
          worker: "worker.ts",
          input: (ctx) => ({ theme: ctx.state.theme }),
          apply() {},
        },
        second: {
          worker: "worker.ts",
          input: () => ({}),
          apply(ctx) { ctx.state.theme = "dark"; },
        },
      },
    },
  },
});
"#,
        )
        .unwrap();
        let host = Rc::new(RefCell::new(host));
        let mode = ScriptHost::script_modes(&host).pop().unwrap();
        let content = ContentId(0);
        let mut contents = ContentStore::default();
        contents
            .insert(content, Content::Buffer(Buffer::new()))
            .unwrap();
        let context = ModeContentContext::new(content, &contents);
        let mut state = mode.create_content_state(&context).unwrap();

        let requests = mode.take_background_jobs(state.as_mut(), &context);
        assert_eq!(requests.len(), 2);
        for (request, expected_slot) in requests
            .into_iter()
            .zip(["analysis:first", "analysis:second"])
        {
            let (slot, version, _) = request.into_parts();
            assert_eq!(slot.as_str(), expected_slot);
            mode.apply_background_job(
                state.as_mut(),
                &context,
                &slot,
                version,
                Ok(Box::new(ScriptJobOutput::Response(serde_json::json!({})))),
            )
            .unwrap();
        }

        let refreshed = mode
            .take_background_jobs(state.as_mut(), &context)
            .into_iter()
            .next()
            .unwrap();
        let (slot, version, _) = refreshed.into_parts();
        assert_eq!(slot.as_str(), "analysis:first");
        assert_eq!(version, 2);
    }

    #[test]
    fn disabled_analysis_emits_a_new_same_slot_request() {
        let mut host = ScriptHost::new();
        host.execute_embedded_plugin(
            "tree-sitter/disabled-analysis.ts",
            r#"
editor.modes.define({
  name: "disabled-analysis",
  on: {
    buffer: {
      state: () => ({ enabled: true }),
      analysis: {
        syntax: {
          worker: "worker.ts",
          input: (ctx) => ctx.state.enabled ? {} : undefined,
          apply() {},
        },
      },
    },
  },
});
"#,
        )
        .unwrap();
        let host = Rc::new(RefCell::new(host));
        let mode = ScriptHost::script_modes(&host).pop().unwrap();
        let content = ContentId(0);
        let mut contents = ContentStore::default();
        contents
            .insert(content, Content::Buffer(Buffer::new()))
            .unwrap();
        let context = ModeContentContext::new(content, &contents);
        let mut state = mode.create_content_state(&context).unwrap();

        let first = mode
            .take_background_jobs(state.as_mut(), &context)
            .into_iter()
            .next()
            .unwrap();
        let (slot, version, _) = first.into_parts();
        assert_eq!(slot.as_str(), "analysis:syntax");
        assert_eq!(version, 0);

        script_state_mut(state.as_mut(), mode.name())
            .unwrap()
            .publish_external_data(serde_json::json!({ "enabled": false }));
        let disabled = mode
            .take_background_jobs(state.as_mut(), &context)
            .into_iter()
            .next()
            .unwrap();
        let (slot, version, run) = disabled.into_parts();
        assert_eq!(slot.as_str(), "analysis:syntax");
        assert_eq!(version, 1);
        let output = run(tokio_util::sync::CancellationToken::new()).unwrap();
        assert!(matches!(
            *output.downcast::<ScriptJobOutput>().unwrap(),
            ScriptJobOutput::Disabled
        ));
    }

    #[test]
    fn one_poll_invalidates_every_changed_analysis_slot() {
        let mut host = ScriptHost::new();
        host.execute_embedded_plugin(
            "tree-sitter/stale-analysis-slots.ts",
            r#"
editor.modes.define({
  name: "stale-analysis-slots",
  on: {
    buffer: {
      state: () => ({ theme: "light" }),
      analysis: {
        first: {
          worker: "worker.ts",
          input: (ctx) => ({ theme: ctx.state.theme }),
          apply() {},
        },
        second: {
          worker: "worker.ts",
          input: (ctx) => ({ theme: ctx.state.theme }),
          apply() {},
        },
      },
    },
  },
});
"#,
        )
        .unwrap();
        let host = Rc::new(RefCell::new(host));
        let mode = ScriptHost::script_modes(&host).pop().unwrap();
        let content = ContentId(0);
        let mut contents = ContentStore::default();
        contents
            .insert(content, Content::Buffer(Buffer::new()))
            .unwrap();
        let context = ModeContentContext::new(content, &contents);
        let mut state = mode.create_content_state(&context).unwrap();

        let mut old = mode.take_background_jobs(state.as_mut(), &context);
        assert_eq!(old.len(), 2);
        let _old_first = old.remove(0);
        let (old_second_slot, old_second_version, _) = old.remove(0).into_parts();

        script_state_mut(state.as_mut(), mode.name())
            .unwrap()
            .publish_external_data(serde_json::json!({ "theme": "dark" }));
        let replacements = mode.take_background_jobs(state.as_mut(), &context);
        assert_eq!(replacements.len(), 2);
        let replacements = replacements
            .into_iter()
            .map(|request| {
                let (slot, version, _) = request.into_parts();
                (slot.as_str().to_owned(), version)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            replacements,
            vec![
                ("analysis:first".to_owned(), 2),
                ("analysis:second".to_owned(), 3),
            ]
        );

        assert!(
            !mode
                .apply_background_job(
                    state.as_mut(),
                    &context,
                    &old_second_slot,
                    old_second_version,
                    Ok(Box::new(ScriptJobOutput::Response(serde_json::json!({})))),
                )
                .unwrap()
        );
    }

    #[test]
    fn analysis_apply_accepts_its_own_post_state_without_feedback_loop() {
        let mut host = ScriptHost::new();
        host.execute_embedded_plugin(
            "tree-sitter/self-state-analysis.ts",
            r#"
editor.modes.define({
  name: "self-state-analysis",
  on: {
    buffer: {
      state: () => ({ count: 0 }),
      analysis: {
        syntax: {
          worker: "worker.ts",
          input: (ctx) => ({ count: ctx.state.count }),
          apply(ctx) { ctx.state.count++; },
        },
      },
    },
  },
});
"#,
        )
        .unwrap();
        let host = Rc::new(RefCell::new(host));
        let mode = ScriptHost::script_modes(&host).pop().unwrap();
        let content = ContentId(0);
        let mut contents = ContentStore::default();
        contents
            .insert(content, Content::Buffer(Buffer::new()))
            .unwrap();
        let context = ModeContentContext::new(content, &contents);
        let mut state = mode.create_content_state(&context).unwrap();

        let request = mode
            .take_background_jobs(state.as_mut(), &context)
            .remove(0);
        let (slot, version, _) = request.into_parts();
        mode.apply_background_job(
            state.as_mut(),
            &context,
            &slot,
            version,
            Ok(Box::new(ScriptJobOutput::Response(serde_json::json!({})))),
        )
        .unwrap();

        assert_eq!(
            script_state(state.as_ref(), mode.name()).unwrap().data,
            serde_json::json!({ "count": 1 })
        );
        assert!(
            mode.take_background_jobs(state.as_mut(), &context)
                .is_empty()
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
        let view_state = contents.create_view_state(content_id).unwrap();
        let context = ModeViewContext::new(ViewId(0), content_id, &view_state, &contents).unwrap();
        let before = context.buffer().unwrap().text_snapshot().unwrap();
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
        let vell_mode::operation::OperationRequest::View {
            operation:
                vell_mode::operation::ViewOperation::ApplyContent(ContentAction::Text(change)),
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
        let view_state = contents.create_view_state(content_id).unwrap();
        let context = ModeViewContext::new(ViewId(0), content_id, &view_state, &contents).unwrap();
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
