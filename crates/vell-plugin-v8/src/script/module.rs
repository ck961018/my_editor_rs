use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use deno_ast::{
    EmitOptions, MediaType, ModuleSpecifier, ParseParams, TranspileModuleOptions, TranspileOptions,
    parse_module, parse_program,
};

use super::{
    MAX_MODULE_GRAPH_BYTES, MAX_SCRIPT_SOURCE_BYTES, ScriptError, ensure_file_size, ensure_size,
};

/// Controls how module source bytes are resolved.
/// Stored as an isolate slot: `Filesystem` for the main isolate
/// (reads from disk), `Embedded` for worker isolates (reads from
/// `DEFAULT_PLUGIN_ASSETS`).
///
/// `Copy` + `Clone` so it can be stored in an isolate slot and
/// retrieved by value.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AssetSource {
    Filesystem,
    Embedded,
}

/// Read source bytes for a path, dispatching on the isolate's
/// `AssetSource` slot.  Falls back to `Filesystem` when no slot
/// is set (main isolate behaviour).
fn read_source(scope: &mut v8::PinScope<'_, '_>, path: &Path) -> Result<String, ScriptError> {
    let source_kind = scope
        .get_slot::<AssetSource>()
        .copied()
        .unwrap_or(AssetSource::Filesystem);
    match source_kind {
        AssetSource::Filesystem => {
            ensure_file_size(path, "module source", MAX_SCRIPT_SOURCE_BYTES)?;
            fs::read_to_string(path).map_err(|error| {
                ScriptError::new(format!("failed to read {}: {error}", path.display()))
            })
        }
        AssetSource::Embedded => {
            let key = path_to_asset_key(path);
            let bytes = super::DEFAULT_PLUGIN_ASSETS
                .iter()
                .find_map(|(candidate, bytes)| (*candidate == key).then_some(*bytes))
                .ok_or_else(|| ScriptError::new(format!("embedded module not found: {key}")))?;
            ensure_size("module source", bytes.len(), MAX_SCRIPT_SOURCE_BYTES)?;
            std::str::from_utf8(bytes)
                .map_err(|error| ScriptError::new(format!("invalid UTF-8 in {key}: {error}")))
                .map(|s| s.to_owned())
        }
    }
}

/// Convert a `Path` to an asset key (forward-slash, relative to
/// the plugin root).  For embedded workers the path is already
/// relative (e.g. `test-worker/meta-worker.ts`).
fn path_to_asset_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub(super) fn transpile_typescript(specifier: &str, source: &str) -> Result<String, ScriptError> {
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

    // For embedded assets the path may be relative (e.g.
    // "test-worker/meta-worker.ts").  ModuleSpecifier requires an
    // absolute path, so prefix with a synthetic base for relative
    // paths.
    let specifier = if path.is_absolute() {
        ModuleSpecifier::from_file_path(path)
            .map_err(|_| ScriptError::new(format!("invalid script path: {}", path.display())))?
    } else {
        ModuleSpecifier::parse(&format!("file:///runtime/plugins/{}", path.display()))
            .map_err(|_| ScriptError::new(format!("invalid script path: {}", path.display())))?
    };
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
pub(super) struct ModuleMap {
    root: PathBuf,
    source_bytes: usize,
    by_path: HashMap<PathBuf, v8::Global<v8::Module>>,
    by_id: HashMap<i32, Vec<(PathBuf, v8::Global<v8::Module>)>>,
}

impl ModuleMap {
    pub(super) fn reset(&mut self, root: PathBuf) {
        self.root = root;
        self.source_bytes = 0;
        self.by_path.clear();
        self.by_id.clear();
    }

    fn insert(&mut self, path: PathBuf, module: v8::Global<v8::Module>, id: i32) {
        self.by_path.insert(path.clone(), module.clone());
        self.by_id.entry(id).or_default().push((path, module));
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn reserve_source(&mut self, bytes: usize) -> Result<(), ScriptError> {
        let total = self.source_bytes.saturating_add(bytes);
        ensure_size("module graph", total, MAX_MODULE_GRAPH_BYTES)?;
        self.source_bytes = total;
        Ok(())
    }

    fn path_for(&self, id: i32, module: &v8::Global<v8::Module>) -> Option<&PathBuf> {
        self.by_id
            .get(&id)?
            .iter()
            .find(|(_, candidate)| candidate == module)
            .map(|(path, _)| path)
    }
}

pub(super) fn load_module_tree<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    path: &Path,
    modules: &Rc<RefCell<ModuleMap>>,
) -> Result<v8::Local<'scope, v8::Module>, ScriptError> {
    if let Some(module) = modules.borrow().by_path.get(path).cloned() {
        return Ok(v8::Local::new(scope, module));
    }

    let source = read_source(scope, path)?;
    ensure_size("module source", source.len(), MAX_SCRIPT_SOURCE_BYTES)?;
    modules.borrow_mut().reserve_source(source.len())?;
    let source = transpile_module(path, &source)?;
    ensure_size("transpiled module", source.len(), MAX_SCRIPT_SOURCE_BYTES)?;
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
    // For filesystem paths, canonicalize resolves symlinks and
    // normalizes `..` segments.  For embedded assets (not on disk),
    // skip canonicalize and normalize manually.
    let candidate = match std::fs::canonicalize(&candidate) {
        Ok(canonical) => canonical,
        Err(_) => normalize_path(&candidate),
    };
    if !candidate.starts_with(root) {
        return Err(ScriptError::new(format!(
            "script import escapes the config directory: {specifier}"
        )));
    }
    Ok(candidate)
}

