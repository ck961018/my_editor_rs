use std::any::Any;
use std::sync::LazyLock;

use tokio_util::sync::CancellationToken;

use crate::command::{Command, ModeValue};
use crate::mode_name::{ModeActionName, ModeName};
use crate::{
    Mode, ModeActionScope, ModeAdapters, ModeContentContext, ModeError, ModeJobRequest,
    ModeJobResult, ModeJobSlot, ModeRegistrationError, ModeRegistry, ModeResult, ModeState,
    ModeStateKind, ModeViewContext, ModeViewPolicy,
};
use vell_core::content::ContentChange;
use vell_core::input::{InputDecision, InputStatus};
use vell_core::keymap::Keymap;
use vell_protocol::content_query::{Face, FaceName, NamedTextDecoration, RowRange};
use vell_protocol::key_event::KeyEvent;

static EMPTY_TYPED_KEYMAP: LazyLock<Keymap<Command>> = LazyLock::new(Keymap::new);

struct TypedState<T>(T);

impl<T: Any + Clone + PartialEq> ModeState for TypedState<T> {
    fn as_any(&self) -> &dyn Any {
        &self.0
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        &mut self.0
    }

    fn clone_box(&self) -> Box<dyn ModeState> {
        #[cfg(feature = "test-support")]
        let started = std::time::Instant::now();
        let cloned: Box<dyn ModeState> = Box::new(Self(self.0.clone()));
        #[cfg(feature = "test-support")]
        crate::runtime::record_mode_state_clone(started, &self.0);
        cloned
    }

    fn eq_box(&self, other: &dyn ModeState) -> Option<bool> {
        Some(other.as_any().downcast_ref::<T>() == Some(&self.0))
    }
}

pub type TypedModeJobResult<T> = Result<T, String>;
type TypedModeJobRunner<T> = Box<dyn FnOnce(CancellationToken) -> TypedModeJobResult<T> + Send>;

pub struct TypedModeJobRequest<T> {
    slot: ModeJobSlot,
    version: u64,
    run: TypedModeJobRunner<T>,
}

impl<T: Any + Send> TypedModeJobRequest<T> {
    pub fn new(
        slot: impl Into<ModeJobSlot>,
        version: u64,
        run: impl FnOnce(CancellationToken) -> TypedModeJobResult<T> + Send + 'static,
    ) -> Self {
        Self {
            slot: slot.into(),
            version,
            run: Box::new(run),
        }
    }

    fn erase(self) -> ModeJobRequest {
        ModeJobRequest::new(self.slot, self.version, move |cancellation| {
            (self.run)(cancellation).map(|result| Box::new(result) as Box<dyn Any + Send>)
        })
    }
}

