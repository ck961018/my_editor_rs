//! TypeScript runtime owned by the application layer.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Once};
use std::time::Duration;

use crate::api::{LoadedEditorConfiguration, LoadedScriptModes};
use vell_core::content::ContentKind;
use vell_mode::command::ModeValue;
use vell_mode::mode_name::{ModeActionName, ModeName};
use vell_mode::operation::MAX_MODE_CALLBACK_OPERATIONS;
use vell_mode::{
    CompoundViewDefinition, CompoundViewDirection, LanguageId, MAX_COMPOUND_VIEW_BINDINGS,
    MAX_COMPOUND_VIEW_CHILD_BINDINGS, Mode, ModeAttachmentRule, ModeBackground, ModeContentContext,
    ModeError, ModeResult, ModeState, ModeViewContext, NamedLineSegment, NamedLinesPresentation,
    ViewChildBinding, ViewChildDefinition, ViewDefinitionOwner, ViewExtension,
    ViewExtensionContext, ViewExtensionDefinition, ViewExtensionId, ViewExtensionOwner,
    ViewExtensionPaneDefinition, ViewExtensionPaneSide, ViewExtensionPresentation,
};
use vell_protocol::content_query::{
    Color, Face, FaceDefinition, FaceName, FaceOverride, FacePatch, FaceValue, NamedTextDecoration,
    RowRange, ThemeName, UnderlineStyle,
};
use vell_protocol::ids::ContentId;
use vell_protocol::key_event::{ArrowKey, KeyCode, KeyEvent};
use vell_protocol::view::{BindingKey, ViewDefinitionId};

mod bridge;
mod command_line;
mod commands;
mod global_script;
mod host;
mod invocation;
mod mode_adapter;
mod module;
mod primitives;
mod schema;
mod type_environment;
mod view_extension_adapter;
mod worker;
mod worker_channel;
mod worker_quota;

use bridge::{
    content_change_to_v8, content_context_object, json_to_mode_value, json_to_v8, optional_string,
    parse_position, property, required_object, required_string, set_number, set_object,
    set_resource_facts, set_save_state, set_string, set_value, throw_dom_exception,
    throw_script_error, throw_type_error, v8_to_json, view_policy_from_json,
};
use commands::{ActiveCommandHost, ScriptCommands};
pub use host::ScriptHost;
use invocation::{
    HeapLimitState, InvocationWatchdog, ScriptExecutionBudget, ScriptInvocationKind,
    WatchdogOutcome, call_script_callback, install_heap_limit, perform_microtask_checkpoint,
    recover_heap_limit,
};
use mode_adapter::{ScriptBackground, ScriptMode};
#[cfg(any(test, feature = "test-support"))]
use module::transpile_typescript;
use module::{
    AssetSource, ModuleMap, current_exception, host_import_module_dynamically,
    host_initialize_import_meta, load_module_tree, resolve_module, transpile_typescript_program,
};
use primitives::PrimitiveRuntime;
use schema::install_editor_api;
pub use type_environment::TYPESCRIPT_COMPILER_VERSION;
use type_environment::TypeEnvironment;
use view_extension_adapter::ScriptViewExtension;

static V8_INIT: Once = Once::new();
const INPUT_ACTION: &str = "$input";
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

fn view_extension_context_json(context: &ViewExtensionContext) -> serde_json::Value {
    let document = context.document.as_ref().map(|document| {
        serde_json::json!({
            "contentId": document.content_id.0,
            "revision": document.revision.0,
            "text": document.text,
            "resourceName": document.resource_name,
            "selections": document.selections.iter().map(|selection| serde_json::json!({
                "anchor": {
                    "line": selection.anchor.line,
                    "character": selection.anchor.character,
                },
                "head": {
                    "line": selection.head.line,
                    "character": selection.head.character,
                },
            })).collect::<Vec<_>>(),
            "primarySelection": {
                "anchor": {
                    "line": document.primary_selection.anchor.line,
                    "character": document.primary_selection.anchor.character,
                },
                "head": {
                    "line": document.primary_selection.head.line,
                    "character": document.primary_selection.head.character,
                },
            },
        })
    });
    serde_json::json!({
        "viewId": context.view_id.0,
        "definition": context.definition.as_str(),
        "revision": context.revision.0,
        "bindings": context.bindings.iter().map(|(binding, content)| serde_json::json!({
            "name": binding.as_str(),
            "contentId": content.0,
        })).collect::<Vec<_>>(),
        "document": document,
    })
}

fn parse_view_extension_presentation(
    value: serde_json::Value,
) -> Result<ViewExtensionPresentation, ScriptError> {
    let object = value.as_object().ok_or_else(|| {
        ScriptError::new("View extension render must return a presentation object")
    })?;
    ensure_json_fields(object, &["type", "baseFace", "rows"], "lines presentation")?;
    if object.get("type").and_then(serde_json::Value::as_str) != Some("lines") {
        return Err(ScriptError::new(
            "View extension presentation type must be 'lines'",
        ));
    }
    let base_face = optional_json_face(object.get("baseFace"), "baseFace")?;
    let rows = object
        .get("rows")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| ScriptError::new("lines presentation rows must be an array"))?
        .iter()
        .map(parse_view_extension_row)
        .collect::<Result<Vec<_>, _>>()?;
    let presentation = ViewExtensionPresentation::Lines(NamedLinesPresentation { base_face, rows });
    presentation
        .validate()
        .map_err(|error| ScriptError::new(error.to_string()))?;
    Ok(presentation)
}

fn parse_view_extension_row(
    value: &serde_json::Value,
) -> Result<Vec<NamedLineSegment>, ScriptError> {
    if let Some(text) = value.as_str() {
        return Ok(vec![NamedLineSegment {
            text: text.to_owned(),
            face: None,
        }]);
    }
    let segments = value.as_array().ok_or_else(|| {
        ScriptError::new("lines presentation rows must contain strings or arrays")
    })?;
    segments
        .iter()
        .map(|segment| {
            let object = segment
                .as_object()
                .ok_or_else(|| ScriptError::new("lines presentation segments must be objects"))?;
            ensure_json_fields(object, &["text", "face"], "lines segment")?;
            let text = object
                .get("text")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| ScriptError::new("lines segment text must be a string"))?;
            let face = optional_json_face(object.get("face"), "segment face")?;
            Ok(NamedLineSegment {
                text: text.to_owned(),
                face,
            })
        })
        .collect()
}

fn optional_json_face(
    value: Option<&serde_json::Value>,
    label: &str,
) -> Result<Option<FaceName>, ScriptError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .as_str()
        .ok_or_else(|| ScriptError::new(format!("{label} must be a string")))?;
    if value.is_empty() || value.len() > 256 {
        return Err(ScriptError::new(format!(
            "{label} must contain between 1 and 256 bytes"
        )));
    }
    Ok(Some(FaceName::new(value)))
}