/// Normalize a path without touching the filesystem — for embedded
/// assets that don't exist on disk.
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                components.pop();
            }
            other => components.push(other.as_os_str()),
        }
    }
    components.iter().collect()
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
pub(super) fn resolve_module<'scope>(
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

/// `HostInitializeImportMetaObjectCallback` — injects `import.meta.url`
/// the first time a module accesses `import.meta`.
///
/// Looks up the module's path in the `ModuleMap` slot and sets
/// `meta.url` to a `file:///...` URL.
pub(super) extern "C" fn host_initialize_import_meta(
    context: v8::Local<v8::Context>,
    module: v8::Local<v8::Module>,
    meta: v8::Local<v8::Object>,
) {
    v8::callback_scope!(unsafe scope, context);
    let Some(modules) = scope.get_slot::<Rc<RefCell<ModuleMap>>>().cloned() else {
        return;
    };
    let map = modules.borrow();
    let module_global = v8::Global::new(scope, module);
    let Some(path) = map.path_for(module.get_identity_hash().get(), &module_global) else {
        return;
    };
    // Build a file:// URL from the path.  For embedded assets,
    // the path is relative (e.g. "test-worker/meta-worker.ts") —
    // prepend "runtime/plugins/" to make a valid-looking URL.
    let display = path.display().to_string();
    let url = if path.is_absolute() {
        ModuleSpecifier::from_file_path(path)
            .map(|s| s.to_string())
            .unwrap_or_else(|_| display)
    } else {
        format!("file:///runtime/plugins/{display}")
    };
    let Some(key) = v8::String::new(scope, "url") else {
        return;
    };
    let Some(val) = v8::String::new(scope, &url) else {
        return;
    };
    meta.create_data_property(scope, key.into(), val.into());
}

/// `HostImportModuleDynamicallyCallback` — handles `import(specifier)`.
///
/// Resolves the specifier via the existing `resolve_path` +
/// `load_module_tree`, then resolves the returned promise with the
/// module's namespace object.
pub(super) fn host_import_module_dynamically<'a, 'i, 's>(
    scope: &'a mut v8::PinnedRef<'s, v8::HandleScope<'i>>,
    _host_defined_options: v8::Local<'s, v8::Data>,
    resource_name: v8::Local<'s, v8::Value>,
    specifier: v8::Local<'s, v8::String>,
    _import_attributes: v8::Local<'s, v8::FixedArray>,
) -> Option<v8::Local<'s, v8::Promise>> {
    let specifier = specifier.to_rust_string_lossy(scope);
    let resolver = v8::PromiseResolver::new(scope)?;
    let promise = resolver.get_promise(scope);

    let modules = scope.get_slot::<Rc<RefCell<ModuleMap>>>().cloned()?;
    let root = modules.borrow().root.clone();
    let resource_name = resource_name.to_rust_string_lossy(scope);
    let referrer = if resource_name.is_empty() {
        root.join("<dynamic-import>")
    } else {
        PathBuf::from(resource_name)
    };
    let path = resolve_path(&referrer, &specifier, &root);
    let path = match path {
        Ok(p) => p,
        Err(error) => {
            let msg = v8::String::new(scope, &error.to_string())?;
            let _ = resolver.reject(scope, msg.into());
            scope.perform_microtask_checkpoint();
            return Some(promise);
        }
    };

    // Load, instantiate, and evaluate the module.
    match load_module_tree(scope, &path, &modules) {
        Ok(module) => {
            if module.instantiate_module(scope, resolve_module).is_none() {
                let msg =
                    v8::String::new(scope, "failed to instantiate dynamically imported module")?;
                scope.throw_exception(msg.into());
                return None;
            }
            if module.evaluate(scope).is_none() {
                let msg = v8::String::new(scope, "failed to evaluate dynamically imported module")?;
                scope.throw_exception(msg.into());
                return None;
            }
            let namespace = module.get_module_namespace();
            let _ = resolver.resolve(scope, namespace);
        }
        Err(error) => {
            let msg = v8::String::new(scope, &error.to_string())?;
            let _ = resolver.reject(scope, msg.into());
        }
    }

    // Pump microtasks so the promise resolves synchronously.
    scope.perform_microtask_checkpoint();
    Some(promise)
}

pub(super) fn current_exception(
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
