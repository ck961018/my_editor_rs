use super::*;

/// The single long-lived V8 isolate used by script modes.
#[allow(dead_code)]
pub struct ScriptHost {
    isolate: v8::OwnedIsolate,
    heap_limit: Box<HeapLimitState>,
    heap_limit_bytes: usize,
    budget: ScriptExecutionBudget,
    pub(super) context: v8::Global<v8::Context>,
    modules: Rc<RefCell<ModuleMap>>,
    pub(super) definitions: Rc<RefCell<Vec<ScriptModeDefinition>>>,
    pub(super) diagnostics: Rc<RefCell<ScriptDiagnostics>>,
    plugin_root: Rc<RefCell<Option<String>>>,
    primitives: Rc<RefCell<PrimitiveRuntime>>,
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
        let modules = Rc::new(RefCell::new(ModuleMap::default()));
        let definitions = Rc::new(RefCell::new(Vec::new()));
        let diagnostics = Rc::new(RefCell::new(ScriptDiagnostics::default()));
        let plugin_root = Rc::new(RefCell::new(None));
        let primitives = PrimitiveRuntime::new();
        isolate.set_slot(modules.clone());
        isolate.set_slot(definitions.clone());
        isolate.set_slot(diagnostics.clone());
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
            plugin_root,
            primitives,
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