pub trait TypedMode: 'static {
    type ContentState: Clone + PartialEq + 'static;
    type ViewState: Clone + PartialEq + 'static;
    type JobOutput: Any + Send + 'static;

    fn name(&self) -> &ModeName;
    fn actions(&self) -> &[ModeActionName];
    fn adapters(&self) -> ModeAdapters;

    fn before(&self) -> Option<&ModeName> {
        None
    }

    fn faces(&self) -> Vec<(FaceName, Face)> {
        Vec::new()
    }

    fn action_scope(&self, _action: &ModeActionName) -> ModeActionScope {
        ModeActionScope::View
    }

    fn create_content_state(
        &self,
        context: &ModeContentContext<'_>,
    ) -> Result<Self::ContentState, ModeError>;

    fn create_view_state(
        &self,
        content_state: &Self::ContentState,
        context: &ModeViewContext<'_>,
    ) -> Result<Self::ViewState, ModeError>;

    fn execute_content_with_arguments(
        &self,
        _state: &mut Self::ContentState,
        _context: &ModeContentContext<'_>,
        action: &ModeActionName,
        _arguments: &ModeValue,
    ) -> Result<ModeResult, ModeError> {
        Err(ModeError::UnknownAction {
            mode: self.name().clone(),
            action: action.clone(),
        })
    }

    fn on_content_changed(
        &self,
        _state: &mut Self::ContentState,
        _context: &ModeContentContext<'_>,
        _change: &ContentChange,
    ) -> Result<(), ModeError> {
        Ok(())
    }

    fn take_background_jobs(
        &self,
        _state: &mut Self::ContentState,
        _context: &ModeContentContext<'_>,
    ) -> Vec<TypedModeJobRequest<Self::JobOutput>> {
        Vec::new()
    }

    fn apply_background_job(
        &self,
        _state: &mut Self::ContentState,
        _context: &ModeContentContext<'_>,
        _slot: &ModeJobSlot,
        _version: u64,
        _result: TypedModeJobResult<Self::JobOutput>,
    ) -> Result<bool, ModeError> {
        Ok(false)
    }

    fn on_view_content_changed(
        &self,
        _content_state: &mut Self::ContentState,
        _view_state: &mut Self::ViewState,
        _context: &ModeViewContext<'_>,
        _change: &ContentChange,
    ) -> Result<(), ModeError> {
        Ok(())
    }

    fn content_decorations(
        &self,
        _content_state: &Self::ContentState,
        _context: &ModeContentContext<'_>,
        _visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        Vec::new()
    }

    fn view_decorations(
        &self,
        _content_state: &Self::ContentState,
        _view_state: &Self::ViewState,
        _context: &ModeViewContext<'_>,
        _visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        Vec::new()
    }

    fn view_policy(
        &self,
        _content_state: &Self::ContentState,
        _view_state: &Self::ViewState,
        _context: &ModeViewContext<'_>,
    ) -> ModeViewPolicy {
        ModeViewPolicy::default()
    }

    fn input_keymap(
        &self,
        _content_state: &Self::ContentState,
        _view_state: &Self::ViewState,
        _context: &ModeViewContext<'_>,
    ) -> &Keymap<Command> {
        &EMPTY_TYPED_KEYMAP
    }

    fn input_typing(
        &self,
        _content_state: &Self::ContentState,
        _view_state: &Self::ViewState,
        _context: &ModeViewContext<'_>,
        _key: KeyEvent,
    ) -> Option<Command> {
        None
    }

    fn execute_input(
        &self,
        _content_state: &mut Self::ContentState,
        _view_state: &mut Self::ViewState,
        _context: &ModeViewContext<'_>,
        _key: KeyEvent,
    ) -> Result<ModeResult, ModeError> {
        Err(ModeError::UnknownAction {
            mode: self.name().clone(),
            action: ModeActionName::new("<input>"),
        })
    }

    fn mode_input_status(
        &self,
        _content_state: &Self::ContentState,
        _view_state: &Self::ViewState,
        _context: &ModeViewContext<'_>,
    ) -> InputStatus {
        InputStatus::Ready
    }

    fn input_capture(
        &self,
        _content_state: &mut Self::ContentState,
        _view_state: &mut Self::ViewState,
        _context: &ModeViewContext<'_>,
        _key: KeyEvent,
    ) -> InputDecision<Command> {
        InputDecision::Pass
    }

    fn input_timeout(
        &self,
        _content_state: &mut Self::ContentState,
        _view_state: &mut Self::ViewState,
        _context: &ModeViewContext<'_>,
    ) -> ModeResult {
        ModeResult::none()
    }

    fn input_cancel(
        &self,
        _content_state: &mut Self::ContentState,
        _view_state: &mut Self::ViewState,
        _context: &ModeViewContext<'_>,
    ) {
    }

    fn execute_view_with_arguments(
        &self,
        _content_state: &mut Self::ContentState,
        _view_state: &mut Self::ViewState,
        _context: &ModeViewContext<'_>,
        action: &ModeActionName,
        _arguments: &ModeValue,
    ) -> Result<ModeResult, ModeError> {
        Err(ModeError::UnknownAction {
            mode: self.name().clone(),
            action: action.clone(),
        })
    }
}

