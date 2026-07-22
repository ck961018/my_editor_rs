use super::*;

pub(super) fn install_editor_api(scope: &mut v8::PinScope<'_, '_>) {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let editor = v8::Object::new(scope);
    let modes = v8::Object::new(scope);
    let define_name = v8::String::new(scope, "define").unwrap();
    let define = v8::FunctionTemplate::new(scope, define_mode)
        .get_function(scope)
        .unwrap();
    modes.set(scope, define_name.into(), define.into());
    let theme = v8::Object::new(scope);
    let use_name = v8::String::new(scope, "use").unwrap();
    let use_theme = v8::FunctionTemplate::new(scope, select_theme)
        .get_function(scope)
        .unwrap();
    theme.set(scope, use_name.into(), use_theme.into());
    let faces = v8::Object::new(scope);
    let override_name = v8::String::new(scope, "override").unwrap();
    let override_face = v8::FunctionTemplate::new(scope, define_face_override)
        .get_function(scope)
        .unwrap();
    faces.set(scope, override_name.into(), override_face.into());
    set_object(scope, editor, "modes", modes);
    set_object(scope, editor, "theme", theme);
    set_object(scope, editor, "faces", faces);
    set_object(scope, global, "editor", editor);
}

fn select_theme(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let value = arguments.get(0);
    if !value.is_string() {
        throw_script_error(scope, "editor.theme.use expects a theme name");
        return;
    }
    let name = value.to_rust_string_lossy(scope);
    if name.is_empty() {
        throw_script_error(scope, "editor.theme.use expects a non-empty theme name");
        return;
    }
    let Some(configuration) = scope
        .get_slot::<Rc<RefCell<ScriptConfigurationDraft>>>()
        .cloned()
    else {
        throw_script_error(scope, "script configuration draft is unavailable");
        return;
    };
    configuration.borrow_mut().theme = Some(ThemeName::new(name));
    return_value.set_undefined();
}