    pub fn execute_typescript(&mut self, specifier: &str, source: &str) -> Result<(), ScriptError> {
        let definition_count = self.definitions.borrow().len();
        let diagnostics = self.diagnostics.borrow().clone();
        let result = self.evaluate_typescript(specifier, source).map(|_| ());
        if result.is_err() {
            self.definitions.borrow_mut().truncate(definition_count);
            self.diagnostics.replace(diagnostics);
        }
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

    pub(crate) fn execute_module(&mut self, entry: &Path) -> Result<(), ScriptError> {
        let entry = entry
            .canonicalize()
            .map_err(|error| ScriptError::new(format!("failed to open script: {error}")))?;
        let root = entry
            .parent()
            .ok_or_else(|| ScriptError::new("script entry has no parent directory"))?
            .to_owned();
        self.modules.borrow_mut().reset(root.clone());
        let definition_count = self.definitions.borrow().len();
        let diagnostics = self.diagnostics.borrow().clone();

        let modules = self.modules.clone();
        let context = self.context.clone();
        let result = self.invoke(ScriptInvocationKind::ModuleEvaluation, |isolate| {
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
        if result.is_err() {
            self.definitions.borrow_mut().truncate(definition_count);
            self.diagnostics.replace(diagnostics);
            self.modules.borrow_mut().reset(root);
        }
        result
    }

    pub(super) fn script_modes(host: &Rc<RefCell<Self>>) -> Vec<ScriptMode> {
        let definitions = host.borrow().definitions.borrow().clone();
        definitions
            .into_iter()
            .map(|definition| ScriptMode::new(host.clone(), definition))
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
        let v8_context = self.context.clone();
        let content_state_name = version.content_state_name();
        let current = state.data.clone();
        let callback = callback.clone();
        let next = self.invoke(ScriptInvocationKind::ContentChanged, |isolate| {
            v8::scope_with_context!(scope, isolate, v8_context);
            v8::tc_scope!(let scope, scope);
            let argument =
                content_context_object(scope, context, false, version == ScriptApiVersion::V1)?;
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

    pub(super) fn take_content_job(
        &mut self,
        callback: &v8::Global<v8::Function>,
        api_version: ScriptApiVersion,
        context: &ModeContentContext<'_>,
        state: &mut ScriptModeState,
    ) -> Result<Option<ScriptJob>, ScriptError> {
        let v8_context = self.context.clone();
        let content_state_name = api_version.content_state_name();
        let current = state.data.clone();
        let callback = callback.clone();
        let (next, job) = self.invoke(ScriptInvocationKind::ContentJob, |isolate| {
            v8::scope_with_context!(scope, isolate, v8_context);
            v8::tc_scope!(let scope, scope);
            let argument =
                content_context_object(scope, context, false, api_version == ScriptApiVersion::V1)?;
            let content_state = json_to_v8(scope, &current)?;
            set_value(scope, argument, content_state_name, content_state);
            let callback = v8::Local::new(scope, callback);
            let receiver = v8::undefined(scope).into();
            let value = call_script_callback(scope, callback, receiver, &[argument.into()])
                .ok_or_else(|| current_exception(scope, "script content job", "execute"))?;
            let next = property(scope, argument, content_state_name).ok_or_else(|| {
                ScriptError::new(format!("script removed context.{content_state_name}"))
            })?;
            let next = v8_to_json(scope, next, content_state_name)?;
            let job = if value.is_null_or_undefined() {
                None
            } else {
                Some(v8_to_json(scope, value, "content job")?)
            };
            perform_microtask_checkpoint(scope);
            Ok((next, job))
        })?;
        let Some(job) = job else {
            state.data = next;
            return Ok(None);
        };
        let mut job = ScriptJob::from_json(job)?;
        if job.include_text {
            job.text_snapshot = Some(
                context
                    .buffer()
                    .and_then(|context| context.text_snapshot())
                    .ok_or_else(|| ScriptError::new("content job text requires text content"))?,
            );
        }
        state.data = next;
        Ok(Some(job))
    }

    pub(super) fn prepare_analysis_job(
        &mut self,
        analysis: &ScriptAnalysisDefinition,
        context: &ModeContentContext<'_>,
        state: &ScriptModeState,
    ) -> Result<PreparedAnalysisJob, ScriptError> {
        let v8_context = self.context.clone();
        let current = state.data.clone();
        let callback = analysis.input.clone();
        let message = self.invoke(ScriptInvocationKind::AnalysisInput, |isolate| {
            v8::scope_with_context!(scope, isolate, v8_context);
            v8::tc_scope!(let scope, scope);
            let argument = content_context_object(scope, context, false, false)?;
            let content_state = json_to_v8(scope, &current)?;
            set_value(scope, argument, "state", content_state);
            let callback = v8::Local::new(scope, callback);
            let receiver = v8::undefined(scope).into();
            let value = call_script_callback(scope, callback, receiver, &[argument.into()])
                .ok_or_else(|| current_exception(scope, "script analysis input", "execute"))?;
            let message = if value.is_null_or_undefined() {
                None
            } else {
                Some(v8_to_json(scope, value, "analysis input")?)
            };
            perform_microtask_checkpoint(scope);
            Ok(message)
        })?;
        let Some(message) = message else {
            return Ok(PreparedAnalysisJob {
                message: None,
                text_snapshot: None,
            });
        };
        let text_snapshot = if analysis.snapshot_text {
            let object = message.as_object().ok_or_else(|| {
                ScriptError::new("analysis input must return an object for a text snapshot")
            })?;
            if object.contains_key("text") {
                return Err(ScriptError::new(
                    "analysis input.text is reserved for the text snapshot",
                ));
            }
            Some(
                context
                    .buffer()
                    .and_then(|context| context.text_snapshot())
                    .ok_or_else(|| ScriptError::new("analysis text snapshot requires a Buffer"))?,
            )
        } else {
            None
        };
        Ok(PreparedAnalysisJob {
            message: Some(message),
            text_snapshot,
        })
    }

    pub(super) fn apply_content_job(
        &mut self,
        callback: &v8::Global<v8::Function>,
        api_version: ScriptApiVersion,
        context: &ModeContentContext<'_>,
        state: &mut ScriptModeState,
        version: u64,
        result: &serde_json::Value,
    ) -> Result<bool, ScriptError> {
        let v8_context = self.context.clone();
        let content_state_name = api_version.content_state_name();
        let current = state.data.clone();
        let callback = callback.clone();
        let (next, decorations) = self.invoke(ScriptInvocationKind::ContentJob, |isolate| {
            v8::scope_with_context!(scope, isolate, v8_context);
            v8::tc_scope!(let scope, scope);
            let argument =
                content_context_object(scope, context, false, api_version == ScriptApiVersion::V1)?;
            let content_state = json_to_v8(scope, &current)?;
            set_value(scope, argument, content_state_name, content_state);
            set_number(scope, argument, "jobVersion", version as f64);
            let result_value = json_to_v8(scope, result)?;
            set_value(scope, argument, "arguments", result_value);
            let callback = v8::Local::new(scope, callback);
            let receiver = v8::undefined(scope).into();
            let value = call_script_callback(scope, callback, receiver, &[argument.into()])
                .ok_or_else(|| current_exception(scope, "script content applyJob", "execute"))?;
            let decorations = parse_decorations_property(
                scope,
                value,
                "contentDecorations",
                context.buffer().and_then(|context| context.text_snapshot()),
                context.content_revision(),
            )?;
            let next = property(scope, argument, content_state_name).ok_or_else(|| {
                ScriptError::new(format!("script removed context.{content_state_name}"))
            })?;
            let next = v8_to_json(scope, next, content_state_name)?;
            perform_microtask_checkpoint(scope);
            Ok((next, decorations))
        })?;
        let changed = next != state.data || decorations.is_some();
        state.data = next;
        if let Some(decorations) = decorations {
            state.decorations = DecorationSet::new(decorations);
        }
        Ok(changed)
    }

    pub(super) fn apply_analysis_result(
        &mut self,
        analysis: &ScriptAnalysisDefinition,
        context: &ModeContentContext<'_>,
        state: &mut ScriptModeState,
        result: &serde_json::Value,
    ) -> Result<bool, ScriptError> {
        let v8_context = self.context.clone();
        let current = state.data.clone();
        let callback = analysis.apply.clone();
        let (next, decorations) = self.invoke(ScriptInvocationKind::AnalysisApply, |isolate| {
            v8::scope_with_context!(scope, isolate, v8_context);
            v8::tc_scope!(let scope, scope);
            let argument = content_context_object(scope, context, false, false)?;
            let content_state = json_to_v8(scope, &current)?;
            set_value(scope, argument, "state", content_state);
            let result_value = json_to_v8(scope, result)?;
            set_value(scope, argument, "arguments", result_value);
            let callback = v8::Local::new(scope, callback);
            let receiver = v8::undefined(scope).into();
            let value = call_script_callback(scope, callback, receiver, &[argument.into()])
                .ok_or_else(|| current_exception(scope, "script analysis apply", "execute"))?;
            let decorations = parse_decorations_property(
                scope,
                value,
                "contentDecorations",
                context.buffer().and_then(|context| context.text_snapshot()),
                context.content_revision(),
            )?;
            let next = property(scope, argument, "state")
                .ok_or_else(|| ScriptError::new("script removed context.state"))?;
            let next = v8_to_json(scope, next, "state")?;
            perform_microtask_checkpoint(scope);
            Ok((next, decorations))
        })?;
        let changed = next != state.data || decorations.is_some();
        state.data = next;
        if let Some(decorations) = decorations {
            state
                .analysis_decorations
                .insert(analysis.slot.clone(), DecorationSet::new(decorations));
        }
        Ok(changed)
    }

    pub(super) fn evaluate_typescript(
        &mut self,
        specifier: &str,
        source: &str,
    ) -> Result<String, ScriptError> {
        ensure_size("TypeScript source", source.len(), MAX_SCRIPT_SOURCE_BYTES)?;
        let context = self.context.clone();
        self.invoke(ScriptInvocationKind::ModuleEvaluation, |isolate| {
            let javascript = transpile_typescript(specifier, source)?;
            ensure_size(
                "transpiled JavaScript",
                javascript.len(),
                MAX_SCRIPT_SOURCE_BYTES,
            )?;
            v8::scope_with_context!(scope, isolate, context);
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

            perform_microtask_checkpoint(scope);
            Ok(value.to_rust_string_lossy(scope))
        })
    }
}