pub struct ErasedMode<M> {
    mode: M,
}

impl<M> ErasedMode<M> {
    pub fn new(mode: M) -> Self {
        Self { mode }
    }

    pub fn into_inner(self) -> M {
        self.mode
    }
}

impl<M: TypedMode> ErasedMode<M> {
    fn state_error(&self, state: ModeStateKind) -> ModeError {
        ModeError::StateTypeMismatch {
            mode: self.mode.name().clone(),
            state,
        }
    }

    fn content_state<'a>(
        &self,
        state: &'a dyn ModeState,
    ) -> Result<&'a M::ContentState, ModeError> {
        state
            .as_any()
            .downcast_ref()
            .ok_or_else(|| self.state_error(ModeStateKind::Content))
    }

    fn content_state_mut<'a>(
        &self,
        state: &'a mut dyn ModeState,
    ) -> Result<&'a mut M::ContentState, ModeError> {
        state
            .as_any_mut()
            .downcast_mut()
            .ok_or_else(|| self.state_error(ModeStateKind::Content))
    }

    fn view_state<'a>(&self, state: &'a dyn ModeState) -> Result<&'a M::ViewState, ModeError> {
        state
            .as_any()
            .downcast_ref()
            .ok_or_else(|| self.state_error(ModeStateKind::View))
    }

    fn view_state_mut<'a>(
        &self,
        state: &'a mut dyn ModeState,
    ) -> Result<&'a mut M::ViewState, ModeError> {
        state
            .as_any_mut()
            .downcast_mut()
            .ok_or_else(|| self.state_error(ModeStateKind::View))
    }

    fn expect_content_state<'a>(&self, state: &'a dyn ModeState) -> &'a M::ContentState {
        self.content_state(state)
            .expect("ErasedMode owns its content state type")
    }

    fn expect_content_state_mut<'a>(
        &self,
        state: &'a mut dyn ModeState,
    ) -> &'a mut M::ContentState {
        self.content_state_mut(state)
            .expect("ErasedMode owns its content state type")
    }

    fn expect_view_state<'a>(&self, state: &'a dyn ModeState) -> &'a M::ViewState {
        self.view_state(state)
            .expect("ErasedMode owns its view state type")
    }

    fn expect_view_state_mut<'a>(&self, state: &'a mut dyn ModeState) -> &'a mut M::ViewState {
        self.view_state_mut(state)
            .expect("ErasedMode owns its view state type")
    }
}

impl<M: TypedMode> Mode for ErasedMode<M> {
    fn name(&self) -> &ModeName {
        self.mode.name()
    }

    fn actions(&self) -> &[ModeActionName] {
        self.mode.actions()
    }

    fn adapters(&self) -> ModeAdapters {
        self.mode.adapters()
    }

    fn before(&self) -> Option<&ModeName> {
        self.mode.before()
    }

    fn faces(&self) -> Vec<(FaceName, Face)> {
        self.mode.faces()
    }

    fn action_scope(&self, action: &ModeActionName) -> ModeActionScope {
        self.mode.action_scope(action)
    }

    fn create_content_state(
        &self,
        context: &ModeContentContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        self.mode
            .create_content_state(context)
            .map(|state| Box::new(TypedState(state)) as Box<dyn ModeState>)
    }

    fn create_view_state(
        &self,
        content_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        self.mode
            .create_view_state(self.content_state(content_state)?, context)
            .map(|state| Box::new(TypedState(state)) as Box<dyn ModeState>)
    }

    fn execute_content_with_arguments(
        &self,
        state: &mut dyn ModeState,
        context: &ModeContentContext<'_>,
        action: &ModeActionName,
        arguments: &ModeValue,
    ) -> Result<ModeResult, ModeError> {
        self.mode.execute_content_with_arguments(
            self.content_state_mut(state)?,
            context,
            action,
            arguments,
        )
    }

    fn on_content_changed(
        &self,
        state: &mut dyn ModeState,
        context: &ModeContentContext<'_>,
        change: &ContentChange,
    ) -> Result<(), ModeError> {
        self.mode
            .on_content_changed(self.content_state_mut(state)?, context, change)
    }