fn define_face_override(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let name = arguments.get(0);
    if !name.is_string() {
        throw_script_error(scope, "editor.faces.override expects a face name");
        return;
    }
    let name = name.to_rust_string_lossy(scope);
    if name.is_empty() {
        throw_script_error(scope, "editor.faces.override expects a non-empty face name");
        return;
    }
    let patch = match v8::Local::<v8::Object>::try_from(arguments.get(1)) {
        Ok(object) => match parse_face_patch(scope, object) {
            Ok(patch) => patch,
            Err(error) => {
                throw_script_error(scope, &error.to_string());
                return;
            }
        },
        Err(_) => {
            throw_script_error(scope, "editor.faces.override expects a face object");
            return;
        }
    };
    let theme = match v8::Local::<v8::Object>::try_from(arguments.get(2)) {
        Ok(options) => match optional_string(scope, options, "theme") {
            Ok(theme) => theme.map(ThemeName::new),
            Err(error) => {
                throw_script_error(scope, &error.to_string());
                return;
            }
        },
        Err(_) if arguments.get(2).is_null_or_undefined() => None,
        Err(_) => {
            throw_script_error(scope, "face override options must be an object");
            return;
        }
    };
    let Some(configuration) = scope
        .get_slot::<Rc<RefCell<ScriptConfigurationDraft>>>()
        .cloned()
    else {
        throw_script_error(scope, "script configuration draft is unavailable");
        return;
    };
    configuration
        .borrow_mut()
        .face_overrides
        .push(FaceOverride {
            face: FaceName::new(name),
            theme,
            patch,
        });
    return_value.set_undefined();
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
            if definition.version == ScriptApiVersion::V1 {
                let Some(diagnostics) = scope.get_slot::<Rc<RefCell<ScriptDiagnostics>>>().cloned()
                else {
                    throw_script_error(scope, "script diagnostic registry is unavailable");
                    return;
                };
                let mut diagnostics = diagnostics.borrow_mut();
                if !diagnostics.v1_deprecation_reported {
                    diagnostics.v1_deprecation_reported = true;
                    diagnostics
                        .messages
                        .push(ScriptDiagnostic::v1_deprecation());
                }
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
    let before = optional_string(scope, object, "before")?.map(ModeName::new);
    let face_definitions = parse_face_definitions(scope, object)?;
    let (version, adapters) = match property(scope, object, "on") {
        Some(value) if !value.is_null_or_undefined() => (
            ScriptApiVersion::V2,
            parse_v2_adapters(scope, object, value)?,
        ),
        _ => (
            ScriptApiVersion::V1,
            ScriptAdapterDefinitions {
                buffer: Some(parse_v1_adapter(scope, object)?),
                status_bar: None,
            },
        ),
    };
    Ok(ScriptModeDefinition {
        name: ModeName::new(name),
        version,
        face_definitions,
        before,
        adapters,
    })
}

fn parse_v1_adapter(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
) -> Result<ScriptAdapterDefinition, ScriptError> {
    let actions = parse_actions(scope, object, "actions", true)?;
    let bindings = parse_bindings(scope, object, &actions)?;
    let input_action = parse_input_action(scope, object, &actions)?;
    let create_content = optional_factory(scope, object, "content")?;
    let content_changed = optional_section_callback(scope, object, "content", "changed")?;
    let content_job = optional_section_callback(scope, object, "content", "job")?;
    let content_apply_job = optional_section_callback(scope, object, "content", "applyJob")?;
    let create_view = optional_factory(scope, object, "view")?;
    let worker = parse_worker(scope, object)?;
    if content_job.is_some() != worker.is_some() || content_apply_job.is_some() != worker.is_some()
    {
        return Err(ScriptError::new(
            "mode worker, content.job, and content.applyJob must be defined together",
        ));
    }
    Ok(ScriptAdapterDefinition {
        actions,
        bindings,
        input_action,
        input: None,
        create_content,
        content_changed,
        content_job,
        content_apply_job,
        create_view,
        worker,
        analyses: Vec::new(),
    })
}

fn parse_v2_adapters(
    scope: &mut v8::PinScope,
    definition: v8::Local<v8::Object>,
    value: v8::Local<v8::Value>,
) -> Result<ScriptAdapterDefinitions, ScriptError> {
    for legacy in ["content", "view", "actions", "keys", "input", "worker"] {
        if property(scope, definition, legacy).is_some_and(|value| !value.is_null_or_undefined()) {
            return Err(ScriptError::new(format!(
                "v2 mode definition cannot combine 'on' with legacy '{legacy}'"
            )));
        }
    }
    let object = v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| ScriptError::new("mode on must be an object"))?;
    let keys = object
        .get_own_property_names(scope, Default::default())
        .ok_or_else(|| ScriptError::new("failed to enumerate mode adapters"))?;
    let mut adapters = ScriptAdapterDefinitions::default();
    for index in 0..keys.length() {
        let key = keys
            .get_index(scope, index)
            .ok_or_else(|| ScriptError::new("failed to read adapter name"))?;
        let name = key.to_rust_string_lossy(scope);
        let value = object
            .get(scope, key)
            .ok_or_else(|| ScriptError::new(format!("mode adapter '{name}' is missing")))?;
        let adapter = v8::Local::<v8::Object>::try_from(value)
            .map_err(|_| ScriptError::new(format!("mode adapter '{name}' must be an object")))?;
        match name.as_str() {
            "buffer" => {
                adapters.buffer = Some(parse_v2_adapter(scope, adapter, ContentKind::Buffer)?)
            }
            "statusBar" => {
                adapters.status_bar =
                    Some(parse_v2_adapter(scope, adapter, ContentKind::StatusBar)?)
            }
            _ => return Err(ScriptError::new(format!("unknown mode adapter '{name}'"))),
        }
    }
    if adapters.buffer.is_none() && adapters.status_bar.is_none() {
        return Err(ScriptError::new(
            "v2 mode definition must provide on.buffer or on.statusBar",
        ));
    }
    Ok(adapters)
}

fn parse_v2_adapter(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    kind: ContentKind,
) -> Result<ScriptAdapterDefinition, ScriptError> {
    let actions = parse_actions(scope, object, "commands", false)?;
    if actions
        .iter()
        .any(|action| action.name.as_str() == V2_INPUT_ACTION)
    {
        return Err(ScriptError::new(format!(
            "mode command '{V2_INPUT_ACTION}' is reserved for raw input"
        )));
    }
    let input = optional_function(scope, object, "input")?;
    let (content_changed, analyses) = match kind {
        ContentKind::Buffer => {
            for field in ["worker", "job", "applyJob"] {
                if property(scope, object, field).is_some_and(|value| !value.is_null_or_undefined())
                {
                    return Err(ScriptError::new(format!(
                        "mode buffer.{field} moved to named analysis"
                    )));
                }
            }
            (
                optional_function(scope, object, "changed")?,
                parse_analyses(scope, object)?,
            )
        }
        ContentKind::StatusBar => {
            for field in ["changed", "worker", "job", "applyJob", "analysis"] {
                if property(scope, object, field).is_some_and(|value| !value.is_null_or_undefined())
                {
                    return Err(ScriptError::new(format!(
                        "mode statusBar.{field} is not supported"
                    )));
                }
            }
            (None, Vec::new())
        }
    };
    Ok(ScriptAdapterDefinition {
        bindings: parse_bindings(scope, object, &actions)?,
        input_action: None,
        input,
        actions,
        create_content: optional_function(scope, object, "state")?,
        content_changed,
        content_job: None,
        content_apply_job: None,
        create_view: optional_function(scope, object, "viewState")?,
        worker: None,
        analyses,
    })
}

