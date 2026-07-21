use std::cell::RefCell;
use std::rc::Rc;

use vell_core::content::ContentKind;
use vell_core::keymap::Keymap;
use vell_mode::command::{Command, ModeCommand, ModeValue};
use vell_mode::mode_name::{ModeActionName, ModeName};
use vell_mode::{
    Mode, ModeAdapters, ModeContentContext, ModeError, ModeJobRequest, ModeJobResult, ModeJobSlot,
    ModeResult, ModeState, ModeViewContext, ModeViewPolicy,
};
use vell_protocol::content_query::{Face, FaceName, NamedTextDecoration, RowRange};
use vell_protocol::key_event::KeyEvent;

use super::bridge::view_policy_from_json;
use super::{
    ScriptActionDefinition, ScriptAdapterDefinition, ScriptAnalysisDefinition, ScriptApiVersion,
    ScriptHost, ScriptJob, ScriptJobOutput, ScriptModeDefinition, ScriptModeState,
    disabled_script_job, failed_script_job, key_event_arguments, map_decoration_set,
    script_job_request, script_state, script_state_mut,
};
use super::worker::ScriptWorker;

pub(super) struct ScriptMode {
    host: Rc<RefCell<ScriptHost>>,
    name: ModeName,
    version: ScriptApiVersion,
    actions: Vec<ModeActionName>,
    adapters: ScriptAdapters,
    faces: Vec<(FaceName, Face)>,
    before: Option<ModeName>,
}

struct ScriptAdapter {
    actions: Vec<ScriptActionDefinition>,
    keymap: Keymap<Command>,
    input_action: Option<ModeActionName>,
    input: Option<v8::Global<v8::Function>>,
    create_content: Option<v8::Global<v8::Function>>,
    content_changed: Option<v8::Global<v8::Function>>,
    content_job: Option<v8::Global<v8::Function>>,
    content_apply_job: Option<v8::Global<v8::Function>>,
    create_view: Option<v8::Global<v8::Function>>,
    worker: Option<ScriptWorker>,
    analyses: Vec<ScriptAnalysisDefinition>,
}

#[derive(Default)]
struct ScriptAdapters {
    buffer: Option<ScriptAdapter>,
    status_bar: Option<ScriptAdapter>,
}

impl ScriptAdapters {
    fn get(&self, kind: ContentKind) -> Option<&ScriptAdapter> {
        match kind {
            ContentKind::Buffer => self.buffer.as_ref(),
            ContentKind::StatusBar => self.status_bar.as_ref(),
        }
    }
}

impl ScriptAdapter {
    fn new(mode: &ModeName, definition: ScriptAdapterDefinition) -> Self {
        let mut keymap = Keymap::new();
        for (key, action_index) in &definition.bindings {
            let action = definition.actions[*action_index].name.clone();
            keymap.bind(*key, Command::Mode(ModeCommand::new(mode.clone(), action)));
        }
        let input_action = definition
            .input_action
            .map(|index| definition.actions[index].name.clone());
        Self {
            actions: definition.actions,
            keymap,
            input_action,
            input: definition.input,
            create_content: definition.create_content,
            content_changed: definition.content_changed,
            content_job: definition.content_job,
            content_apply_job: definition.content_apply_job,
            create_view: definition.create_view,
            worker: definition.worker,
            analyses: definition.analyses,
        }
    }
}

impl ScriptMode {
    pub(super) fn new(
        host: Rc<RefCell<ScriptHost>>,
        definition: ScriptModeDefinition,
    ) -> Self {
        let mut actions = Vec::new();
        for adapter in [
            definition.adapters.buffer.as_ref(),
            definition.adapters.status_bar.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            for action in &adapter.actions {
                if !actions.contains(&action.name) {
                    actions.push(action.name.clone());
                }
            }
        }
        let adapters = ScriptAdapters {
            buffer: definition
                .adapters
                .buffer
                .map(|adapter| ScriptAdapter::new(&definition.name, adapter)),
            status_bar: definition
                .adapters
                .status_bar
                .map(|adapter| ScriptAdapter::new(&definition.name, adapter)),
        };
        Self {
            host,
            name: definition.name,
            version: definition.version,
            actions,
            adapters,
            faces: definition.faces,
            before: definition.before,
        }
    }

    fn adapter(&self, kind: ContentKind) -> &ScriptAdapter {
        self.adapters
            .get(kind)
            .expect("registered ScriptMode keeps its declared adapter")
    }
}

