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
use vell_protocol::content_query::{FaceDefinition, NamedTextDecoration, RowRange};
use vell_protocol::key_event::KeyEvent;

use super::bridge::view_policy_from_json;
use super::{
    ScriptActionDefinition, ScriptAdapterDefinition, ScriptApiVersion, ScriptError, ScriptHost,
    ScriptModeDefinition, ScriptModeState, key_event_arguments, map_decoration_set, script_state,
    script_state_mut,
};

pub(super) struct ScriptMode {
    host: Rc<RefCell<ScriptHost>>,
    name: ModeName,
    version: ScriptApiVersion,
    actions: Vec<ModeActionName>,
    adapters: ScriptAdapters,
    face_definitions: Vec<FaceDefinition>,
    before: Option<ModeName>,
}

struct ScriptAdapter {
    actions: Vec<ScriptActionDefinition>,
    keymap: Keymap<Command>,
    input_action: Option<ModeActionName>,
    input: Option<v8::Global<v8::Function>>,
    create_content: Option<v8::Global<v8::Function>>,
    content_changed: Option<v8::Global<v8::Function>>,
    create_view: Option<v8::Global<v8::Function>>,
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
            create_view: definition.create_view,
        }
    }
}

impl ScriptMode {
    pub(super) fn new(host: Rc<RefCell<ScriptHost>>, definition: ScriptModeDefinition) -> Self {
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
            face_definitions: definition.face_definitions,
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

    fn face_definitions(&self) -> Vec<FaceDefinition> {
        self.face_definitions.clone()
    }

    fn create_content_state(
        &self,
        context: &ModeContentContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        let adapter = self.adapter(context.content_kind());
        let mut host = self.host.borrow_mut();
        let result = (|| {
            let data =
                host.create_content_state(adapter.create_content.as_ref(), self.version, context)?;
            let mut state = ScriptModeState::new(data);
            if let Some(callback) = adapter.content_changed.as_ref() {
                let change = vell_core::content::ContentChange::Text(
                    vell_core::transaction::TextChangeSet::empty(),
                );
                host.content_changed(callback, self.version, context, &mut state, &change)?;
            }
            Ok(Box::new(state) as Box<dyn ModeState>)
        })();
        result.map_err(|error: ScriptError| ModeError::CallbackFailed {
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

    fn poll_background(&self) {
        self.host.borrow_mut().pump_worker_messages();
    }

    fn take_background_jobs(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeContentContext<'_>,
    ) -> Vec<ModeJobRequest> {
        Vec::new()
    }

    fn apply_background_job(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeContentContext<'_>,
        _slot: &ModeJobSlot,
        _version: u64,
        _result: ModeJobResult,
    ) -> Result<bool, ModeError> {
        // ponytail: analysis/v1 job apply removed in Task 7.
        Ok(false)
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
        let mut decorations = script_state(content_state, &self.name)
            .map(|state| state.decorations.visible(&snapshot, visible_rows))
            .unwrap_or_default();
        if let Some(revision) = context.content_revision() {
            let content_id = context.content_id();
            // Self-heal stale `current` from native edits that advanced
            // the revision outside a script Mode action frame.
            self.host
                .borrow()
                .worker_decorations
                .borrow_mut()
                .track_current(content_id, revision.0, Some(snapshot.clone()));
            if let Some(set) = self
                .host
                .borrow()
                .worker_decorations
                .borrow()
                .read(content_id, revision.0)
            {
                decorations.extend(set.visible(&snapshot, visible_rows));
            }
        }
        decorations
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
