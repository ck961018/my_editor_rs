use super::*;

/// The single long-lived V8 isolate used by script modes.
#[allow(dead_code)]
pub struct ScriptHost {
    isolate: v8::OwnedIsolate,
    heap_limit: Box<HeapLimitState>,
    heap_limit_bytes: usize,
    budget: ScriptExecutionBudget,
    pub(super) context: v8::Global<v8::Context>,
    pub(super) modules: Rc<RefCell<ModuleMap>>,
    pub(super) definitions: Rc<RefCell<Vec<ScriptModeDefinition>>>,
    pub(super) diagnostics: Rc<RefCell<ScriptDiagnostics>>,
    pub(super) configuration: Rc<RefCell<ScriptConfigurationDraft>>,
    pub(super) commands: Rc<RefCell<ScriptCommands>>,
    pub(super) command_host: Rc<ActiveCommandHost>,
    pub(super) next_interactive_script: u64,
    pub(super) type_environment: TypeEnvironment,
    plugin_root: Rc<RefCell<Option<String>>>,
    primitives: Rc<RefCell<PrimitiveRuntime>>,
    pub(super) worker_decorations: Rc<RefCell<WorkerDecorationBuffer>>,
}

impl Default for ScriptHost {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl ScriptHost {
    pub fn new() -> Self {
        Self::with_budget_and_heap(ScriptExecutionBudget::default(), SCRIPT_HEAP_LIMIT_BYTES)
    }