fn parse_analyses(
    scope: &mut v8::PinScope,
    adapter: v8::Local<v8::Object>,
) -> Result<Vec<ScriptAnalysisDefinition>, ScriptError> {
    let Some(value) = property(scope, adapter, "analysis") else {
        return Ok(Vec::new());
    };
    if value.is_null_or_undefined() {
        return Ok(Vec::new());
    }
    let object = v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| ScriptError::new("mode buffer.analysis must be an object"))?;
    let keys = object
        .get_own_property_names(scope, Default::default())
        .ok_or_else(|| ScriptError::new("failed to enumerate mode analyses"))?;
    let mut analyses = Vec::new();
    for index in 0..keys.length() {
        let key = keys
            .get_index(scope, index)
            .ok_or_else(|| ScriptError::new("failed to read analysis name"))?;
        let name = key.to_rust_string_lossy(scope);
        if name.is_empty() {
            return Err(ScriptError::new("mode analysis name must not be empty"));
        }
        let value = object
            .get(scope, key)
            .ok_or_else(|| ScriptError::new(format!("failed to read analysis '{name}'")))?;
        let definition = v8::Local::<v8::Object>::try_from(value)
            .map_err(|_| ScriptError::new(format!("mode analysis '{name}' must be an object")))?;
        let fields = definition
            .get_own_property_names(scope, Default::default())
            .ok_or_else(|| ScriptError::new(format!("failed to enumerate analysis '{name}'")))?;
        for field_index in 0..fields.length() {
            let field = fields
                .get_index(scope, field_index)
                .ok_or_else(|| ScriptError::new("failed to read analysis field"))?
                .to_rust_string_lossy(scope);
            if !matches!(field.as_str(), "worker" | "snapshot" | "input" | "apply") {
                return Err(ScriptError::new(format!(
                    "mode analysis '{name}' has unknown field '{field}'"
                )));
            }
        }
        let snapshot_text = match optional_string(scope, definition, "snapshot")? {
            None => false,
            Some(snapshot) if snapshot == "text" => true,
            Some(snapshot) => {
                return Err(ScriptError::new(format!(
                    "mode analysis '{name}' has unknown snapshot '{snapshot}'"
                )));
            }
        };
        let worker = parse_worker(scope, definition)?.ok_or_else(|| {
            ScriptError::new(format!("mode analysis '{name}'.worker is required"))
        })?;
        let input = optional_function(scope, definition, "input")?
            .ok_or_else(|| ScriptError::new(format!("mode analysis '{name}'.input is required")))?;
        let apply = optional_function(scope, definition, "apply")?
            .ok_or_else(|| ScriptError::new(format!("mode analysis '{name}'.apply is required")))?;
        analyses.push(ScriptAnalysisDefinition {
            slot: format!("analysis:{name}"),
            input,
            apply,
            worker,
            snapshot_text,
        });
    }
    Ok(analyses)
}

fn parse_worker(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
) -> Result<Option<ScriptWorker>, ScriptError> {
    optional_string(scope, object, "worker")?
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
        .transpose()
}

fn parse_actions(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    property_name: &str,
    required: bool,
) -> Result<Vec<ScriptActionDefinition>, ScriptError> {
    let actions_object = match property(scope, object, property_name) {
        Some(value) if !value.is_null_or_undefined() => v8::Local::<v8::Object>::try_from(value)
            .map_err(|_| ScriptError::new(format!("mode {property_name} must be an object")))?,
        _ if required => {
            return Err(ScriptError::new(format!(
                "mode {property_name} must be an object"
            )));
        }
        _ => return Ok(Vec::new()),
    };
    let action_keys = actions_object
        .get_own_property_names(scope, Default::default())
        .ok_or_else(|| ScriptError::new(format!("failed to enumerate mode {property_name}")))?;
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
                ScriptError::new(format!("mode command '{action_name}' must be a function"))
            })?;
        actions.push(ScriptActionDefinition {
            name: ModeActionName::new(action_name),
            callback: v8::Global::new(scope, callback),
        });
    }
    Ok(actions)
}