impl Mode for ScriptMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &self.actions
    }

    fn adapters(&self) -> ModeAdapters {
        match (
            self.adapters.buffer.is_some(),
            self.adapters.status_bar.is_some(),
        ) {
            (true, true) => ModeAdapters::buffer_and_status_bar(),
            (true, false) => ModeAdapters::buffer(),
            (false, true) => ModeAdapters::status_bar(),
            (false, false) => unreachable!("script parser requires at least one adapter"),
        }
    }

    fn before(&self) -> Option<&ModeName> {
        self.before.as_ref()
    }

    fn faces(&self) -> Vec<(FaceName, Face)> {
        self.faces.clone()
    }

    fn create_content_state(
        &self,
        context: &ModeContentContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        let adapter = self.adapter(context.content_kind());
        self.host
            .borrow_mut()
            .create_content_state(adapter.create_content.as_ref(), self.version, context)
            .map(|state| Box::new(ScriptModeState::new(state)) as Box<dyn ModeState>)
            .map_err(|error| ModeError::CallbackFailed {
                mode: self.name.clone(),
                message: format!("callback '<content-state>': {error}"),
            })
    }

    fn create_view_state(
        &self,
        content_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        let adapter = self.adapter(context.content_kind());
        let content_state = &script_state(content_state, &self.name)?.data;
        let state = self
            .host
            .borrow_mut()
            .create_state(adapter.create_view.as_ref(), Some(content_state))
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
        context: &ModeViewContext<'_>,
    ) -> &'a Keymap<Command> {
        &self.adapter(context.content_kind()).keymap
    }

    fn input_typing(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> Option<Command> {
        let adapter = self.adapter(context.content_kind());
        if adapter.input.is_some() {
            return Some(Command::ModeInput(
                vell_mode::command::ModeInputCommand::new(self.name.clone(), key),
            ));
        }
        let action = adapter.input_action.clone()?;
        Some(Command::Mode(
            ModeCommand::new(self.name.clone(), action).with_arguments(key_event_arguments(key)),
        ))
    }

    fn execute_input(
        &self,
        content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> Result<ModeResult, ModeError> {
        let adapter = self.adapter(context.content_kind());
        let callback = adapter
            .input
            .as_ref()
            .ok_or_else(|| ModeError::UnknownAction {
                mode: self.name.clone(),
                action: ModeActionName::new("<input>"),
            })?;
        let content_state = script_state_mut(content_state, &self.name)?;
        let view_state = script_state_mut(view_state, &self.name)?;
        self.host
            .borrow_mut()
            .execute_action(
                callback,
                self.version,
                context,
                &key_event_arguments(key),
                content_state,
                view_state,
            )
            .map_err(|error| ModeError::CallbackFailed {
                mode: self.name.clone(),
                message: format!("callback '<input>': {error}"),
            })
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
        change: &vell_core::content::ContentChange,
    ) -> Result<(), ModeError> {
        let state = script_state_mut(state, &self.name)?;
        let adapter = self.adapter(context.content_kind());
        let vell_core::content::ContentChange::Text(text_change) = change;
        state.decorations = map_decoration_set(&state.decorations, text_change);
        for decorations in state.analysis_decorations.values_mut() {
            *decorations = map_decoration_set(decorations, text_change);
        }
        if let Some(callback) = adapter.content_changed.as_ref() {
            self.host
                .borrow_mut()
                .content_changed(callback, self.version, context, state, change)
                .map_err(|error| ModeError::CallbackFailed {
                    mode: self.name.clone(),
                    message: error.to_string(),
                })?;
        }
        Ok(())
    }

    fn take_background_jobs(
        &self,
        state: &mut dyn ModeState,
        context: &ModeContentContext<'_>,
    ) -> Vec<ModeJobRequest> {
        let adapter = self.adapter(context.content_kind());
        let state = match script_state_mut(state, &self.name) {
            Ok(state) => state,
            Err(error) => {
                return vec![failed_script_job(error.to_string())];
            }
        };
        if let (Some(callback), Some(worker)) =
            (adapter.content_job.as_ref(), adapter.worker.as_ref())
        {
            let job = match self.host.borrow_mut().take_content_job(
                callback,
                self.version,
                context,
                state,
            ) {
                Ok(Some(job)) => job,
                Ok(None) => return Vec::new(),
                Err(error) => return vec![failed_script_job(error.to_string())],
            };
            return vec![script_job_request(job, worker.clone())];
        }
        let Some(content_revision) = context.content_revision().map(|revision| revision.0) else {
            return Vec::new();
        };
        let prepared = adapter
            .analyses
            .iter()
            .map(|analysis| {
                self.host
                    .borrow_mut()
                    .prepare_analysis_job(analysis, context, state)
            })
            .collect::<Result<Vec<_>, _>>();
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return vec![failed_script_job(error.to_string())],
        };
        let mut requests = Vec::new();
        for (analysis, prepared) in adapter.analyses.iter().zip(prepared) {
            if state.reconcile_analysis_input(&analysis.slot, content_revision, &prepared.message) {
                continue;
            }
            let version = state.record_analysis_request(
                &analysis.slot,
                content_revision,
                prepared.message.clone(),
            );
            let Some(message) = prepared.message else {
                requests.push(disabled_script_job(analysis.slot.clone(), version));
                continue;
            };
            requests.push(script_job_request(
                ScriptJob {
                    slot: analysis.slot.clone(),
                    version,
                    message,
                    include_text: analysis.snapshot_text,
                    text_snapshot: prepared.text_snapshot,
                },
                analysis.worker.clone(),
            ));
        }
        requests
    }

    fn apply_background_job(
        &self,
        state: &mut dyn ModeState,
        context: &ModeContentContext<'_>,
        slot: &ModeJobSlot,
        version: u64,
        result: ModeJobResult,
    ) -> Result<bool, ModeError> {
        let slot = slot.as_str();
        let adapter = self.adapter(context.content_kind());
        let state = script_state_mut(state, &self.name)?;
        let current_revision = context.content_revision().map(|revision| revision.0);
        let Ok(result) = result else {
            if adapter.content_apply_job.is_some() {
                return Ok(false);
            }
            let Some(content_revision) = current_revision else {
                return Ok(false);
            };
            if !state.analysis_request_is_current(slot, version, content_revision) {
                return Ok(false);
            }
            return Ok(true);
        };
        let result =
            result
                .downcast::<ScriptJobOutput>()
                .map_err(|_| ModeError::CallbackFailed {
                    mode: self.name.clone(),
                    message: "script worker returned an invalid host value".to_owned(),
                })?;
        let result = match *result {
            ScriptJobOutput::Response(result) => Some(result),
            ScriptJobOutput::Disabled => None,
            ScriptJobOutput::CallbackError(message) => {
                return Err(ModeError::CallbackFailed {
                    mode: self.name.clone(),
                    message,
                });
            }
        };
        if let Some(callback) = adapter.content_apply_job.as_ref() {
            return self
                .host
                .borrow_mut()
                .apply_content_job(
                    callback,
                    self.version,
                    context,
                    state,
                    version,
                    result
                        .as_ref()
                        .expect("legacy jobs always return a response"),
                )
                .map_err(|error| ModeError::CallbackFailed {
                    mode: self.name.clone(),
                    message: error.to_string(),
                });
        }
        let Some(analysis) = adapter
            .analyses
            .iter()
            .find(|analysis| analysis.slot == slot)
        else {
            return Ok(false);
        };
        let Some(content_revision) = current_revision else {
            return Ok(false);
        };
        if !state.analysis_request_is_current(slot, version, content_revision) {
            return Ok(false);
        }
        if let Some(result) = result {
            let previous_state = state.data.clone();
            self.host
                .borrow_mut()
                .apply_analysis_result(analysis, context, state, &result)
                .map_err(|error| ModeError::CallbackFailed {
                    mode: self.name.clone(),
                    message: error.to_string(),
                })?;
            if state.data != previous_state {
                state.mark_analysis_output_change();
            }
        }
        let accepted = self
            .host
            .borrow_mut()
            .prepare_analysis_job(analysis, context, state)
            .map_err(|error| ModeError::CallbackFailed {
                mode: self.name.clone(),
                message: error.to_string(),
            })?;
        state.accept_analysis_input(slot, version, content_revision, accepted.message);
        // Poll all named analyses after any completion. Their input messages are
        // the dependency signatures, so only changed inputs produce new jobs.
        Ok(true)
    }

    fn content_decorations(
        &self,
        content_state: &dyn ModeState,
        context: &ModeContentContext<'_>,
        visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        let Some(snapshot) = context.buffer().and_then(|context| context.text_snapshot()) else {
            return Vec::new();
        };
        let adapter = self.adapter(context.content_kind());
        script_state(content_state, &self.name)
            .map(|state| {
                let mut decorations = state.decorations.visible(&snapshot, visible_rows);
                for analysis in &adapter.analyses {
                    if let Some(layer) = state.analysis_decorations.get(&analysis.slot) {
                        decorations.extend(layer.visible(&snapshot, visible_rows));
                    }
                }
                decorations
            })
            .unwrap_or_default()
    }

    fn view_decorations(
        &self,
        _content_state: &dyn ModeState,
        view_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
        visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        let Some(snapshot) = context.buffer().and_then(|context| context.text_snapshot()) else {
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
        let adapter = self.adapter(context.content_kind());
        let callback = adapter
            .actions
            .iter()
            .find(|candidate| &candidate.name == action)
            .ok_or_else(|| ModeError::UnknownAction {
                mode: self.name.clone(),
                action: action.clone(),
            })?;
        let content_state = script_state_mut(content_state, &self.name)?;
        let view_state = script_state_mut(view_state, &self.name)?;
        self.host
            .borrow_mut()
            .execute_action(
                &callback.callback,
                self.version,
                context,
                arguments,
                content_state,
                view_state,
            )
            .map_err(|error| ModeError::CallbackFailed {
                mode: self.name.clone(),
                message: format!("callback '{}': {error}", action.as_str()),
            })
    }
}