    pub(super) fn with_budget_and_heap(
        budget: ScriptExecutionBudget,
        heap_limit_bytes: usize,
    ) -> Self {
        initialize_v8();

        let params = v8::CreateParams::default().heap_limits(0, heap_limit_bytes);
        let mut isolate = v8::Isolate::new(params);
        isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
        isolate.set_capture_stack_trace_for_uncaught_exceptions(true, 10);
        // Wire import.meta.url and dynamic import() support.
        isolate.set_host_initialize_import_meta_object_callback(host_initialize_import_meta);
        isolate.set_host_import_module_dynamically_callback(host_import_module_dynamically);
        let modules = Rc::new(RefCell::new(ModuleMap::default()));
        let definitions = Rc::new(RefCell::new(Vec::new()));
        let diagnostics = Rc::new(RefCell::new(ScriptDiagnostics::default()));
        let configuration = Rc::new(RefCell::new(ScriptConfigurationDraft::default()));
        let commands = Rc::new(RefCell::new(ScriptCommands::default()));
        let command_host = Rc::new(ActiveCommandHost::default());
        let plugin_root = Rc::new(RefCell::new(None));
        let primitives = PrimitiveRuntime::new();
        isolate.set_slot(modules.clone());
        isolate.set_slot(definitions.clone());
        isolate.set_slot(diagnostics.clone());
        isolate.set_slot(configuration.clone());
        isolate.set_slot(commands.clone());
        isolate.set_slot(command_host.clone());
        isolate.set_slot(plugin_root.clone());
        isolate.set_slot(primitives.clone());
        // Worker quota: per-plugin 8, global 32, depth 4.
        let worker_quota = Some(Arc::new(worker::WorkerQuota::new(8, 32, 4)));
        isolate.set_slot(worker_quota);

        let context = {
            v8::scope!(scope, &mut isolate);
            let context = v8::Context::new(scope, Default::default());
            v8::Global::new(scope, context)
        };
        {
            v8::scope_with_context!(scope, &mut isolate, context.clone());
            install_editor_api(scope);
            worker::install_global_worker_constructor(scope);
            worker::install_abort_controller_global(scope);
            worker::install_url_global(scope);
        }
        let worker_registry: worker::WorkerRegistrySlot = Rc::new(RefCell::new(Vec::new()));
        isolate.set_slot(worker_registry);
        let worker_decorations = Rc::new(RefCell::new(WorkerDecorationBuffer::default()));
        isolate.set_slot(worker_decorations.clone());
        let heap_limit = install_heap_limit(&mut isolate);

        Self {
            isolate,
            heap_limit,
            heap_limit_bytes,
            budget,
            context,
            modules,
            definitions,
            diagnostics,
            configuration,
            commands,
            command_host,
            next_interactive_script: 0,
            type_environment: TypeEnvironment::default(),
            plugin_root,
            primitives,
            worker_decorations,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn with_timeouts(callback_timeout: Duration, startup_timeout: Duration) -> Self {
        Self::with_budget_and_heap(
            ScriptExecutionBudget {
                callback_timeout,
                startup_timeout,
            },
            SCRIPT_HEAP_LIMIT_BYTES,
        )
    }

    pub(super) fn invoke<T>(
        &mut self,
        kind: ScriptInvocationKind,
        callback: impl FnOnce(&mut v8::OwnedIsolate) -> Result<T, ScriptError>,
    ) -> Result<T, ScriptError> {
        let watchdog = InvocationWatchdog::start(
            self.isolate.thread_safe_handle(),
            kind,
            kind.timeout(self.budget),
        )?;
        let result = callback(&mut self.isolate);
        let result = watchdog.finish(result);
        if recover_heap_limit(
            &mut self.isolate,
            &mut self.heap_limit,
            self.heap_limit_bytes,
        ) {
            return Err(ScriptError::new("script heap limit exceeded"));
        }
        result
    }

    pub(super) fn publish_command_types(
        &mut self,
        registrations: &[commands::ScriptCommandRegistration],
    ) {
        self.type_environment.publish(registrations);
    }

    pub(super) fn publish_command_types_since(&mut self, change_count: usize) {
        let registrations = self.commands.borrow().changes_since(change_count);
        self.publish_command_types(&registrations);
    }

    pub(super) fn update_type_source(&mut self, identity: impl Into<String>, source: &str) {
        self.type_environment
            .update_source(identity, source.to_owned());
    }

    pub(super) fn sync_module_type_sources(&mut self) {
        let sources = self.modules.borrow().source_snapshots();
        for (identity, source) in sources {
            self.type_environment.update_source(identity, source);
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn type_diagnostics(
        &mut self,
        identity: impl Into<String>,
        source: impl Into<String>,
    ) -> Result<Vec<String>, ScriptError> {
        self.type_environment.diagnostics(identity, source)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn generated_command_declarations(&self) -> &str {
        self.type_environment.generated_declarations()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn typescript_compiler_version(&mut self) -> Result<String, ScriptError> {
        self.type_environment.compiler_version()
    }

    /// Drain all pending worker→main messages and dispatch them to
    /// registered JS event listeners on each Worker object.
    #[allow(dead_code)]
    pub fn pump_worker_messages(&mut self) -> bool {
        let context = self.context.clone();
        self.invoke(ScriptInvocationKind::Action, |isolate| {
            v8::scope_with_context!(scope, isolate, context);
            let Some(registry) = scope.get_slot::<worker::WorkerRegistrySlot>().cloned() else {
                return Ok(false);
            };
            let messages: Vec<(usize, worker::WorkerChannelMessage)> = {
                let registry = registry.borrow();
                let mut all = Vec::new();
                for (index, handle) in registry.iter().enumerate() {
                    if let Some(handle) = handle {
                        for message in handle.drain() {
                            all.push((index, message));
                        }
                    }
                }
                all
            };
            let changed = !messages.is_empty();
            let mut terminated = Vec::new();
            for (index, message) in messages {
                match message {
                    worker::WorkerChannelMessage::FromWorker(data) => {
                        worker::dispatch_message_event(scope, &registry, index, data)?;
                    }
                    worker::WorkerChannelMessage::Error { message, name } => {
                        worker::dispatch_error_event(scope, &registry, index, message, name)?;
                    }
                    worker::WorkerChannelMessage::Terminated => terminated.push(index),
                    worker::WorkerChannelMessage::ToWorker(_) => {}
                }
            }
            let mut registry = registry.borrow_mut();
            for index in terminated {
                if let Some(handle) = registry.get_mut(index) {
                    handle.take();
                }
                let worker_key = v8::String::new(scope, &format!("_worker_{index}"))
                    .expect("worker registry key");
                scope
                    .get_current_context()
                    .global(scope)
                    .delete(scope, worker_key.into());
            }
            Ok(changed)
        })
        .unwrap_or(false)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn evaluate_script(&mut self, source: &str) -> Result<serde_json::Value, ScriptError> {
        let context = self.context.clone();
        let source_owned = source.to_owned();
        self.invoke(ScriptInvocationKind::Action, |isolate| {
            v8::scope_with_context!(scope, isolate, context);
            v8::tc_scope!(let scope, scope);
            let javascript = transpile_typescript("file:///runtime/test-inline.ts", &source_owned)?;
            let source = v8::String::new(scope, &javascript)
                .ok_or_else(|| ScriptError::new("source too large for V8"))?;
            let script = v8::Script::compile(scope, source, None)
                .ok_or_else(|| current_exception(scope, "test-inline", "compile"))?;
            let value = script
                .run(scope)
                .ok_or_else(|| current_exception(scope, "test-inline", "execute"))?;
            v8_to_json(scope, value, "test-inline")
        })
    }

    pub fn execute_typescript(&mut self, specifier: &str, source: &str) -> Result<(), ScriptError> {
        let command_count = self.commands.borrow().change_count();
        let definition_count = self.definitions.borrow().len();
        let diagnostics = self.diagnostics.borrow().clone();
        let configuration = self.configuration.borrow().clone();
        let result = self.evaluate_typescript(specifier, source).map(|_| ());
        if result.is_err() {
            self.definitions.borrow_mut().truncate(definition_count);
            self.diagnostics.replace(diagnostics);
            self.configuration.replace(configuration);
        }
        self.publish_command_types_since(command_count);
        result
    }

    pub fn execute_embedded_plugin(&mut self, path: &str, source: &str) -> Result<(), ScriptError> {
        let root = path
            .rsplit_once('/')
            .map(|(root, _)| format!("{root}/"))
            .unwrap_or_default();
        self.plugin_root.replace(Some(root));
        let result = self.execute_typescript(&format!("file:///runtime/plugins/{path}"), source);
        self.plugin_root.replace(None);
        result
    }

    pub(super) fn execute_embedded_module(&mut self, path: &str) -> Result<(), ScriptError> {
        let command_count = self.commands.borrow().change_count();
        let definition_count = self.definitions.borrow().len();
        let diagnostics = self.diagnostics.borrow().clone();
        let configuration = self.configuration.borrow().clone();
        let root = path
            .rsplit_once('/')
            .map(|(root, _)| format!("{root}/"))
            .unwrap_or_default();
        self.modules
            .borrow_mut()
            .reset(PathBuf::from(root.trim_end_matches('/')));
        self.plugin_root.replace(Some(root));

        let context = self.context.clone();
        let modules = self.modules.clone();
        let path = path.to_owned();
        let result = self.invoke(ScriptInvocationKind::ModuleEvaluation, |isolate| {
            isolate.set_slot(AssetSource::Embedded);
            v8::scope_with_context!(scope, isolate, context);
            v8::tc_scope!(let scope, scope);
            let module = load_module_tree(scope, Path::new(&path), &modules)?;
            match module.instantiate_module(scope, resolve_module) {
                Some(true) => {}
                _ => return Err(current_exception(scope, &path, "link")),
            }
            if module.evaluate(scope).is_none() {
                return Err(current_exception(scope, &path, "execute"));
            }
            perform_microtask_checkpoint(scope);
            if module.get_status() == v8::ModuleStatus::Errored {
                return Err(ScriptError::new(format!(
                    "failed to execute {path}: {}",
                    module.get_exception().to_rust_string_lossy(scope)
                )));
            }
            Ok(())
        });

        self.plugin_root.replace(None);
        self.sync_module_type_sources();
        if result.is_err() {
            self.definitions.borrow_mut().truncate(definition_count);
            self.diagnostics.replace(diagnostics);
            self.configuration.replace(configuration);
        }
        self.publish_command_types_since(command_count);
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
        self.modules.borrow_mut().reset(root.clone());
        let command_count = self.commands.borrow().change_count();
        let definition_count = self.definitions.borrow().len();
        let diagnostics = self.diagnostics.borrow().clone();
        let configuration = self.configuration.borrow().clone();

        let modules = self.modules.clone();
        let context = self.context.clone();
        let result = self.invoke(ScriptInvocationKind::ModuleEvaluation, |isolate| {
            isolate.set_slot(AssetSource::Filesystem);
            v8::scope_with_context!(scope, isolate, context);
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
            perform_microtask_checkpoint(scope);
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
        });
        self.sync_module_type_sources();
        self.publish_command_types_since(command_count);
        if result.is_err() {
            self.definitions.borrow_mut().truncate(definition_count);
            self.diagnostics.replace(diagnostics);
            self.configuration.replace(configuration);
            self.modules.borrow_mut().reset(root);
        }
        result
    }

    pub(super) fn script_modes(host: &Rc<RefCell<Self>>) -> Vec<ScriptMode> {
        let definitions = host.borrow().definitions.borrow().clone();
        let decoration_owner = definitions
            .iter()
            .position(|definition| definition.adapters.buffer.is_some());
        definitions
            .into_iter()
            .enumerate()
            .map(|(index, definition)| {
                ScriptMode::new(host.clone(), definition, decoration_owner == Some(index))
            })
            .collect()
    }

    #[cfg(feature = "test-support")]
    pub fn modes(host: &Rc<RefCell<Self>>) -> Vec<Box<dyn Mode>> {
        Self::script_modes(host)
            .into_iter()
            .map(|mode| Box::new(mode) as Box<dyn Mode>)
            .collect()
    }

    pub(crate) fn take_diagnostics(&mut self) -> Vec<ScriptDiagnostic> {
        std::mem::take(&mut self.diagnostics.borrow_mut().messages)
    }

    pub(super) fn execute_action(
        &mut self,
        callback: &v8::Global<v8::Function>,
        version: ScriptApiVersion,
        context: &ModeViewContext<'_>,
        arguments: &ModeValue,
        content_state: &mut ScriptModeState,
        view_state: &mut ScriptModeState,
    ) -> Result<ModeResult, ScriptError> {
        let callback = callback.clone();
        let v8_context = self.context.clone();
        let primitives = self.primitives.clone();
        let current_content = content_state.data.clone();
        let current_view = view_state.data.clone();
        if let Some(revision) = context.content_revision() {
            self.worker_decorations.borrow_mut().track_current(
                context.content_id(),
                revision.0,
                context.buffer().and_then(|context| context.text_snapshot()),
            );
        }
        let (result, next_content, next_view, content_decorations, view_decorations) = self
            .invoke(ScriptInvocationKind::Action, |isolate| {
                v8::scope_with_context!(scope, isolate, v8_context);
                v8::tc_scope!(let scope, scope);

                let argument = v8::Object::new(scope);
                set_number(scope, argument, "contentId", context.content_id().0 as f64);
                set_number(scope, argument, "viewId", context.view_id().0 as f64);
                if let Some(revision) = context.content_revision() {
                    set_number(scope, argument, "revision", revision.0 as f64);
                }
                if version == ScriptApiVersion::V2 {
                    if let Some(buffer) = context.buffer() {
                        set_buffer_editing_facts(scope, argument, buffer)?;
                        set_resource_facts(
                            scope,
                            argument,
                            buffer.resource_name(),
                            buffer.resource_path(),
                            buffer.backing_state(),
                            buffer.dirty_state(),
                            buffer.text_metrics(),
                        );
                        set_save_state(scope, argument, buffer.save_state());
                    } else if let Some(status) = context.status_bar() {
                        set_number(
                            scope,
                            argument,
                            "targetViewId",
                            status.target_view_id().0 as f64,
                        );
                        set_number(
                            scope,
                            argument,
                            "targetContentId",
                            status.target_content_id().0 as f64,
                        );
                        set_resource_facts(
                            scope,
                            argument,
                            status.resource_name(),
                            status.resource_path(),
                            status.backing_state(),
                            status.dirty_state(),
                            status.text_metrics(),
                        );
                        set_save_state(scope, argument, status.save_state());
                    }
                }
                let arguments = json_to_v8(scope, &mode_value_to_json(arguments))?;
                set_value(scope, argument, "arguments", arguments);
                let content_value = json_to_v8(scope, &current_content)?;
                let view_value = json_to_v8(scope, &current_view)?;
                let content_state_name = version.content_state_name();
                set_value(scope, argument, content_state_name, content_value);
                set_value(scope, argument, "viewState", view_value);
                let primitive_id = primitives.borrow_mut().begin(context)?;
                let pass = match version {
                    ScriptApiVersion::V1 => {
                        primitives::install_v1(scope, argument, primitive_id);
                        None
                    }
                    ScriptApiVersion::V2 => Some(primitives::install_v2(
                        scope,
                        argument,
                        primitive_id,
                        context.content_kind(),
                    )),
                };
                let callback = v8::Local::new(scope, callback);
                let receiver = v8::undefined(scope).into();
                let callback_result =
                    call_script_callback(scope, callback, receiver, &[argument.into()]);
                let operations = primitives.borrow_mut().finish(primitive_id)?;
                ensure_count("operations", operations.len(), MAX_SCRIPT_OPERATIONS)?;
                let value = callback_result
                    .ok_or_else(|| current_exception(scope, "script mode action", "execute"))?;
                let content_decorations = parse_decorations_property(
                    scope,
                    value,
                    "contentDecorations",
                    context.buffer().and_then(|context| context.text_snapshot()),
                    context.content_revision(),
                )?;
                let view_decorations = parse_decorations_property(
                    scope,
                    value,
                    "viewDecorations",
                    context.buffer().and_then(|context| context.text_snapshot()),
                    context.content_revision(),
                )?;
                ensure_count(
                    "decorations",
                    content_decorations.as_ref().map_or(0, Vec::len)
                        + view_decorations.as_ref().map_or(0, Vec::len),
                    MAX_SCRIPT_DECORATIONS,
                )?;
                let result = match version {
                    ScriptApiVersion::V1 => parse_action_result(scope, value, operations)?,
                    ScriptApiVersion::V2 => {
                        parse_v2_action_result(scope, value, pass.as_ref().unwrap(), operations)?
                    }
                };
                let next_content =
                    property(scope, argument, content_state_name).ok_or_else(|| {
                        ScriptError::new(format!("script removed context.{content_state_name}"))
                    })?;
                let next_view = property(scope, argument, "viewState")
                    .ok_or_else(|| ScriptError::new("script removed context.viewState"))?;
                let next_content = v8_to_json(scope, next_content, content_state_name)?;
                let next_view = v8_to_json(scope, next_view, "viewState")?;
                view_policy_from_json(&next_view)?;
                perform_microtask_checkpoint(scope);
                Ok((
                    result,
                    next_content,
                    next_view,
                    content_decorations,
                    view_decorations,
                ))
            })?;
        content_state.publish_external_data(next_content);
        view_state.data = next_view;
        if let Some(decorations) = content_decorations {
            content_state.decorations = DecorationSet::new(decorations);
        }
        if let Some(decorations) = view_decorations {
            view_state.decorations = DecorationSet::new(decorations);
        }
        Ok(result)
    }

    pub(super) fn create_state(
        &mut self,
        callback: Option<&v8::Global<v8::Function>>,
        parent: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, ScriptError> {
        let Some(callback) = callback.cloned() else {
            return Ok(serde_json::Value::Null);
        };
        let context = self.context.clone();
        self.invoke(ScriptInvocationKind::StateFactory, |isolate| {
            v8::scope_with_context!(scope, isolate, context);
            v8::tc_scope!(let scope, scope);
            let callback = v8::Local::new(scope, callback);
            let receiver = v8::undefined(scope).into();
            let arguments = parent
                .map(|value| json_to_v8(scope, value))
                .transpose()?
                .into_iter()
                .collect::<Vec<_>>();
            let value = call_script_callback(scope, callback, receiver, &arguments)
                .ok_or_else(|| current_exception(scope, "script mode state factory", "execute"))?;
            let result = v8_to_json(scope, value, "mode state")?;
            perform_microtask_checkpoint(scope);
            Ok(result)
        })
    }

    pub(super) fn create_content_state(
        &mut self,
        callback: Option<&v8::Global<v8::Function>>,
        version: ScriptApiVersion,
        context: &ModeContentContext<'_>,
    ) -> Result<serde_json::Value, ScriptError> {
        let Some(callback) = callback.cloned() else {
            return Ok(serde_json::Value::Null);
        };
        let v8_context = self.context.clone();
        self.invoke(ScriptInvocationKind::StateFactory, |isolate| {
            v8::scope_with_context!(scope, isolate, v8_context);
            v8::tc_scope!(let scope, scope);
            let legacy = version == ScriptApiVersion::V1;
            let argument = content_context_object(scope, context, legacy, legacy)?;
            let callback = v8::Local::new(scope, callback);
            let receiver = v8::undefined(scope).into();
            let value = call_script_callback(scope, callback, receiver, &[argument.into()])
                .ok_or_else(|| {
                    current_exception(scope, "script content state factory", "execute")
                })?;
            let result = v8_to_json(scope, value, "mode content state")?;
            perform_microtask_checkpoint(scope);
            Ok(result)
        })
    }

    pub(super) fn content_changed(
        &mut self,
        callback: &v8::Global<v8::Function>,
        version: ScriptApiVersion,
        context: &ModeContentContext<'_>,
        state: &mut ScriptModeState,
        change: &vell_core::content::ContentChange,
    ) -> Result<(), ScriptError> {
        if let Some(revision) = context.content_revision() {
            self.worker_decorations.borrow_mut().track_current(
                context.content_id(),
                revision.0,
                context.buffer().and_then(|buffer| buffer.text_snapshot()),
            );
        }
        let v8_context = self.context.clone();
        let content_state_name = version.content_state_name();
        let current = state.data.clone();
        let callback = callback.clone();
        let next = self.invoke(ScriptInvocationKind::ContentChanged, |isolate| {
            v8::scope_with_context!(scope, isolate, v8_context);
            v8::tc_scope!(let scope, scope);
            let argument =
                content_context_object(scope, context, true, version == ScriptApiVersion::V1)?;
            let content_state = json_to_v8(scope, &current)?;
            set_value(scope, argument, content_state_name, content_state);
            let change_value = content_change_to_v8(scope, change)?;
            set_value(scope, argument, "change", change_value);
            let callback = v8::Local::new(scope, callback);
            let receiver = v8::undefined(scope).into();
            call_script_callback(scope, callback, receiver, &[argument.into()])
                .ok_or_else(|| current_exception(scope, "script content changed", "execute"))?;
            let next = property(scope, argument, content_state_name).ok_or_else(|| {
                ScriptError::new(format!("script removed context.{content_state_name}"))
            })?;
            let next = v8_to_json(scope, next, content_state_name)?;
            perform_microtask_checkpoint(scope);
            Ok(next)
        })?;
        state.publish_external_data(next);
        Ok(())
    }

    pub(super) fn evaluate_typescript(
        &mut self,
        specifier: &str,
        source: &str,
    ) -> Result<String, ScriptError> {
        ensure_size("TypeScript source", source.len(), MAX_SCRIPT_SOURCE_BYTES)?;
        let program = transpile_typescript_program(specifier, source)?;
        if let Some(source_map) = &program.source_map {
            self.commands
                .borrow_mut()
                .record_source_map(specifier.to_owned(), source_map)?;
        }
        let javascript = program.code;
        ensure_size(
            "transpiled JavaScript",
            javascript.len(),
            MAX_SCRIPT_SOURCE_BYTES,
        )?;
        self.update_type_source(specifier, source);
        let context = self.context.clone();
        self.invoke(ScriptInvocationKind::ModuleEvaluation, |isolate| {
            v8::scope_with_context!(scope, isolate, context);
            v8::tc_scope!(let scope, scope);

            let source = v8::String::new(scope, &javascript)
                .ok_or_else(|| ScriptError::new("script source is too large for V8"))?;
            let resource_name = v8::String::new(scope, specifier)
                .ok_or_else(|| ScriptError::new("script source identity is too large for V8"))?;
            let origin = v8::ScriptOrigin::new(
                scope,
                resource_name.into(),
                0,
                0,
                false,
                0,
                None,
                false,
                false,
                false,
                None,
            );
            let script = match v8::Script::compile(scope, source, Some(&origin)) {
                Some(script) => script,
                None => return Err(current_exception(scope, specifier, "compile")),
            };
            let value = match script.run(scope) {
                Some(value) => value,
                None => return Err(current_exception(scope, specifier, "execute")),
            };

            perform_microtask_checkpoint(scope);
            Ok(value.to_rust_string_lossy(scope))
        })
    }
}

fn set_buffer_editing_facts(
    scope: &mut v8::PinScope,
    argument: v8::Local<v8::Object>,
    buffer: &vell_mode::BufferModeViewContext<'_>,
) -> Result<(), ScriptError> {
    let snapshot = buffer
        .text_snapshot()
        .ok_or_else(|| ScriptError::new("buffer text snapshot is unavailable"))?;
    let text = snapshot.to_owned_string();
    ensure_size("buffer text", text.len(), MAX_SCRIPT_INPUT_BYTES)?;
    set_string(scope, argument, "text", &text);

    let selections = buffer.selections().all().collect::<Vec<_>>();
    let values = v8::Array::new(
        scope,
        i32::try_from(selections.len()).map_err(|_| ScriptError::new("too many selections"))?,
    );
    for (index, selection) in selections.into_iter().enumerate() {
        let value = v8::Object::new(scope);
        let anchor = position_object(scope, &snapshot, selection.anchor.char_index)?;
        let head = position_object(scope, &snapshot, selection.head.char_index)?;
        set_object(scope, value, "anchor", anchor);
        set_object(scope, value, "head", head);
        values.set_index(
            scope,
            u32::try_from(index).map_err(|_| ScriptError::new("too many selections"))?,
            value.into(),
        );
    }
    set_value(scope, argument, "selections", values.into());
    let primary = buffer.selections().primary();
    let primary_value = v8::Object::new(scope);
    let anchor = position_object(scope, &snapshot, primary.anchor.char_index)?;
    let head = position_object(scope, &snapshot, primary.head.char_index)?;
    set_object(scope, primary_value, "anchor", anchor);
    set_object(scope, primary_value, "head", head);
    set_object(scope, argument, "primarySelection", primary_value);
    Ok(())
}

fn position_object<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    snapshot: &vell_core::text_snapshot::TextSnapshot,
    char_offset: usize,
) -> Result<v8::Local<'scope, v8::Object>, ScriptError> {
    let (line, character) = snapshot
        .char_to_utf16_position(char_offset)
        .ok_or_else(|| ScriptError::new("selection is outside the buffer snapshot"))?;
    let value = v8::Object::new(scope);
    set_number(scope, value, "line", line as f64);
    set_number(scope, value, "character", character as f64);
    Ok(value)
}