fn parse_bindings(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    actions: &[ScriptActionDefinition],
) -> Result<Vec<(KeyEvent, usize)>, ScriptError> {
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
                    ScriptError::new(format!("unknown command '{action_name}' in key bindings"))
                })?;
            bindings.push((parse_key(&key_name)?, action_index));
        }
    }
    Ok(bindings)
}

fn parse_input_action(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    actions: &[ScriptActionDefinition],
) -> Result<Option<usize>, ScriptError> {
    optional_string(scope, object, "input")?
        .map(|name| {
            actions
                .iter()
                .position(|action| action.name.as_str() == name)
                .ok_or_else(|| ScriptError::new(format!("unknown input command '{name}'")))
        })
        .transpose()
}

fn optional_function(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    name: &str,
) -> Result<Option<v8::Global<v8::Function>>, ScriptError> {
    let Some(value) = property(scope, object, name) else {
        return Ok(None);
    };
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    let callback = v8::Local::<v8::Function>::try_from(value)
        .map_err(|_| ScriptError::new(format!("mode {name} must be a function")))?;
    Ok(Some(v8::Global::new(scope, callback)))
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

fn parse_face_definitions(
    scope: &mut v8::PinScope,
    definition: v8::Local<v8::Object>,
) -> Result<Vec<FaceDefinition>, ScriptError> {
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
        let name = FaceName::new(name.to_rust_string_lossy(scope));
        let extended = property(scope, face, "inherits")
            .is_some_and(|value| !value.is_null_or_undefined())
            || property(scope, face, "fallback").is_some_and(|value| !value.is_null_or_undefined());
        let (inherits, fallback) = if extended {
            let inherits = parse_face_inherits(scope, face)?;
            let fallback = match property(scope, face, "fallback") {
                Some(value) if !value.is_null_or_undefined() => {
                    let fallback = v8::Local::<v8::Object>::try_from(value)
                        .map_err(|_| ScriptError::new("face fallback must be an object"))?;
                    parse_face_patch(scope, fallback)?
                }
                _ => FacePatch::default(),
            };
            (inherits, fallback)
        } else {
            (
                Vec::new(),
                FacePatch::from(&parse_legacy_face(scope, face)?),
            )
        };
        parsed.push(FaceDefinition {
            name,
            inherits,
            fallback,
        });
    }
    Ok(parsed)
}

fn parse_legacy_face(
    scope: &mut v8::PinScope,
    face: v8::Local<v8::Object>,
) -> Result<Face, ScriptError> {
    Ok(Face {
        foreground: parse_color(scope, face, "foreground")?,
        background: parse_color(scope, face, "background")?,
        bold: optional_bool(scope, face, "bold")?,
        dim: optional_bool(scope, face, "dim")?,
        italic: optional_bool(scope, face, "italic")?,
        underline: optional_bool(scope, face, "underline")?,
        underline_style: optional_underline_style(scope, face)?,
        strikethrough: optional_bool(scope, face, "strikethrough")?,
    })
}

fn parse_face_inherits(
    scope: &mut v8::PinScope,
    face: v8::Local<v8::Object>,
) -> Result<Vec<FaceName>, ScriptError> {
    let Some(value) = property(scope, face, "inherits") else {
        return Ok(Vec::new());
    };
    if value.is_null_or_undefined() {
        return Ok(Vec::new());
    }
    let values = v8::Local::<v8::Array>::try_from(value)
        .map_err(|_| ScriptError::new("face inherits must be an array"))?;
    let mut inherits = Vec::with_capacity(values.length() as usize);
    for index in 0..values.length() {
        let value = values
            .get_index(scope, index)
            .ok_or_else(|| ScriptError::new("failed to read inherited face"))?;
        if !value.is_string() {
            return Err(ScriptError::new("inherited face names must be strings"));
        }
        let name = value.to_rust_string_lossy(scope);
        if name.is_empty() {
            return Err(ScriptError::new("inherited face name must not be empty"));
        }
        inherits.push(FaceName::new(name));
    }
    Ok(inherits)
}