    fn take_background_jobs(
        &self,
        state: &mut dyn ModeState,
        context: &ModeContentContext<'_>,
    ) -> Vec<ModeJobRequest> {
        self.mode
            .take_background_jobs(self.expect_content_state_mut(state), context)
            .into_iter()
            .map(TypedModeJobRequest::erase)
            .collect()
    }

    fn apply_background_job(
        &self,
        state: &mut dyn ModeState,
        context: &ModeContentContext<'_>,
        slot: &ModeJobSlot,
        version: u64,
        result: ModeJobResult,
    ) -> Result<bool, ModeError> {
        let result = match result {
            Ok(result) => result
                .downcast::<M::JobOutput>()
                .map(|result| Ok(*result))
                .map_err(|_| self.state_error(ModeStateKind::JobOutput))?,
            Err(error) => Err(error),
        };
        self.mode.apply_background_job(
            self.content_state_mut(state)?,
            context,
            slot,
            version,
            result,
        )
    }

    fn on_view_content_changed(
        &self,
        content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
        change: &ContentChange,
    ) -> Result<(), ModeError> {
        self.mode.on_view_content_changed(
            self.content_state_mut(content_state)?,
            self.view_state_mut(view_state)?,
            context,
            change,
        )
    }

    fn content_decorations(
        &self,
        content_state: &dyn ModeState,
        context: &ModeContentContext<'_>,
        visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        self.mode.content_decorations(
            self.expect_content_state(content_state),
            context,
            visible_rows,
        )
    }

    fn view_decorations(
        &self,
        content_state: &dyn ModeState,
        view_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
        visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        self.mode.view_decorations(
            self.expect_content_state(content_state),
            self.expect_view_state(view_state),
            context,
            visible_rows,
        )
    }

    fn view_policy(
        &self,
        content_state: &dyn ModeState,
        view_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> ModeViewPolicy {
        self.mode.view_policy(
            self.expect_content_state(content_state),
            self.expect_view_state(view_state),
            context,
        )
    }

    fn input_keymap<'a>(
        &'a self,
        content_state: &dyn ModeState,
        view_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> &'a Keymap<Command> {
        self.mode.input_keymap(
            self.expect_content_state(content_state),
            self.expect_view_state(view_state),
            context,
        )
    }

