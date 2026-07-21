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

    ensure_file_size(path, "module source", MAX_SCRIPT_SOURCE_BYTES)?;
    let source = fs::read_to_string(path)
        .map_err(|error| ScriptError::new(format!("failed to read {}: {error}", path.display())))?;
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