pub(super) fn parse_face_patch(
    scope: &mut v8::PinScope,
    face: v8::Local<v8::Object>,
) -> Result<FacePatch, ScriptError> {
    Ok(FacePatch {
        foreground: parse_patch_color(scope, face, "foreground")?,
        background: parse_patch_color(scope, face, "background")?,
        bold: parse_patch_bool(scope, face, "bold")?,
        dim: parse_patch_bool(scope, face, "dim")?,
        italic: parse_patch_bool(scope, face, "italic")?,
        underline: parse_patch_bool(scope, face, "underline")?,
        underline_style: parse_patch_underline_style(scope, face)?,
        strikethrough: parse_patch_bool(scope, face, "strikethrough")?,
    })
}

fn optional_underline_style(
    scope: &mut v8::PinScope,
    face: v8::Local<v8::Object>,
) -> Result<Option<UnderlineStyle>, ScriptError> {
    let Some(value) = property(scope, face, "underlineStyle") else {
        return Ok(None);
    };
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    parse_underline_style(scope, value).map(Some)
}

fn parse_patch_underline_style(
    scope: &mut v8::PinScope,
    face: v8::Local<v8::Object>,
) -> Result<FaceValue<UnderlineStyle>, ScriptError> {
    let Some(value) = property(scope, face, "underlineStyle") else {
        return Ok(FaceValue::Unspecified);
    };
    if value.is_null_or_undefined() {
        return Ok(FaceValue::Unspecified);
    }
    if is_reset_value(scope, value)? {
        return Ok(FaceValue::Reset);
    }
    parse_underline_style(scope, value).map(FaceValue::Value)
}

fn parse_underline_style(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
) -> Result<UnderlineStyle, ScriptError> {
    if !value.is_string() {
        return Err(ScriptError::new("face underlineStyle must be a string"));
    }
    match value.to_rust_string_lossy(scope).as_str() {
        "line" => Ok(UnderlineStyle::Line),
        "double" => Ok(UnderlineStyle::Double),
        "curl" => Ok(UnderlineStyle::Curl),
        "dotted" => Ok(UnderlineStyle::Dotted),
        "dashed" => Ok(UnderlineStyle::Dashed),
        _ => Err(ScriptError::new(
            "face underlineStyle must be line, double, curl, dotted, or dashed",
        )),
    }
}

fn parse_patch_color(
    scope: &mut v8::PinScope,
    face: v8::Local<v8::Object>,
    name: &str,
) -> Result<FaceValue<Color>, ScriptError> {
    let Some(value) = property(scope, face, name) else {
        return Ok(FaceValue::Unspecified);
    };
    if value.is_null_or_undefined() {
        return Ok(FaceValue::Unspecified);
    }
    if is_reset_value(scope, value)? {
        return Ok(FaceValue::Reset);
    }
    Ok(match parse_color(scope, face, name)? {
        Some(color) => FaceValue::Value(color),
        None => FaceValue::Unspecified,
    })
}

fn parse_patch_bool(
    scope: &mut v8::PinScope,
    face: v8::Local<v8::Object>,
    name: &str,
) -> Result<FaceValue<bool>, ScriptError> {
    let Some(value) = property(scope, face, name) else {
        return Ok(FaceValue::Unspecified);
    };
    if value.is_null_or_undefined() {
        return Ok(FaceValue::Unspecified);
    }
    if is_reset_value(scope, value)? {
        return Ok(FaceValue::Reset);
    }
    if !value.is_boolean() {
        return Err(ScriptError::new(format!(
            "face {name} must be a boolean or reset"
        )));
    }
    Ok(FaceValue::Value(value.boolean_value(scope)))
}

fn is_reset_value(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
) -> Result<bool, ScriptError> {
    let Ok(object) = v8::Local::<v8::Object>::try_from(value) else {
        return Ok(false);
    };
    let Some(reset) = property(scope, object, "reset") else {
        return Err(ScriptError::new("face reset must be { reset: true }"));
    };
    if !reset.is_boolean() || !reset.boolean_value(scope) {
        return Err(ScriptError::new("face reset must be { reset: true }"));
    }
    Ok(true)
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
    if value.is_number() {
        let ansi = value
            .number_value(scope)
            .filter(|value| value.is_finite() && value.fract() == 0.0)
            .filter(|value| (0.0..=u8::MAX as f64).contains(value))
            .map(|value| value as u8);
        return ansi.map(Color::Ansi).map(Some).ok_or_else(|| {
            ScriptError::new(format!(
                "face {name} ANSI index must be an integer from 0 to 255"
            ))
        });
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