    fn input_typing(
        &self,
        content_state: &dyn ModeState,
        view_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> Option<Command> {
        self.mode.input_typing(
            self.expect_content_state(content_state),
            self.expect_view_state(view_state),
            context,
            key,
        )
    }

    fn execute_input(
        &self,
        content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> Result<ModeResult, ModeError> {
        self.mode.execute_input(
            self.content_state_mut(content_state)?,
            self.view_state_mut(view_state)?,
            context,
            key,
        )
    }

    fn mode_input_status(
        &self,
        content_state: &dyn ModeState,
        view_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> InputStatus {
        self.mode.mode_input_status(
            self.expect_content_state(content_state),
            self.expect_view_state(view_state),
            context,
        )
    }

    fn input_capture(
        &self,
        content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> InputDecision<Command> {
        self.mode.input_capture(
            self.expect_content_state_mut(content_state),
            self.expect_view_state_mut(view_state),
            context,
            key,
        )
    }

    fn input_timeout(
        &self,
        content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> ModeResult {
        self.mode.input_timeout(
            self.expect_content_state_mut(content_state),
            self.expect_view_state_mut(view_state),
            context,
        )
    }

    fn input_cancel(
        &self,
        content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
    ) {
        self.mode.input_cancel(
            self.expect_content_state_mut(content_state),
            self.expect_view_state_mut(view_state),
            context,
        );
    }

    fn execute_view_with_arguments(
        &self,
        content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
        action: &ModeActionName,
        arguments: &ModeValue,
    ) -> Result<ModeResult, ModeError> {
        self.mode.execute_view_with_arguments(
            self.content_state_mut(content_state)?,
            self.view_state_mut(view_state)?,
            context,
            action,
            arguments,
        )
    }
}

impl ModeRegistry {
    pub fn register_typed<M: TypedMode>(
        &mut self,
        mode: M,
    ) -> Result<crate::ModeId, ModeRegistrationError> {
        self.register(ErasedMode::new(mode))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vell_core::buffer::Buffer;
    use vell_core::content::Content;
    use vell_core::content_store::ContentStore;
    use vell_protocol::ids::ContentId;

    struct CounterMode {
        name: ModeName,
        actions: Vec<ModeActionName>,
    }

    impl TypedMode for CounterMode {
        type ContentState = u8;
        type ViewState = ();
        type JobOutput = u8;

        fn name(&self) -> &ModeName {
            &self.name
        }

        fn actions(&self) -> &[ModeActionName] {
            &self.actions
        }

        fn adapters(&self) -> ModeAdapters {
            ModeAdapters::buffer()
        }

        fn create_content_state(
            &self,
            _context: &ModeContentContext<'_>,
        ) -> Result<Self::ContentState, ModeError> {
            Ok(0)
        }

        fn create_view_state(
            &self,
            _content_state: &Self::ContentState,
            _context: &ModeViewContext<'_>,
        ) -> Result<Self::ViewState, ModeError> {
            Ok(())
        }

        fn execute_content_with_arguments(
            &self,
            state: &mut Self::ContentState,
            _context: &ModeContentContext<'_>,
            _action: &ModeActionName,
            _arguments: &ModeValue,
        ) -> Result<ModeResult, ModeError> {
            *state += 1;
            Ok(ModeResult::none())
        }

        fn take_background_jobs(
            &self,
            _state: &mut Self::ContentState,
            _context: &ModeContentContext<'_>,
        ) -> Vec<TypedModeJobRequest<Self::JobOutput>> {
            vec![TypedModeJobRequest::new("count", 1, |_| Ok(3))]
        }

        fn apply_background_job(
            &self,
            state: &mut Self::ContentState,
            _context: &ModeContentContext<'_>,
            _slot: &ModeJobSlot,
            _version: u64,
            result: TypedModeJobResult<Self::JobOutput>,
        ) -> Result<bool, ModeError> {
            *state += result.map_err(|message| ModeError::CallbackFailed {
                mode: self.name.clone(),
                message,
            })?;
            Ok(true)
        }
    }

    #[test]
    fn erased_mode_centralizes_state_and_job_output_types() {
        let content = ContentId(1);
        let mut contents = ContentStore::default();
        contents
            .insert(content, Content::Buffer(Buffer::new()))
            .unwrap();
        let context = ModeContentContext::new(content, &contents);
        let action = ModeActionName::new("increment");
        let mode = ErasedMode::new(CounterMode {
            name: ModeName::new("typed-counter"),
            actions: vec![action.clone()],
        });
        let mut state = Mode::create_content_state(&mode, &context).unwrap();

        Mode::execute_content_with_arguments(
            &mode,
            state.as_mut(),
            &context,
            &action,
            &ModeValue::Null,
        )
        .unwrap();
        let request = Mode::take_background_jobs(&mode, state.as_mut(), &context)
            .pop()
            .unwrap();
        let (slot, version, run) = request.into_parts();
        let output = run(CancellationToken::new());
        Mode::apply_background_job(&mode, state.as_mut(), &context, &slot, version, output)
            .unwrap();

        assert_eq!(state.as_any().downcast_ref::<u8>(), Some(&4));

        let mut invalid: Box<dyn ModeState> = Box::new(String::new());
        assert!(matches!(
            Mode::execute_content_with_arguments(
                &mode,
                invalid.as_mut(),
                &context,
                &action,
                &ModeValue::Null,
            ),
            Err(ModeError::StateTypeMismatch {
                state: ModeStateKind::Content,
                ..
            })
        ));
    }
}