fn ensure_json_fields(
    object: &serde_json::Map<String, serde_json::Value>,
    allowed: &[&str],
    label: &str,
) -> Result<(), ScriptError> {
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(ScriptError::new(format!(
            "{label} contains unknown field '{field}'"
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

#[derive(Clone, Default)]
struct ScriptConfigurationDraft {
    theme: Option<ThemeName>,
    face_overrides: Vec<FaceOverride>,
}

#[derive(Clone)]
struct ScriptAdapterDefinition {
    actions: Vec<ScriptActionDefinition>,
    bindings: Vec<(KeyEvent, usize)>,
    input_action: Option<usize>,
    input: Option<v8::Global<v8::Function>>,
    create_content: Option<v8::Global<v8::Function>>,
    content_changed: Option<v8::Global<v8::Function>>,
    create_view: Option<v8::Global<v8::Function>>,
}

#[derive(Clone, Default)]
struct ScriptAdapterDefinitions {
    buffer: Option<ScriptAdapterDefinition>,
}

#[derive(Clone)]
struct ScriptModeDefinition {
    name: ModeName,
    face_definitions: Vec<FaceDefinition>,
    before: Option<ModeName>,
    attachment: ModeAttachmentRule,
    adapters: ScriptAdapterDefinitions,
}

#[derive(Clone)]
struct ScriptViewExtensionDefinition {
    definition: ViewExtensionDefinition,
    callbacks: HashMap<String, v8::Global<v8::Function>>,
}

#[derive(Default)]
struct ScriptViewExtensionRegistration {
    open: Cell<bool>,
    owner: RefCell<Option<ViewExtensionOwner>>,
}

#[derive(Default)]
struct ScriptViewDefinitionRegistration {
    open: Cell<bool>,
    owner: RefCell<Option<ViewDefinitionOwner>>,
}

#[derive(Default)]
struct ScriptViewExtensionRenderScope {
    active: Cell<bool>,
}

impl ScriptViewExtensionRenderScope {
    fn enter(&self) {
        self.active.set(true);
    }

    fn leave(&self) {
        self.active.set(false);
    }
}

fn reject_view_extension_host_mutation(scope: &mut v8::PinScope, api: &str) -> bool {
    if scope
        .get_slot::<Rc<ScriptViewExtensionRenderScope>>()
        .is_some_and(|render| render.active.get())
    {
        throw_script_error(
            scope,
            &format!("{api} is not available during View extension rendering"),
        );
        true
    } else {
        false
    }
}

impl ScriptViewExtensionRegistration {
    fn begin(&self, owner: ViewExtensionOwner) {
        self.owner.replace(Some(owner));
        self.open.set(true);
    }

    fn finish(&self) {
        self.open.set(false);
        self.owner.replace(None);
    }

    fn is_open(&self) -> bool {
        self.open.get()
    }

    fn owner(&self) -> Option<ViewExtensionOwner> {
        self.open
            .get()
            .then(|| self.owner.borrow().clone())
            .flatten()
    }
}

impl ScriptViewDefinitionRegistration {
    fn begin(&self, owner: ViewDefinitionOwner) {
        self.owner.replace(Some(owner));
        self.open.set(true);
    }

    fn finish(&self) {
        self.open.set(false);
        self.owner.replace(None);
    }

    fn is_open(&self) -> bool {
        self.open.get()
    }

    fn owner(&self) -> Option<ViewDefinitionOwner> {
        self.open
            .get()
            .then(|| self.owner.borrow().clone())
            .flatten()
    }
}

fn source_view_extension_owner(namespace: &str, identity: &str) -> ViewExtensionOwner {
    ViewExtensionOwner::new(source_owner_key(namespace, identity))
}

fn source_view_definition_owner(namespace: &str, identity: &str) -> ViewDefinitionOwner {
    ViewDefinitionOwner::new(source_owner_key(namespace, identity))
}

fn filesystem_view_extension_owner(path: &Path) -> ViewExtensionOwner {
    ViewExtensionOwner::new(filesystem_owner_key("filesystem", path))
}

fn filesystem_view_definition_owner(path: &Path) -> ViewDefinitionOwner {
    ViewDefinitionOwner::new(filesystem_owner_key("filesystem", path))
}

fn source_owner_key(namespace: &str, identity: &str) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(namespace.len() + 1 + identity.len() * 2);
    encoded.push_str(namespace);
    encoded.push('.');
    for byte in identity.as_bytes() {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn filesystem_owner_key(namespace: &str, path: &Path) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::from(namespace);
    encoded.push('.');
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        for byte in path.as_os_str().as_bytes() {
            write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;

        for unit in path.as_os_str().encode_wide() {
            write!(&mut encoded, "{unit:04x}").expect("writing to String cannot fail");
        }
    }
    encoded
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

/// Buffer for `editor.writeDecorations` calls from worker `onmessage`
/// listeners. Stores decorations keyed by `ContentId` with the content
/// revision they were written at. Stale entries (revision mismatch) are
/// dropped on both write and read.
#[derive(Default)]
pub(super) struct WorkerDecorationBuffer {
    current: HashMap<ContentId, (u64, Option<vell_core::text_snapshot::TextSnapshot>)>,
    entries: HashMap<ContentId, (u64, DecorationSet)>,
}

impl WorkerDecorationBuffer {
    /// Record the latest known revision and text snapshot for a content.
    fn track_current(
        &mut self,
        content_id: ContentId,
        revision: u64,
        snapshot: Option<vell_core::text_snapshot::TextSnapshot>,
    ) {
        self.current.insert(content_id, (revision, snapshot));
    }

    fn current_revision(&self, content_id: ContentId) -> Option<u64> {
        self.current.get(&content_id).map(|(revision, _)| *revision)
    }

    fn snapshot(&self, content_id: ContentId) -> Option<&vell_core::text_snapshot::TextSnapshot> {
        self.current
            .get(&content_id)
            .and_then(|(_, snapshot)| snapshot.as_ref())
    }

    /// Write decorations for the given `revision`. Silently drops if the
    /// revision doesn't match the current content revision.
    fn write(&mut self, content_id: ContentId, revision: u64, set: DecorationSet) {
        if self.current_revision(content_id) == Some(revision) {
            self.entries.insert(content_id, (revision, set));
        }
    }

    /// Read decorations for `content_id` if the stored revision matches
    /// `current_revision`. Returns `None` if stale or absent.
    fn read(&self, content_id: ContentId, current_revision: u64) -> Option<&DecorationSet> {
        let (stored_rev, set) = self.entries.get(&content_id)?;
        (*stored_rev == current_revision).then_some(set)
    }

    /// Reflow the last valid decorations onto the next revision so
    /// they stay visible until the worker returns fresh results.
    fn reflow(
        &mut self,
        content_id: ContentId,
        current_revision: u64,
        change: &vell_core::transaction::TextChangeSet,
    ) {
        let Some((stored_rev, set)) = self.entries.get_mut(&content_id) else {
            return;
        };
        // A missed change leaves no safe position mapping.
        if *stored_rev + 1 != current_revision {
            return;
        }
        *set = map_decoration_set(set, change);
        *stored_rev = current_revision;
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

impl ScriptModeState {
    fn new(data: serde_json::Value) -> Self {
        Self {
            data,
            decorations: DecorationSet::default(),
        }
    }

    fn publish_external_data(&mut self, data: serde_json::Value) {
        self.data = data;
    }
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
    for (_, path, _) in plugins {
        host.execute_embedded_module(path)?;
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
    result
}

pub fn load_default_configuration() -> Result<LoadedEditorConfiguration, ScriptError> {
    loaded_editor_configuration(load_default_plugins()?)
}

pub fn load_user_configuration() -> Result<LoadedEditorConfiguration, ScriptError> {
    loaded_editor_configuration(load_user_config()?)
}

fn loaded_editor_configuration(
    host: Rc<RefCell<ScriptHost>>,
) -> Result<LoadedEditorConfiguration, ScriptError> {
    let modes = ScriptHost::script_modes(&host)
        .into_iter()
        .map(|mode| Box::new(mode) as Box<dyn Mode>)
        .collect();
    let configuration = {
        let host = host.borrow();
        host.configuration.borrow().clone()
    };
    Ok(LoadedEditorConfiguration {
        modes,
        backgrounds: vec![Box::new(ScriptBackground::new(host.clone()))],
        view_definitions: ScriptHost::script_view_definitions(&host),
        view_extensions: ScriptHost::script_view_extensions(&host),
        theme: configuration.theme,
        face_overrides: configuration.face_overrides,
        host,
    })
}

pub fn load_typescript_modes(
    specifier: &str,
    source: &str,
) -> Result<LoadedScriptModes, ScriptError> {
    let mut host = ScriptHost::new();
    host.execute_typescript(specifier, source)?;
    let host = Rc::new(RefCell::new(host));
    let modes = ScriptHost::script_modes(&host)
        .into_iter()
        .map(|mode| Box::new(mode) as Box<dyn Mode>)
        .collect();
    let backgrounds =
        vec![Box::new(ScriptBackground::new(host.clone())) as Box<dyn ModeBackground>];
    let commands = ScriptHost::command_entries(&host);
    Ok(LoadedScriptModes {
        modes,
        backgrounds,
        view_definitions: ScriptHost::script_view_definitions(&host),
        view_extensions: ScriptHost::script_view_extensions(&host),
        commands,
        host,
    })
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
        KeyCode::Tab => {
            value.insert("code".to_owned(), ModeValue::String("tab".to_owned()));
        }
        KeyCode::BackTab => {
            value.insert("code".to_owned(), ModeValue::String("backtab".to_owned()));
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
        "command must return undefined or ctx.pass()",
    ))
}

/// Parse a raw spans array (as passed to `editor.writeDecorations`)
/// into sorted `NamedTextDecoration`s.
pub(super) fn parse_decoration_spans(
    scope: &mut v8::PinScope,
    spans: v8::Local<v8::Array>,
    snapshot: &vell_core::text_snapshot::TextSnapshot,
) -> Result<Vec<NamedTextDecoration>, ScriptError> {
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
        let start = parse_position(scope, start_value, snapshot)?;
        let end_value = required_object(scope, range, "end")?;
        let end = parse_position(scope, end_value, snapshot)?;
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
    Ok(decorations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vell_core::action::ContentAction;
    use vell_core::buffer::Buffer;
    use vell_core::command::EditCommand;
    use vell_core::content::{Content, ContentKind};
    use vell_core::content_store::ContentStore;
    use vell_mode::{InputFlow, ModeRegistry};
    use vell_protocol::ids::{ContentId, ViewId};
    use vell_protocol::revision::Revision;
    use vell_protocol::view::{BindingKey, ViewDefinitionId};

    fn extension_context() -> ViewExtensionContext {
        ViewExtensionContext {
            view_id: ViewId(7),
            definition: ViewDefinitionId::new("core.buffer"),
            revision: Revision(3),
            bindings: vec![(BindingKey::new("document"), ContentId(2))],
            document: Some(vell_mode::ViewExtensionDocument {
                content_id: ContentId(2),
                revision: Revision(5),
                text: "alpha\nbeta".to_owned(),
                resource_name: Some("sample.rs".to_owned()),
                selections: vec![vell_mode::ViewExtensionSelection {
                    anchor: vell_mode::ViewExtensionPosition {
                        line: 1,
                        character: 1,
                    },
                    head: vell_mode::ViewExtensionPosition {
                        line: 1,
                        character: 1,
                    },
                }],
                primary_selection: vell_mode::ViewExtensionSelection {
                    anchor: vell_mode::ViewExtensionPosition {
                        line: 1,
                        character: 1,
                    },
                    head: vell_mode::ViewExtensionPosition {
                        line: 1,
                        character: 1,
                    },
                },
            }),
        }
    }

    #[test]
    fn view_extension_schema_callback_and_unload_share_one_contract() {
        let mut host = ScriptHost::new();
        host.execute_typescript(
            "file:///minimap.ts",
            r#"
editor.views.extend("core.buffer", {
  id: "example.minimap",
  panes: {
    minimap: {
      side: "right",
      size: 8,
      render(context) {
        return {
          type: "lines",
          baseFace: "ui.editor",
          rows: [
            context.document?.resourceName ?? "none",
            [{
              text: `${context.document?.primarySelection.head.line}:` +
                String(context.document?.primarySelection.head.character),
              face: "ui.selection",
            }],
          ],
        };
      },
    },
  },
});
"#,
        )
        .unwrap();
        let host = Rc::new(RefCell::new(host));
        let mut extensions = ScriptHost::script_view_extensions(&host);

        assert_eq!(extensions.len(), 1);
        assert_eq!(extensions[0].definition().id().as_str(), "example.minimap");
        let presentation = extensions[0]
            .present("minimap", &extension_context())
            .unwrap();
        let ViewExtensionPresentation::Lines(lines) = presentation;
        assert_eq!(lines.rows[0][0].text, "sample.rs");
        assert_eq!(lines.rows[1][0].text, "1:1");

        extensions[0].unload();
        assert!(host.borrow().view_extension_definitions.borrow().is_empty());
    }

    #[test]
    fn compound_view_definition_schema_is_strict_atomic_and_module_scoped() {
        let mut host = ScriptHost::new();
        host.execute_typescript(
            "file:///diff-view.ts",
            r#"
editor.views.define({
  name: "example.diff",
  bindings: ["left", "right"],
  layout: {
    direction: "horizontal",
    children: [
      { key: "before", view: "core.buffer", bindings: { document: "left" } },
      { key: "after", view: "core.buffer", bindings: { document: "right" } },
    ],
  },
});
"#,
        )
        .unwrap();
        let definitions = host.view_definitions.borrow();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].definition().id().as_str(), "example.diff");
        assert_eq!(definitions[0].children()[0].key(), "before");
        drop(definitions);

        let error = host
            .execute_typescript(
                "file:///invalid-view.ts",
                r#"
editor.views.define({
  name: "example.partial",
  bindings: ["left", "right"],
  layout: {
    direction: "horizontal",
    children: [
      { key: "left", view: "core.buffer", bindings: { document: "left" } },
      { key: "right", view: "core.buffer", bindings: { document: "right" } },
    ],
  },
});
editor.views.define({
  name: "example.invalid",
  bindings: ["left"],
  unknown: true,
  layout: { direction: "vertical", children: [] },
});
"#,
            )
            .unwrap_err();
        assert!(error.to_string().contains("unknown field 'unknown'"));
        assert_eq!(host.view_definitions.borrow().len(), 1);

        let error = host
            .execute_typescript(
                "file:///oversized-view.ts",
                r#"
const bindings = [];
bindings.length = 1_000_000_000;
editor.views.define({
  name: "example.oversized",
  bindings,
  layout: {
    direction: "horizontal",
    children: [
      { key: "left", view: "core.buffer", bindings: { document: "left" } },
      { key: "right", view: "core.buffer", bindings: { document: "right" } },
    ],
  },
});
"#,
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("script limit exceeded for View definition bindings")
        );
        assert_eq!(host.view_definitions.borrow().len(), 1);

        let error = host
            .evaluate_script(
                r#"editor.views.define({
  name: "example.dynamic",
  bindings: ["left"],
  layout: { direction: "horizontal", children: [] },
})"#,
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("only available during module loading")
        );
    }

    #[test]
    fn view_extension_registration_is_strict_atomic_and_budgeted() {
        let mut host =
            ScriptHost::with_timeouts(Duration::from_millis(50), Duration::from_millis(100));
        let error = host
            .execute_typescript(
                "file:///invalid-extension.ts",
                r#"
editor.views.extend("core.buffer", {
  id: "example.partial",
  panes: { map: { side: "right", size: 4, render: () => ({ type: "lines", rows: [] }) } },
});
editor.views.extend("core.buffer", {
  id: "example.invalid",
  unknown: true,
  panes: { map: { side: "right", size: 4, render: () => ({ type: "lines", rows: [] }) } },
});
"#,
            )
            .unwrap_err();
        assert!(error.to_string().contains("unknown field 'unknown'"));
        assert!(host.view_extension_definitions.borrow().is_empty());

        host.execute_typescript(
            "file:///slow-extension.ts",
            r#"
editor.views.extend("core.buffer", {
  id: "example.slow",
  panes: {
    map: {
      side: "right",
      size: 4,
      render() { while (true) {} },
    },
  },
});
"#,
        )
        .unwrap();
        let host = Rc::new(RefCell::new(host));
        let mut extensions = ScriptHost::script_view_extensions(&host);
        let error = extensions[0]
            .present("map", &extension_context())
            .unwrap_err();
        assert!(
            error.to_string().contains("timeout"),
            "unexpected callback error: {error}"
        );
    }

    #[test]
    fn view_extension_callback_cannot_register_another_extension() {
        let mut host = ScriptHost::new();
        host.execute_typescript(
            "file:///dynamic-extension.ts",
            r#"
editor.views.extend("core.buffer", {
  id: "example.dynamic",
  panes: {
    map: {
      side: "right",
      size: 4,
      render() {
        const errors = [];
        const attempt = (callback) => {
          try { callback(); } catch (error) { errors.push(String(error)); }
        };
        attempt(() => editor.views.define({
          name: "example.leaked-view",
          bindings: ["document"],
          layout: { direction: "horizontal", children: [] },
        }));
        attempt(() => editor.views.extend("core.buffer", {
          id: "example.leaked",
          panes: { leaked: {
            side: "left",
            size: 2,
            render: () => ({ type: "lines", rows: [] }),
          } },
        }));
        attempt(() => editor.theme.use("catppuccin-mocha"));
        attempt(() => editor.faces.override("ui.editor", { bold: true }));
        attempt(() => editor.modes.define({
          name: "example.leaked-mode",
          on: { buffer: {} },
        }));
        attempt(() => editor.commands.register("example.leaked", () => {}));
        attempt(() => editor.writeDecorations(2, 5, []));
        attempt(() => new Worker({}));
        return { type: "lines", rows: errors };
      },
    },
  },
});
"#,
        )
        .unwrap();
        let host = Rc::new(RefCell::new(host));
        let mut extensions = ScriptHost::script_view_extensions(&host);

        let presentation = extensions[0].present("map", &extension_context()).unwrap();

        let ViewExtensionPresentation::Lines(lines) = presentation;
        assert_eq!(lines.rows.len(), 8);
        assert!(lines.rows.iter().all(|row| {
            row[0]
                .text
                .contains("not available during View extension rendering")
                || row[0].text.contains("only available during module loading")
        }));
        let definitions = host.borrow().view_extension_definitions.borrow().clone();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].definition.id().as_str(), "example.dynamic");
        assert!(host.borrow().view_definitions.borrow().is_empty());
        assert!(host.borrow().definitions.borrow().is_empty());
        assert_eq!(host.borrow().commands.borrow().change_count(), 0);
        let configuration = host.borrow().configuration.borrow().clone();
        assert!(configuration.theme.is_none());
        assert!(configuration.face_overrides.is_empty());
    }

    #[test]
    fn filesystem_view_extension_owners_are_unique_and_unload_independently() {
        let directory = tempfile::tempdir().unwrap();
        let first_root = directory.path().join("first");
        let second_root = directory.path().join("second");
        fs::create_dir_all(&first_root).unwrap();
        fs::create_dir_all(&second_root).unwrap();
        let source = |id: &str| {
            format!(
                r#"editor.views.extend("core.buffer", {{
  id: "{id}",
  panes: {{ map: {{
    side: "right",
    size: 4,
    render: () => ({{ type: "lines", rows: [] }}),
  }} }},
}});"#
            )
        };
        let first = first_root.join("plugin.ts");
        let second = second_root.join("plugin.ts");
        fs::write(&first, source("example.first")).unwrap();
        fs::write(&second, source("example.second")).unwrap();
        let mut host = ScriptHost::new();
        host.execute_module(&first).unwrap();
        host.execute_module(&second).unwrap();
        let host = Rc::new(RefCell::new(host));
        let mut extensions = ScriptHost::script_view_extensions(&host);
        let first_owner = extensions[0].definition().owner().clone();
        let second_owner = extensions[1].definition().owner().clone();
        assert_ne!(first_owner, second_owner);

        extensions[0].unload();

        let definitions = host.borrow().view_extension_definitions.borrow().clone();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].definition.id().as_str(), "example.second");
    }

    #[test]
    fn filesystem_view_definition_owners_are_collision_free() {
        let directory = tempfile::tempdir().unwrap();
        let first_root = directory.path().join("a-b");
        let second_root = directory.path().join("a_b");
        fs::create_dir_all(&first_root).unwrap();
        fs::create_dir_all(&second_root).unwrap();
        let source = |name: &str| {
            format!(
                r#"editor.views.define({{
  name: "{name}",
  bindings: ["left", "right"],
  layout: {{ direction: "horizontal", children: [
    {{ key: "left", view: "core.buffer", bindings: {{ document: "left" }} }},
    {{ key: "right", view: "core.buffer", bindings: {{ document: "right" }} }},
  ] }},
}});"#
            )
        };
        let first = first_root.join("plugin.ts");
        let second = second_root.join("plugin.ts");
        fs::write(&first, source("example.first-view")).unwrap();
        fs::write(&second, source("example.second-view")).unwrap();
        let mut host = ScriptHost::new();
        host.execute_module(&first).unwrap();
        host.execute_module(&second).unwrap();
        let definitions = host.view_definitions.borrow();

        assert_eq!(definitions.len(), 2);
        assert_ne!(definitions[0].owner(), definitions[1].owner());
    }

    #[test]
    fn tab_input_uses_the_public_key_codes() {
        for (code, expected) in [(KeyCode::Tab, "tab"), (KeyCode::BackTab, "backtab")] {
            let ModeValue::Map(arguments) = key_event_arguments(KeyEvent::plain(code)) else {
                panic!("key arguments must be an object");
            };
            assert_eq!(
                arguments.get("code"),
                Some(&ModeValue::String(expected.to_owned()))
            );
        }
    }

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
  dim: true,
  italic: false,
  underlineStyle: "double",
  strikethrough: true,
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
            configuration.face_overrides[0].patch.underline_style,
            FaceValue::Value(UnderlineStyle::Double)
        );
        assert_eq!(
            configuration.face_overrides[0].patch.strikethrough,
            FaceValue::Value(true)
        );
        assert_eq!(
            configuration.face_overrides[1].patch.background,
            FaceValue::Reset
        );
    }

    #[test]
    fn user_config_worker_loads_after_embedded_plugins() {
        let host = load_default_plugins().unwrap();
        let mode_count = ScriptHost::script_modes(&host).len();
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.ts");
        fs::write(
            &config,
            r#"
const worker = new Worker(
  new URL("./worker.ts", import.meta.url),
  { type: "module" },
);
worker.onmessage = (event) => {
  if (event.data !== "ready") throw new Error("unexpected worker response");
  editor.modes.define({
    name: "user-worker-after-defaults",
    on: { buffer: {} },
  });
};
worker.postMessage(null);
"#,
        )
        .unwrap();
        fs::write(
            directory.path().join("worker.ts"),
            "self.onmessage = () => self.postMessage(\n\
editor.resources.readText('data.txt'));",
        )
        .unwrap();
        fs::write(directory.path().join("data.txt"), "ready").unwrap();

        load_optional_user_config(&host, &config).unwrap();
        for _ in 0..50 {
            host.borrow_mut().pump_worker_messages();
            if ScriptHost::script_modes(&host).len() > mode_count {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(ScriptHost::script_modes(&host).len(), mode_count + 1);
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
        assert!(
            host.borrow()
                .configuration
                .borrow()
                .face_overrides
                .is_empty()
        );
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
editor.modes.define({ name: "partial", on: { buffer: {} } });
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
  attach: {
    view: "core.buffer",
    binding: "document",
    languages: ["rust", "markdown"],
  },
  on: {
    buffer: {
      state: () => ({ calls: 0 }),
      viewState: (content: { calls: number }) => ({ initial: content.calls }),
      commands: {
        quote(ctx) {
          ctx.state.calls++;
          ctx.viewState.initial++;
          ctx.edit.insert("\"\"");
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
        let registered_mode = ScriptHost::script_modes(&host).pop().unwrap();
        let mut registry = ModeRegistry::new();
        let registered = registry.register(registered_mode).unwrap();
        assert!(registry.adapter(registered, ContentKind::Buffer).is_some());
        let mut modes = ScriptHost::script_modes(&host);
        let mode = modes.pop().unwrap();
        assert_eq!(mode.name().as_str(), "pairs");
        assert_eq!(mode.before().unwrap().as_str(), "base-mode");
        let attachment = mode.attachment();
        assert_eq!(attachment.view().as_str(), "core.buffer");
        assert_eq!(attachment.binding().unwrap().as_str(), "document");
        assert_eq!(
            attachment
                .languages()
                .unwrap()
                .map(LanguageId::as_str)
                .collect::<Vec<_>>(),
            ["markdown", "rust"]
        );

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
    fn rejects_malformed_mode_attachment_definition() {
        let mut host = ScriptHost::new();
        let error = host
            .execute_typescript(
                "file:///invalid-attachment.ts",
                r#"
editor.modes.define({
  name: "invalid-attachment",
  attach: {
    view: "core.buffer",
    binding: "document",
    languages: "rust",
  },
  on: { buffer: {} },
});
"#,
            )
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("mode attach languages must be an array"),
            "{error}"
        );
    }

    #[test]
    fn parses_view_only_mode_attachment_without_a_content_binding() {
        let mut host = ScriptHost::new();
        host.execute_typescript(
            "file:///view-only-attachment.ts",
            r#"
editor.modes.define({
  name: "diff-navigation",
  attach: { view: "example.diff" },
  on: { buffer: {} },
});
"#,
        )
        .unwrap();
        let host = Rc::new(RefCell::new(host));
        let mode = ScriptHost::script_modes(&host).pop().unwrap();

        assert_eq!(mode.attachment().view().as_str(), "example.diff");
        assert!(mode.attachment().binding().is_none());
    }

    #[test]
    fn rejects_language_matching_without_a_content_binding() {
        let error = ScriptHost::new()
            .execute_typescript(
                "file:///view-only-language.ts",
                r#"
editor.modes.define({
  name: "invalid-view-only-language",
  attach: { view: "example.diff", languages: ["rust"] },
  on: { buffer: {} },
});
"#,
            )
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("mode attach languages require a binding"),
            "{error}"
        );
    }

    #[test]
    fn registers_buffer_commands_with_void_and_qualified_invocation() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.ts");
        fs::write(
            &config,
            r#"
editor.modes.define({
  name: "pairs",
  faces: {
    "plugin.pairs.match": {
      inherits: ["syntax.string"],
      fallback: { bold: true },
    },
  },
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
        emphasize(ctx) {
          ctx.faces.addRelative(
            "plugin.pairs.match",
            ["syntax.string", { underline: true }],
            "view",
          );
        },
        createContent(ctx) {
          ctx.content.create();
        },
        saveContent(ctx) {
          ctx.content.save(ctx.contentId, true);
        },
        focusView(ctx) {
          ctx.view.focus(ctx.viewId);
        },
        switchView(ctx) {
          ctx.view.switch({ type: "core.buffer", create: true });
        },
        switchDiff(ctx) {
          ctx.view.switch({ type: "core.diff", left: 1, right: 2 });
        },
        switchDefined(ctx) {
          ctx.view.switch({
            type: "defined",
            definition: "example.diff",
            bindings: { left: 1, right: 2 },
          });
        },
        invalidDiff(ctx) {
          ctx.view.switch({ type: "core.diff", left: 1, right: 2, extra: true });
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
        let definitions = mode.face_definitions();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].name.as_str(), "plugin.pairs.match");
        assert_eq!(
            definitions[0].inherits,
            vec![FaceName::new("syntax.string")]
        );

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

        let emphasize = mode
            .execute_view_with_arguments(
                content_state.as_mut(),
                view_state.as_mut(),
                &context,
                &ModeActionName::new("emphasize"),
                &ModeValue::Null,
            )
            .unwrap();
        let (_, operations) = emphasize.into_parts();
        assert!(matches!(
            operations.as_slice(),
            [vell_mode::operation::OperationRequest::Face(
                vell_mode::operation::FaceOperation::AddRelative {
                    target: vell_mode::operation::FaceRemapTarget::CurrentView,
                    face,
                    token: vell_protocol::content_query::FaceRemapToken(1),
                    expressions,
                }
            )] if face.as_str() == "plugin.pairs.match" && expressions.len() == 2
        ));

        let mut execute = |action: &str| {
            mode.execute_view_with_arguments(
                content_state.as_mut(),
                view_state.as_mut(),
                &context,
                &ModeActionName::new(action),
                &ModeValue::Null,
            )
            .unwrap()
            .into_parts()
            .1
        };
        assert!(matches!(
            execute("createContent").as_slice(),
            [vell_mode::operation::OperationRequest::ContentLifecycle(
                vell_mode::operation::ContentLifecycleOperation::Create
            )]
        ));
        assert!(matches!(
            execute("saveContent").as_slice(),
            [vell_mode::operation::OperationRequest::ContentLifecycle(
                vell_mode::operation::ContentLifecycleOperation::Save {
                    target: vell_mode::operation::ContentTarget::Id(ContentId(0)),
                    force: true,
                }
            )]
        ));
        assert!(matches!(
            execute("focusView").as_slice(),
            [vell_mode::operation::OperationRequest::ViewLifecycle(
                vell_mode::operation::ViewLifecycleOperation::Focus { view: ViewId(0) }
            )]
        ));
        assert!(matches!(
            execute("switchView").as_slice(),
            [vell_mode::operation::OperationRequest::ViewLifecycle(
                vell_mode::operation::ViewLifecycleOperation::Switch {
                    spec: vell_mode::operation::ViewSpec::Buffer {
                        source: vell_mode::operation::BufferViewSource::Create,
                    },
                }
            )]
        ));
        assert!(matches!(
            execute("switchDiff").as_slice(),
            [vell_mode::operation::OperationRequest::ViewLifecycle(
                vell_mode::operation::ViewLifecycleOperation::Switch {
                    spec: vell_mode::operation::ViewSpec::Diff { left, right },
                }
            )] if *left == ContentId(1) && *right == ContentId(2)
        ));
        assert!(matches!(
            execute("switchDefined").as_slice(),
            [vell_mode::operation::OperationRequest::ViewLifecycle(
                vell_mode::operation::ViewLifecycleOperation::Switch {
                    spec: vell_mode::operation::ViewSpec::Defined {
                        definition,
                        bindings,
                    },
                }
            )] if definition.as_str() == "example.diff"
                && bindings.get("left") == Some(&ContentId(1))
                && bindings.get("right") == Some(&ContentId(2))
        ));
        let error = mode
            .execute_view_with_arguments(
                content_state.as_mut(),
                view_state.as_mut(),
                &context,
                &ModeActionName::new("invalidDiff"),
                &ModeValue::Null,
            )
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("diff view spec contains an unknown field"),
            "{error}"
        );
    }

    #[test]
    fn editing_strategies_receive_snapshot_and_validate_before_publish() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.ts");
        fs::write(
            &config,
            r#"
editor.modes.define({
  name: "editing-strategies",
  on: {
    buffer: {
      state: () => null,
      viewState: () => ({ calls: 0 }),
      commands: {
        probe(ctx) {
          if (ctx.text !== "a\u{1F600}b") throw new Error("missing text snapshot");
          if (ctx.primarySelection.head.character !== 3) {
            throw new Error("selection must use UTF-16 positions");
          }
          ctx.edit.insertNewline({ indent: "  ", closingIndent: "" });
          ctx.edit.toggleLineComment({ delimiter: "//" });
          ctx.edit.toggleBlockComment({ open: "/*", close: "*/" });
          ctx.edit.insertPair({ open: "(", close: ")" });
          ctx.edit.insertClosingPair({ open: "(", close: ")" });
          ctx.edit.deletePairBackward({ open: "(", close: ")" });
          ctx.search.find(
            { kind: "literal", value: "b" },
            { caseSensitive: false, direction: "backward", wrap: false },
          );
          ctx.search.replaceNext(
            { kind: "regex", value: "(b)" },
            "$1",
          );
          ctx.search.replaceAll(
            { kind: "literal", value: "b" },
            "c",
            { caseSensitive: true },
          );
        },
        invalid(ctx) {
          ctx.viewState.calls++;
          ctx.edit.insertPair({ open: "", close: ")" });
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
        let mut buffer = Buffer::new();
        buffer.insert_at_selections(
            &mut vell_protocol::selection::Selections::single(
                vell_protocol::selection::Selection::collapsed(
                    vell_protocol::selection::TextOffset::origin(),
                ),
            ),
            "a\u{1F600}b",
        );
        let content_id = ContentId(0);
        let mut contents = ContentStore::default();
        contents
            .insert(content_id, Content::Buffer(buffer))
            .unwrap();
        let mut view_data = contents.create_view_state(content_id).unwrap();
        view_data.replace_selections(vell_protocol::selection::Selections::single(
            vell_protocol::selection::Selection::collapsed(vell_protocol::selection::TextOffset {
                char_index: 2,
            }),
        ));
        let context = ModeViewContext::new(ViewId(0), content_id, &view_data, &contents).unwrap();
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
                &ModeActionName::new("probe"),
                &ModeValue::Null,
            )
            .unwrap()
            .into_parts();
        assert_eq!(operations.len(), 9);
        assert!(matches!(
            &operations[0],
            vell_mode::operation::OperationRequest::View {
                operation: vell_mode::operation::ViewOperation::Edit(
                    EditCommand::InsertNewline { indent, closing_indent }
                ),
                ..
            } if indent == "  " && closing_indent.as_deref() == Some("")
        ));
        assert!(matches!(
            &operations[6],
            vell_mode::operation::OperationRequest::Search {
                operation: vell_mode::operation::SearchOperation::Find {
                    expected_revision: vell_protocol::revision::Revision(0),
                    start: 2,
                    pattern: vell_core::search::SearchPattern::Literal(pattern),
                    options: vell_core::search::SearchOptions {
                        case: vell_core::search::CaseSensitivity::Insensitive,
                        direction: vell_core::search::SearchDirection::Backward,
                        wrap: false,
                    },
                },
                ..
            } if pattern == "b"
        ));
        assert!(matches!(
            &operations[8],
            vell_mode::operation::OperationRequest::Search {
                operation: vell_mode::operation::SearchOperation::ReplaceAll {
                    replacement,
                    case: vell_core::search::CaseSensitivity::Sensitive,
                    ..
                },
                ..
            } if replacement == "c"
        ));

        let error = mode
            .execute_view_with_arguments(
                content_state.as_mut(),
                view_state.as_mut(),
                &context,
                &ModeActionName::new("invalid"),
                &ModeValue::Null,
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("open must not be empty"), "{error}");
        assert_eq!(
            script_state(view_state.as_ref(), mode.name()).unwrap().data,
            serde_json::json!({ "calls": 0 })
        );
    }

    #[test]
    fn pass_continues_flow_and_errors_do_not_publish_state() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.ts");
        fs::write(
            &config,
            r#"
editor.modes.define({
  name: "flow",
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
    fn schema_rejects_unknown_adapters_and_invalid_keys() {
        for (name, body, expected) in [
            (
                "unknown-adapter",
                r#"on: { terminal: { commands: {} } }"#,
                "unknown mode adapter 'terminal'",
            ),
            (
                "mixed-schema",
                r#"on: { buffer: { commands: {} } }, actions: {}"#,
                "cannot combine 'on' with 'actions'",
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
                "raw-worker-lifecycle",
                r#"on: { buffer: { job() {} } }"#,
                "mode buffer.job is not supported",
            ),
            (
                "buffer-analysis-field-rejected",
                r#"on: { buffer: { analysis: { syntax: {} } } }"#,
                "mode buffer.analysis is not supported",
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
                .execute_typescript("file:///invalid.ts", &source)
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
  on: {
    buffer: {
      state: () => ({ calls: 0 }),
      viewState: () => ({
        calls: 0,
        viewPolicy: { cursorStyle: "block" },
      }),
      commands: {
        throwing(ctx) {
          ctx.state.calls++;
          ctx.viewState.calls++;
          throw new Error("action exploded");
        },
        invalid(ctx) {
          ctx.state.calls++;
          ctx.viewState.calls++;
          ctx.viewState.viewPolicy.cursorStyle = 42;
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
  on: {
    buffer: {
      state: () => ({ calls: 0 }),
      viewState: () => ({
        calls: 0,
        viewPolicy: { cursorStyle: "bar" },
      }),
      commands: {
        hang(ctx) {
          ctx.state.calls++;
          ctx.viewState.calls++;
          ctx.viewState.viewPolicy.cursorStyle = "block";
          ctx.edit.insert("discarded");
          while (true) {}
        },
        recover(ctx) {
          ctx.state.calls++;
          ctx.viewState.calls++;
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
  on: {{
    buffer: {{
      state: () => ({{ calls: 0 }}),
      commands: {{
        operations(ctx) {{
          ctx.state.calls++;
          for (let index = 0; index < {}; index++) ctx.edit.insert("x");
        }},
        operationCount(ctx) {{
          ctx.state.calls++;
          ctx.cursor.moveWordForward({});
        }},
      }},
    }},
  }},
}});
"#,
                MAX_SCRIPT_OPERATIONS + 1,
                MAX_SCRIPT_OPERATIONS + 1,
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
            vec![
                "vim",
                "syntax-highlighting-markdown",
                "syntax-highlighting-rust"
            ]
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
                .all(|action| action.name.as_str() != INPUT_ACTION)
        );
        let highlighting = definitions
            .iter()
            .find(|definition| definition.name.as_str() == "syntax-highlighting-rust")
            .unwrap();
        let adapter = highlighting.adapters.buffer.as_ref().unwrap();
        assert!(adapter.create_content.is_some());
    }

    #[test]
    fn raw_input_is_not_a_registered_mode_command() {
        let host = load_default_plugins().unwrap();
        let vim = ScriptHost::script_modes(&host)
            .into_iter()
            .find(|mode| mode.name().as_str() == "vim")
            .unwrap();
        let mut registry = ModeRegistry::new();
        registry.register(vim).unwrap();

        let error = registry
            .resolve_command_checked(&ModeName::new("vim"), &ModeActionName::new(INPUT_ACTION))
            .unwrap_err();

        assert!(matches!(error, ModeError::UnknownAction { .. }));
    }

    #[test]
    fn worker_only_plugin_keeps_a_background_owner() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.ts");
        let worker_path = directory.path().join("worker.ts");
        fs::write(
            &worker_path,
            "self.onmessage = () => self.postMessage('done');",
        )
        .unwrap();
        fs::write(
            &config,
            r#"
const worker = new Worker(new URL("./worker.ts", import.meta.url), { type: "module" });
worker.onmessage = () => {};
worker.postMessage(null);
"#,
        )
        .unwrap();
        let mut host = ScriptHost::new();
        host.execute_module(&config).unwrap();
        let host = Rc::new(RefCell::new(host));

        assert!(ScriptHost::script_modes(&host).is_empty());
        let background = ScriptBackground::new(host);
        let delivered = (0..100).any(|_| {
            if background.poll_background() {
                true
            } else {
                std::thread::sleep(Duration::from_millis(5));
                false
            }
        });
        assert!(
            delivered,
            "worker-only plugin must remain alive and be polled"
        );
    }

    #[test]
    fn public_contract_keeps_the_declaration_surface_current() {
        assert!(crate::TYPESCRIPT_DECLARATIONS.contains("interface ModeDefinition"));
        assert!(!crate::TYPESCRIPT_DECLARATIONS.contains("ModeDefinitionV2"));
        assert!(!crate::TYPESCRIPT_DECLARATIONS.contains("@deprecated Removed in Vell"));
        assert!(!crate::TYPESCRIPT_DECLARATIONS.contains("interface ModeDefinition<ContentState"));
    }

    #[test]
    fn public_contract_executes_the_worker_platform_example() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("worker-platform.ts");
        fs::write(
            &config,
            include_str!("../../../../runtime/examples/worker-platform.ts"),
        )
        .unwrap();
        fs::write(
            directory.path().join("worker-platform-worker.ts"),
            include_str!("../../../../runtime/examples/worker-platform-worker.ts"),
        )
        .unwrap();

        let mut host = ScriptHost::new();
        host.execute_module(&config).unwrap();

        assert!(
            ScriptHost::script_modes(&Rc::new(RefCell::new(host)))
                .iter()
                .any(|mode| mode.name().as_str() == "worker-platform-example")
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
  on: {
    buffer: {
      commands: {
        replace(ctx) {
          ctx.edit.applyEdits([{
            range: {
              start: { line: 0, character: 1 },
              end: { line: 0, character: 3 },
            },
            text: "中",
          }]);
        },
      },
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
  on: {
    buffer: {
      commands: {
        retain(ctx) {
          retained = ctx;
        },
        reuse(ctx) {
          retained.cursor.moveLeft();
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

    #[test]
    fn write_decorations_installs_for_current_revision() {
        let mut host = ScriptHost::new();
        host.execute_embedded_plugin(
            "tree-sitter/write-decorations.ts",
            r#"
editor.modes.define({
  name: "write-decorations",
  on: {
    buffer: {
      state: () => ({}),
      commands: {
        touch() {},
      },
    },
  },
});
"#,
        )
        .unwrap();
        let host = Rc::new(RefCell::new(host));
        let mode = ScriptHost::script_modes(&host).pop().unwrap();
        let content_id = ContentId(0);
        let view_id = ViewId(0);
        let mut contents = ContentStore::default();
        contents
            .insert(content_id, Content::Buffer(Buffer::new()))
            .unwrap();
        let view_state_data = contents.create_view_state(content_id).unwrap();
        let context =
            ModeViewContext::new(view_id, content_id, &view_state_data, &contents).unwrap();
        let content_context = ModeContentContext::new(content_id, &contents);
        let mut content_state = mode.create_content_state(&content_context).unwrap();
        let mut view_state = mode
            .create_view_state(content_state.as_ref(), &context)
            .unwrap();

        // Execute a content action to set the current revision.
        mode.execute_view_with_arguments(
            content_state.as_mut(),
            view_state.as_mut(),
            &context,
            &ModeActionName::new("touch"),
            &ModeValue::Null,
        )
        .unwrap();

        // Now call editor.writeDecorations with the current revision (0).
        host.borrow_mut()
            .execute_typescript(
                "file:///test.ts",
                r#"
editor.writeDecorations(0, 0, []);
"#,
            )
            .unwrap();

        // With 0 decorations, content_decorations should be empty.
        let decorations = mode.content_decorations(
            content_state.as_ref(),
            &content_context,
            RowRange { start: 0, end: 1 },
        );
        assert!(decorations.is_empty());
    }

    #[test]
    fn write_decorations_routes_equal_revisions_by_content_id() {
        let mut host = ScriptHost::new();
        let first = ContentId(7);
        let second = ContentId(8);
        {
            let mut buffer = host.worker_decorations.borrow_mut();
            buffer.track_current(
                first,
                3,
                Some(vell_core::text_snapshot::TextSnapshot::from_text("a")),
            );
            buffer.track_current(
                second,
                3,
                Some(vell_core::text_snapshot::TextSnapshot::from_text("b")),
            );
        }

        host.execute_typescript(
            "file:///write-two-contents.ts",
            r#"
editor.writeDecorations(7, 3, [{
  range: {
    start: { line: 0, character: 0 },
    end: { line: 0, character: 1 },
  },
  face: "syntax.first",
}]);
editor.writeDecorations(8, 3, [{
  range: {
    start: { line: 0, character: 0 },
    end: { line: 0, character: 1 },
  },
  face: "syntax.second",
}]);
"#,
        )
        .unwrap();

        let buffer = host.worker_decorations.borrow();
        assert_eq!(
            buffer.read(first, 3).unwrap().iter().next().unwrap().face,
            FaceName::new("syntax.first")
        );
        assert_eq!(
            buffer.read(second, 3).unwrap().iter().next().unwrap().face,
            FaceName::new("syntax.second")
        );
    }

    #[test]
    fn write_decorations_drops_stale_revision() {
        let mut host = ScriptHost::new();
        host.execute_embedded_plugin(
            "tree-sitter/write-decorations-stale.ts",
            r#"
editor.modes.define({
  name: "write-decorations-stale",
  on: {
    buffer: {
      state: () => ({}),
      commands: {
        touch() {},
      },
    },
  },
});
"#,
        )
        .unwrap();
        let host = Rc::new(RefCell::new(host));
        let mode = ScriptHost::script_modes(&host).pop().unwrap();
        let content_id = ContentId(0);
        let view_id = ViewId(0);
        let mut contents = ContentStore::default();
        contents
            .insert(content_id, Content::Buffer(Buffer::new()))
            .unwrap();
        let view_state_data = contents.create_view_state(content_id).unwrap();
        let context =
            ModeViewContext::new(view_id, content_id, &view_state_data, &contents).unwrap();
        let content_context = ModeContentContext::new(content_id, &contents);
        let mut content_state = mode.create_content_state(&content_context).unwrap();
        let mut view_state = mode
            .create_view_state(content_state.as_ref(), &context)
            .unwrap();

        // Execute a content action to set the current revision (0).
        mode.execute_view_with_arguments(
            content_state.as_mut(),
            view_state.as_mut(),
            &context,
            &ModeActionName::new("touch"),
            &ModeValue::Null,
        )
        .unwrap();

        // Call writeDecorations with a stale revision (99).
        host.borrow_mut()
            .execute_typescript(
                "file:///test.ts",
                r#"
editor.writeDecorations(0, 99, [{
  range: { start: { line: 0, character: 0 }, end: { line: 0, character: 0 } },
  face: "test",
}]);
"#,
            )
            .unwrap();

        // Stale write should be dropped — no decorations.
        let decorations = mode.content_decorations(
            content_state.as_ref(),
            &content_context,
            RowRange { start: 0, end: 1 },
        );
        assert!(decorations.is_empty());
    }

    #[test]
    fn write_decorations_visible_in_content_decorations() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test.txt");
        fs::write(&path, "hello\nworld\n").unwrap();
        let mut host = ScriptHost::new();
        host.execute_embedded_plugin(
            "tree-sitter/write-decorations-visible.ts",
            r#"
editor.modes.define({
  name: "write-decorations-visible",
  on: {
    buffer: {
      state: () => ({}),
      commands: {
        touch() {},
      },
    },
  },
});
"#,
        )
        .unwrap();
        let host = Rc::new(RefCell::new(host));
        let mode = ScriptHost::script_modes(&host).pop().unwrap();
        let content_id = ContentId(0);
        let view_id = ViewId(0);
        let mut buffer = Buffer::new();
        buffer.open_path(path.to_str().unwrap()).unwrap();
        let mut contents = ContentStore::default();
        contents
            .insert(content_id, Content::Buffer(buffer))
            .unwrap();
        let view_state_data = contents.create_view_state(content_id).unwrap();
        let context =
            ModeViewContext::new(view_id, content_id, &view_state_data, &contents).unwrap();
        let content_context = ModeContentContext::new(content_id, &contents);
        let mut content_state = mode.create_content_state(&content_context).unwrap();
        let mut view_state = mode
            .create_view_state(content_state.as_ref(), &context)
            .unwrap();

        // Execute a content action to set the current revision.
        mode.execute_view_with_arguments(
            content_state.as_mut(),
            view_state.as_mut(),
            &context,
            &ModeActionName::new("touch"),
            &ModeValue::Null,
        )
        .unwrap();

        // Get the current revision from the buffer.
        let revision = contents.revision(content_id).unwrap().0;

        // Call writeDecorations with a valid span.
        host.borrow_mut()
            .execute_typescript(
                "file:///test.ts",
                &format!(
                    r#"
editor.writeDecorations(0, {revision}, [{{
  range: {{ start: {{ line: 0, character: 0 }}, end: {{ line: 0, character: 5 }} }},
  face: "syntax.test",
}}]);
"#,
                ),
            )
            .unwrap();

        let decorations = mode.content_decorations(
            content_state.as_ref(),
            &content_context,
            RowRange { start: 0, end: 2 },
        );
        assert_eq!(decorations.len(), 1);
        assert_eq!(decorations[0].face, FaceName::new("syntax.test"));
    }

    #[test]
    fn worker_decorations_are_emitted_once_across_script_modes() {
        let mut host = ScriptHost::new();
        host.execute_embedded_plugin(
            "test-worker/two-modes.ts",
            r#"
for (const name of ["worker-owner-a", "worker-owner-b"]) {
  editor.modes.define({ name, on: { buffer: {} } });
}
"#,
        )
        .unwrap();
        let host = Rc::new(RefCell::new(host));
        let modes = ScriptHost::script_modes(&host);
        let content_id = ContentId(0);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test.txt");
        fs::write(&path, "x").unwrap();
        let mut buffer = Buffer::new();
        buffer.open_path(path.to_str().unwrap()).unwrap();
        let mut contents = ContentStore::default();
        contents
            .insert(content_id, Content::Buffer(buffer))
            .unwrap();
        let context = ModeContentContext::new(content_id, &contents);
        let states: Vec<_> = modes
            .iter()
            .map(|mode| mode.create_content_state(&context).unwrap())
            .collect();
        let revision = contents.revision(content_id).unwrap().0;
        host.borrow().worker_decorations.borrow_mut().track_current(
            content_id,
            revision,
            context.buffer().unwrap().text_snapshot(),
        );
        host.borrow_mut()
            .execute_typescript(
                "file:///test.ts",
                &format!(
                    r#"editor.writeDecorations(0, {revision}, [{{
  range: {{ start: {{ line: 0, character: 0 }}, end: {{ line: 0, character: 1 }} }},
  face: "syntax.test",
}}]);"#,
                ),
            )
            .unwrap();

        let count: usize = modes
            .iter()
            .zip(&states)
            .map(|(mode, state)| {
                mode.content_decorations(state.as_ref(), &context, RowRange { start: 0, end: 1 })
                    .len()
            })
            .sum();
        assert_eq!(
            count, 1,
            "host decorations must have one presentation owner"
        );
    }

    #[test]
    fn worker_decorations_reflow_notified_changes_and_drop_untracked_revisions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test.txt");
        fs::write(&path, "hello\nworld\n").unwrap();
        let mut host = ScriptHost::new();
        host.execute_embedded_plugin(
            "tree-sitter/write-decorations-stale.ts",
            r#"
editor.modes.define({
  name: "write-decorations-stale",
  on: {
    buffer: {
      state: () => ({}),
      commands: {
        touch() {},
      },
    },
  },
});
"#,
        )
        .unwrap();
        let host = Rc::new(RefCell::new(host));
        let mode = ScriptHost::script_modes(&host).pop().unwrap();
        let content_id = ContentId(0);
        let view_id = ViewId(0);
        let mut buffer = Buffer::new();
        buffer.open_path(path.to_str().unwrap()).unwrap();
        let mut contents = ContentStore::default();
        contents
            .insert(content_id, Content::Buffer(buffer))
            .unwrap();
        let view_state_data = contents.create_view_state(content_id).unwrap();
        let context =
            ModeViewContext::new(view_id, content_id, &view_state_data, &contents).unwrap();
        let content_context = ModeContentContext::new(content_id, &contents);
        let mut content_state = mode.create_content_state(&content_context).unwrap();
        let mut view_state = mode
            .create_view_state(content_state.as_ref(), &context)
            .unwrap();

        // Execute a content action to set the current revision.
        mode.execute_view_with_arguments(
            content_state.as_mut(),
            view_state.as_mut(),
            &context,
            &ModeActionName::new("touch"),
            &ModeValue::Null,
        )
        .unwrap();

        let revision = contents.revision(content_id).unwrap().0;

        // Write decorations at the current revision.
        host.borrow_mut()
            .execute_typescript(
                "file:///test.ts",
                &format!(
                    r#"
editor.writeDecorations(0, {revision}, [{{
  range: {{ start: {{ line: 0, character: 0 }}, end: {{ line: 0, character: 5 }} }},
  face: "syntax.test",
}}]);
"#,
                ),
            )
            .unwrap();

        // Verify decorations are present at the current revision.
        {
            let content_context = ModeContentContext::new(content_id, &contents);
            let decorations = mode.content_decorations(
                content_state.as_ref(),
                &content_context,
                RowRange { start: 0, end: 2 },
            );
            assert_eq!(decorations.len(), 1);
        }

        // A normal content-change notification reflows the previous
        // layer while its replacement is still being computed.
        let snapshot = contents
            .text_snapshot(content_id)
            .expect("buffer should have a snapshot");
        let len = snapshot.len_chars();
        let edit = vell_core::transaction::TextEdit::new(0..0, "x".to_string());
        let text_change =
            vell_core::transaction::TextChangeSet::from_edits(len, vec![edit]).unwrap();
        contents.apply(content_id, ContentAction::Text(text_change.clone()));
        let change = vell_core::content::ContentChange::Text(text_change);
        let content_context = ModeContentContext::new(content_id, &contents);
        mode.on_content_changed(content_state.as_mut(), &content_context, &change)
            .unwrap();
        let decorations = mode.content_decorations(
            content_state.as_ref(),
            &content_context,
            RowRange { start: 0, end: 2 },
        );
        assert_eq!(decorations.len(), 1);
        assert_eq!(decorations[0].start.char_index, 1);
        assert_eq!(decorations[0].end.char_index, 6);

        // Without the change set there is no safe mapping, so the
        // existing stale-on-read behavior still drops the layer.
        let snapshot = contents.text_snapshot(content_id).unwrap();
        let len = snapshot.len_chars();
        let edit = vell_core::transaction::TextEdit::new(len..len, "x".to_string());
        let change = vell_core::transaction::TextChangeSet::from_edits(len, vec![edit]).unwrap();
        contents.apply(content_id, ContentAction::Text(change));
        let content_context = ModeContentContext::new(content_id, &contents);
        let decorations = mode.content_decorations(
            content_state.as_ref(),
            &content_context,
            RowRange { start: 0, end: 2 },
        );
        assert!(decorations.is_empty());
    }
}
