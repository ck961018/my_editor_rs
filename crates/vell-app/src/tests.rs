use std::cell::{Cell, RefCell};
use std::io;
use std::rc::Rc;

use super::App;
use super::behavior::{
    BehaviorRecorder, BehaviorSnapshot, EffectBehavior, ExecutionOutcome, ModeFaultBehavior,
    ModeFaultScope, ModeProbeBehavior,
};
use super::bootstrap::{bootstrap_editor, create_editor_session};
use super::command_resolver::default_global_keymap;
use super::dispatcher::{DispatchCommand, Dispatcher};
use super::layout::{LayoutError, NewView, StatusBarPlacement};
use super::message::{AppMessage, OpenedBuffer, OpenedPath};
use super::query::AppQuery;
use super::view::View;
use super::view_workspace::{focusable_view_count, resolve_focus, scene_views, view_for_space};
use crate::action::{TransactionIntent, ViewAction};
use crate::buffer_lifecycle::normalize_path;
use crate::command::{
    AppCommand, Command, ContentCommand, ModeCommand, ModeValue, TransactionCommand,
};
use crate::kernel::FileBaseline;
use crate::mode::{
    Mode, ModeActionScope, ModeAdapters, ModeAttachmentError, ModeContentContext, ModeError,
    ModeFaultPhase, ModeResult, ModeState, ModeViewContext, ModeViewInstance, ModeViewPolicy,
    NamedStatusBarPresentation, NamedStatusBarSegment,
};
use crate::mode_name::{ModeActionName, ModeName};
use crate::operation::{
    AppOperation, BufferViewSource, ClipboardDestination, ClipboardOperation, ClipboardSource,
    ContentLifecycleOperation, ContentOperation, ContentTarget, FaceOperation, FaceRemapTarget,
    ModeFlowPropagation, ModeInvocation, ModeTarget, OperationRequest, SearchOperation,
    ViewEditPlan, ViewLifecycleOperation, ViewOperation, ViewPrecondition, ViewSpec, ViewTarget,
};
use std::collections::VecDeque;
use vell_core::action::ContentAction;
use vell_core::buffer::Buffer;
use vell_core::clipboard::{ClipboardKind, PastePlacement};
use vell_core::command::EditCommand;
use vell_core::content::{
    Content, ContentChange, ContentEffect, ContentInput, ContentKind, ContentResult,
};
use vell_core::keymap::Keymap;
use vell_core::search::{CaseSensitivity, SearchDirection, SearchOptions, SearchPattern};
use vell_core::transaction::{TextChangeSet, TextEdit};
use vell_frontend::Frontend;
use vell_mode::command_registry::{
    CommandCompletion, CommandEntry, CommandError, CommandHost, CommandId, CommandInvocation,
    CommandQuery, CommandRequest, CommandValue,
};
use vell_plugin_v8::ScriptHost;
use vell_protocol::content_query::{
    BufferBackingState, Color, ContentData, ContentQuery, CursorStyle, DirtyState, Face, FaceExpr,
    FaceName, NamedTextDecoration, RenderQuery, RenderQueryError, RowRange, SaveState,
    StatusBarPresentation, TextPresentation, ViewData, ViewPresentation,
};
use vell_protocol::frontend_event::{FrontendEvent, ResizeEvent};
use vell_protocol::ids::{ContentId, SpaceId, ViewId};
use vell_protocol::key_event::{ArrowKey, KeyCode, KeyEvent};
use vell_protocol::revision::Revision;
use vell_protocol::scene::Scene;
use vell_protocol::selection::{Selection, Selections, TextOffset, TextPoint};
use vell_protocol::space::{Sizing, SpaceKind, SplitDirection};
use vell_protocol::viewport::{
    ResolvedViewportCommand, ViewportCommand, ViewportCursorBehavior, ViewportMoveAmount,
    ViewportMoveDirection,
};

mod baseline;

struct ScriptedFrontend {
    events: VecDeque<FrontendEvent>,
    next_event_at: Option<tokio::time::Instant>,
    // When set, next_event waits until a render carries decorations for the
    // given view body pane (bounded by next_event_at) before delivering the
    // first event. Lets tests that rely on async worker results wait
    // adaptively on slow CI runners instead of a fixed wall-clock window.
    wait_for_decorations: Option<(ViewId, SpaceId, RowRange)>,
    decorations_seen: bool,
    renders: usize,
    scene_revisions: Vec<Revision>,
    fail_next_event: bool,
    fail_render: bool,
    fail_viewport: bool,
    viewport_height: usize,
    viewport_commands: Vec<(ViewId, ResolvedViewportCommand)>,
    focus_targets: VecDeque<Option<SpaceId>>,
    focus_directions: Vec<SplitDirection>,
    system_clipboard: Option<String>,
    clipboard_writes: Vec<String>,
    fail_clipboard_read: bool,
    fail_clipboard_write: bool,
}

struct LoopMode {
    name: ModeName,
    actions: Vec<ModeActionName>,
    keymap: Keymap<Command>,
}

struct CaptureFailureMode {
    name: ModeName,
    keymap: Keymap<Command>,
}

struct PresentationMutationMode {
    name: ModeName,
    keymap: Keymap<Command>,
}

struct ContentAwareKeymapMode {
    name: ModeName,
    actions: Vec<ModeActionName>,
    empty_keymap: Keymap<Command>,
    nonempty_keymap: Keymap<Command>,
}

struct SharedContentMode {
    name: ModeName,
    actions: Vec<ModeActionName>,
    keymap: Keymap<Command>,
}

struct AdapterProbeMode {
    name: ModeName,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AdapterProbeState {
    kind: ContentKind,
}

#[derive(Clone, PartialEq, Eq)]
struct SharedContentState {
    executions: u8,
}

#[derive(Clone, PartialEq, Eq)]
struct SharedViewState {
    awaiting: bool,
}

struct ChainProbeMode {
    name: ModeName,
    actions: Vec<ModeActionName>,
    keymap: Keymap<Command>,
    operations: Vec<OperationRequest>,
    continue_input: bool,
}

fn view_edit(command: EditCommand) -> OperationRequest {
    OperationRequest::View {
        target: ViewTarget::Current,
        operation: ViewOperation::Edit(command),
    }
}

fn view_content(action: ContentAction) -> OperationRequest {
    OperationRequest::View {
        target: ViewTarget::Current,
        operation: ViewOperation::ApplyContent(action),
    }
}

fn content_action(action: ContentAction) -> OperationRequest {
    OperationRequest::Content {
        target: ContentTarget::Current,
        operation: ContentOperation::Apply(action),
    }
}

fn history(operation: TransactionIntent) -> OperationRequest {
    OperationRequest::History {
        target: ContentTarget::Current,
        operation,
    }
}

fn save() -> OperationRequest {
    OperationRequest::Content {
        target: ContentTarget::Current,
        operation: ContentOperation::Save,
    }
}

fn nested_mode(command: ModeCommand) -> OperationRequest {
    OperationRequest::Mode {
        target: ModeTarget::CurrentView,
        invocation: ModeInvocation {
            command,
            nested: true,
            flow: ModeFlowPropagation::Propagate,
        },
    }
}

fn app_command(command: AppCommand) -> OperationRequest {
    OperationRequest::App(AppOperation::Command(command))
}

fn viewport(command: ViewportCommand) -> OperationRequest {
    OperationRequest::View {
        target: ViewTarget::Current,
        operation: ViewOperation::Viewport(command),
    }
}

fn view_action(action: ViewAction) -> OperationRequest {
    OperationRequest::View {
        target: ViewTarget::Current,
        operation: ViewOperation::Apply(action),
    }
}

fn clipboard(operation: ClipboardOperation) -> OperationRequest {
    OperationRequest::Clipboard {
        target: ViewTarget::Current,
        operation,
    }
}

fn search(operation: SearchOperation) -> OperationRequest {
    OperationRequest::Search {
        target: ViewTarget::Current,
        operation,
    }
}

fn search_options(direction: SearchDirection, wrap: bool) -> SearchOptions {
    SearchOptions {
        case: CaseSensitivity::Sensitive,
        direction,
        wrap,
    }
}

struct HighlightMode {
    name: ModeName,
    syntax_color: Color,
}

struct PresentationProbeMode {
    name: ModeName,
    calls: Rc<Cell<usize>>,
    max_rows: Option<Rc<Cell<usize>>>,
}

struct FaultingHighlightMode {
    name: ModeName,
}

struct FactoryFaultMode {
    name: ModeName,
    fail_content: bool,
}

struct ArgumentProbeMode {
    name: ModeName,
    actions: Vec<ModeActionName>,
}

struct SaveStatusMode {
    name: ModeName,
}

impl Mode for HighlightMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &[]
    }

    fn adapters(&self) -> ModeAdapters {
        ModeAdapters::buffer()
    }

    fn create_content_state(
        &self,
        context: &ModeContentContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        assert_eq!(context.content_id(), editor_cid());
        assert_eq!(
            context
                .buffer()
                .expect("highlight mode has a Buffer adapter")
                .text_snapshot()
                .expect("text mode requires a snapshot")
                .to_owned_string(),
            String::new()
        );
        assert_eq!(context.content_revision(), Some(Revision(0)));
        Ok(Box::new(true))
    }

    fn create_view_state(
        &self,
        content_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        assert_eq!(content_state.as_any().downcast_ref::<bool>(), Some(&true));
        assert_eq!(context.content_id(), editor_cid());
        assert_eq!(context.content_revision(), Some(Revision(0)));
        Ok(Box::new(()))
    }

    fn faces(&self) -> Vec<(FaceName, Face)> {
        vec![
            (
                FaceName::new("plugin.highlight-test.syntax"),
                Face {
                    foreground: Some(self.syntax_color),
                    ..Face::default()
                },
            ),
            (
                FaceName::new("selection.test"),
                Face {
                    background: Some(Color::Ansi(4)),
                    ..Face::default()
                },
            ),
        ]
    }

    fn view_policy(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> ModeViewPolicy {
        ModeViewPolicy {
            selection_face: Some(FaceName::new("selection.test")),
            tab_width: Some(8),
            ..ModeViewPolicy::default()
        }
    }

    fn view_decorations(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        _visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        vec![NamedTextDecoration {
            start: TextOffset { char_index: 0 },
            end: TextOffset { char_index: 1 },
            face: FaceName::new("plugin.highlight-test.syntax"),
        }]
    }
}

impl Mode for PresentationProbeMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &[]
    }

    fn adapters(&self) -> ModeAdapters {
        ModeAdapters::buffer()
    }

    fn content_decorations(
        &self,
        _content_state: &dyn ModeState,
        _context: &ModeContentContext<'_>,
        visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        assert_ne!(visible_rows.end, usize::MAX);
        if let Some(max_rows) = &self.max_rows {
            max_rows.set(max_rows.get().max(visible_rows.end));
        }
        self.calls.set(self.calls.get() + 1);
        Vec::new()
    }

    fn view_policy(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> ModeViewPolicy {
        self.calls.set(self.calls.get() + 1);
        ModeViewPolicy::default()
    }

    fn view_decorations(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        assert_ne!(visible_rows.end, usize::MAX);
        if let Some(max_rows) = &self.max_rows {
            max_rows.set(max_rows.get().max(visible_rows.end));
        }
        self.calls.set(self.calls.get() + 1);
        Vec::new()
    }
}

impl Mode for FaultingHighlightMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &[]
    }

    fn adapters(&self) -> ModeAdapters {
        ModeAdapters::buffer()
    }

    fn view_decorations(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        _visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        vec![NamedTextDecoration {
            start: TextOffset { char_index: 0 },
            end: TextOffset { char_index: 1 },
            face: FaceName::new("fault.test"),
        }]
    }

    fn on_content_changed(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeContentContext<'_>,
        _change: &ContentChange,
    ) -> Result<(), ModeError> {
        Err(ModeError::UnknownAction {
            mode: self.name.clone(),
            action: ModeActionName::new("content-changed"),
        })
    }
}

impl Mode for FactoryFaultMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &[]
    }

    fn adapters(&self) -> ModeAdapters {
        ModeAdapters::buffer()
    }

    fn create_content_state(
        &self,
        _context: &ModeContentContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        if self.fail_content {
            return Err(ModeError::CallbackFailed {
                mode: self.name.clone(),
                message: "content factory".to_string(),
            });
        }
        Ok(Box::new(()))
    }

    fn create_view_state(
        &self,
        _content_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        Err(ModeError::CallbackFailed {
            mode: self.name.clone(),
            message: "view factory".to_string(),
        })
    }

    fn view_decorations(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        _visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        vec![NamedTextDecoration {
            start: TextOffset { char_index: 0 },
            end: TextOffset { char_index: 1 },
            face: FaceName::new("unexpected.factory-decoration"),
        }]
    }
}

impl Mode for ArgumentProbeMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &self.actions
    }

    fn adapters(&self) -> ModeAdapters {
        ModeAdapters::buffer()
    }

    fn execute_view_with_arguments(
        &self,
        _content_state: &mut dyn ModeState,
        _view_state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        action: &ModeActionName,
        arguments: &ModeValue,
    ) -> Result<ModeResult, ModeError> {
        if action != &self.actions[0] {
            return Err(ModeError::UnknownAction {
                mode: self.name.clone(),
                action: action.clone(),
            });
        }
        let ModeValue::String(text) = arguments else {
            return Ok(ModeResult::none());
        };
        Ok(ModeResult::operations(vec![view_edit(
            EditCommand::InsertText(text.clone()),
        )]))
    }
}

impl ChainProbeMode {
    fn new(name: &str, operations: Vec<OperationRequest>, continue_input: bool) -> Self {
        Self::with_sequence(name, vec![KeyEvent::char('q')], operations, continue_input)
    }

    fn with_sequence(
        name: &str,
        sequence: Vec<KeyEvent>,
        operations: Vec<OperationRequest>,
        continue_input: bool,
    ) -> Self {
        let name = ModeName::new(name);
        let actions = vec![ModeActionName::new("run")];
        let mut keymap = Keymap::new();
        keymap.bind(
            sequence,
            Command::Mode(ModeCommand {
                mode: name.clone(),
                action: actions[0].clone(),
                arguments: Default::default(),
            }),
        );
        Self {
            name,
            actions,
            keymap,
            operations,
            continue_input,
        }
    }
}

impl Mode for ChainProbeMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &self.actions
    }

    fn adapters(&self) -> ModeAdapters {
        ModeAdapters::buffer()
    }

    fn input_keymap<'a>(
        &'a self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> &'a Keymap<Command> {
        &self.keymap
    }

    fn execute_view_with_arguments(
        &self,
        _content_state: &mut dyn ModeState,
        _view_state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        action: &ModeActionName,
        _arguments: &ModeValue,
    ) -> Result<ModeResult, ModeError> {
        if action != &self.actions[0] {
            return Err(ModeError::UnknownAction {
                mode: self.name.clone(),
                action: action.clone(),
            });
        }
        Ok(if self.continue_input {
            ModeResult::continue_with(self.operations.clone())
        } else {
            ModeResult::operations(self.operations.clone())
        })
    }
}

impl SharedContentMode {
    fn new() -> Self {
        let name = ModeName::new("shared-content");
        let actions = vec![ModeActionName::new("advance")];
        let mut keymap = Keymap::new();
        keymap.bind(
            KeyEvent::char('q'),
            Command::Mode(ModeCommand {
                mode: name.clone(),
                action: actions[0].clone(),
                arguments: Default::default(),
            }),
        );
        Self {
            name,
            actions,
            keymap,
        }
    }
}

impl Mode for SharedContentMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &self.actions
    }

    fn adapters(&self) -> ModeAdapters {
        ModeAdapters::buffer()
    }

    fn action_scope(&self, _action: &ModeActionName) -> ModeActionScope {
        ModeActionScope::Content
    }

    fn create_content_state(
        &self,
        _context: &ModeContentContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        Ok(Box::new(SharedContentState { executions: 0 }))
    }

    fn create_view_state(
        &self,
        _content_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        Ok(Box::new(SharedViewState { awaiting: false }))
    }

    fn input_keymap<'a>(
        &'a self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> &'a Keymap<Command> {
        &self.keymap
    }

    fn input_typing(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        _key: KeyEvent,
    ) -> Option<Command> {
        None
    }

    fn mode_input_status(
        &self,
        _content_state: &dyn ModeState,
        view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> vell_core::input::InputStatus {
        if view_state
            .as_any()
            .downcast_ref::<SharedViewState>()
            .expect("shared mode owns its view state")
            .awaiting
        {
            vell_core::input::InputStatus::Awaiting(vell_core::input::TimeoutPolicy::Never)
        } else {
            vell_core::input::InputStatus::Ready
        }
    }

    fn input_capture(
        &self,
        _content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> vell_core::input::InputDecision<Command> {
        if key != KeyEvent::char('x') {
            return vell_core::input::InputDecision::Pass;
        }
        view_state
            .as_any_mut()
            .downcast_mut::<SharedViewState>()
            .expect("shared mode owns its view state")
            .awaiting = true;
        vell_core::input::InputDecision::Consumed
    }

    fn input_cancel(
        &self,
        _content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) {
        view_state
            .as_any_mut()
            .downcast_mut::<SharedViewState>()
            .expect("shared mode owns its view state")
            .awaiting = false;
    }

    fn execute_content_with_arguments(
        &self,
        state: &mut dyn ModeState,
        context: &ModeContentContext<'_>,
        _action: &ModeActionName,
        _arguments: &ModeValue,
    ) -> Result<ModeResult, ModeError> {
        assert_eq!(context.content_id(), editor_cid());
        let count = state
            .as_any_mut()
            .downcast_mut::<SharedContentState>()
            .expect("shared mode owns its content state");
        count.executions += 1;
        Ok(ModeResult::operations(vec![match count.executions {
            1 => history(TransactionIntent::Undo),
            2 => history(TransactionIntent::Redo),
            _ => save(),
        }]))
    }
}

impl Mode for AdapterProbeMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &[]
    }

    fn adapters(&self) -> ModeAdapters {
        ModeAdapters::buffer()
    }

    fn create_content_state(
        &self,
        context: &ModeContentContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        assert!(context.buffer().is_some());
        Ok(Box::new(AdapterProbeState {
            kind: context.content_kind(),
        }))
    }

    fn create_view_state(
        &self,
        _content_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        assert_eq!(
            context.buffer().unwrap().selections().primary().head(),
            TextOffset::origin()
        );
        Ok(Box::new(AdapterProbeState {
            kind: context.content_kind(),
        }))
    }
}

impl Mode for SaveStatusMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &[]
    }

    fn adapters(&self) -> ModeAdapters {
        ModeAdapters::buffer()
    }

    fn view_policy(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> ModeViewPolicy {
        let buffer = context.buffer().expect("save status mode targets a buffer");
        let save_state = buffer.save_state().expect("buffer has save state");
        let backing_state = buffer.backing_state().expect("buffer has backing state");
        ModeViewPolicy {
            status_bar: Some(NamedStatusBarPresentation {
                center: vec![NamedStatusBarSegment {
                    text: format!("{backing_state:?}/{save_state:?}"),
                    face: None,
                }],
                ..NamedStatusBarPresentation::default()
            }),
            ..ModeViewPolicy::default()
        }
    }
}

impl LoopMode {
    fn new() -> Self {
        Self {
            name: ModeName::new("loop"),
            actions: vec![ModeActionName::new("again")],
            keymap: Keymap::new(),
        }
    }
}

impl CaptureFailureMode {
    fn new() -> Self {
        Self {
            name: ModeName::new("capture-failure"),
            keymap: Keymap::new(),
        }
    }
}

impl PresentationMutationMode {
    fn new() -> Self {
        Self {
            name: ModeName::new("presentation-mutation"),
            keymap: Keymap::new(),
        }
    }
}

impl ContentAwareKeymapMode {
    fn new() -> Self {
        let name = ModeName::new("content-aware-keymap");
        let actions = vec![ModeActionName::new("insert")];
        let mut empty_keymap = Keymap::new();
        empty_keymap.bind(
            KeyEvent::char('q'),
            Command::Mode(ModeCommand {
                mode: name.clone(),
                action: actions[0].clone(),
                arguments: Default::default(),
            }),
        );
        let mut nonempty_keymap = Keymap::new();
        nonempty_keymap.bind(KeyEvent::char('q'), Command::Noop);
        Self {
            name,
            actions,
            empty_keymap,
            nonempty_keymap,
        }
    }
}

impl Mode for PresentationMutationMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &[]
    }

    fn adapters(&self) -> ModeAdapters {
        ModeAdapters::buffer()
    }

    fn create_view_state(
        &self,
        _content_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        Ok(Box::new(false))
    }

    fn input_keymap<'a>(
        &'a self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> &'a Keymap<Command> {
        &self.keymap
    }

    fn input_typing(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        _key: KeyEvent,
    ) -> Option<Command> {
        None
    }

    fn mode_input_status(
        &self,
        _content_state: &dyn ModeState,
        view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> vell_core::input::InputStatus {
        if *view_state.as_any().downcast_ref::<bool>().unwrap() {
            vell_core::input::InputStatus::Ready
        } else {
            vell_core::input::InputStatus::Awaiting(vell_core::input::TimeoutPolicy::After(
                std::time::Duration::ZERO,
            ))
        }
    }

    fn input_capture(
        &self,
        _content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> vell_core::input::InputDecision<Command> {
        if key != KeyEvent::char('x') {
            return vell_core::input::InputDecision::Pass;
        }
        *view_state.as_any_mut().downcast_mut::<bool>().unwrap() = true;
        vell_core::input::InputDecision::Consumed
    }

    fn input_timeout(
        &self,
        _content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> ModeResult {
        *view_state.as_any_mut().downcast_mut::<bool>().unwrap() = true;
        ModeResult::none()
    }

    fn view_policy(
        &self,
        _content_state: &dyn ModeState,
        view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> ModeViewPolicy {
        ModeViewPolicy {
            cursor_style: Some(if *view_state.as_any().downcast_ref::<bool>().unwrap() {
                CursorStyle::Bar
            } else {
                CursorStyle::Default
            }),
            ..ModeViewPolicy::default()
        }
    }

    fn execute_view_with_arguments(
        &self,
        _content_state: &mut dyn ModeState,
        _view_state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        action: &ModeActionName,
        _arguments: &ModeValue,
    ) -> Result<ModeResult, ModeError> {
        Err(ModeError::UnknownAction {
            mode: self.name.clone(),
            action: action.clone(),
        })
    }
}

impl Mode for ContentAwareKeymapMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &self.actions
    }

    fn adapters(&self) -> ModeAdapters {
        ModeAdapters::buffer()
    }

    fn action_scope(&self, _action: &ModeActionName) -> ModeActionScope {
        ModeActionScope::Content
    }

    fn input_keymap<'a>(
        &'a self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> &'a Keymap<Command> {
        match context
            .buffer()
            .expect("content-aware mode has a Buffer adapter")
            .text_rows(RowRange { start: 0, end: 1 })
        {
            Some(rows) if rows.first().is_some_and(String::is_empty) => &self.empty_keymap,
            Some(_) => &self.nonempty_keymap,
            None => unreachable!("content-aware mode is bound to text content"),
        }
    }

    fn input_typing(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        _key: KeyEvent,
    ) -> Option<Command> {
        None
    }

    fn execute_content_with_arguments(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeContentContext<'_>,
        action: &ModeActionName,
        _arguments: &ModeValue,
    ) -> Result<ModeResult, ModeError> {
        if action != &self.actions[0] {
            return Err(ModeError::UnknownAction {
                mode: self.name.clone(),
                action: action.clone(),
            });
        }
        Ok(ModeResult::operations(vec![content_action(
            ContentAction::Text(
                TextChangeSet::from_edits(0, vec![TextEdit::new(0..0, "a")]).unwrap(),
            ),
        )]))
    }
}

impl Mode for CaptureFailureMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &[]
    }

    fn adapters(&self) -> ModeAdapters {
        ModeAdapters::buffer()
    }

    fn create_view_state(
        &self,
        _content_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        Ok(Box::new(0_u8))
    }

    fn input_keymap<'a>(
        &'a self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> &'a Keymap<Command> {
        assert_eq!(context.content_id(), editor_cid());
        &self.keymap
    }

    fn input_typing(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        _key: KeyEvent,
    ) -> Option<Command> {
        None
    }

    fn mode_input_status(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> vell_core::input::InputStatus {
        vell_core::input::InputStatus::Awaiting(vell_core::input::TimeoutPolicy::After(
            std::time::Duration::ZERO,
        ))
    }

    fn input_capture(
        &self,
        _content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
        _key: KeyEvent,
    ) -> vell_core::input::InputDecision<Command> {
        assert_eq!(context.view_id(), ViewId(0));
        *view_state
            .as_any_mut()
            .downcast_mut::<u8>()
            .expect("capture failure mode owns its state") = 1;
        vell_core::input::InputDecision::Emit(Command::Mode(ModeCommand {
            mode: ModeName::new("missing"),
            action: ModeActionName::new("missing"),
            arguments: Default::default(),
        }))
    }

    fn input_timeout(
        &self,
        _content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> ModeResult {
        assert_eq!(context.view_id(), ViewId(0));
        *view_state
            .as_any_mut()
            .downcast_mut::<u8>()
            .expect("capture failure mode owns its state") = 1;
        ModeResult::operations(vec![nested_mode(ModeCommand {
            mode: ModeName::new("missing"),
            action: ModeActionName::new("missing"),
            arguments: Default::default(),
        })])
    }

    fn view_policy(
        &self,
        _content_state: &dyn ModeState,
        view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> ModeViewPolicy {
        ModeViewPolicy {
            cursor_style: Some(
                if *view_state
                    .as_any()
                    .downcast_ref::<u8>()
                    .expect("capture failure mode owns its state")
                    == 0
                {
                    CursorStyle::Default
                } else {
                    CursorStyle::Bar
                },
            ),
            ..ModeViewPolicy::default()
        }
    }

    fn execute_view_with_arguments(
        &self,
        _content_state: &mut dyn ModeState,
        _view_state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        action: &ModeActionName,
        _arguments: &ModeValue,
    ) -> Result<ModeResult, ModeError> {
        Err(ModeError::UnknownAction {
            mode: self.name.clone(),
            action: action.clone(),
        })
    }
}

impl Mode for LoopMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &self.actions
    }

    fn adapters(&self) -> ModeAdapters {
        ModeAdapters::buffer()
    }

    fn create_view_state(
        &self,
        _content_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        Ok(Box::new(0_u16))
    }

    fn input_keymap<'a>(
        &'a self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> &'a Keymap<Command> {
        &self.keymap
    }

    fn input_typing(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        _key: KeyEvent,
    ) -> Option<Command> {
        None
    }

    fn view_policy(
        &self,
        _content_state: &dyn ModeState,
        view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> ModeViewPolicy {
        ModeViewPolicy {
            cursor_style: Some(
                if *view_state
                    .as_any()
                    .downcast_ref::<u16>()
                    .expect("loop mode owns its state")
                    == 0
                {
                    CursorStyle::Default
                } else {
                    CursorStyle::Bar
                },
            ),
            ..ModeViewPolicy::default()
        }
    }

    fn execute_view_with_arguments(
        &self,
        _content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
        _action: &ModeActionName,
        _arguments: &ModeValue,
    ) -> Result<ModeResult, ModeError> {
        *view_state
            .as_any_mut()
            .downcast_mut::<u16>()
            .expect("loop mode owns its state") += 1;
        assert_eq!(context.content_id(), editor_cid());
        let _ = context.view_id();
        let buffer = context.buffer().expect("loop mode has a Buffer adapter");
        let _ = buffer.selections();
        let _ = buffer.resource_name();
        let _ = buffer.backing_state();
        let _ = buffer.dirty_state();
        let _ = buffer.save_state();
        let _ = buffer.text_metrics();
        let rows = buffer
            .text_rows(RowRange { start: 0, end: 1 })
            .expect("loop mode is bound to text content");
        let offset = rows[0].chars().count();
        let change = TextChangeSet::from_edits(offset, vec![TextEdit::new(offset..offset, "x")])
            .expect("loop mode creates a valid insertion");
        Ok(ModeResult::operations(vec![
            view_content(ContentAction::Text(change)),
            nested_mode(ModeCommand {
                mode: self.name.clone(),
                action: self.actions[0].clone(),
                arguments: Default::default(),
            }),
        ]))
    }
}

impl ScriptedFrontend {
    fn new(events: Vec<FrontendEvent>) -> Self {
        Self {
            events: events.into(),
            next_event_at: None,
            wait_for_decorations: None,
            decorations_seen: false,
            renders: 0,
            scene_revisions: Vec::new(),
            fail_next_event: false,
            fail_render: false,
            fail_viewport: false,
            viewport_height: 4,
            viewport_commands: Vec::new(),
            focus_targets: VecDeque::new(),
            focus_directions: Vec::new(),
            system_clipboard: None,
            clipboard_writes: Vec::new(),
            fail_clipboard_read: false,
            fail_clipboard_write: false,
        }
    }
}

impl Frontend for ScriptedFrontend {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        if let Some(deadline) = self.next_event_at {
            if self.wait_for_decorations.is_some() {
                while !self.decorations_seen && tokio::time::Instant::now() < deadline {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            } else {
                tokio::time::sleep_until(deadline).await;
            }
        }
        if self.fail_next_event {
            self.fail_next_event = false;
            return Err(io::Error::other("scripted frontend failure"));
        }
        Ok(self.events.pop_front())
    }

    fn read_clipboard(&mut self) -> io::Result<Option<String>> {
        if self.fail_clipboard_read {
            return Err(io::Error::other("scripted clipboard read failure"));
        }
        Ok(self.system_clipboard.clone())
    }

    fn write_clipboard(&mut self, text: &str) -> io::Result<()> {
        if self.fail_clipboard_write {
            return Err(io::Error::other("scripted clipboard write failure"));
        }
        self.system_clipboard = Some(text.to_owned());
        self.clipboard_writes.push(text.to_owned());
        Ok(())
    }

    fn render(
        &mut self,
        _scene: &Scene,
        scene_revision: Revision,
        query: &dyn RenderQuery,
        _focused: SpaceId,
    ) -> io::Result<()> {
        self.renders += 1;
        self.scene_revisions.push(scene_revision);
        if let Some((view, space, rows)) = self.wait_for_decorations
            && !self.decorations_seen
            && query
                .decorations(view, space, rows)
                .is_ok_and(|decorations| !decorations.is_empty())
        {
            self.decorations_seen = true;
        }
        if self.fail_render {
            self.fail_render = false;
            return Err(io::Error::other("scripted render failure"));
        }
        Ok(())
    }

    fn resolve_viewport_command(
        &mut self,
        _scene: &Scene,
        _scene_revision: Revision,
        _space: SpaceId,
        cursor_row: usize,
        command: ViewportCommand,
    ) -> io::Result<ResolvedViewportCommand> {
        if self.fail_viewport {
            self.fail_viewport = false;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "scripted viewport failure",
            ));
        }
        Ok(match command {
            ViewportCommand::Scroll {
                direction, amount, ..
            } => {
                let lines = match amount {
                    ViewportMoveAmount::HalfPage => (self.viewport_height / 2).max(1),
                    ViewportMoveAmount::FullPage => self.viewport_height,
                };
                ResolvedViewportCommand::Scroll { direction, lines }
            }
            ViewportCommand::Align { alignment } => ResolvedViewportCommand::SetTopRow {
                top_row: cursor_row.saturating_sub(alignment.row_offset(self.viewport_height)),
            },
        })
    }

    fn apply_viewport_command(&mut self, view: ViewId, command: ResolvedViewportCommand) {
        self.viewport_commands.push((view, command));
    }

    fn resolve_focus_direction(
        &mut self,
        _scene: &Scene,
        _scene_revision: Revision,
        _focused: SpaceId,
        direction: SplitDirection,
    ) -> io::Result<Option<SpaceId>> {
        self.focus_directions.push(direction);
        Ok(self.focus_targets.pop_front().flatten())
    }
}

fn make_app(events: Vec<FrontendEvent>, path: Option<&str>) -> App<ScriptedFrontend> {
    let mut configuration = vell_plugin_v8::load_default_configuration().unwrap();
    let commands = configuration
        .prepare_commands(&crate::native_command_ids())
        .unwrap();
    let mut app = App::with_modes_visuals_and_backgrounds(
        path,
        40,
        5,
        ScriptedFrontend::new(events),
        configuration.modes,
        configuration.backgrounds,
        configuration.theme,
        configuration.face_overrides,
    )
    .unwrap();
    for command in commands {
        app.register_command(command);
    }
    app
}

async fn send_key(app: &mut App<ScriptedFrontend>, key: KeyEvent) {
    app.handle_event(FrontendEvent::Key(key)).await.unwrap();
}

async fn send_text(app: &mut App<ScriptedFrontend>, text: &str) {
    for character in text.chars() {
        send_key(app, KeyEvent::char(character)).await;
    }
}

async fn send_vim_command(app: &mut App<ScriptedFrontend>, command: &str) {
    send_key(app, KeyEvent::char(':')).await;
    send_text(app, command).await;
    send_key(app, KeyEvent::plain(KeyCode::Enter)).await;
}

fn vim_command_events(command: &str) -> Vec<FrontendEvent> {
    std::iter::once(':')
        .chain(command.chars())
        .map(|character| FrontendEvent::Key(KeyEvent::char(character)))
        .chain(std::iter::once(FrontendEvent::Key(KeyEvent::plain(
            KeyCode::Enter,
        ))))
        .collect()
}

fn make_script_app(source: &str) -> App<ScriptedFrontend> {
    let mut host = ScriptHost::new();
    host.execute_typescript("file:///test-config.ts", source)
        .unwrap();
    let host = Rc::new(RefCell::new(host));
    let bootstrap = bootstrap_editor(Buffer::new(), 40, 5, ScriptHost::modes(&host)).unwrap();
    App {
        kernel: bootstrap.kernel,
        session: bootstrap.session,
        frontend: ScriptedFrontend::new(Vec::new()),
        runtime_diagnostics: Vec::new(),
        next_command_task: 0,
        command_tasks: Default::default(),
        pending_commands: Vec::new(),
        behavior: BehaviorRecorder::default(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn script_timeout_keeps_native_edit_save_and_quit_available() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("timeout-recovery.txt");
    std::fs::write(&path, "before").unwrap();
    let mut buffer = Buffer::new();
    buffer.open_path(path.to_str().unwrap()).unwrap();
    let mut host = ScriptHost::with_timeouts(
        std::time::Duration::from_millis(50),
        std::time::Duration::from_millis(100),
    );
    host.execute_typescript(
        "file:///timeout-recovery.ts",
        r#"
editor.modes.define({
  name: "timeout-recovery",
  on: {
    buffer: {
      state: () => ({ calls: 0 }),
      commands: {
        hang(ctx) {
          ctx.state.calls++;
          ctx.edit.insert("discarded");
          while (true) {}
        },
      },
    },
  },
});
"#,
    )
    .unwrap();
    let host = Rc::new(RefCell::new(host));
    let bootstrap = bootstrap_editor(buffer, 40, 5, ScriptHost::modes(&host)).unwrap();
    let mut app = App {
        kernel: bootstrap.kernel,
        session: bootstrap.session,
        frontend: ScriptedFrontend::new(Vec::new()),
        runtime_diagnostics: Vec::new(),
        next_command_task: 0,
        command_tasks: Default::default(),
        pending_commands: Vec::new(),
        behavior: BehaviorRecorder::default(),
    };
    let (_, identity) = normalize_path(&path).unwrap();
    app.kernel
        .register_buffer_path(
            editor_cid(),
            identity,
            path.clone(),
            FileBaseline::Materialized("before".to_owned()),
        )
        .unwrap();
    let view = view_id(&app, app.session.focused());

    let error = app
        .execute_command(DispatchCommand::Mode {
            command: ModeCommand::new(
                ModeName::new("timeout-recovery"),
                ModeActionName::new("hang"),
            ),
            view,
            content: editor_cid(),
        })
        .unwrap_err()
        .to_string();

    assert!(error.contains("timeout during action"), "{error}");
    assert_eq!(text_rows(&app, editor_cid()), vec!["before"]);
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("native-".to_owned())),
        view,
        content: editor_cid(),
    })
    .unwrap();
    app.execute_command(DispatchCommand::Content {
        command: ContentCommand::Save,
        content: editor_cid(),
    })
    .unwrap();
    app.shutdown_tasks().await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "native-before");
    app.execute_command(DispatchCommand::App(AppCommand::Quit))
        .unwrap();
}

#[test]
fn dirty_last_buffer_blocks_quit_and_last_pane_close_until_forced() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("dirty".to_owned())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert!(
        app.execute_command(DispatchCommand::App(AppCommand::Quit))
            .is_err()
    );
    assert!(
        app.execute_command(DispatchCommand::App(AppCommand::Close))
            .is_err()
    );
    assert!(!app.kernel.is_cancelled());

    app.execute_command(DispatchCommand::App(AppCommand::ForceQuit))
        .unwrap();
    assert!(app.kernel.is_cancelled());
}

#[tokio::test]
async fn frontend_quit_request_respects_dirty_guard() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("dirty".to_owned())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    let error = app
        .handle_event(FrontendEvent::QuitRequest)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("unsaved changes"));
    assert!(!app.kernel.is_cancelled());
}

#[tokio::test(flavor = "multi_thread")]
async fn active_script_failure_faults_only_its_view_and_run_loop_continues() {
    let mut app = make_script_app(
        r#"
editor.modes.define({
  name: "recoverable-script",
  on: {
    buffer: {
      commands: {
        fail(ctx) {
          ctx.edit.insert("discarded");
          throw new Error("recoverable failure");
        },
      },
      keys: { x: "fail" },
    },
  },
});
"#,
    );
    app.frontend.events = VecDeque::from([
        FrontendEvent::Key(KeyEvent::char('x')),
        FrontendEvent::Key(KeyEvent::ctrl('q')),
    ]);
    let view = view_id(&app, app.session.focused());
    let mode = app
        .kernel
        .modes()
        .resolve_mode(&ModeName::new("recoverable-script"))
        .unwrap();

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec![""]);
    assert!(app.kernel.is_cancelled());
    assert!(
        app.runtime_diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("recoverable failure"))
    );
    let fault = app.session.view_modes().fault(mode, view).unwrap().clone();
    assert_eq!(fault.phase, ModeFaultPhase::Action);
    assert_eq!(fault.callback, "fail");
    assert!(fault.message.contains("recoverable failure"));
    let diagnostic = app
        .mode_diagnostics()
        .into_iter()
        .find(|diagnostic| diagnostic.view == view)
        .unwrap()
        .decorations
        .into_iter()
        .find(|diagnostic| diagnostic.mode == ModeName::new("recoverable-script"))
        .unwrap();
    assert!(diagnostic.faulted);
    assert_eq!(diagnostic.faults, vec![fault]);
}

#[tokio::test(flavor = "multi_thread")]
async fn frontend_invalid_data_error_remains_fatal() {
    let mut app = make_app(vec![FrontendEvent::Key(KeyEvent::ctrl('d'))], None);
    app.frontend.fail_viewport = true;

    let error = app.run().await.unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("scripted viewport failure"));
    assert!(app.runtime_diagnostics().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn rust_highlighting_is_parsed_and_updated_in_background() {
    let file = tempfile::Builder::new().suffix(".rs").tempfile().unwrap();
    std::fs::write(file.path(), "fn main() {}\n").unwrap();
    let mut app = make_app(vec![], file.path().to_str());
    let view = view_id(&app, app.session.focused());

    app.kernel.schedule_mode_jobs();
    for _ in 0..100 {
        let query = AppQuery {
            contents: app.kernel.contents(),
            views: app.session.views(),
            presentation: app.session.presentation(),
            faces: app.session.faces(),
        };
        let decorations = query
            .decorations(
                view,
                app.session.body_space_for_view(view).unwrap(),
                RowRange { start: 0, end: 1 },
            )
            .unwrap();
        if decorations.iter().any(|d| d.end.char_index == 2) {
            break;
        }
        app.kernel.schedule_mode_jobs();
        app.session
            .refresh_presentation(app.kernel.contents(), app.kernel.content_modes());
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };
    let decorations = query
        .decorations(
            view,
            app.session.body_space_for_view(view).unwrap(),
            RowRange { start: 0, end: 1 },
        )
        .unwrap();
    assert!(decorations.iter().any(|decoration| {
        decoration.start.char_index == 0
            && decoration.end.char_index == 2
            && decoration.face.foreground == Some(Color::Ansi(170))
    }));

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("// 中\n".to_string())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };
    let decorations = query
        .decorations(
            view,
            app.session.body_space_for_view(view).unwrap(),
            RowRange { start: 0, end: 2 },
        )
        .unwrap();
    assert!(!decorations.iter().any(|decoration| {
        decoration.start.char_index == 0
            && decoration.end.char_index == 2
            && decoration.face.foreground == Some(Color::Ansi(170))
    }));

    for _ in 0..100 {
        let query = AppQuery {
            contents: app.kernel.contents(),
            views: app.session.views(),
            presentation: app.session.presentation(),
            faces: app.session.faces(),
        };
        let decorations = query
            .decorations(
                view,
                app.session.body_space_for_view(view).unwrap(),
                RowRange { start: 0, end: 1 },
            )
            .unwrap();
        if decorations.iter().any(|d| d.end.char_index == 4) {
            break;
        }
        app.kernel.schedule_mode_jobs();
        app.session
            .refresh_presentation(app.kernel.contents(), app.kernel.content_modes());
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };
    let decorations = query
        .decorations(
            view,
            app.session.body_space_for_view(view).unwrap(),
            RowRange { start: 0, end: 1 },
        )
        .unwrap();
    assert!(
        decorations.iter().any(|decoration| {
            decoration.start.char_index == 0
                && decoration.end.char_index == 4
                && decoration.face.foreground == Some(Color::Ansi(244))
                && decoration.face.italic == Some(true)
        }),
        "{decorations:#?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn markdown_and_fenced_rust_are_highlighted() {
    let source = concat!(
        "# Heading\r\n",
        "- **bold** [link](https://example.com) `code`\r\n",
        "> quote\r\n",
        "```rust\r\n",
        "fn embedded() {}\r\n",
        "```\r\n",
    );
    let file = tempfile::Builder::new().suffix(".md").tempfile().unwrap();
    std::fs::write(file.path(), source).unwrap();
    let mut app = make_app(vec![], file.path().to_str());
    let view = view_id(&app, app.session.focused());

    app.kernel.schedule_mode_jobs();
    for _ in 0..100 {
        let query = AppQuery {
            contents: app.kernel.contents(),
            views: app.session.views(),
            presentation: app.session.presentation(),
            faces: app.session.faces(),
        };
        let decorations = query
            .decorations(
                view,
                app.session.body_space_for_view(view).unwrap(),
                RowRange { start: 0, end: 6 },
            )
            .unwrap();
        if !decorations.is_empty() {
            break;
        }
        app.kernel.schedule_mode_jobs();
        app.session
            .refresh_presentation(app.kernel.contents(), app.kernel.content_modes());
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };
    let decorations = query
        .decorations(
            view,
            app.session.body_space_for_view(view).unwrap(),
            RowRange { start: 0, end: 6 },
        )
        .unwrap();
    assert!(decorations.iter().any(|decoration| {
        decoration.start.char_index == 0
            && decoration.end.char_index == "# Heading".len()
            && decoration.face.foreground == Some(Color::Ansi(75))
            && decoration.face.bold == Some(true)
    }));
    let bold_start = source.find("**bold**").unwrap();
    assert!(decorations.iter().any(|decoration| {
        decoration.start.char_index == bold_start
            && decoration.end.char_index == bold_start + "**bold**".len()
            && decoration.face.bold == Some(true)
    }));
    let link_start = source.find("[link]").unwrap();
    assert!(decorations.iter().any(|decoration| {
        decoration.start.char_index == link_start
            && decoration.face.foreground == Some(Color::Ansi(75))
            && decoration.face.underline == Some(true)
    }));
    let code_start = source.find("`code`").unwrap();
    assert!(decorations.iter().any(|decoration| {
        decoration.start.char_index == code_start
            && decoration.end.char_index == code_start + "`code`".len()
            && decoration.face.foreground == Some(Color::Ansi(114))
    }));
    let keyword_start = source.find("fn").unwrap();
    assert!(
        decorations.iter().any(|decoration| {
            decoration.start.char_index == keyword_start
                && decoration.end.char_index == keyword_start + 2
                && decoration.face.foreground == Some(Color::Ansi(170))
                && decoration.face.bold == Some(true)
        }),
        "{decorations:#?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_polls_worker_results_without_input() {
    let file = tempfile::Builder::new().suffix(".md").tempfile().unwrap();
    std::fs::write(file.path(), "# Heading\n").unwrap();
    let mut app = make_app(
        vec![FrontendEvent::Key(KeyEvent::ctrl('q'))],
        file.path().to_str(),
    );
    // Wait for the worker decoration render before quitting. The wait is
    // adaptive (it ends as soon as decorations land); the deadline is only a
    // fallback because worker cold start (V8 isolate + tree-sitter wasm
    // compile) is slow on 2-core CI runners.
    let view = view_id(&app, app.session.focused());
    app.frontend.wait_for_decorations = Some((
        view,
        app.session.body_space_for_view(view).unwrap(),
        RowRange { start: 0, end: 1 },
    ));
    app.frontend.next_event_at =
        Some(tokio::time::Instant::now() + std::time::Duration::from_secs(120));

    app.run().await.unwrap();

    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };
    let decorations = query
        .decorations(
            view,
            app.session.body_space_for_view(view).unwrap(),
            RowRange { start: 0, end: 1 },
        )
        .unwrap();
    // A fast worker may finish before the initial render, while a slower one
    // is installed by the runtime tick and causes another render. Both
    // schedules are valid; observing the decoration is the relevant result.
    assert!(app.frontend.decorations_seen);
    assert!(decorations.iter().any(|decoration| {
        decoration.start.char_index == 0
            && decoration.end.char_index == "# Heading".len()
            && decoration.face.foreground == Some(Color::Ansi(75))
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn rust_highlighting_survives_crlf_comment_edits() {
    let file = tempfile::Builder::new().suffix(".rs").tempfile().unwrap();
    std::fs::write(file.path(), "fn main() {}\r\n").unwrap();
    let mut app = make_app(vec![], file.path().to_str());
    let view = view_id(&app, app.session.focused());

    app.kernel.schedule_mode_jobs();
    for _ in 0..100 {
        let query = AppQuery {
            contents: app.kernel.contents(),
            views: app.session.views(),
            presentation: app.session.presentation(),
            faces: app.session.faces(),
        };
        let decorations = query
            .decorations(
                view,
                app.session.body_space_for_view(view).unwrap(),
                RowRange { start: 0, end: 2 },
            )
            .unwrap();
        if !decorations.is_empty() {
            break;
        }
        app.kernel.schedule_mode_jobs();
        app.session
            .refresh_presentation(app.kernel.contents(), app.kernel.content_modes());
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("// note\r\n".to_owned())),
        view,
        content: editor_cid(),
    })
    .unwrap();
    for _ in 0..100 {
        let query = AppQuery {
            contents: app.kernel.contents(),
            views: app.session.views(),
            presentation: app.session.presentation(),
            faces: app.session.faces(),
        };
        let decorations = query
            .decorations(
                view,
                app.session.body_space_for_view(view).unwrap(),
                RowRange { start: 0, end: 2 },
            )
            .unwrap();
        if decorations
            .iter()
            .any(|d| d.end.char_index == "// note".len())
        {
            break;
        }
        app.kernel.schedule_mode_jobs();
        app.session
            .refresh_presentation(app.kernel.contents(), app.kernel.content_modes());
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };
    let decorations = query
        .decorations(
            view,
            app.session.body_space_for_view(view).unwrap(),
            RowRange { start: 0, end: 2 },
        )
        .unwrap();
    assert!(
        decorations.iter().any(|decoration| {
            decoration.start.char_index == 0
                && decoration.end.char_index == "// note".len()
                && decoration.face.foreground == Some(Color::Ansi(244))
                && decoration.face.italic == Some(true)
        }),
        "{decorations:#?}"
    );
}

fn editor_cid() -> ContentId {
    ContentId(0)
}

fn view_id(app: &App<ScriptedFrontend>, space: SpaceId) -> ViewId {
    view_for_space(app.session.scene(), space).expect("space hosts a view")
}

fn switch_focused_view(
    app: &mut App<ScriptedFrontend>,
    content: ContentId,
) -> Result<ViewId, LayoutError> {
    let target = app.switch_target().expect("focused view is switchable");
    app.switch_view_at(target, content)
}

fn view_at(app: &App<ScriptedFrontend>, space: SpaceId) -> &View {
    &app.session.views()[&view_id(app, space)]
}

fn replace_view_mode_for_test(
    app: &mut App<ScriptedFrontend>,
    view: ViewId,
    mut mode: ModeViewInstance,
) {
    let content = app.session.views()[&view].content();
    let removed = app.session.view_modes_mut_for_test().remove(view);
    {
        let (contents, mode_contents) = app.kernel.mode_runtime_parts();
        for mode_id in removed {
            mode_contents.detach_view(content, mode_id);
        }
        let content_context = ModeContentContext::new(content, contents);
        let view_data = &app.session.views()[&view];
        let view_context =
            ModeViewContext::new(view, view_data.content(), view_data.state(), contents).unwrap();
        mode_contents.attach_view_with_context(content, &mut mode, &content_context, &view_context);
    }
    app.session.view_modes_mut_for_test().insert(view, mode);
    app.session
        .refresh_presentation(app.kernel.contents(), app.kernel.content_modes());
}

fn text_presentation(view: &ViewData) -> &TextPresentation {
    match &view.presentation {
        ViewPresentation::Text(text) => text,
        ViewPresentation::StatusBar(_) => panic!("expected text presentation"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_sharing_one_kernel_keep_client_state_independent() {
    let mut app = make_app(vec![], None);
    let first_view = view_id(&app, app.session.focused());
    let editor_modes = app.session.view_modes().mode_names(first_view);
    let (contents, modes, mode_contents) = app.kernel.mode_attachment_parts();
    let mut second = create_editor_session(
        contents,
        modes,
        mode_contents,
        80,
        20,
        editor_cid(),
        editor_modes,
    );
    let second_view = view_for_space(second.scene(), second.focused()).unwrap();

    second.resize(100, 30);
    app.handle_event(FrontendEvent::Key(KeyEvent::char('i')))
        .await
        .unwrap();
    app.handle_event(FrontendEvent::Key(KeyEvent::char('x')))
        .await
        .unwrap();

    assert_eq!(app.session.views()[&first_view].content(), editor_cid());
    assert_eq!(second.views()[&second_view].content(), editor_cid());
    assert_eq!(app.session.scene().size.width, 40);
    assert_eq!(second.scene().size.width, 100);
    assert_eq!(
        app.session.views()[&first_view]
            .selections()
            .unwrap()
            .primary()
            .head()
            .char_index,
        1
    );
    assert_eq!(
        second.views()[&second_view]
            .selections()
            .unwrap()
            .primary()
            .head(),
        TextOffset::origin()
    );
}

#[test]
fn production_content_paths_use_closed_static_dispatch() {
    let app = [
        include_str!("application.rs"),
        include_str!("kernel.rs"),
        include_str!("layout.rs"),
        include_str!("query.rs"),
        include_str!("runtime.rs"),
        include_str!("save.rs"),
    ]
    .concat();
    let content = include_str!("../../vell-core/src/content.rs");
    let content_view_state = include_str!("../../vell-core/src/content_view_state.rs");
    let view = include_str!("view.rs");
    let transaction = include_str!("transaction.rs")
        .split("#[cfg(test)]")
        .next()
        .unwrap();
    let dynamic_handler = concat!("Box<dyn ", "Content", "Handler>");
    let buffer_probe = concat!("buffer", "_mut(");
    let buffer_read_probe = concat!("as_", "buffer(");
    let forbidden = [
        ["Box<dyn ", "ContentViewState>"].concat(),
        ["Box<dyn ", "Content>"].concat(),
    ];

    assert!(!app.contains(dynamic_handler));
    assert!(!app.contains(buffer_probe));
    assert!(!content.contains(buffer_read_probe));
    for fragment in forbidden {
        assert!(!content_view_state.contains(&fragment), "{fragment}");
    }
    assert!(content_view_state.contains("pub enum ContentViewState"));
    assert!(content_view_state.contains("Buffer(BufferViewState)"));
    assert!(!content_view_state.contains("Option<Selections>"));
    assert!(!view.contains("match self.state"));
    assert!(!view.contains("match &mut self.state"));
    for concrete_transaction in ["BufferTransactionData", "TransactionData::Buffer"] {
        assert!(!app.contains(concrete_transaction));
        assert!(!transaction.contains(concrete_transaction));
    }
}

fn text_rows(app: &App<ScriptedFrontend>, content: ContentId) -> Vec<String> {
    match app.kernel.contents().query(
        content,
        ContentQuery::TextRows(RowRange { start: 0, end: 5 }),
    ) {
        ContentData::TextRows(rows) => rows,
        data => panic!("expected text rows, got {data:?}"),
    }
}

fn text_point(
    app: &App<ScriptedFrontend>,
    content: ContentId,
    offset: TextOffset,
) -> vell_protocol::selection::TextPoint {
    match app
        .kernel
        .contents()
        .query(content, ContentQuery::TextPoints(vec![offset]))
    {
        ContentData::TextPoints(mut points) => points.remove(0),
        _ => panic!("expected text point"),
    }
}

fn dirty_state(app: &App<ScriptedFrontend>, content: ContentId) -> DirtyState {
    match app
        .kernel
        .contents()
        .query(content, ContentQuery::DirtyState)
    {
        ContentData::DirtyState(state) => state,
        data => panic!("expected dirty state, got {data:?}"),
    }
}

fn resource_path(app: &App<ScriptedFrontend>, content: ContentId) -> Option<String> {
    match app
        .kernel
        .contents()
        .query(content, ContentQuery::ResourcePath)
    {
        ContentData::ResourcePath(path) => path,
        data => panic!("expected resource path, got {data:?}"),
    }
}

fn backing_state(app: &App<ScriptedFrontend>, content: ContentId) -> BufferBackingState {
    match app
        .kernel
        .contents()
        .query(content, ContentQuery::BackingState)
    {
        ContentData::BackingState(state) => state,
        data => panic!("expected backing state, got {data:?}"),
    }
}

fn save_state(app: &App<ScriptedFrontend>, content: ContentId) -> SaveState {
    match app
        .kernel
        .contents()
        .query(content, ContentQuery::SaveState)
    {
        ContentData::SaveState(state) => state,
        data => panic!("expected save state, got {data:?}"),
    }
}

async fn successful_behavior_snapshot(path: &std::path::Path) -> BehaviorSnapshot {
    std::fs::write(path, "").unwrap();
    let mut app = make_app(vec![], path.to_str());
    app.behavior.reset();
    let view = view_id(&app, app.session.focused());
    let viewport = ViewportCommand::new(
        ViewportMoveDirection::Down,
        ViewportMoveAmount::HalfPage,
        ViewportCursorBehavior::Move,
    );
    let result = app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![
            view_edit(EditCommand::InsertText("abc".to_string())),
            history(TransactionIntent::Commit),
            save(),
            self::viewport(viewport),
        ],
        view,
        content: editor_cid(),
    });
    let snapshot =
        BehaviorSnapshot::capture(&app, ExecutionOutcome::from_result(&result), Vec::new());
    app.shutdown_tasks().await.unwrap();
    snapshot
}

#[tokio::test(flavor = "multi_thread")]
async fn behavior_snapshot_normalizes_successful_execution_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("behavior.txt");

    let first = successful_behavior_snapshot(&path).await;
    let second = successful_behavior_snapshot(&path).await;

    assert_eq!(first, second);
    assert_eq!(first.outcome, ExecutionOutcome::Succeeded);
    assert_eq!(first.prepared_effects, first.published_effects);
    assert!(matches!(
        first.prepared_effects.as_slice(),
        [
            EffectBehavior::HistoryCommit { content: ContentId(0) },
            EffectBehavior::Save {
                content: ContentId(0),
                bytes,
                ..
            },
            EffectBehavior::Viewport {
                view: ViewId(0),
                command: ResolvedViewportCommand::Scroll {
                    direction: ViewportMoveDirection::Down,
                    lines: 2,
                },
            }
        ] if bytes == "abc"
    ));
    let history = first
        .history
        .iter()
        .find(|history| history.content == editor_cid())
        .unwrap();
    assert_eq!(history.undo_depth, 1);
    assert_eq!(history.redo_depth, 0);
}

#[test]
fn behavior_snapshot_distinguishes_prepared_from_published_effects_on_failure() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut app = make_app(vec![], file.path().to_str());
    app.behavior.reset();
    let view = view_id(&app, app.session.focused());
    let result = app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![
            view_edit(EditCommand::InsertText("x".to_string())),
            save(),
            nested_mode(ModeCommand::new(
                ModeName::new("missing"),
                ModeActionName::new("run"),
            )),
        ],
        view,
        content: editor_cid(),
    });

    let snapshot =
        BehaviorSnapshot::capture(&app, ExecutionOutcome::from_result(&result), Vec::new());

    assert!(matches!(
        snapshot.outcome,
        ExecutionOutcome::Failed(ref message) if message.contains("unknown mode 'missing'")
    ));
    assert!(matches!(
        snapshot.prepared_effects.as_slice(),
        [EffectBehavior::Save { bytes, .. }] if bytes == "x"
    ));
    assert!(snapshot.published_effects.is_empty());
    assert_eq!(
        snapshot
            .contents
            .iter()
            .find(|content| content.content == editor_cid())
            .and_then(|content| content.text.as_deref()),
        Some("")
    );
    let history = snapshot
        .history
        .iter()
        .find(|history| history.content == editor_cid())
        .unwrap();
    assert_eq!((history.undo_depth, history.redo_depth), (0, 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn behavior_snapshot_uses_explicit_mode_probes_and_reports_faults() {
    let mut app = make_app(vec![], None);
    let shared = ModeName::new("shared-content");
    app.kernel
        .modes_mut()
        .register(SharedContentMode::new())
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &shared).unwrap();
    let shared_id = app.kernel.modes().resolve_mode(&shared).unwrap();
    let view = view_id(&app, app.session.focused());

    app.handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();

    let faulting = ModeName::new("faulting-highlight");
    app.kernel
        .modes_mut()
        .register(FaultingHighlightMode {
            name: faulting.clone(),
        })
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &faulting).unwrap();
    app.behavior.reset();
    let result = app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("x".to_string())),
        view,
        content: editor_cid(),
    });

    let content_state = app
        .kernel
        .content_modes()
        .state_for_test::<SharedContentState>(shared_id, editor_cid())
        .unwrap();
    let view_state = app
        .session
        .view_modes()
        .state_for_test::<SharedViewState>(shared_id, view)
        .unwrap();
    let snapshot = BehaviorSnapshot::capture(
        &app,
        ExecutionOutcome::from_result(&result),
        vec![
            ModeProbeBehavior::new("shared.view.awaiting", view_state.awaiting.to_string()),
            ModeProbeBehavior::new(
                "shared.content.executions",
                content_state.executions.to_string(),
            ),
        ],
    );

    assert_eq!(
        snapshot.mode_probes,
        vec![
            ModeProbeBehavior::new("shared.content.executions", "1"),
            ModeProbeBehavior::new("shared.view.awaiting", "false"),
        ]
    );
    assert!(snapshot.faults.contains(&ModeFaultBehavior {
        mode: faulting.as_str().to_owned(),
        scope: ModeFaultScope::Content(editor_cid()),
    }));
}

#[test]
fn content_query_reads_buffer_and_view() {
    let mut app = make_app(vec![], None);
    let focused_view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("hi".to_string())),
        view: focused_view,
        content: editor_cid(),
    })
    .unwrap();
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };
    assert_eq!(
        query
            .content(
                editor_cid(),
                ContentQuery::TextRows(RowRange { start: 0, end: 5 })
            )
            .unwrap(),
        ContentData::TextRows(vec!["hi".to_string()])
    );
    let view = query
        .view(
            focused_view,
            app.session.body_space_for_view(focused_view).unwrap(),
        )
        .unwrap();
    let text = text_presentation(&view);
    assert_eq!(text.selections.primary().head().char_index, 2);
    assert_eq!(text.cursor_style, CursorStyle::Block);
    assert_eq!(
        query.content(editor_cid(), ContentQuery::TextMetrics),
        Ok(ContentData::TextMetrics(
            vell_protocol::content_query::TextMetrics {
                line_count: 1,
                char_count: 2,
            }
        ))
    );
    assert_eq!(
        query.content(editor_cid(), ContentQuery::ResourceName),
        Ok(ContentData::ResourceName(None))
    );
    assert_eq!(
        query.content(
            editor_cid(),
            ContentQuery::TextRows(RowRange { start: 0, end: 1 })
        ),
        Ok(ContentData::TextRows(vec!["hi".to_string()]))
    );
    assert_eq!(
        query.content(
            ContentId(99),
            ContentQuery::TextRows(RowRange { start: 0, end: 1 }),
        ),
        Err(RenderQueryError::MissingContent(ContentId(99)))
    );
    assert_eq!(
        query.content(ContentId(1), ContentQuery::DirtyState),
        Err(RenderQueryError::MissingContent(ContentId(1)))
    );
    assert_eq!(
        query.decorations(ViewId(99), SpaceId(0), RowRange { start: 0, end: 1 }),
        Err(RenderQueryError::MissingView(ViewId(99)))
    );
}

#[test]
fn unknown_mode_command_returns_a_diagnostic_error() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());

    let error = app
        .execute_command(DispatchCommand::Mode {
            command: ModeCommand {
                mode: ModeName::new("missing"),
                action: ModeActionName::new("action"),
                arguments: Default::default(),
            },
            view,
            content: editor_cid(),
        })
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("unknown mode 'missing'"));
}

#[test]
fn recursive_mode_command_chain_stops_at_the_execution_limit() {
    let mut app = make_app(vec![], None);
    let mode_name = ModeName::new("loop");
    app.kernel.modes_mut().register(LoopMode::new()).unwrap();
    let state = app
        .kernel
        .contents()
        .create_view_state(editor_cid())
        .unwrap();
    let focused = app.session.focused();
    let (contents, modes, content_modes) = app.kernel.mode_attachment_parts();
    app.session
        .replace_space_content(
            focused,
            NewView {
                view: View::new(editor_cid(), state),
                mode_names: vec![mode_name.clone()],
            },
            true,
            modes,
            content_modes,
            contents,
        )
        .unwrap();
    let view = view_id(&app, focused);
    let content_revision = app.kernel.contents().revision(editor_cid());
    let view_revision = app.session.views()[&view].revision();

    let error = app
        .execute_command(DispatchCommand::Mode {
            command: ModeCommand {
                mode: mode_name,
                action: ModeActionName::new("again"),
                arguments: Default::default(),
            },
            view,
            content: editor_cid(),
        })
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("exceeded the limit of 256"));
    assert_eq!(
        app.kernel.contents().query(
            editor_cid(),
            ContentQuery::TextRows(RowRange { start: 0, end: 1 }),
        ),
        ContentData::TextRows(vec![String::new()])
    );
    assert_eq!(
        app.kernel.contents().revision(editor_cid()),
        content_revision
    );
    assert_eq!(app.session.views()[&view].revision(), view_revision);
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };
    assert_eq!(
        text_presentation(
            &query
                .view(view, app.session.body_space_for_view(view).unwrap())
                .unwrap()
        )
        .cursor_style,
        CursorStyle::Default
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_ordered_result_does_not_start_an_earlier_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ordered-save.txt");
    std::fs::write(&path, "old").unwrap();
    let mut app = make_app(vec![], Some(path.to_str().unwrap()));
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("new".to_string())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    let error = app
        .execute_command(DispatchCommand::ModeOperations {
            operations: vec![
                save(),
                nested_mode(ModeCommand {
                    mode: ModeName::new("missing"),
                    action: ModeActionName::new("missing"),
                    arguments: Default::default(),
                }),
            ],
            view,
            content: editor_cid(),
        })
        .unwrap_err();

    assert!(error.to_string().contains("unknown mode 'missing'"));
    assert!(!app.kernel.has_pending_save(editor_cid()));
    app.shutdown_tasks().await.unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "old");
}

#[test]
fn failed_ordered_result_does_not_apply_an_earlier_quit() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());

    let error = app
        .execute_command(DispatchCommand::ModeOperations {
            operations: vec![
                app_command(AppCommand::Quit),
                nested_mode(ModeCommand {
                    mode: ModeName::new("missing"),
                    action: ModeActionName::new("missing"),
                    arguments: Default::default(),
                }),
            ],
            view,
            content: editor_cid(),
        })
        .unwrap_err();

    assert!(error.to_string().contains("unknown mode 'missing'"));
    assert!(!app.kernel.is_cancelled());
}

#[test]
fn failed_ordered_result_does_not_apply_an_earlier_viewport_move() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    let command = ViewportCommand::new(
        vell_protocol::viewport::ViewportMoveDirection::Down,
        vell_protocol::viewport::ViewportMoveAmount::HalfPage,
        ViewportCursorBehavior::Move,
    );

    let error = app
        .execute_command(DispatchCommand::ModeOperations {
            operations: vec![
                viewport(command),
                nested_mode(ModeCommand {
                    mode: ModeName::new("missing"),
                    action: ModeActionName::new("missing"),
                    arguments: Default::default(),
                }),
            ],
            view,
            content: editor_cid(),
        })
        .unwrap_err();

    assert!(error.to_string().contains("unknown mode 'missing'"));
    assert!(app.frontend.viewport_commands.is_empty());
}

#[test]
fn failed_ordered_result_does_not_apply_an_earlier_split() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    let next_view = app.session.next_view_id_for_test();

    let error = app
        .execute_command(DispatchCommand::ModeOperations {
            operations: vec![
                app_command(AppCommand::Split(SplitDirection::Right)),
                nested_mode(ModeCommand {
                    mode: ModeName::new("missing"),
                    action: ModeActionName::new("missing"),
                    arguments: Default::default(),
                }),
            ],
            view,
            content: editor_cid(),
        })
        .unwrap_err();

    assert!(error.to_string().contains("unknown mode 'missing'"));
    assert_eq!(focusable_view_count(app.session.scene()), 1);
    assert_eq!(app.session.next_view_id_for_test(), next_view);
}

#[test]
fn failed_ordered_result_does_not_apply_an_earlier_close() {
    let mut app = make_app(vec![], None);
    let left = app.session.focused();
    let right = app
        .split_space(left, editor_cid(), true, SplitDirection::Right, true)
        .unwrap()
        .new_space;
    let view = view_id(&app, right);
    app.behavior.reset();

    let error = app
        .execute_command(DispatchCommand::ModeOperations {
            operations: vec![
                app_command(AppCommand::Close),
                nested_mode(ModeCommand {
                    mode: ModeName::new("missing"),
                    action: ModeActionName::new("missing"),
                    arguments: Default::default(),
                }),
            ],
            view,
            content: editor_cid(),
        })
        .unwrap_err();
    let snapshot = BehaviorSnapshot::capture(
        &app,
        ExecutionOutcome::from_result(&Err::<(), _>(error)),
        vec![],
    );

    assert!(matches!(
        snapshot.prepared_effects.as_slice(),
        [EffectBehavior::Close { target }] if *target == right
    ));
    assert!(snapshot.published_effects.is_empty());
    assert_eq!(focusable_view_count(app.session.scene()), 2);
    assert_eq!(app.session.focused(), right);
    assert!(app.session.views().contains_key(&view));
}

#[test]
fn conflicting_topology_effects_fail_without_publishing_layout() {
    for operations in [
        vec![
            app_command(AppCommand::Close),
            app_command(AppCommand::Close),
        ],
        vec![
            app_command(AppCommand::Close),
            app_command(AppCommand::Split(SplitDirection::Right)),
        ],
    ] {
        let mut app = make_app(vec![], None);
        let left = app.session.focused();
        let right = app
            .split_space(left, editor_cid(), true, SplitDirection::Right, true)
            .unwrap()
            .new_space;
        let view = view_id(&app, right);

        let error = app
            .execute_command(DispatchCommand::ModeOperations {
                operations,
                view,
                content: editor_cid(),
            })
            .unwrap_err();

        assert!(error.to_string().contains("only one topology effect"));
        assert_eq!(focusable_view_count(app.session.scene()), 2);
        assert_eq!(app.session.focused(), right);
    }

    let mut app = make_app(vec![], None);
    let focused = app.session.focused();
    let view = view_id(&app, focused);
    let error = app
        .execute_command(DispatchCommand::ModeOperations {
            operations: vec![
                app_command(AppCommand::Split(SplitDirection::Right)),
                app_command(AppCommand::Close),
            ],
            view,
            content: editor_cid(),
        })
        .unwrap_err();

    assert!(error.to_string().contains("only one topology effect"));
    assert_eq!(focusable_view_count(app.session.scene()), 1);
    assert_eq!(app.session.focused(), focused);
    assert!(!app.kernel.is_cancelled());
}

#[test]
fn failed_history_branch_restores_records_truncated_after_undo() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    for text in ["a", "b"] {
        app.execute_command(DispatchCommand::ContentWithView {
            command: ContentCommand::Edit(EditCommand::InsertText(text.to_string())),
            view,
            content: editor_cid(),
        })
        .unwrap();
    }

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![
            history(TransactionIntent::Undo),
            view_edit(EditCommand::InsertText("c".to_string())),
            nested_mode(ModeCommand {
                mode: ModeName::new("missing"),
                action: ModeActionName::new("missing"),
                arguments: Default::default(),
            }),
        ],
        view,
        content: editor_cid(),
    })
    .unwrap_err();

    assert_eq!(text_rows(&app, editor_cid()), vec!["ab"]);
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Undo,
        view,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["a"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_capture_output_restores_the_pre_input_mode_state() {
    let mut app = make_app(vec![], None);
    let mode = {
        let modes = app.kernel.modes_mut();
        modes.register(CaptureFailureMode::new()).unwrap();
        modes
            .instantiate(&ModeName::new("capture-failure"))
            .unwrap()
    };
    let focused = app.session.focused();
    let view = view_id(&app, focused);
    replace_view_mode_for_test(&mut app, view, mode);
    app.session.sync_focused_input(
        std::time::Instant::now(),
        app.kernel.content_modes(),
        app.kernel.contents(),
    );

    let error = app
        .handle_event(FrontendEvent::Key(KeyEvent::char('x')))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("unknown mode 'missing'"));
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };
    assert_eq!(
        text_presentation(
            &query
                .view(view, app.session.body_space_for_view(view).unwrap())
                .unwrap()
        )
        .cursor_style,
        CursorStyle::Default
    );
}

#[test]
fn failed_timeout_output_restores_the_pre_timeout_mode_state() {
    let mut app = make_app(vec![], None);
    let mode = {
        let modes = app.kernel.modes_mut();
        modes.register(CaptureFailureMode::new()).unwrap();
        modes
            .instantiate(&ModeName::new("capture-failure"))
            .unwrap()
    };
    let view = view_id(&app, app.session.focused());
    replace_view_mode_for_test(&mut app, view, mode);
    app.session.sync_focused_input(
        std::time::Instant::now(),
        app.kernel.content_modes(),
        app.kernel.contents(),
    );

    let error = app.handle_input_timeout().unwrap_err();

    assert!(error.to_string().contains("unknown mode 'missing'"));
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };
    assert_eq!(
        text_presentation(
            &query
                .view(view, app.session.body_space_for_view(view).unwrap())
                .unwrap()
        )
        .cursor_style,
        CursorStyle::Default
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mutable_view_mode_callbacks_advance_revision_after_success() {
    let setup = || {
        let mut app = make_app(vec![], None);
        let mode = {
            let modes = app.kernel.modes_mut();
            modes.register(PresentationMutationMode::new()).unwrap();
            modes
                .instantiate(&ModeName::new("presentation-mutation"))
                .unwrap()
        };
        let view = view_id(&app, app.session.focused());
        replace_view_mode_for_test(&mut app, view, mode);
        app.session.sync_focused_input(
            std::time::Instant::now(),
            app.kernel.content_modes(),
            app.kernel.contents(),
        );
        (app, view)
    };

    let (mut captured, captured_view) = setup();
    let captured_revision = captured.session.views()[&captured_view].revision();
    captured
        .handle_event(FrontendEvent::Key(KeyEvent::char('x')))
        .await
        .unwrap();
    assert!(captured.session.views()[&captured_view].revision() > captured_revision);

    let (mut timed_out, timeout_view) = setup();
    let timeout_revision = timed_out.session.views()[&timeout_view].revision();
    timed_out.handle_input_timeout().unwrap();
    assert!(timed_out.session.views()[&timeout_view].revision() > timeout_revision);
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_view_presentation_layer_is_not_observed() {
    let mut app = make_app(vec![], None);
    let mode = {
        let modes = app.kernel.modes_mut();
        modes.register(PresentationMutationMode::new()).unwrap();
        modes
            .instantiate(&ModeName::new("presentation-mutation"))
            .unwrap()
    };
    let view = view_id(&app, app.session.focused());
    replace_view_mode_for_test(&mut app, view, mode);
    app.session.sync_focused_input(
        std::time::Instant::now(),
        app.kernel.content_modes(),
        app.kernel.contents(),
    );

    app.handle_event(FrontendEvent::Key(KeyEvent::char('x')))
        .await
        .unwrap();
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };
    assert_eq!(
        text_presentation(
            &query
                .view(view, app.session.body_space_for_view(view).unwrap())
                .unwrap()
        )
        .cursor_style,
        CursorStyle::Bar
    );

    app.session.view_mut(view).unwrap().touch();
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };
    assert_eq!(
        text_presentation(
            &query
                .view(view, app.session.body_space_for_view(view).unwrap())
                .unwrap()
        )
        .cursor_style,
        CursorStyle::Default
    );
}

#[test]
fn status_pane_query_returns_status_bar_presentation() {
    let app = make_app(vec![], None);
    let editor = view_id(&app, app.session.focused());
    let status = app
        .status_bar_for_view(editor)
        .expect("default editor has a status bar");
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };

    let view = query.view(status.target_view, status.space).unwrap();
    assert!(matches!(view.presentation, ViewPresentation::StatusBar(_)));
}

#[test]
fn switch_target_resolves_nearest_switchable_ancestor() {
    let mut app = make_app(vec![], None);
    let left_space = app.session.focused();
    let left = view_id(&app, left_space);
    let right_space = app
        .split_space(left_space, editor_cid(), true, SplitDirection::Right, false)
        .unwrap()
        .new_space;
    let right = view_id(&app, right_space);
    let coordinator_space = app
        .split_space(right_space, editor_cid(), true, SplitDirection::Down, false)
        .unwrap()
        .new_space;
    let coordinator = view_id(&app, coordinator_space);

    // 独立 BufferView 默认 switchable：切换目标是焦点 view 自身。
    assert_eq!(app.switch_target(), Some(left));

    // DiffView 语义树的结构模型：left/right 作为复合 view 的子 view 不可
    // 切换，coordinator 充当复合 view（无需真实 diff 数据即可验证解析）。
    app.session
        .compose_views_for_test(coordinator, &[(left, false), (right, false)])
        .unwrap();

    // 焦点位于任一子 view 的 Pane 时，通用切换作用于整个复合 view。
    assert_eq!(app.session.switch_target(left_space), Some(coordinator));
    assert_eq!(app.session.switch_target(right_space), Some(coordinator));
    assert_eq!(
        app.session.switch_target(coordinator_space),
        Some(coordinator)
    );

    // 整条 parent 链都不可切换时没有切换目标。
    app.session
        .set_view_switchable_for_test(coordinator, false)
        .unwrap();
    assert_eq!(app.session.switch_target(left_space), None);
}

#[test]
fn compound_view_switch_replaces_the_complete_subtree_atomically() {
    let mut app = make_app(vec![], None);
    let left_space = app.session.focused();
    let left = view_id(&app, left_space);
    let right_space = app
        .split_space(left_space, editor_cid(), true, SplitDirection::Right, false)
        .unwrap()
        .new_space;
    let right = view_id(&app, right_space);
    let coordinator_space = app
        .split_space(right_space, editor_cid(), true, SplitDirection::Down, false)
        .unwrap()
        .new_space;
    let coordinator = view_id(&app, coordinator_space);
    app.session
        .compose_views_for_test(coordinator, &[(left, false), (right, false)])
        .unwrap();
    let replacement = app.new_buffer();
    let scene_revision = app.session.scene_revision();
    let buffer_count = app.buffers().len();

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![OperationRequest::ViewLifecycle(
            ViewLifecycleOperation::Switch {
                spec: ViewSpec::buffer(replacement),
            },
        )],
        view: left,
        content: editor_cid(),
    })
    .unwrap();

    let replacement_view = view_id(&app, coordinator_space);
    assert_eq!(app.session.scene_revision(), Revision(scene_revision.0 + 1));
    assert_eq!(app.session.views().len(), 1);
    assert_eq!(app.buffers().len(), buffer_count);
    assert_eq!(app.session.focused(), coordinator_space);
    assert_eq!(
        app.session.view(replacement_view).unwrap().content(),
        replacement
    );
    for removed in [left, right, coordinator] {
        assert!(app.session.view(removed).is_none());
        assert!(app.session.view_modes().mode_ids(removed).is_empty());
    }
    assert!(!app.session.scene().contains(left_space));
    assert!(!app.session.scene().contains(right_space));
    assert!(app.session.scene().contains(coordinator_space));
    assert!(
        scene_views(app.session.scene())
            .into_iter()
            .all(|(_, view)| view == replacement_view)
    );
    let replacement = app.session.view(replacement_view).unwrap();
    assert!(
        replacement
            .panes()
            .spaces()
            .all(|space| view_for_space(app.session.scene(), space) == Some(replacement_view))
    );
}

#[test]
fn same_content_switch_still_replaces_a_compound_view_with_a_leaf() {
    let mut app = make_app(vec![], None);
    let left_space = app.session.focused();
    let left = view_id(&app, left_space);
    let right_space = app
        .split_space(left_space, editor_cid(), true, SplitDirection::Right, false)
        .unwrap()
        .new_space;
    let right = view_id(&app, right_space);
    let coordinator_space = app
        .split_space(right_space, editor_cid(), true, SplitDirection::Down, false)
        .unwrap()
        .new_space;
    let coordinator = view_id(&app, coordinator_space);
    app.session
        .compose_views_for_test(coordinator, &[(left, false), (right, false)])
        .unwrap();

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![OperationRequest::ViewLifecycle(
            ViewLifecycleOperation::Switch {
                spec: ViewSpec::buffer(editor_cid()),
            },
        )],
        view: left,
        content: editor_cid(),
    })
    .unwrap();

    let replacement = view_id(&app, coordinator_space);
    assert!(![left, right, coordinator].contains(&replacement));
    assert_eq!(app.session.views().len(), 1);
    assert!(app.session.view(replacement).unwrap().children().is_empty());
    assert!(!app.session.scene().contains(left_space));
    assert!(!app.session.scene().contains(right_space));
}

#[test]
fn compound_view_switch_removes_descendant_per_pane_status_bars() {
    let mut app = make_app(vec![], None);
    app.set_status_bar_placement(StatusBarPlacement::PerPane)
        .unwrap();
    let left_space = app.session.focused();
    let left = view_id(&app, left_space);
    let left_status = app.status_bar_for_view(left).unwrap().space;
    let right_space = app
        .split_space(left_space, editor_cid(), true, SplitDirection::Right, false)
        .unwrap()
        .new_space;
    let right = view_id(&app, right_space);
    let right_status = app.status_bar_for_view(right).unwrap().space;
    let coordinator_space = app
        .split_space(right_space, editor_cid(), true, SplitDirection::Down, false)
        .unwrap()
        .new_space;
    let coordinator = view_id(&app, coordinator_space);
    let coordinator_status = app.status_bar_for_view(coordinator).unwrap().space;
    app.session
        .compose_views_for_test(coordinator, &[(left, false), (right, false)])
        .unwrap();
    let replacement = app.new_buffer();

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![OperationRequest::ViewLifecycle(
            ViewLifecycleOperation::Switch {
                spec: ViewSpec::buffer(replacement),
            },
        )],
        view: left,
        content: editor_cid(),
    })
    .unwrap();

    let replacement_view = view_id(&app, coordinator_space);
    assert_eq!(
        app.status_bar_for_view(replacement_view).unwrap().space,
        coordinator_status
    );
    for removed in [left_space, left_status, right_space, right_status] {
        assert!(!app.session.scene().contains(removed));
    }
    assert_eq!(scene_views(app.session.scene()).len(), 2);
    assert!(
        scene_views(app.session.scene())
            .into_iter()
            .all(|(_, view)| view == replacement_view)
    );
}

#[test]
fn content_close_removes_overlapping_compound_view_targets_atomically() {
    let mut app = make_app(vec![], None);
    let left_space = app.session.focused();
    let left = view_id(&app, left_space);
    let right_space = app
        .split_space(left_space, editor_cid(), true, SplitDirection::Right, false)
        .unwrap()
        .new_space;
    let right = view_id(&app, right_space);
    let coordinator_space = app
        .split_space(right_space, editor_cid(), true, SplitDirection::Down, false)
        .unwrap()
        .new_space;
    let coordinator = view_id(&app, coordinator_space);
    app.session
        .compose_views_for_test(coordinator, &[(left, false), (right, false)])
        .unwrap();

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![OperationRequest::ContentLifecycle(
            ContentLifecycleOperation::Close {
                target: ContentTarget::Id(editor_cid()),
                force: true,
            },
        )],
        view: left,
        content: editor_cid(),
    })
    .unwrap();

    let replacement = view_id(&app, coordinator_space);
    assert!(!app.kernel.contents().contains(editor_cid()));
    assert_eq!(app.session.views().len(), 1);
    assert_ne!(
        app.session.view(replacement).unwrap().content(),
        editor_cid()
    );
    assert_eq!(app.session.focused(), coordinator_space);
    for removed in [left, right, coordinator] {
        assert!(app.session.view(removed).is_none());
        assert!(app.session.view_modes().mode_ids(removed).is_empty());
    }
    assert!(!app.session.scene().contains(left_space));
    assert!(!app.session.scene().contains(right_space));
    assert!(
        scene_views(app.session.scene())
            .into_iter()
            .all(|(_, view)| view == replacement)
    );
}

#[test]
fn closing_a_compound_view_removes_the_complete_subtree_atomically() {
    let mut app = make_app(vec![], None);
    let left_space = app.session.focused();
    let left = view_id(&app, left_space);
    let right_space = app
        .split_space(left_space, editor_cid(), true, SplitDirection::Right, false)
        .unwrap()
        .new_space;
    let right = view_id(&app, right_space);
    let coordinator_space = app
        .split_space(right_space, editor_cid(), true, SplitDirection::Down, false)
        .unwrap()
        .new_space;
    let coordinator = view_id(&app, coordinator_space);
    let survivor_space = app
        .split_space(
            coordinator_space,
            editor_cid(),
            true,
            SplitDirection::Right,
            false,
        )
        .unwrap()
        .new_space;
    let survivor = view_id(&app, survivor_space);
    app.session
        .compose_views_for_test(coordinator, &[(left, false), (right, false)])
        .unwrap();

    app.close_space(coordinator_space).unwrap();

    assert_eq!(app.session.focused(), survivor_space);
    assert_eq!(app.session.views().len(), 1);
    assert!(app.session.view(survivor).is_some());
    for (space, removed) in [
        (left_space, left),
        (right_space, right),
        (coordinator_space, coordinator),
    ] {
        assert!(!app.session.scene().contains(space));
        assert!(app.session.view(removed).is_none());
        assert!(app.session.view_modes().mode_ids(removed).is_empty());
    }
    assert!(
        scene_views(app.session.scene())
            .into_iter()
            .all(|(_, view)| view == survivor)
    );
}

#[test]
fn compound_view_children_keep_independent_selections() {
    let mut app = make_app(vec![], None);
    let left_space = app.session.focused();
    let left = view_id(&app, left_space);
    let right_space = app
        .split_space(left_space, editor_cid(), true, SplitDirection::Right, false)
        .unwrap()
        .new_space;
    let right = view_id(&app, right_space);
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("shared".to_string())),
        view: left,
        content: editor_cid(),
    })
    .unwrap();

    // 与复合 view 相同的绑定形态：两个子 view 共享 content，但 selection
    // 相互独立（子 view 各自持有完整 ContentViewState）。
    app.session
        .apply_view_action(
            right,
            ViewAction::SetSelections(Selections::single(Selection::collapsed(TextOffset {
                char_index: 3,
            }))),
            app.kernel.contents(),
        )
        .unwrap();

    let left_head = app.session.views()[&left]
        .selections()
        .unwrap()
        .primary()
        .head()
        .char_index;
    let right_head = app.session.views()[&right]
        .selections()
        .unwrap()
        .primary()
        .head()
        .char_index;
    assert_eq!(left_head, "shared".chars().count());
    assert_eq!(right_head, 3);
}

#[test]
fn status_bar_is_globally_shared_by_default_and_can_be_hidden() {
    let mut app = make_app(vec![], None);
    let editor = view_id(&app, app.session.focused());
    let status = app
        .status_bar_for_view(editor)
        .expect("default editor has a status bar");

    assert_eq!(app.status_bar_placement(), StatusBarPlacement::Global);
    assert_eq!(app.status_bars_for_content(editor_cid()), vec![status]);
    assert_eq!(app.status_bar_for_view(ViewId(99)), None);

    app.set_status_bar_visible(None, false).unwrap();

    assert!(matches!(
        app.session.scene().node(status.space).space.sizing,
        Sizing::Fixed(0)
    ));
}

#[test]
fn global_status_bar_retargets_after_close_and_replace() {
    let mut app = make_app(vec![], None);
    let left_space = app.session.focused();
    let left_view = view_id(&app, left_space);
    let other = ContentId(9);
    app.kernel
        .contents_mut()
        .insert(other, Content::Buffer(Buffer::new()))
        .unwrap();
    let right_space = app
        .split_space(left_space, other, true, SplitDirection::Right, true)
        .unwrap()
        .new_space;

    app.close_space(right_space).unwrap();

    let status = app.status_bar_for_view(left_view).unwrap();
    assert_eq!(status.target_view, left_view);

    app.replace_space_content(left_space, other, true).unwrap();
    let replacement = view_id(&app, left_space);
    let status = app.status_bar_for_view(replacement).unwrap();
    assert_eq!(status.target_view, replacement);
}

#[test]
fn managed_status_bar_spaces_reject_layout_mutations() {
    for placement in [StatusBarPlacement::Global, StatusBarPlacement::PerPane] {
        let mut app = make_app(vec![], None);
        app.set_status_bar_placement(placement).unwrap();
        let editor_space = app.session.focused();
        let editor = view_id(&app, editor_space);
        let status = app.status_bar_for_view(editor).unwrap();
        let status_space = status.space;
        let revision = app.session.scene_revision();
        let next_view = app.session.next_view_id_for_test();

        assert!(matches!(
            app.close_space(status_space),
            Err(LayoutError::StatusBarSpace(space)) if space == status_space
        ));
        assert!(matches!(
            app.split_space(
                status_space,
                editor_cid(),
                true,
                SplitDirection::Right,
                true,
            ),
            Err(LayoutError::StatusBarSpace(space)) if space == status_space
        ));
        assert!(matches!(
            app.replace_space_content(status_space, editor_cid(), true),
            Err(LayoutError::StatusBarSpace(space)) if space == status_space
        ));
        assert!(matches!(
            app.set_space_sizing(status_space, Sizing::Grow(1)),
            Err(LayoutError::StatusBarSpace(space)) if space == status_space
        ));
        assert_eq!(app.session.scene_revision(), revision);
        assert_eq!(app.session.next_view_id_for_test(), next_view);
        assert!(matches!(
            app.session.scene().node(status_space).space.sizing,
            Sizing::Fixed(1)
        ));
        assert_eq!(app.status_bar_for_view(editor), Some(status));
    }
}

#[test]
fn per_pane_status_bars_cover_inert_buffer_views_in_both_creation_orders() {
    let mut before = make_app(vec![], None);
    let inert_space = before
        .split_space(
            before.session.focused(),
            editor_cid(),
            false,
            SplitDirection::Right,
            false,
        )
        .unwrap()
        .new_space;
    let inert_view = view_id(&before, inert_space);
    let nested_space = before
        .split_space(
            inert_space,
            editor_cid(),
            true,
            SplitDirection::Right,
            false,
        )
        .unwrap()
        .new_space;
    let nested_view = view_id(&before, nested_space);
    before
        .set_status_bar_placement(StatusBarPlacement::PerPane)
        .unwrap();
    assert!(before.status_bar_for_view(inert_view).is_some());
    assert!(before.status_bar_for_view(nested_view).is_some());

    let mut after = make_app(vec![], None);
    after
        .set_status_bar_placement(StatusBarPlacement::PerPane)
        .unwrap();
    let inert_space = after
        .split_space(
            after.session.focused(),
            editor_cid(),
            false,
            SplitDirection::Right,
            false,
        )
        .unwrap()
        .new_space;
    let inert_view = view_id(&after, inert_space);
    assert!(matches!(
        after.session.scene().node(inert_space).space.kind,
        SpaceKind::Content {
            focusable: false,
            ..
        }
    ));
    assert!(after.status_bar_for_view(inert_view).is_some());
    let nested_space = after
        .split_space(
            inert_space,
            editor_cid(),
            true,
            SplitDirection::Right,
            false,
        )
        .unwrap()
        .new_space;
    let nested_view = view_id(&after, nested_space);
    assert!(after.status_bar_for_view(inert_view).is_some());
    assert!(after.status_bar_for_view(nested_view).is_some());
}

#[test]
fn missing_content_is_rejected_before_layout_mutation() {
    let mut app = make_app(vec![], None);
    let focused = app.session.focused();
    let revision = app.session.scene_revision();
    let next_view = app.session.next_view_id_for_test();

    assert!(matches!(
        app.split_space(focused, ContentId(1), true, SplitDirection::Right, true,),
        Err(LayoutError::MissingContent(ContentId(1)))
    ));
    assert!(matches!(
        app.replace_space_content(focused, ContentId(1), true),
        Err(LayoutError::MissingContent(ContentId(1)))
    ));
    assert_eq!(app.session.scene_revision(), revision);
    assert_eq!(app.session.next_view_id_for_test(), next_view);
}

#[test]
fn per_pane_status_bars_are_distinct_and_independently_hidden() {
    let mut app = make_app(vec![], None);
    let left_space = app.session.focused();
    let right_space = app
        .split_space(left_space, editor_cid(), true, SplitDirection::Right, false)
        .unwrap()
        .new_space;
    let left = view_id(&app, left_space);
    let right = view_id(&app, right_space);

    app.set_status_bar_placement(StatusBarPlacement::PerPane)
        .unwrap();

    let left_status = app.status_bar_for_view(left).unwrap();
    let right_status = app.status_bar_for_view(right).unwrap();
    assert_ne!(left_status, right_status);
    assert_eq!(
        app.status_bars_for_content(editor_cid()),
        vec![left_status, right_status]
    );

    let queried_left_status = app
        .status_bars_for_content(editor_cid())
        .into_iter()
        .find(|bar| bar.target_view == left)
        .unwrap();
    app.set_status_bar_visible(Some(queried_left_status.target_view), false)
        .unwrap();
    let left_status_space = left_status.space;
    let right_status_space = right_status.space;
    assert!(matches!(
        app.session.scene().node(left_status_space).space.sizing,
        Sizing::Fixed(0)
    ));
    assert!(matches!(
        app.session.scene().node(right_status_space).space.sizing,
        Sizing::Fixed(1)
    ));

    app.close_space(right_space).unwrap();
    assert_eq!(app.status_bars_for_content(editor_cid()), vec![left_status]);
}

#[test]
fn buffer_mode_view_policy_can_replace_the_default_status_bar_presentation() {
    let app = make_script_app(
        r#"
editor.modes.define({
  name: "custom-status",
  on: {
    buffer: {
      state: () => ({}),
      viewState: () => ({
        viewPolicy: {
          statusBar: {
            left: [{ text: "left" }],
            center: [{ text: "center" }],
            right: [{ text: "right" }],
          },
        },
      }),
    },
  },
});
editor.modes.define({
  name: "shadowed-status",
  on: {
    buffer: {
      state: () => ({}),
      viewState: () => ({
        viewPolicy: {
          statusBar: {
            center: [{ text: "shadowed" }],
          },
        },
      }),
    },
  },
});
"#,
    );
    let editor = view_id(&app, app.session.focused());
    let status = app.status_bar_for_view(editor).unwrap();
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };

    let presentation = match query
        .view(status.target_view, status.space)
        .unwrap()
        .presentation
    {
        ViewPresentation::StatusBar(presentation) => presentation,
        ViewPresentation::Text(_) => panic!("expected status-bar presentation"),
    };
    assert_eq!(
        status_region_texts(&presentation),
        (
            vec!["left".to_owned()],
            vec!["center".to_owned()],
            vec!["right".to_owned()]
        )
    );
}

fn status_region_texts(
    presentation: &StatusBarPresentation,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let texts = |segments: &[vell_protocol::content_query::StatusBarSegment]| {
        segments
            .iter()
            .map(|segment| segment.text.clone())
            .collect()
    };
    (
        texts(&presentation.left),
        texts(&presentation.center),
        texts(&presentation.right),
    )
}

fn attach_save_status_mode(app: &mut App<ScriptedFrontend>) {
    let name = ModeName::new("save-status");
    app.kernel
        .modes_mut()
        .register(SaveStatusMode { name: name.clone() })
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &name).unwrap();
}

fn custom_status_center(app: &App<ScriptedFrontend>) -> String {
    status_center(app)
}

fn status_center(app: &App<ScriptedFrontend>) -> String {
    status_texts(app).1.concat()
}

fn status_texts(app: &App<ScriptedFrontend>) -> (Vec<String>, Vec<String>, Vec<String>) {
    let editor = view_id(app, app.session.focused());
    let status = app.status_bar_for_view(editor).unwrap();
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };
    let ViewPresentation::StatusBar(presentation) = query
        .view(status.target_view, status.space)
        .unwrap()
        .presentation
    else {
        panic!("expected status-bar presentation");
    };
    status_region_texts(&presentation)
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_status_message_clears_on_the_next_frontend_event() {
    let mut app = make_app(vec![], None);
    app.session
        .set_status_message("temporary failure".to_owned());
    assert_eq!(status_center(&app), "temporary failure");

    app.handle_event(FrontendEvent::Resize(ResizeEvent {
        width: 41,
        height: 6,
    }))
    .await
    .unwrap();

    assert_ne!(status_center(&app), "temporary failure");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_views_of_one_buffer_keep_independent_mode_instances() {
    let mut app = make_app(vec![], None);
    let left = app.session.focused();
    app.handle_event(FrontendEvent::Key(KeyEvent::char('i')))
        .await
        .unwrap();
    app.handle_event(FrontendEvent::Key(KeyEvent::char('a')))
        .await
        .unwrap();
    let left_id = view_id(&app, left);
    let left_revision = app.session.views()[&left_id].revision();
    let content_layer_count = app.session.presentation().content_layer_count();
    let view_layer_count = app.session.presentation().view_layer_count();
    assert!(content_layer_count > 0);
    assert!(view_layer_count > 0);
    let right = app
        .split_space(left, editor_cid(), true, SplitDirection::Right, true)
        .unwrap()
        .new_space;
    assert_eq!(
        app.session.presentation().content_layer_count(),
        content_layer_count
    );
    assert_eq!(
        app.session.presentation().view_layer_count(),
        view_layer_count * 2
    );
    assert_eq!(app.session.focused(), right);
    assert!(app.session.views()[&left_id].revision() > left_revision);

    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };
    let right_id = view_id(&app, right);
    let left_view = query
        .view(left_id, app.session.body_space_for_view(left_id).unwrap())
        .unwrap();
    let right_view = query
        .view(right_id, app.session.body_space_for_view(right_id).unwrap())
        .unwrap();
    let left_text = text_presentation(&left_view);
    let right_text = text_presentation(&right_view);

    assert_eq!(left_text.cursor_style, CursorStyle::Bar);
    assert_eq!(right_text.cursor_style, CursorStyle::Block);
    assert_ne!(left_id, right_id);
    assert_eq!(
        Some(&left_text.selections),
        app.session.views()[&left_id].selections()
    );
    assert_eq!(
        Some(&right_text.selections),
        app.session.views()[&right_id].selections()
    );
    assert_eq!(left_text.selections.primary().head().char_index, 1);
    assert_eq!(right_text.selections.primary().head().char_index, 1);
}

#[test]
fn one_mode_can_attach_canonical_adapter_to_buffer_content() {
    let mut app = make_app(vec![], None);
    let name = ModeName::new("adapter-probe");
    let mode = app
        .kernel
        .modes_mut()
        .register(AdapterProbeMode { name: name.clone() })
        .unwrap();

    assert!(
        app.kernel
            .modes()
            .adapter(mode, ContentKind::Buffer)
            .is_some()
    );
    app.attach_mode_to_content(editor_cid(), &name).unwrap();

    assert_eq!(
        app.kernel
            .content_modes()
            .state_for_test::<AdapterProbeState>(mode, editor_cid()),
        Some(&AdapterProbeState {
            kind: ContentKind::Buffer,
        })
    );
    for (view, kind) in app
        .session
        .views()
        .iter()
        .map(|(id, view)| (*id, app.kernel.contents().kind(view.content()).unwrap()))
    {
        assert_eq!(
            app.session
                .view_modes()
                .state_for_test::<AdapterProbeState>(mode, view),
            Some(&AdapterProbeState { kind })
        );
    }
}

#[test]
fn attach_to_missing_content_is_structured_and_leaves_no_partial_profile() {
    let mut app = make_app(vec![], None);
    let name = ModeName::new("buffer-only");
    app.kernel
        .modes_mut()
        .register(HighlightMode {
            name: name.clone(),
            syntax_color: Color::Rgb {
                red: 1,
                green: 2,
                blue: 3,
            },
        })
        .unwrap();
    let missing = ContentId(1);
    let profile_before = app.session.mode_chain_for_new_view(missing);

    let error = app.attach_mode_to_content(missing, &name).unwrap_err();

    assert_eq!(error, ModeAttachmentError::UnknownContent(missing));
    assert_eq!(app.session.mode_chain_for_new_view(missing), profile_before);

    assert_eq!(
        app.attach_mode_to_content(editor_cid(), &ModeName::new("missing")),
        Err(ModeAttachmentError::UnknownMode(ModeName::new("missing")))
    );
    assert_eq!(
        app.attach_mode_to_content(ContentId(99), &name),
        Err(ModeAttachmentError::UnknownContent(ContentId(99)))
    );
}

#[test]
fn mode_invocation_rejects_an_unregistered_content() {
    let mut app = make_app(vec![], None);
    let mode = ModeName::new("shared-content");
    app.kernel
        .modes_mut()
        .register(SharedContentMode::new())
        .unwrap();
    let missing = ContentId(1);

    let error = app
        .execute_command(DispatchCommand::Mode {
            command: ModeCommand::new(mode, ModeActionName::new("advance")),
            view: view_id(&app, app.session.focused()),
            content: missing,
        })
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("targets missing content"));
}

#[tokio::test(flavor = "multi_thread")]
async fn content_mode_binding_is_shared_and_coexists_with_view_modes() {
    let mut app = make_app(vec![], None);
    let mode = ModeName::new("shared-content");
    app.kernel
        .modes_mut()
        .register(SharedContentMode::new())
        .unwrap();
    let existing_view = view_id(&app, app.session.focused());
    let existing_revision = app.session.views()[&existing_view].revision();
    app.attach_mode_to_content(editor_cid(), &mode).unwrap();
    assert!(app.session.views()[&existing_view].revision() > existing_revision);

    let left = app.session.focused();
    let right = app
        .split_space(left, editor_cid(), true, SplitDirection::Right, true)
        .unwrap()
        .new_space;
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };
    for space in [left, right] {
        let view = query.view(view_id(&app, space), space).unwrap();
        assert_eq!(text_presentation(&view).cursor_style, CursorStyle::Block);
    }

    let command = ModeCommand {
        mode: mode.clone(),
        action: ModeActionName::new("advance"),
        arguments: Default::default(),
    };
    app.handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();
    assert_eq!(
        app.kernel
            .execute_mode_content_action(editor_cid(), &command)
            .unwrap(),
        ModeResult::operations(vec![history(TransactionIntent::Redo)])
    );

    app.close_space(left).unwrap();
    assert_eq!(
        app.kernel
            .execute_mode_content_action(editor_cid(), &command)
            .unwrap(),
        ModeResult::operations(vec![save()])
    );
}

#[test]
fn new_buffers_have_unique_ids_and_the_default_mode_profile() {
    let mut app = make_app(vec![], None);
    let expected_modes = app.session.mode_chain_for_new_view(editor_cid());

    let first = app.new_buffer();
    let second = app.new_buffer();

    assert_ne!(first, second);
    assert!(app.kernel.contents().contains(first));
    assert!(app.kernel.contents().contains(second));
    assert_eq!(app.kernel.contents().kind(first), Some(ContentKind::Buffer));
    assert_eq!(app.session.mode_chain_for_new_view(first), expected_modes);
    assert_eq!(app.session.mode_chain_for_new_view(second), expected_modes);
    let info = app
        .buffers()
        .into_iter()
        .find(|buffer| buffer.content == first)
        .unwrap();
    assert_eq!(info.resource_name, None);
    assert_eq!(info.resource_path, None);
    assert_eq!(info.backing_state, BufferBackingState::Untitled);
    assert_eq!(info.dirty_state, DirtyState::Clean);
    assert_eq!(info.save_state, SaveState::Idle);
}

#[tokio::test]
async fn typed_content_open_does_not_switch_the_focused_view() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("typed-open.txt");
    std::fs::write(&path, "opened").unwrap();
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![OperationRequest::ContentLifecycle(
            ContentLifecycleOperation::Open {
                path: path.to_string_lossy().into_owned(),
            },
        )],
        view,
        content: editor_cid(),
    })
    .unwrap();
    let message = app.kernel.receive_message().await.unwrap();
    app.handle_app_message(message).unwrap();

    let content = app
        .buffers()
        .into_iter()
        .find(|buffer| buffer.resource_name.as_deref() == Some("typed-open.txt"))
        .unwrap()
        .content;
    let focused_view = view_id(&app, app.session.focused());
    assert_eq!(
        app.session.view(focused_view).unwrap().content(),
        editor_cid()
    );
    assert_eq!(text_rows(&app, content), vec!["opened"]);
}

#[tokio::test]
async fn typed_content_and_view_operations_cover_the_lifecycle() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("typed-lifecycle.txt");
    let mut app = make_app(vec![], None);
    let original = editor_cid();
    let view = view_id(&app, app.session.focused());

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![OperationRequest::ContentLifecycle(
            ContentLifecycleOperation::Create,
        )],
        view,
        content: original,
    })
    .unwrap();
    let created = app
        .buffers()
        .into_iter()
        .map(|buffer| buffer.content)
        .find(|content| *content != original)
        .unwrap();
    assert_ne!(created, original);
    assert_eq!(app.session.views()[&view].content(), original);

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![OperationRequest::ViewLifecycle(
            ViewLifecycleOperation::Switch {
                spec: ViewSpec::buffer(created),
            },
        )],
        view,
        content: original,
    })
    .unwrap();
    let view = view_id(&app, app.session.focused());
    assert_eq!(app.session.views()[&view].content(), created);
    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![OperationRequest::ViewLifecycle(
            ViewLifecycleOperation::Switch {
                spec: ViewSpec::buffer(original),
            },
        )],
        view,
        content: created,
    })
    .unwrap();
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("local".to_owned())),
        view,
        content: original,
    })
    .unwrap();
    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![OperationRequest::ContentLifecycle(
            ContentLifecycleOperation::SaveAs {
                target: ContentTarget::Current,
                path: path.to_string_lossy().into_owned(),
                force: false,
            },
        )],
        view,
        content: original,
    })
    .unwrap();
    while app.kernel.has_pending_save(original) {
        let message = app.kernel.receive_message().await.unwrap();
        app.handle_app_message(message).unwrap();
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "local");

    std::fs::write(&path, "external").unwrap();
    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![OperationRequest::ContentLifecycle(
            ContentLifecycleOperation::Reload {
                target: ContentTarget::Current,
                force: true,
            },
        )],
        view,
        content: original,
    })
    .unwrap();
    assert_eq!(text_rows(&app, original), vec!["external"]);
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("!".to_owned())),
        view,
        content: original,
    })
    .unwrap();

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![OperationRequest::ContentLifecycle(
            ContentLifecycleOperation::Save {
                target: ContentTarget::Current,
                force: false,
            },
        )],
        view,
        content: original,
    })
    .unwrap();
    while app.kernel.has_pending_save(original) {
        let message = app.kernel.receive_message().await.unwrap();
        app.handle_app_message(message).unwrap();
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "external!");

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![OperationRequest::ContentLifecycle(
            ContentLifecycleOperation::Close {
                target: ContentTarget::Id(created),
                force: false,
            },
        )],
        view,
        content: original,
    })
    .unwrap();
    assert!(!app.kernel.contents().contains(created));
}

#[tokio::test]
async fn typed_cross_buffer_save_and_reload_use_the_target_frame() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cross-target.txt");
    std::fs::write(&path, "before").unwrap();
    let mut app = make_app(vec![], None);
    let origin = editor_cid();
    let target = app.open_buffer(&path).unwrap();
    std::fs::write(&path, "after").unwrap();
    let view = view_id(&app, app.session.focused());

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![OperationRequest::ContentLifecycle(
            ContentLifecycleOperation::Reload {
                target: ContentTarget::Id(target),
                force: true,
            },
        )],
        view,
        content: origin,
    })
    .unwrap();
    assert_eq!(text_rows(&app, target), vec!["after"]);

    let target_view = switch_focused_view(&mut app, target).unwrap();
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("!".to_owned())),
        view: target_view,
        content: target,
    })
    .unwrap();
    switch_focused_view(&mut app, origin).unwrap();
    let origin_view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![OperationRequest::ContentLifecycle(
            ContentLifecycleOperation::Save {
                target: ContentTarget::Id(target),
                force: false,
            },
        )],
        view: origin_view,
        content: origin,
    })
    .unwrap();
    while app.kernel.has_pending_save(target) {
        let message = app.kernel.receive_message().await.unwrap();
        app.handle_app_message(message).unwrap();
    }
    assert_eq!(std::fs::read_to_string(path).unwrap(), "!after");
}

#[tokio::test]
async fn mode_input_can_save_and_reload_an_explicit_buffer_target() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mode-target.txt");
    std::fs::write(&path, "before").unwrap();
    let mut app = make_app(vec![], None);
    let target = app.open_buffer(&path).unwrap();
    let target_view = switch_focused_view(&mut app, target).unwrap();
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("!".to_owned())),
        view: target_view,
        content: target,
    })
    .unwrap();
    switch_focused_view(&mut app, editor_cid()).unwrap();

    let save_mode = ModeName::new("cross-save-probe");
    app.kernel
        .modes_mut()
        .register(ChainProbeMode::with_sequence(
            save_mode.as_str(),
            vec![KeyEvent::char('q')],
            vec![OperationRequest::ContentLifecycle(
                ContentLifecycleOperation::Save {
                    target: ContentTarget::Id(target),
                    force: false,
                },
            )],
            false,
        ))
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &save_mode)
        .unwrap();

    app.handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();
    while app.kernel.has_pending_save(target) {
        let message = app.kernel.receive_message().await.unwrap();
        app.handle_app_message(message).unwrap();
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "!before");

    std::fs::write(&path, "after").unwrap();
    let reload_mode = ModeName::new("cross-reload-probe");
    app.kernel
        .modes_mut()
        .register(ChainProbeMode::with_sequence(
            reload_mode.as_str(),
            vec![KeyEvent::char('r')],
            vec![OperationRequest::ContentLifecycle(
                ContentLifecycleOperation::Reload {
                    target: ContentTarget::Id(target),
                    force: true,
                },
            )],
            false,
        ))
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &reload_mode)
        .unwrap();

    app.handle_event(FrontendEvent::Key(KeyEvent::char('r')))
        .await
        .unwrap();
    assert_eq!(text_rows(&app, target), vec!["after"]);
}

#[test]
fn typed_content_list_surfaces_metadata() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    app.new_buffer();

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![OperationRequest::ContentLifecycle(
            ContentLifecycleOperation::List,
        )],
        view,
        content: editor_cid(),
    })
    .unwrap();

    let listing = &app.runtime_diagnostics.last().unwrap().message;
    assert!(listing.contains("0: [untitled]"), "{listing}");
    assert!(listing.contains("1: [untitled]"), "{listing}");
}

#[tokio::test]
async fn async_open_ignores_superseded_and_stale_target_results() {
    let directory = tempfile::tempdir().unwrap();
    let first_path = directory.path().join("first.txt");
    let second_path = directory.path().join("second.txt");
    let mut app = make_app(vec![], None);
    let target = app.session.focused();
    let expected_view = app.session.view_for_space(target);
    let first = app
        .kernel
        .queue_view_open(target, expected_view, first_path.clone());
    let second = app
        .kernel
        .queue_view_open(target, expected_view, second_path.clone());
    let opened = |path: &std::path::Path, text: &str| OpenedPath {
        path: path.to_owned(),
        identity: normalize_path(path).unwrap().1,
        buffer: OpenedBuffer {
            content: Content::buffer_from_file(path.to_owned(), text.to_owned()),
            baseline: FileBaseline::Materialized(text.to_owned()),
        },
    };

    assert!(
        !app.complete_async_open(first, Ok(opened(&first_path, "first")))
            .unwrap()
    );
    assert!(
        app.complete_async_open(second, Ok(opened(&second_path, "second")))
            .unwrap()
    );
    let focused_view = view_id(&app, target);
    let focused_content = app.session.view(focused_view).unwrap().content();
    assert_eq!(text_rows(&app, focused_content), vec!["second"]);

    let third_path = directory.path().join("third.txt");
    let expected_view = app.session.view_for_space(target);
    let third = app
        .kernel
        .queue_view_open(target, expected_view, third_path.clone());
    let replacement = app.new_buffer();
    app.replace_space_content(target, replacement, true)
        .unwrap();

    assert!(
        app.complete_async_open(third, Ok(opened(&third_path, "third")))
            .unwrap()
    );
    assert_eq!(
        app.session
            .view_for_space(target)
            .and_then(|view| app.session.view(view))
            .unwrap()
            .content(),
        replacement
    );
    assert!(!app.kernel.contents().contains(third));
    app.kernel.cancel();
}

#[test]
fn typed_view_focus_targets_an_existing_view() {
    let mut app = make_app(vec![], None);
    let left_space = app.session.focused();
    let left = view_id(&app, left_space);
    let right_space = app
        .split_space(left_space, editor_cid(), true, SplitDirection::Right, false)
        .unwrap()
        .new_space;
    let right = view_id(&app, right_space);

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![OperationRequest::ViewLifecycle(
            ViewLifecycleOperation::Focus { view: right },
        )],
        view: left,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(app.session.focused(), right_space);
}

#[test]
fn typed_view_switch_can_create_its_buffer_view_source() {
    let mut app = make_app(vec![], None);
    let original_view = view_id(&app, app.session.focused());

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![OperationRequest::ViewLifecycle(
            ViewLifecycleOperation::Switch {
                spec: ViewSpec::Buffer {
                    source: BufferViewSource::Create,
                },
            },
        )],
        view: original_view,
        content: editor_cid(),
    })
    .unwrap();

    let switched = view_id(&app, app.session.focused());
    assert_ne!(switched, original_view);
    assert_ne!(app.session.view(switched).unwrap().content(), editor_cid());
    assert_eq!(app.buffers().len(), 2);
}

#[test]
fn typed_lifecycle_operation_rejects_a_mixed_frame_before_mutating() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    let before = app.buffers().len();

    let error = app
        .execute_command(DispatchCommand::ModeOperations {
            operations: vec![
                OperationRequest::ContentLifecycle(ContentLifecycleOperation::Create),
                view_edit(EditCommand::InsertText("unexpected".to_owned())),
            ],
            view,
            content: editor_cid(),
        })
        .unwrap_err();

    assert!(error.to_string().contains("only operation"));
    assert_eq!(app.buffers().len(), before);
    assert_eq!(text_rows(&app, editor_cid()), vec![""]);

    let error = app
        .execute_command(DispatchCommand::ModeOperations {
            operations: vec![
                view_edit(EditCommand::InsertText("unexpected".to_owned())),
                OperationRequest::ContentLifecycle(ContentLifecycleOperation::Create),
            ],
            view,
            content: editor_cid(),
        })
        .unwrap_err();

    assert!(error.to_string().contains("only operation"));
    assert_eq!(app.buffers().len(), before);
    assert_eq!(text_rows(&app, editor_cid()), vec![""]);
}

#[test]
fn open_buffer_deduplicates_normalized_paths_and_loads_text() {
    let directory = tempfile::tempdir().unwrap();
    let nested = directory.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    let path = directory.path().join("open.txt");
    std::fs::write(&path, "loaded\n").unwrap();
    let alias = nested.join("..").join("open.txt");
    let mut app = make_app(vec![], None);

    let content = app.open_buffer(&path).unwrap();
    let duplicate = app.open_buffer(&alias).unwrap();

    assert_eq!(duplicate, content);
    assert_eq!(
        app.kernel
            .contents()
            .text_snapshot(content)
            .unwrap()
            .to_owned_string(),
        "loaded\n"
    );
    let info = app
        .buffers()
        .into_iter()
        .find(|buffer| buffer.content == content)
        .unwrap();
    assert_eq!(info.backing_state, BufferBackingState::Materialized);
    assert_eq!(info.dirty_state, DirtyState::Clean);
}

#[cfg(windows)]
#[test]
fn missing_windows_paths_use_case_insensitive_identity_by_default() {
    let directory = tempfile::tempdir().unwrap();
    let lower = directory.path().join("missing.rs");
    let upper = directory.path().join("MISSING.RS");

    let lower_identity = normalize_path(&lower).unwrap().1;
    let upper_identity = normalize_path(&upper).unwrap().1;

    assert_eq!(lower_identity, upper_identity);
}

#[cfg(windows)]
#[test]
fn windows_path_separators_share_identity() {
    let directory = tempfile::tempdir().unwrap();
    let native = directory.path().join("same.rs");
    std::fs::write(&native, "text").unwrap();
    let forward = std::path::PathBuf::from(
        native
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/"),
    );

    assert_eq!(
        normalize_path(&native).unwrap().1,
        normalize_path(&forward).unwrap().1
    );
}

#[cfg(unix)]
#[test]
fn open_buffer_preserves_symlink_parent_semantics() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    let outside = directory.path().join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir_all(outside.join("dir")).unwrap();
    std::fs::write(root.join("file.txt"), "wrong").unwrap();
    std::fs::write(outside.join("file.txt"), "outside").unwrap();
    symlink(outside.join("dir"), root.join("link")).unwrap();
    let requested = root.join("link").join("..").join("file.txt");
    let mut app = make_app(vec![], None);

    let content = app.open_buffer(requested).unwrap();

    assert_eq!(text_rows(&app, content), vec!["outside"]);
}

#[test]
fn open_buffer_tracks_a_missing_path_as_unmaterialized() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("new.txt");
    let mut app = make_app(vec![], None);

    let content = app.open_buffer(&path).unwrap();

    let info = app
        .buffers()
        .into_iter()
        .find(|buffer| buffer.content == content)
        .unwrap();
    assert_eq!(info.backing_state, BufferBackingState::Unmaterialized);
    let expected_path = super::buffer_lifecycle::normalize_path(&path)
        .unwrap()
        .0
        .to_string_lossy()
        .into_owned();
    assert_eq!(info.resource_path.as_deref(), Some(expected_path.as_str()));
    assert!(matches!(
        app.kernel.buffer_path_record(content),
        Some((_, super::kernel::FileBaseline::Missing))
    ));
}

#[test]
fn reload_buffer_guards_dirty_text_and_force_installs_disk_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reload.txt");
    std::fs::write(&path, "before").unwrap();
    let mut app = make_app(vec![], None);
    let content = app.open_buffer(&path).unwrap();
    let view = switch_focused_view(&mut app, content).unwrap();
    app.split_space(
        app.session.focused(),
        content,
        true,
        SplitDirection::Right,
        true,
    )
    .unwrap();
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("local ".to_owned())),
        view,
        content,
    })
    .unwrap();
    let other_view = *app
        .session
        .views()
        .iter()
        .find(|(candidate, state)| **candidate != view && state.content() == content)
        .unwrap()
        .0;
    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![view_action(ViewAction::SetSelections(Selections::single(
            Selection::collapsed(TextOffset::origin()),
        )))],
        view: other_view,
        content,
    })
    .unwrap();
    let mut before_heads = app
        .session
        .views()
        .values()
        .filter(|view| view.content() == content)
        .map(|view| view.selections().unwrap().primary().head().char_index)
        .collect::<Vec<_>>();
    before_heads.sort_unstable();
    assert_eq!(before_heads, vec![0, "local ".chars().count()]);
    std::fs::write(&path, "external").unwrap();

    assert!(matches!(
        app.reload_buffer(content, false),
        Err(super::BufferLifecycleError::Dirty(target)) if target == content
    ));
    assert_eq!(text_rows(&app, content), vec!["local before"]);

    app.reload_buffer(content, true).unwrap();

    assert_eq!(text_rows(&app, content), vec!["external"]);
    let mut reloaded_heads = app
        .session
        .views()
        .values()
        .filter(|view| view.content() == content)
        .map(|view| view.selections().unwrap().primary().head().char_index)
        .collect::<Vec<_>>();
    reloaded_heads.sort_unstable();
    assert_eq!(
        reloaded_heads,
        vec!["external".chars().count(), "external".chars().count()]
    );
    assert_eq!(dirty_state(&app, content), DirtyState::Clean);
    assert_eq!(
        app.kernel.history_behavior_for_test(content),
        (false, None, 0, 0)
    );
}

#[test]
fn reload_buffer_marks_a_deleted_file_unmaterialized() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("deleted.txt");
    std::fs::write(&path, "before").unwrap();
    let mut app = make_app(vec![], None);
    let content = app.open_buffer(&path).unwrap();
    std::fs::remove_file(&path).unwrap();

    app.reload_buffer(content, false).unwrap();

    assert_eq!(text_rows(&app, content), vec![""]);
    let info = app
        .buffers()
        .into_iter()
        .find(|buffer| buffer.content == content)
        .unwrap();
    assert_eq!(info.backing_state, BufferBackingState::Unmaterialized);
}

#[test]
fn reload_buffer_rejects_an_untitled_buffer() {
    let mut app = make_app(vec![], None);
    let content = app.new_buffer();

    assert!(matches!(
        app.reload_buffer(content, true),
        Err(super::BufferLifecycleError::NoPath(target)) if target == content
    ));
}

#[test]
fn switching_the_focused_view_replaces_it_and_is_idempotent() {
    let mut app = make_app(vec![], None);
    let original = view_id(&app, app.session.focused());
    let content = app.new_buffer();

    let switched = switch_focused_view(&mut app, content).unwrap();

    assert_ne!(switched, original);
    assert_eq!(view_id(&app, app.session.focused()), switched);
    assert_eq!(app.session.view(switched).unwrap().content(), content);
    assert_eq!(switch_focused_view(&mut app, content).unwrap(), switched);
}

#[test]
fn close_buffer_rejects_dirty_content_and_force_replaces_its_last_view() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    let content_modes = app
        .session
        .mode_chain_for_new_view(editor_cid())
        .iter()
        .map(|name| app.kernel.modes().resolve_mode(name).unwrap())
        .collect::<Vec<_>>();
    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![OperationRequest::Face(FaceOperation::SetBase {
            target: FaceRemapTarget::CurrentContent,
            face: FaceName::new("ui.text"),
            expressions: Some(vec![FaceExpr::Named(FaceName::new("ui.selection"))]),
        })],
        view,
        content: editor_cid(),
    })
    .unwrap();
    assert!(app.session.has_content_face_remaps_for_test(editor_cid()));
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("dirty".to_owned())),
        view,
        content: editor_cid(),
    })
    .unwrap();
    let job_cancellation = app
        .kernel
        .track_mode_job_for_test(content_modes[0], editor_cid());
    assert!(!job_cancellation.is_cancelled());

    assert!(matches!(
        app.close_buffer(editor_cid(), false),
        Err(super::BufferLifecycleError::Dirty(content)) if content == editor_cid()
    ));
    assert!(app.kernel.contents().contains(editor_cid()));

    app.close_buffer(editor_cid(), true).unwrap();

    assert!(!app.kernel.contents().contains(editor_cid()));
    assert!(
        app.session
            .views()
            .values()
            .all(|view| view.content() != editor_cid())
    );
    assert_eq!(
        app.kernel.history_behavior_for_test(editor_cid()),
        (false, None, 0, 0)
    );
    assert!(app.session.mode_chain_for_new_view(editor_cid()).is_empty());
    assert!(content_modes.into_iter().all(|mode| {
        app.kernel
            .content_modes()
            .revision(mode, editor_cid())
            .is_none()
    }));
    assert!(!app.kernel.has_mode_jobs_for_content_for_test(editor_cid()));
    assert!(job_cancellation.is_cancelled());
    assert!(!app.session.has_content_face_remaps_for_test(editor_cid()));
    assert_eq!(app.buffers().len(), 1);
}

#[test]
fn close_buffer_removes_all_views_of_shared_content() {
    let mut app = make_app(vec![], None);
    let left = app.session.focused();
    app.split_space(left, editor_cid(), true, SplitDirection::Right, true)
        .unwrap();

    app.close_buffer(editor_cid(), false).unwrap();

    assert!(!app.kernel.contents().contains(editor_cid()));
    assert!(
        app.session
            .views()
            .values()
            .all(|view| view.content() != editor_cid())
    );
    assert_eq!(focusable_view_count(app.session.scene()), 1);
}

#[test]
fn close_buffer_rejects_a_pending_save_even_when_forced() {
    let mut app = make_app(vec![], None);
    app.kernel.track_pending_save_for_test(
        editor_cid(),
        1,
        vell_core::transaction::TextStateId(1),
        None,
    );

    assert!(matches!(
        app.close_buffer(editor_cid(), true),
        Err(super::BufferLifecycleError::PendingSave(content)) if content == editor_cid()
    ));
    assert!(app.kernel.contents().contains(editor_cid()));
    let directory = tempfile::tempdir().unwrap();
    assert!(matches!(
        app.save_buffer_as(editor_cid(), directory.path().join("pending.txt"), false),
        Err(super::BufferLifecycleError::PendingSave(content)) if content == editor_cid()
    ));
}

#[test]
fn switching_to_a_buffer_view_rejects_missing_content() {
    let mut app = make_app(vec![], None);

    assert!(matches!(
        switch_focused_view(&mut app, ContentId(u64::MAX)),
        Err(LayoutError::MissingContent(_))
    ));
}

#[test]
fn dynamic_attachment_profiles_content_before_its_first_view() {
    let mut app = make_app(vec![], None);
    let mode = ModeName::new("shared-content");
    app.kernel
        .modes_mut()
        .register(SharedContentMode::new())
        .unwrap();
    let other = ContentId(9);
    app.kernel
        .contents_mut()
        .insert(other, Content::Buffer(Buffer::new()))
        .unwrap();

    app.attach_mode_to_content(other, &mode).unwrap();
    let space = app
        .split_space(
            app.session.focused(),
            other,
            true,
            SplitDirection::Right,
            true,
        )
        .unwrap()
        .new_space;
    let view = view_id(&app, space);

    assert_eq!(app.session.view_modes().mode_names(view), vec![mode]);
}

#[tokio::test(flavor = "multi_thread")]
async fn content_mode_keymap_tracks_current_content() {
    let mut app = make_app(vec![], None);
    let mode = ModeName::new("content-aware-keymap");
    app.kernel
        .modes_mut()
        .register(ContentAwareKeymapMode::new())
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &mode).unwrap();

    app.handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();
    app.handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["a"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn mode_can_handle_input_then_continue_to_the_next_mode() {
    let mut app = make_app(vec![], None);
    app.kernel
        .modes_mut()
        .register(ChainProbeMode::new(
            "first-probe",
            vec![view_edit(EditCommand::InsertText("a".to_string()))],
            true,
        ))
        .unwrap();
    app.kernel
        .modes_mut()
        .register(ChainProbeMode::new(
            "second-probe",
            vec![view_edit(EditCommand::InsertText("b".to_string()))],
            false,
        ))
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &ModeName::new("first-probe"))
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &ModeName::new("second-probe"))
        .unwrap();

    app.handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["ab"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn script_pass_continues_through_modes_in_attachment_order() {
    let mut app = make_script_app(
        r#"
editor.modes.define({
  name: "first",
  on: {
    buffer: {
      commands: {
        type(ctx) {
          ctx.edit.insert("a");
          return ctx.pass();
        },
      },
      keys: { "q": "type" },
    },
  },
});
editor.modes.define({
  name: "second",
  on: {
    buffer: {
      commands: {
        type(ctx) {
          ctx.edit.insert("b");
        },
      },
      keys: { "q": "type" },
    },
  },
});
"#,
    );

    app.handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["ab"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_command_flow_does_not_override_the_caller_flow() {
    let mut stopped = make_script_app(
        r#"
editor.modes.define({
  name: "stopping-caller",
  on: {
    buffer: {
      commands: { run(ctx) { ctx.commands.invoke("callee.pass"); } },
      keys: { "q": "run" },
    },
  },
});
editor.modes.define({
  name: "callee",
  on: {
    buffer: {
      commands: {
        pass(ctx) { return ctx.pass(); },
        stop() {},
      },
    },
  },
});
editor.modes.define({
  name: "fallback",
  on: {
    buffer: {
      commands: { run(ctx) { ctx.edit.insert("f"); } },
      keys: { "q": "run" },
    },
  },
});
"#,
    );

    stopped
        .handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();
    assert_eq!(text_rows(&stopped, editor_cid()), vec![""]);

    let mut passed = make_script_app(
        r#"
editor.modes.define({
  name: "passing-caller",
  on: {
    buffer: {
      commands: {
        run(ctx) {
          ctx.commands.invoke("callee.stop");
          return ctx.pass();
        },
      },
      keys: { "q": "run" },
    },
  },
});
editor.modes.define({
  name: "callee",
  on: { buffer: { commands: { stop() {} } } },
});
editor.modes.define({
  name: "fallback",
  on: {
    buffer: {
      commands: { run(ctx) { ctx.edit.insert("f"); } },
      keys: { "q": "run" },
    },
  },
});
"#,
    );

    passed
        .handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();
    assert_eq!(text_rows(&passed, editor_cid()), vec!["f"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_command_isolates_flow_from_the_entire_invocation_subtree() {
    let mut app = make_script_app(
        r#"
editor.modes.define({
  name: "outer",
  on: {
    buffer: {
      commands: { run(ctx) { ctx.commands.invoke("delegator.delegate"); } },
      keys: { "q": "run" },
    },
  },
});
editor.modes.define({
  name: "delegator",
  on: {
    buffer: {
      commands: {
        delegate(ctx) {
          ctx.commands.invoke("passer.pass");
        },
      },
    },
  },
});
editor.modes.define({
  name: "passer",
  on: {
    buffer: {
      commands: {
        pass(ctx) {
          return ctx.pass();
        },
      },
    },
  },
});
editor.modes.define({
  name: "fallback",
  on: {
    buffer: {
      commands: { run(ctx) { ctx.edit.insert("f"); } },
      keys: { "q": "run" },
    },
  },
});
"#,
    );

    app.handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec![""]);
}

#[tokio::test(flavor = "multi_thread")]
async fn later_mode_prefix_does_not_delay_an_earlier_exact_binding() {
    let mut app = make_app(vec![], None);
    app.kernel
        .modes_mut()
        .register(ChainProbeMode::new(
            "first-probe",
            vec![view_edit(EditCommand::InsertText("a".to_string()))],
            true,
        ))
        .unwrap();
    app.kernel
        .modes_mut()
        .register(ChainProbeMode::with_sequence(
            "second-probe",
            vec![KeyEvent::char('q'), KeyEvent::char('r')],
            vec![view_edit(EditCommand::InsertText("b".to_string()))],
            false,
        ))
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &ModeName::new("first-probe"))
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &ModeName::new("second-probe"))
        .unwrap();

    app.handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["a"]);

    app.handle_event(FrontendEvent::Key(KeyEvent::char('r')))
        .await
        .unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["ba"]);
}

#[test]
fn mode_decorations_are_resolved_through_named_faces() {
    let mut app = make_app(vec![], None);
    app.kernel
        .modes_mut()
        .register(HighlightMode {
            name: ModeName::new("highlight-probe"),
            syntax_color: Color::Rgb {
                red: 1,
                green: 2,
                blue: 3,
            },
        })
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &ModeName::new("highlight-probe"))
        .unwrap();
    let view = view_id(&app, app.session.focused());
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };

    let view_data = query
        .view(view, app.session.body_space_for_view(view).unwrap())
        .unwrap();
    let presentation = text_presentation(&view_data);
    let decorations = query
        .decorations(
            view,
            app.session.body_space_for_view(view).unwrap(),
            RowRange { start: 0, end: 1 },
        )
        .unwrap();

    assert_eq!(decorations.len(), 1);
    assert_eq!(
        decorations[0].face.foreground,
        Some(Color::Rgb {
            red: 1,
            green: 2,
            blue: 3,
        })
    );
    assert_eq!(presentation.selection_face.background, Some(Color::Ansi(4)));
    assert_eq!(presentation.tab_width, 8);
}

#[test]
fn mode_diagnostics_report_policy_decorations_and_face_conflicts() {
    let mut app = make_app(vec![], None);
    let first = ModeName::new("diagnostic-highlight-first");
    let second = ModeName::new("diagnostic-highlight-second");
    // The theme registry treats identical face definitions as idempotent, so
    // the second mode must register a diverging definition to surface a conflict.
    for (name, color) in [
        (
            &first,
            Color::Rgb {
                red: 1,
                green: 2,
                blue: 3,
            },
        ),
        (
            &second,
            Color::Rgb {
                red: 4,
                green: 5,
                blue: 6,
            },
        ),
    ] {
        app.kernel
            .modes_mut()
            .register(HighlightMode {
                name: name.clone(),
                syntax_color: color,
            })
            .unwrap();
        app.attach_mode_to_content(editor_cid(), name).unwrap();
    }
    let view = view_id(&app, app.session.focused());

    let diagnostics = app
        .mode_diagnostics()
        .into_iter()
        .find(|entry| entry.view == view)
        .unwrap();
    assert_eq!(
        diagnostics.policy_sources.selection_face,
        Some(first.clone())
    );
    assert_eq!(diagnostics.policy_sources.tab_width, Some(first.clone()));
    assert_eq!(
        diagnostics
            .decorations
            .iter()
            .find(|entry| entry.mode == first)
            .map(|entry| entry.view_count),
        Some(1)
    );
    assert_eq!(
        app.face_provider(&FaceName::new("plugin.highlight-test.syntax")),
        Some(&first)
    );
    assert!(app.face_conflicts().iter().any(|conflict| {
        conflict.face == FaceName::new("plugin.highlight-test.syntax")
            && conflict.active_provider.as_ref() == Some(&first)
            && conflict.rejected_provider == second
    }));
}

#[test]
fn render_reads_cached_presentation_without_calling_mode() {
    let mut app = make_app(vec![], None);
    let calls = Rc::new(Cell::new(0));
    let name = ModeName::new("presentation-probe");
    app.kernel
        .modes_mut()
        .register(PresentationProbeMode {
            name: name.clone(),
            calls: calls.clone(),
            max_rows: None,
        })
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &name).unwrap();
    let calls_after_refresh = calls.get();
    assert!(calls_after_refresh > 0);

    app.render().unwrap();

    assert_eq!(calls.get(), calls_after_refresh);
}

#[test]
fn presentation_refresh_recomputes_only_dirty_layers() {
    let mut app = make_app(vec![], None);
    let calls = Rc::new(Cell::new(0));
    let name = ModeName::new("incremental-presentation-probe");
    app.kernel
        .modes_mut()
        .register(PresentationProbeMode {
            name: name.clone(),
            calls: calls.clone(),
            max_rows: None,
        })
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &name).unwrap();

    let after_attach = calls.get();
    app.session
        .refresh_presentation(app.kernel.contents(), app.kernel.content_modes());
    assert_eq!(calls.get(), after_attach);

    let left = app.session.focused();
    app.split_space(left, editor_cid(), true, SplitDirection::Right, false)
        .unwrap();
    assert_eq!(calls.get(), after_attach + 2);

    let left_view = view_id(&app, left);
    app.session.view_mut(left_view).unwrap().touch();
    app.session
        .refresh_presentation(app.kernel.contents(), app.kernel.content_modes());
    assert_eq!(calls.get(), after_attach + 4);

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("x".to_string())),
        view: left_view,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(calls.get(), after_attach + 9);
}

#[test]
fn presentation_refresh_uses_a_finite_large_document_range() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("x\n".repeat(10_000))),
        view,
        content: editor_cid(),
    })
    .unwrap();

    let calls = Rc::new(Cell::new(0));
    let max_rows = Rc::new(Cell::new(0));
    let name = ModeName::new("large-presentation-probe");
    app.kernel
        .modes_mut()
        .register(PresentationProbeMode {
            name: name.clone(),
            calls,
            max_rows: Some(max_rows.clone()),
        })
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &name).unwrap();

    assert_eq!(max_rows.get(), 10_001);
}

#[test]
fn passive_mode_failure_does_not_rollback_text_and_suspends_presentation() {
    let mut app = make_app(vec![], None);
    let mode = ModeName::new("faulting-highlight");
    app.kernel
        .modes_mut()
        .register(FaultingHighlightMode { name: mode.clone() })
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &mode).unwrap();
    let view = view_id(&app, app.session.focused());
    {
        let query = AppQuery {
            contents: app.kernel.contents(),
            views: app.session.views(),
            presentation: app.session.presentation(),
            faces: app.session.faces(),
        };
        assert_eq!(
            query
                .decorations(
                    view,
                    app.session.body_space_for_view(view).unwrap(),
                    RowRange { start: 0, end: 1 }
                )
                .unwrap()
                .len(),
            1
        );
    }

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("x".to_string())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["x"]);
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };
    assert!(
        query
            .decorations(
                view,
                app.session.body_space_for_view(view).unwrap(),
                RowRange { start: 0, end: 1 }
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn mode_factory_failures_suspend_only_the_failed_attachments() {
    let mut app = make_app(vec![], None);
    for (name, fail_content) in [
        ("content-factory-fault", true),
        ("view-factory-fault", false),
    ] {
        let mode = ModeName::new(name);
        app.kernel
            .modes_mut()
            .register(FactoryFaultMode {
                name: mode.clone(),
                fail_content,
            })
            .unwrap();
        app.attach_mode_to_content(editor_cid(), &mode).unwrap();
    }
    let view = view_id(&app, app.session.focused());

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("x".to_string())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["x"]);
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };
    assert!(
        query
            .decorations(
                view,
                app.session.body_space_for_view(view).unwrap(),
                RowRange { start: 0, end: 1 }
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn mode_command_delivers_owned_language_neutral_arguments() {
    let mut app = make_app(vec![], None);
    let mode = ModeName::new("argument-probe");
    let action = ModeActionName::new("insert");
    app.kernel
        .modes_mut()
        .register(ArgumentProbeMode {
            name: mode.clone(),
            actions: vec![action.clone()],
        })
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &mode).unwrap();
    let view = view_id(&app, app.session.focused());

    app.execute_command(DispatchCommand::Mode {
        command: ModeCommand::new(mode, action)
            .with_arguments(ModeValue::String("script".to_string())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["script"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn failure_in_a_later_mode_rolls_back_the_whole_input() {
    let mut app = make_app(vec![], None);
    app.kernel
        .modes_mut()
        .register(ChainProbeMode::new(
            "first-probe",
            vec![view_edit(EditCommand::InsertText("a".to_string()))],
            true,
        ))
        .unwrap();
    app.kernel
        .modes_mut()
        .register(ChainProbeMode::new(
            "failing-probe",
            vec![nested_mode(ModeCommand {
                mode: ModeName::new("missing"),
                action: ModeActionName::new("run"),
                arguments: Default::default(),
            })],
            false,
        ))
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &ModeName::new("first-probe"))
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &ModeName::new("failing-probe"))
        .unwrap();

    let error = app
        .handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("unknown mode 'missing'"));
    assert_eq!(text_rows(&app, editor_cid()), vec![""]);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_sequence_action_restores_the_modes_pending_prefix() {
    let mut app = make_app(vec![], None);
    app.kernel
        .modes_mut()
        .register(ChainProbeMode::with_sequence(
            "failing-sequence",
            vec![KeyEvent::char('q'), KeyEvent::char('r')],
            vec![nested_mode(ModeCommand::new(
                ModeName::new("missing"),
                ModeActionName::new("run"),
            ))],
            false,
        ))
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &ModeName::new("failing-sequence"))
        .unwrap();

    app.handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();
    assert!(app.session.input_is_pending_for_test());

    let error = app
        .handle_event(FrontendEvent::Key(KeyEvent::char('r')))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("unknown mode 'missing'"));
    assert!(app.session.input_is_pending_for_test());
}

#[tokio::test(flavor = "multi_thread")]
async fn mode_input_state_is_per_view_while_content_state_is_shared() {
    let mut app = make_app(vec![], None);
    let mode = ModeName::new("shared-content");
    app.kernel
        .modes_mut()
        .register(SharedContentMode::new())
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &mode).unwrap();

    let left = app.session.focused();
    let right = app
        .split_space(left, editor_cid(), true, SplitDirection::Right, true)
        .unwrap()
        .new_space;
    app.handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();
    app.close_space(right).unwrap();

    app.handle_event(FrontendEvent::Key(KeyEvent::char('x')))
        .await
        .unwrap();
    let command = ModeCommand {
        mode,
        action: ModeActionName::new("advance"),
        arguments: Default::default(),
    };
    assert_eq!(
        app.kernel
            .execute_mode_content_action(editor_cid(), &command)
            .unwrap(),
        ModeResult::operations(vec![history(TransactionIntent::Redo)])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn leaving_content_cancels_view_input_without_resetting_content_state() {
    let mut app = make_app(vec![], None);
    let mode = ModeName::new("shared-content");
    app.kernel
        .modes_mut()
        .register(SharedContentMode::new())
        .unwrap();
    app.attach_mode_to_content(editor_cid(), &mode).unwrap();
    app.handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();

    let other = ContentId(9);
    app.kernel
        .contents_mut()
        .insert(other, Content::Buffer(Buffer::new()))
        .unwrap();
    app.split_space(
        app.session.focused(),
        other,
        true,
        SplitDirection::Right,
        true,
    )
    .unwrap();

    let command = ModeCommand {
        mode,
        action: ModeActionName::new("advance"),
        arguments: Default::default(),
    };
    assert_eq!(
        app.kernel
            .execute_mode_content_action(editor_cid(), &command)
            .unwrap(),
        ModeResult::operations(vec![history(TransactionIntent::Redo)])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unchanged_space_binding_preserves_its_view_selection() {
    let mut app = make_app(vec![], None);
    for key in ['i', 'a', 'b', 'c'] {
        app.handle_event(FrontendEvent::Key(KeyEvent::char(key)))
            .await
            .unwrap();
    }

    app.set_space_sizing(app.session.focused(), Sizing::Fixed(12))
        .unwrap();

    assert_eq!(
        view_at(&app, app.session.focused())
            .selections()
            .unwrap()
            .primary()
            .head
            .char_index,
        3
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn replace_content_rebuilds_view_from_origin() {
    let mut app = make_app(vec![], None);
    let other = ContentId(9);
    app.kernel
        .contents_mut()
        .insert(other, Content::Buffer(Buffer::new()))
        .unwrap();
    for key in ['i', 'a', 'b', 'c'] {
        app.handle_event(FrontendEvent::Key(KeyEvent::char(key)))
            .await
            .unwrap();
    }

    app.replace_space_content(app.session.focused(), other, true)
        .unwrap();

    let view = view_at(&app, app.session.focused());
    assert_eq!(view.content(), other);
    assert_eq!(
        view.selections().unwrap().primary().head(),
        TextOffset::origin()
    );
    app.handle_event(FrontendEvent::Key(KeyEvent::char('a')))
        .await
        .unwrap();
    assert_eq!(text_rows(&app, other), vec![""]);
}

#[test]
fn close_focused_space_prefers_surviving_neighbor_and_drops_its_view() {
    let mut app = make_app(vec![], None);
    let left = app.session.focused();
    let right = app
        .split_space(left, editor_cid(), true, SplitDirection::Right, true)
        .unwrap()
        .new_space;
    let right_view = view_id(&app, right);

    app.close_space(right).unwrap();

    assert_eq!(app.session.focused(), left);
    assert!(!app.session.views().contains_key(&right_view));
}

#[test]
fn closing_an_unfocused_space_preserves_focus() {
    let mut app = make_app(vec![], None);
    let left = app.session.focused();
    let middle = app
        .split_space(left, editor_cid(), true, SplitDirection::Right, false)
        .unwrap()
        .new_space;
    let right = app
        .split_space(middle, editor_cid(), true, SplitDirection::Right, false)
        .unwrap()
        .new_space;
    assert_eq!(app.session.focused(), left);

    app.close_space(right).unwrap();

    assert_eq!(app.session.focused(), left);
    assert_eq!(focusable_view_count(app.session.scene()), 2);
}

#[test]
fn closing_a_focused_space_prefers_a_focusable_descendant_of_its_neighbor() {
    for placement in [StatusBarPlacement::Global, StatusBarPlacement::PerPane] {
        let mut app = make_app(vec![], None);
        app.set_status_bar_placement(placement).unwrap();
        let first = app.session.focused();
        let closing = app
            .split_space(first, editor_cid(), true, SplitDirection::Right, true)
            .unwrap()
            .new_space;
        let neighbor = app
            .split_space(closing, editor_cid(), true, SplitDirection::Right, true)
            .unwrap()
            .new_space;
        app.split_space(neighbor, editor_cid(), true, SplitDirection::Down, false)
            .unwrap();
        app.frontend.focus_targets.push_back(Some(closing));
        app.execute_command(DispatchCommand::App(AppCommand::Focus(
            SplitDirection::Left,
        )))
        .unwrap();
        assert_eq!(app.session.focused(), closing);

        app.close_space(closing).unwrap();

        assert_eq!(app.session.focused(), neighbor);
        assert_eq!(focusable_view_count(app.session.scene()), 3);
    }
}

#[test]
fn global_status_does_not_hide_the_previous_focusable_close_neighbor() {
    let mut app = make_app(vec![], None);
    let first = app.session.focused();
    let previous = app
        .split_space(first, editor_cid(), true, SplitDirection::Down, true)
        .unwrap()
        .new_space;
    let closing = app
        .split_space(previous, editor_cid(), true, SplitDirection::Down, true)
        .unwrap()
        .new_space;

    app.close_space(closing).unwrap();

    assert_eq!(app.session.focused(), previous);
    assert_eq!(focusable_view_count(app.session.scene()), 2);
}

#[test]
fn missing_content_is_rejected_before_scene_mutation() {
    let mut app = make_app(vec![], None);
    let root = app.session.scene().root();
    let revision = app.session.scene_revision();

    assert!(matches!(
        app.split_space(root, ContentId(999), true, SplitDirection::Right, true),
        Err(LayoutError::MissingContent(ContentId(999)))
    ));
    assert_eq!(app.session.scene().root(), root);
    assert_eq!(app.session.scene_revision(), revision);
}

#[test]
fn successful_layout_mutation_advances_scene_revision() {
    let mut app = make_app(vec![], None);

    app.set_space_sizing(app.session.focused(), Sizing::Fixed(12))
        .unwrap();

    assert_eq!(app.session.scene_revision(), Revision(1));
}

#[test]
fn render_passes_current_scene_revision_to_frontend() {
    let mut app = make_app(vec![], None);
    app.set_space_sizing(app.session.focused(), Sizing::Fixed(12))
        .unwrap();

    app.render().unwrap();

    assert_eq!(app.frontend.scene_revisions, vec![Revision(1)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn edit_commands_advance_view_and_content_revisions() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());

    app.handle_event(FrontendEvent::Key(KeyEvent::char('i')))
        .await
        .unwrap();
    app.handle_event(FrontendEvent::Key(KeyEvent::char('x')))
        .await
        .unwrap();

    assert!(app.session.views()[&view].revision() > Revision(0));
    assert!(app.kernel.contents().revision(editor_cid()).unwrap() > Revision(0));
    assert_eq!(app.session.scene_revision(), Revision(0));
}

#[test]
fn preferred_inert_status_space_is_not_selected() {
    let app = make_app(vec![], None);
    let status = app
        .session
        .scene()
        .node(app.session.scene().root())
        .children[1];

    assert_eq!(
        resolve_focus(app.session.scene(), app.session.focused(), Some(status)),
        Some(app.session.focused())
    );
}

#[test]
fn closing_last_focusable_space_is_rejected() {
    let mut app = make_app(vec![], None);
    let status = app
        .session
        .scene()
        .node(app.session.scene().root())
        .children[1];

    assert!(matches!(
        app.close_space(app.session.focused()),
        Err(LayoutError::WouldRemoveLastFocusable(_))
    ));
    assert_ne!(app.session.focused(), status);
}

#[test]
fn replacing_only_focusable_content_with_inert_space_is_rejected() {
    let mut app = make_app(vec![], None);
    let focused = app.session.focused();
    let other = ContentId(9);
    app.kernel
        .contents_mut()
        .insert(other, Content::Buffer(Buffer::new()))
        .unwrap();

    assert_eq!(
        app.replace_space_content(focused, other, false),
        Err(LayoutError::NoFocusableSpace)
    );
    assert_eq!(app.session.focused(), focused);
    assert!(matches!(
        &app.session.scene().node(focused).space.kind,
        SpaceKind::Content { view, .. }
            if app.session.views()[view].content() == editor_cid()
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn default_vim_a_enters_insert_before_text_input() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    app.run().await.unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["ia"]);
    assert!(app.kernel.is_cancelled());
}

#[tokio::test(flavor = "multi_thread")]
async fn default_vim_a_appends_after_cursor_and_enters_insert() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('h')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('x')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["axb"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn default_vim_ctrl_w_deletes_previous_word() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::char(' ')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::char('d')),
            FrontendEvent::Key(KeyEvent::ctrl('w')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["ab "]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_ctrl_w_s_and_v_split_the_focused_buffer() {
    for (key, expected_axis) in [
        ('s', vell_protocol::space::Axis::Vertical),
        ('v', vell_protocol::space::Axis::Horizontal),
    ] {
        let mut app = make_app(vec![], None);
        let original = app.session.focused();
        for input in [
            KeyEvent::char('i'),
            KeyEvent::char('a'),
            KeyEvent::char('b'),
            KeyEvent::char('c'),
            KeyEvent::plain(KeyCode::Escape),
        ] {
            app.handle_event(FrontendEvent::Key(input)).await.unwrap();
        }
        let original_head = view_at(&app, original)
            .selections()
            .unwrap()
            .primary()
            .head();

        app.handle_event(FrontendEvent::Key(KeyEvent::ctrl('w')))
            .await
            .unwrap();
        app.handle_event(FrontendEvent::Key(KeyEvent::char(key)))
            .await
            .unwrap();

        assert_eq!(focusable_view_count(app.session.scene()), 2);
        assert_ne!(app.session.focused(), original);
        assert_eq!(view_at(&app, app.session.focused()).content(), editor_cid());
        assert_eq!(
            view_at(&app, app.session.focused())
                .selections()
                .unwrap()
                .primary()
                .head(),
            original_head
        );
        let parent = app
            .session
            .scene()
            .node(app.session.focused())
            .parent
            .unwrap();
        assert!(matches!(
            app.session.scene().node(parent).space.kind,
            SpaceKind::Container {
                arrangement: vell_protocol::space::Arrangement::Flex {
                    direction,
                    ..
                }
            } if direction == expected_axis
        ));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_ctrl_w_hjkl_request_directional_focus_from_the_frontend() {
    let mut app = make_app(vec![], None);
    let focused = app.session.focused();
    app.frontend.focus_targets =
        VecDeque::from([Some(focused), Some(focused), Some(focused), Some(focused)]);

    for key in ['h', 'j', 'k', 'l'] {
        app.handle_event(FrontendEvent::Key(KeyEvent::ctrl('w')))
            .await
            .unwrap();
        app.handle_event(FrontendEvent::Key(KeyEvent::char(key)))
            .await
            .unwrap();
    }

    assert_eq!(
        app.frontend.focus_directions,
        vec![
            SplitDirection::Left,
            SplitDirection::Down,
            SplitDirection::Up,
            SplitDirection::Right,
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_ctrl_w_q_closes_the_focused_pane_and_its_status_bar() {
    let mut app = make_app(vec![], None);
    app.set_status_bar_placement(StatusBarPlacement::PerPane)
        .unwrap();
    let original = app.session.focused();

    app.handle_event(FrontendEvent::Key(KeyEvent::ctrl('w')))
        .await
        .unwrap();
    app.handle_event(FrontendEvent::Key(KeyEvent::char('v')))
        .await
        .unwrap();
    let closing = app.session.focused();
    let closing_view = view_id(&app, closing);
    assert_ne!(closing, original);
    assert_eq!(app.status_bars_for_content(editor_cid()).len(), 2);

    app.handle_event(FrontendEvent::Key(KeyEvent::ctrl('w')))
        .await
        .unwrap();
    app.handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();

    assert_eq!(focusable_view_count(app.session.scene()), 1);
    assert_eq!(app.session.focused(), original);
    assert!(!app.session.views().contains_key(&closing_view));
    assert_eq!(app.status_bars_for_content(editor_cid()).len(), 1);
    assert!(!app.kernel.is_cancelled());
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_ctrl_w_q_quits_from_the_last_pane() {
    let mut app = make_app(vec![], None);
    let focused = app.session.focused();

    app.handle_event(FrontendEvent::Key(KeyEvent::ctrl('w')))
        .await
        .unwrap();
    app.handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();

    assert!(app.kernel.is_cancelled());
    assert_eq!(focusable_view_count(app.session.scene()), 1);
    assert_eq!(app.session.focused(), focused);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_ctrl_w_split_adds_an_independent_per_pane_status_bar() {
    let mut app = make_app(vec![], None);
    app.set_status_bar_placement(StatusBarPlacement::PerPane)
        .unwrap();

    app.handle_event(FrontendEvent::Key(KeyEvent::ctrl('w')))
        .await
        .unwrap();
    app.handle_event(FrontendEvent::Key(KeyEvent::char('v')))
        .await
        .unwrap();

    let bars = app.status_bars_for_content(editor_cid());
    assert_eq!(bars.len(), 2);
    assert_ne!(bars[0], bars[1]);
    assert_eq!(focusable_view_count(app.session.scene()), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_normal_w_moves_to_next_word() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('f')),
            FrontendEvent::Key(KeyEvent::char('o')),
            FrontendEvent::Key(KeyEvent::char('o')),
            FrontendEvent::Key(KeyEvent::char(' ')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('r')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('0')),
            FrontendEvent::Key(KeyEvent::char('w')),
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('X')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    app.run().await.unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["foo Xbar"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_normal_w_stops_on_an_empty_line() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('w')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    let focused_view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("one\n\nthree".to_string())),
        view: focused_view,
        content: editor_cid(),
    })
    .unwrap();

    app.run().await.unwrap();

    let head = view_at(&app, app.session.focused())
        .selections()
        .unwrap()
        .primary()
        .head();
    assert_eq!(
        text_point(&app, editor_cid(), head),
        TextPoint { row: 1, col: 0 }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_counted_word_motions_advance_for_each_repetition() {
    let text = [
        'o', 'n', 'e', ' ', 't', 'w', 'o', ' ', 't', 'h', 'r', 'e', 'e',
    ];

    let mut forward = make_app(vec![], None);
    for key in ['i']
        .into_iter()
        .chain(text)
        .chain(['\u{1b}', '0', '2', 'w'])
    {
        let key = if key == '\u{1b}' {
            KeyEvent::plain(KeyCode::Escape)
        } else {
            KeyEvent::char(key)
        };
        forward.handle_event(FrontendEvent::Key(key)).await.unwrap();
    }
    assert_eq!(
        view_at(&forward, forward.session.focused())
            .selections()
            .unwrap()
            .primary()
            .head()
            .char_index,
        8
    );

    let mut end = make_app(vec![], None);
    for key in ['i']
        .into_iter()
        .chain(text)
        .chain(['\u{1b}', '0', '2', 'e'])
    {
        let key = if key == '\u{1b}' {
            KeyEvent::plain(KeyCode::Escape)
        } else {
            KeyEvent::char(key)
        };
        end.handle_event(FrontendEvent::Key(key)).await.unwrap();
    }
    assert_eq!(
        view_at(&end, end.session.focused())
            .selections()
            .unwrap()
            .primary()
            .head()
            .char_index,
        6
    );

    let mut backward = make_app(vec![], None);
    for key in ['i']
        .into_iter()
        .chain(text)
        .chain(['\u{1b}', '$', '2', 'b'])
    {
        let key = if key == '\u{1b}' {
            KeyEvent::plain(KeyCode::Escape)
        } else {
            KeyEvent::char(key)
        };
        backward
            .handle_event(FrontendEvent::Key(key))
            .await
            .unwrap();
    }
    assert_eq!(
        view_at(&backward, backward.session.focused())
            .selections()
            .unwrap()
            .primary()
            .head()
            .char_index,
        4
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_normal_dollar_moves_to_line_end() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('0')),
            FrontendEvent::Key(KeyEvent::char('$')),
            FrontendEvent::Key(KeyEvent::char('x')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    app.run().await.unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["ab"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_counted_dollar_moves_to_the_later_line_end() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('2')),
            FrontendEvent::Key(KeyEvent::char('$')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("one\ntwo\nthree".to_string())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    app.run().await.unwrap();

    let cursor = app.session.views()[&view]
        .selections()
        .unwrap()
        .primary()
        .head();
    assert_eq!(
        text_point(&app, editor_cid(), cursor),
        TextPoint { row: 1, col: 2 }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_normal_x_deletes_char() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('0')),
            FrontendEvent::Key(KeyEvent::char('x')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    app.run().await.unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["bc"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_normal_o_opens_line_below_and_inserts() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('f')),
            FrontendEvent::Key(KeyEvent::char('o')),
            FrontendEvent::Key(KeyEvent::char('o')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('o')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('r')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    app.run().await.unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["foo", "bar"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_normal_capital_a_appends_at_line_end() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('f')),
            FrontendEvent::Key(KeyEvent::char('o')),
            FrontendEvent::Key(KeyEvent::char('o')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('0')),
            FrontendEvent::Key(KeyEvent::char('A')),
            FrontendEvent::Key(KeyEvent::char('!')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    app.run().await.unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["foo!"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_normal_capital_d_deletes_to_line_end() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('0')),
            FrontendEvent::Key(KeyEvent::char('l')),
            FrontendEvent::Key(KeyEvent::char('D')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    app.run().await.unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["a"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_normal_capital_j_joins_lines() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('f')),
            FrontendEvent::Key(KeyEvent::char('o')),
            FrontendEvent::Key(KeyEvent::char('o')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('r')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('k')),
            FrontendEvent::Key(KeyEvent::char('J')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    app.run().await.unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["foo bar"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_normal_tilde_toggles_case() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('0')),
            FrontendEvent::Key(KeyEvent::char('~')),
            FrontendEvent::Key(KeyEvent::char('~')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    app.run().await.unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["AB"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_insert_ctrl_u_deletes_to_line_start() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::ctrl('u')),
            FrontendEvent::Key(KeyEvent::char('x')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    app.run().await.unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["x"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_normal_s_substitutes_char() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('0')),
            FrontendEvent::Key(KeyEvent::char('s')),
            FrontendEvent::Key(KeyEvent::char('X')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    app.run().await.unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["Xb"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn run_supports_backspace_and_arrows() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Backspace)),
            FrontendEvent::Key(KeyEvent::arrow(ArrowKey::Left)),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    app.run().await.unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["a"]);
    let cursor = view_at(&app, app.session.focused())
        .selections()
        .unwrap()
        .primary()
        .head();
    assert_eq!(text_point(&app, editor_cid(), cursor).col, 0);
}

#[test]
fn multi_space_edit_targets_only_focused_content() {
    let mut app = make_app(vec![], None);
    let other_cid = ContentId(9);
    app.kernel
        .contents_mut()
        .insert(other_cid, Content::Buffer(Buffer::new()))
        .unwrap();
    let other_sid = app
        .split_space(
            app.session.focused(),
            other_cid,
            true,
            SplitDirection::Right,
            false,
        )
        .unwrap()
        .new_space;
    let other_view = view_id(&app, other_sid);

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("Z".to_string())),
        view: other_view,
        content: other_cid,
    })
    .unwrap();

    assert_eq!(
        app.kernel.contents().query(
            editor_cid(),
            ContentQuery::TextRows(RowRange { start: 0, end: 1 }),
        ),
        ContentData::TextRows(vec!["".to_string()]),
    );
    assert_eq!(
        app.kernel.contents().query(
            other_cid,
            ContentQuery::TextRows(RowRange { start: 0, end: 1 }),
        ),
        ContentData::TextRows(vec!["Z".to_string()]),
    );
    assert_eq!(
        app.session
            .views()
            .get(&other_view)
            .unwrap()
            .selections()
            .unwrap()
            .primary()
            .head()
            .char_index,
        1
    );
}

#[test]
fn selection_snapshot_only_includes_views_of_target_content() {
    let mut app = make_app(vec![], None);
    let target_view = view_id(&app, app.session.focused());
    let other_content = ContentId(9);
    app.kernel
        .contents_mut()
        .insert(other_content, Content::Buffer(Buffer::new()))
        .unwrap();
    let other_space = app
        .split_space(
            app.session.focused(),
            other_content,
            true,
            SplitDirection::Right,
            false,
        )
        .unwrap()
        .new_space;
    let other_view = view_id(&app, other_space);

    let snapshot = app.session.snapshot_selections(editor_cid());

    assert!(snapshot.contains_key(&target_view));
    assert!(!snapshot.contains_key(&other_view));
}

#[test]
fn editing_from_another_view_checkpoints_the_previous_owner() {
    let mut app = make_app(vec![], None);
    let left_space = app.session.focused();
    let left = view_id(&app, left_space);
    let right_space = app
        .split_space(left_space, editor_cid(), true, SplitDirection::Right, false)
        .unwrap()
        .new_space;
    let right = view_id(&app, right_space);

    for command in [
        ContentCommand::Transaction(TransactionCommand::Begin),
        ContentCommand::Edit(EditCommand::InsertText("a".to_string())),
    ] {
        app.execute_command(DispatchCommand::ContentWithView {
            command,
            view: left,
            content: editor_cid(),
        })
        .unwrap();
    }
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("b".to_string())),
        view: right,
        content: editor_cid(),
    })
    .unwrap();

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Undo,
        view: right,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(
        app.kernel.contents().query(
            editor_cid(),
            ContentQuery::TextRows(RowRange { start: 0, end: 1 }),
        ),
        ContentData::TextRows(vec!["a".to_string()])
    );

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Undo,
        view: left,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(
        app.kernel.contents().query(
            editor_cid(),
            ContentQuery::TextRows(RowRange { start: 0, end: 1 }),
        ),
        ContentData::TextRows(vec![String::new()])
    );
}

#[test]
fn closing_the_owner_view_checkpoints_its_transaction() {
    let mut app = make_app(vec![], None);
    let left_space = app.session.focused();
    let left = view_id(&app, left_space);
    let right_space = app
        .split_space(left_space, editor_cid(), true, SplitDirection::Right, false)
        .unwrap()
        .new_space;
    let right = view_id(&app, right_space);

    for command in [
        ContentCommand::Transaction(TransactionCommand::Begin),
        ContentCommand::Edit(EditCommand::InsertText("a".to_string())),
    ] {
        app.execute_command(DispatchCommand::ContentWithView {
            command,
            view: left,
            content: editor_cid(),
        })
        .unwrap();
    }
    app.close_space(left_space).unwrap();

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Undo,
        view: right,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(
        app.kernel.contents().query(
            editor_cid(),
            ContentQuery::TextRows(RowRange { start: 0, end: 1 }),
        ),
        ContentData::TextRows(vec![String::new()])
    );
}

#[test]
#[should_panic(expected = "view/content target mismatch")]
fn content_with_view_rejects_mismatched_content_target() {
    let mut app = make_app(vec![], None);
    let other_cid = ContentId(9);
    app.kernel
        .contents_mut()
        .insert(other_cid, Content::Buffer(Buffer::new()))
        .unwrap();

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("Z".to_string())),
        view: view_id(&app, app.session.focused()),
        content: other_cid,
    })
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn run_forwards_resize_to_scene() {
    let mut app = make_app(
        vec![
            FrontendEvent::Resize(ResizeEvent {
                width: 100,
                height: 40,
            }),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    app.run().await.unwrap();
    assert_eq!(app.session.scene().size.width, 100);
    assert_eq!(app.session.scene().size.height, 40);
    assert_eq!(app.session.scene_revision(), Revision(1));
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_s_saves_file_and_marks_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    std::fs::write(&path, "hi").unwrap();
    let path_str = path.to_str().unwrap().to_owned();
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('X')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('s')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        Some(&path_str),
    );
    app.run().await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "Xhi");
    assert_eq!(dirty_state(&app, editor_cid()), DirtyState::Clean);
    assert_eq!(save_state(&app, editor_cid()), SaveState::Saved);
    assert_eq!(
        backing_state(&app, editor_cid()),
        BufferBackingState::Materialized
    );
    let editor = view_id(&app, app.session.focused());
    let status = app.status_bar_for_view(editor).unwrap();
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        presentation: app.session.presentation(),
        faces: app.session.faces(),
    };
    let presentation = match query
        .view(status.target_view, status.space)
        .unwrap()
        .presentation
    {
        ViewPresentation::StatusBar(presentation) => presentation,
        ViewPresentation::Text(_) => panic!("expected status-bar presentation"),
    };
    assert_eq!(presentation.center[0].text, "Saved");
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_gg_moves_to_the_first_line() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('X')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["Xa", "b"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_counted_gg_moves_to_the_requested_line() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('2')),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('X')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["a", "Xb"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_f_and_count_use_dynamic_awaiting_input() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('0')),
            FrontendEvent::Key(KeyEvent::char('2')),
            FrontendEvent::Key(KeyEvent::char('f')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('X')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["abacXa"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_capital_f_searches_backward_on_the_current_line() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('F')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('X')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["Xaba"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_counted_j_uses_private_count_state() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('2')),
            FrontendEvent::Key(KeyEvent::char('j')),
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('X')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["a", "b", "Xc"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_counted_dd_deletes_whole_lines() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
            FrontendEvent::Key(KeyEvent::char('d')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('3')),
            FrontendEvent::Key(KeyEvent::char('d')),
            FrontendEvent::Key(KeyEvent::char('d')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["d"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn prefix_key_sequence_saves() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.txt");
    std::fs::write(&path, "x").unwrap();
    let path_str = path.to_str().unwrap().to_owned();
    // 绑定未被 Vim 使用的前缀，覆盖 Ctrl+S 之外的全局 sequence 路径。
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('[')),
            FrontendEvent::Key(KeyEvent::char(']')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        Some(&path_str),
    );
    let mut global = default_global_keymap();
    global.bind(
        [KeyEvent::char('['), KeyEvent::char(']')],
        Command::Content(ContentCommand::Save),
    );
    app.session
        .replace_dispatcher_for_test(Dispatcher::new(global));
    app.run().await.unwrap();
    assert_eq!(dirty_state(&app, editor_cid()), DirtyState::Clean);
    assert_eq!(save_state(&app, editor_cid()), SaveState::Saved);
}

#[test]
fn save_completed_ok_marks_buffer_saved() {
    let mut app = make_app(vec![], None);
    attach_save_status_mode(&mut app);
    assert_eq!(custom_status_center(&app), "Untitled/Idle");
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("x".to_string())),
        view: view_id(&app, app.session.focused()),
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(dirty_state(&app, editor_cid()), DirtyState::Modified);
    app.kernel.track_pending_save_for_test(
        editor_cid(),
        1,
        vell_core::transaction::TextStateId(1),
        None,
    );

    app.handle_app_message(AppMessage::SaveCompleted {
        content: editor_cid(),
        revision: 1,
        state: vell_core::transaction::TextStateId(1),
        result: Ok(()),
    })
    .unwrap();

    assert!(!app.kernel.has_pending_save(editor_cid()));
    assert_eq!(dirty_state(&app, editor_cid()), DirtyState::Clean);
    assert_eq!(save_state(&app, editor_cid()), SaveState::Saved);
    assert_eq!(
        backing_state(&app, editor_cid()),
        BufferBackingState::Materialized
    );
    assert_eq!(custom_status_center(&app), "Materialized/Saved");
}

#[test]
fn save_completed_err_marks_buffer_save_failed() {
    let mut app = make_app(vec![], None);
    attach_save_status_mode(&mut app);
    assert_eq!(custom_status_center(&app), "Untitled/Idle");
    app.kernel.track_pending_save_for_test(
        editor_cid(),
        0,
        vell_core::transaction::TextStateId(0),
        None,
    );

    app.handle_app_message(AppMessage::SaveCompleted {
        content: editor_cid(),
        revision: 0,
        state: vell_core::transaction::TextStateId(0),
        result: Err(io::Error::other("boom")),
    })
    .unwrap();

    assert!(!app.kernel.has_pending_save(editor_cid()));
    assert_eq!(dirty_state(&app, editor_cid()), DirtyState::Clean);
    assert_eq!(save_state(&app, editor_cid()), SaveState::Failed);
    assert_eq!(
        backing_state(&app, editor_cid()),
        BufferBackingState::Untitled
    );
    assert_eq!(custom_status_center(&app), "Untitled/Failed");
}

#[test]
fn save_without_a_path_invalidates_custom_status_policy() {
    let mut app = make_app(vec![], None);
    attach_save_status_mode(&mut app);
    assert_eq!(custom_status_center(&app), "Untitled/Idle");

    app.execute_command(DispatchCommand::Content {
        command: ContentCommand::Save,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(save_state(&app, editor_cid()), SaveState::Failed);
    assert_eq!(custom_status_center(&app), "Untitled/Failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn save_rejects_external_changes_and_force_save_overwrites_explicitly() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("conflict.txt");
    std::fs::write(&path, "before").unwrap();
    let mut app = make_app(vec![], Some(path.to_str().unwrap()));
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("local ".to_owned())),
        view,
        content: editor_cid(),
    })
    .unwrap();
    std::fs::write(&path, "external").unwrap();

    assert!(
        app.execute_command(DispatchCommand::Content {
            command: ContentCommand::Save,
            content: editor_cid(),
        })
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "external");
    assert!(!app.kernel.has_pending_save(editor_cid()));

    assert!(app.save_buffer(editor_cid(), true).unwrap());
    app.shutdown_tasks().await.unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "local before");
    assert_eq!(dirty_state(&app, editor_cid()), DirtyState::Clean);
}

#[tokio::test(flavor = "multi_thread")]
async fn save_as_updates_the_path_only_after_success() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("saved-as.txt");
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("text".to_owned())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert!(app.save_buffer_as(editor_cid(), &path, false).unwrap());
    assert_eq!(resource_path(&app, editor_cid()), None);
    let (_, identity) = normalize_path(&path).unwrap();
    assert_eq!(app.kernel.content_for_path(&identity), None);
    assert_eq!(app.kernel.path_owner(&identity), Some(editor_cid()));
    assert!(matches!(
        app.open_buffer(&path),
        Err(crate::buffer_lifecycle::BufferLifecycleError::PathOccupied {
            content,
            ..
        }) if content == editor_cid()
    ));
    let other = app.new_buffer();
    let view = view_id(&app, app.session.focused());
    let error = app
        .execute_command(DispatchCommand::ModeOperations {
            operations: vec![OperationRequest::ContentLifecycle(
                ContentLifecycleOperation::SaveAs {
                    target: ContentTarget::Id(other),
                    path: path.to_string_lossy().into_owned(),
                    force: false,
                },
            )],
            view,
            content: editor_cid(),
        })
        .unwrap_err();
    assert!(error.to_string().contains("already open"), "{error}");

    app.shutdown_tasks().await.unwrap();

    let expected_path = super::buffer_lifecycle::normalize_path(&path)
        .unwrap()
        .0
        .to_string_lossy()
        .into_owned();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "text");
    assert_eq!(
        resource_path(&app, editor_cid()).as_deref(),
        Some(expected_path.as_str())
    );
    assert_eq!(dirty_state(&app, editor_cid()), DirtyState::Clean);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_save_as_keeps_the_old_path_and_releases_the_reservation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing").join("failed.txt");
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("dirty".to_owned())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert!(app.save_buffer_as(editor_cid(), &path, false).unwrap());
    app.shutdown_tasks().await.unwrap();

    assert_eq!(resource_path(&app, editor_cid()), None);
    assert_eq!(save_state(&app, editor_cid()), SaveState::Failed);
    assert_eq!(dirty_state(&app, editor_cid()), DirtyState::Modified);
    let opened = app.open_buffer(&path).unwrap();
    assert_ne!(opened, editor_cid());
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_save_completion_keeps_newer_edits_modified() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stale-save.txt");
    std::fs::write(&path, "hello").unwrap();
    let path_str = path.to_str().unwrap().to_owned();
    let mut app = make_app(vec![], Some(&path_str));

    app.execute_command(DispatchCommand::Content {
        command: ContentCommand::Save,
        content: editor_cid(),
    })
    .unwrap();
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("X".to_string())),
        view: view_id(&app, app.session.focused()),
        content: editor_cid(),
    })
    .unwrap();

    app.shutdown_tasks().await.unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    assert_eq!(dirty_state(&app, editor_cid()), DirtyState::Modified);
    assert_eq!(save_state(&app, editor_cid()), SaveState::Idle);
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_new_file_save_invalidates_backing_state_presentation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new-stale-save.txt");
    let path_str = path.to_str().unwrap().to_owned();
    let mut app = make_app(vec![], Some(&path_str));
    attach_save_status_mode(&mut app);
    assert_eq!(custom_status_center(&app), "Unmaterialized/Idle");
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("A".to_owned())),
        view,
        content: editor_cid(),
    })
    .unwrap();
    app.execute_command(DispatchCommand::Content {
        command: ContentCommand::Save,
        content: editor_cid(),
    })
    .unwrap();
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("B".to_owned())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    app.shutdown_tasks().await.unwrap();

    assert_eq!(std::fs::read_to_string(path).unwrap(), "A");
    assert_eq!(dirty_state(&app, editor_cid()), DirtyState::Modified);
    assert_eq!(save_state(&app, editor_cid()), SaveState::Idle);
    assert_eq!(
        backing_state(&app, editor_cid()),
        BufferBackingState::Materialized
    );
    assert_eq!(custom_status_center(&app), "Materialized/Idle");
}

#[test]
fn same_revision_force_save_queues_a_forced_retry() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("force-retry.txt");
    std::fs::write(&path, "text").unwrap();
    let path = path.to_string_lossy().into_owned();
    let mut app = make_app(vec![], Some(&path));
    let snapshot = match app.kernel.execute(editor_cid(), ContentInput::Save) {
        ContentResult::Handled(outcome) => match outcome.effect {
            ContentEffect::Save(snapshot) => snapshot,
            ContentEffect::None => panic!("save must prepare a snapshot"),
        },
        ContentResult::NotHandled => panic!("buffer must handle save"),
    };
    let revision = snapshot.revision;
    let state = snapshot.state;
    app.kernel
        .track_pending_save_for_test(editor_cid(), revision, state, None);

    assert!(!app.kernel.queue_save(editor_cid(), snapshot, true));
    let completion = app.kernel.complete_save(
        editor_cid(),
        revision,
        state,
        Err(io::Error::other("conflict")),
    );
    let (_, queued) = completion.into_parts();

    assert!(matches!(queued, Some((_, true))));
}

#[tokio::test(flavor = "multi_thread")]
async fn save_during_pending_write_queues_latest_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queued-save.txt");
    std::fs::write(&path, "hello").unwrap();
    let path_str = path.to_str().unwrap().to_owned();
    let mut app = make_app(vec![], Some(&path_str));

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("A".to_string())),
        view: view_id(&app, app.session.focused()),
        content: editor_cid(),
    })
    .unwrap();
    app.execute_command(DispatchCommand::Content {
        command: ContentCommand::Save,
        content: editor_cid(),
    })
    .unwrap();
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("B".to_string())),
        view: view_id(&app, app.session.focused()),
        content: editor_cid(),
    })
    .unwrap();
    app.execute_command(DispatchCommand::Content {
        command: ContentCommand::Save,
        content: editor_cid(),
    })
    .unwrap();
    assert!(app.kernel.has_pending_save(editor_cid()));

    app.shutdown_tasks().await.unwrap();

    assert!(!app.kernel.has_pending_save(editor_cid()));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ABhello");
    assert_eq!(dirty_state(&app, editor_cid()), DirtyState::Clean);
    assert_eq!(save_state(&app, editor_cid()), SaveState::Saved);
    assert_eq!(
        backing_state(&app, editor_cid()),
        BufferBackingState::Materialized
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn execute_save_uses_resolved_content_target() {
    let dir = tempfile::tempdir().unwrap();
    let focused_path = dir.path().join("focused.txt");
    let other_path = dir.path().join("other.txt");
    std::fs::write(&focused_path, "focused").unwrap();
    std::fs::write(&other_path, "other").unwrap();
    let focused_path_str = focused_path.to_str().unwrap().to_owned();
    let other_path_str = other_path.to_str().unwrap().to_owned();

    let mut app = make_app(vec![], Some(&focused_path_str));
    let other_cid = ContentId(9);
    let mut other = Buffer::new();
    other.open_path(&other_path_str).unwrap();
    let source_len = other.slice().len_chars();
    other
        .apply_content_change(
            TextChangeSet::from_edits(source_len, vec![TextEdit::new(0..0, "X")]).unwrap(),
        )
        .unwrap();
    app.kernel
        .contents_mut()
        .insert(other_cid, Content::Buffer(other))
        .unwrap();
    let (_, identity) = normalize_path(&other_path).unwrap();
    app.kernel
        .register_buffer_path(
            other_cid,
            identity,
            other_path.clone(),
            FileBaseline::Materialized("other".to_owned()),
        )
        .unwrap();

    app.execute_command(DispatchCommand::Content {
        command: ContentCommand::Save,
        content: other_cid,
    })
    .unwrap();
    app.shutdown_tasks().await.unwrap();

    assert_eq!(std::fs::read_to_string(&focused_path).unwrap(), "focused");
    assert_eq!(std::fs::read_to_string(&other_path).unwrap(), "Xother");
}

#[tokio::test(flavor = "multi_thread")]
async fn run_waits_for_pending_save_before_returning() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wait-save.txt");
    std::fs::write(&path, "hi").unwrap();
    let path_str = path.to_str().unwrap().to_owned();
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('X')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('s')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        Some(&path_str),
    );

    let result = tokio::time::timeout(std::time::Duration::from_secs(2), app.run()).await;
    assert!(result.is_ok());
    result.unwrap().unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "Xhi");
    assert!(!app.kernel.has_pending_save(editor_cid()));
    assert_eq!(dirty_state(&app, editor_cid()), DirtyState::Clean);
    assert_eq!(save_state(&app, editor_cid()), SaveState::Saved);
}

#[tokio::test(flavor = "multi_thread")]
async fn shift_arrow_builds_selection_then_input_replaces() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::shift_arrow(ArrowKey::Left)), // 选区 [2,3)
            FrontendEvent::Key(KeyEvent::char('X')),                   // 替换 [2,3) 为 X
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    app.run().await.unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["abX"]);
    let head = view_at(&app, app.session.focused())
        .selections()
        .unwrap()
        .primary()
        .head();
    assert_eq!(head.char_index, 2);
    assert_eq!(
        view_at(&app, app.session.focused())
            .selections()
            .unwrap()
            .primary()
            .anchor,
        head
    ); // collapse
}

#[tokio::test(flavor = "multi_thread")]
async fn escape_canonicalizes_normal_selection_before_h_moves() {
    // vim 语义：Insert 中 shift-Left 建选区 [2,3)；Escape 回 Normal（不 collapse）；
    // 随后 Normal 的 'h' 在非空选区上 shrink 到 min 并 collapse（head=2），再 'h' 左移到 1。
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::shift_arrow(ArrowKey::Left)), // 选区 [2,3)
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),      // 回 Normal（选区保留）
            FrontendEvent::Key(KeyEvent::char('h')),                   // shrink→head=2 collapse
            FrontendEvent::Key(KeyEvent::char('h')),                   // collapsed 左移 → head=1
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    app.run().await.unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["abc"]); // Escape/h 不改文本
    let head = view_at(&app, app.session.focused())
        .selections()
        .unwrap()
        .primary()
        .head();
    assert_eq!(text_point(&app, editor_cid(), head).col, 0);
    assert_eq!(
        view_at(&app, app.session.focused())
            .selections()
            .unwrap()
            .primary()
            .anchor,
        head
    ); // collapsed
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_visual_counted_motion_then_delete_removes_selected_range() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::char('d')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('0')),
            FrontendEvent::Key(KeyEvent::char('v')),
            FrontendEvent::Key(KeyEvent::char('2')),
            FrontendEvent::Key(KeyEvent::char('l')),
            FrontendEvent::Key(KeyEvent::char('d')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["d"]);
    let selection = view_at(&app, app.session.focused())
        .selections()
        .unwrap()
        .primary();
    assert_eq!(selection.head.char_index, 0);
    assert_eq!(selection.anchor, selection.head);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_visual_delete_without_motion_removes_current_char() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('0')),
            FrontendEvent::Key(KeyEvent::char('v')),
            FrontendEvent::Key(KeyEvent::char('d')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["b"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_visual_left_includes_the_original_cursor_character() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('0')),
            FrontendEvent::Key(KeyEvent::char('l')),
            FrontendEvent::Key(KeyEvent::char('v')),
            FrontendEvent::Key(KeyEvent::char('h')),
            FrontendEvent::Key(KeyEvent::char('d')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["c"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn leaving_multiline_visual_selection_clamps_normal_cursor_to_a_character() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('3')),
            FrontendEvent::Key(KeyEvent::char('l')),
            FrontendEvent::Key(KeyEvent::char('v')),
            FrontendEvent::Key(KeyEvent::char('j')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    let focused_view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("abcd\nx".to_string())),
        view: focused_view,
        content: editor_cid(),
    })
    .unwrap();

    app.run().await.unwrap();

    let head = view_at(&app, app.session.focused())
        .selections()
        .unwrap()
        .primary()
        .head();
    assert_eq!(
        text_point(&app, editor_cid(), head),
        TextPoint { row: 1, col: 0 }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_line_visual_ctrl_d_deletes_frontend_sized_line_range() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('V')),
            FrontendEvent::Key(KeyEvent::ctrl('d')),
            FrontendEvent::Key(KeyEvent::char('d')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    let focused_view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("one\ntwo\nthree\nfour".to_string())),
        view: focused_view,
        content: editor_cid(),
    })
    .unwrap();

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["four"]);
    assert_eq!(app.frontend.viewport_commands.len(), 1);
    assert_eq!(
        app.frontend.viewport_commands[0],
        (
            focused_view,
            ResolvedViewportCommand::Scroll {
                direction: ViewportMoveDirection::Down,
                lines: 2,
            },
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_zz_zt_and_zb_align_the_viewport_without_moving_the_cursor() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('5')),
            FrontendEvent::Key(KeyEvent::char('j')),
            FrontendEvent::Key(KeyEvent::char('z')),
            FrontendEvent::Key(KeyEvent::char('z')),
            FrontendEvent::Key(KeyEvent::char('z')),
            FrontendEvent::Key(KeyEvent::char('t')),
            FrontendEvent::Key(KeyEvent::char('z')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText(
            "0\n1\n2\n3\n4\n5\n6\n7\n8".to_string(),
        )),
        view,
        content: editor_cid(),
    })
    .unwrap();

    app.run().await.unwrap();

    assert_eq!(
        app.frontend.viewport_commands,
        vec![
            (view, ResolvedViewportCommand::SetTopRow { top_row: 4 }),
            (view, ResolvedViewportCommand::SetTopRow { top_row: 5 }),
            (view, ResolvedViewportCommand::SetTopRow { top_row: 2 }),
        ]
    );
    let cursor = app.session.views()[&view]
        .selections()
        .unwrap()
        .primary()
        .head();
    assert_eq!(text_point(&app, editor_cid(), cursor).row, 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_counted_zz_moves_to_the_line_before_centering_it() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('3')),
            FrontendEvent::Key(KeyEvent::char('z')),
            FrontendEvent::Key(KeyEvent::char('z')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText(
            "00000\n11111\n22222\n33333\n44444\n55555".to_string(),
        )),
        view,
        content: editor_cid(),
    })
    .unwrap();

    app.run().await.unwrap();

    assert_eq!(
        app.frontend.viewport_commands,
        vec![(view, ResolvedViewportCommand::SetTopRow { top_row: 1 })]
    );
    let cursor = app.session.views()[&view]
        .selections()
        .unwrap()
        .primary()
        .head();
    assert_eq!(
        text_point(&app, editor_cid(), cursor),
        TextPoint { row: 2, col: 4 }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_visual_counted_zz_zt_and_zb_preserve_the_selection() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('2')),
            FrontendEvent::Key(KeyEvent::char('l')),
            FrontendEvent::Key(KeyEvent::char('v')),
            FrontendEvent::Key(KeyEvent::char('3')),
            FrontendEvent::Key(KeyEvent::char('z')),
            FrontendEvent::Key(KeyEvent::char('z')),
            FrontendEvent::Key(KeyEvent::char('z')),
            FrontendEvent::Key(KeyEvent::char('t')),
            FrontendEvent::Key(KeyEvent::char('z')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText(
            "00000\n11111\n22222\n33333".to_string(),
        )),
        view,
        content: editor_cid(),
    })
    .unwrap();

    app.run().await.unwrap();

    assert_eq!(
        app.frontend.viewport_commands,
        vec![
            (view, ResolvedViewportCommand::SetTopRow { top_row: 1 }),
            (view, ResolvedViewportCommand::SetTopRow { top_row: 2 }),
            (view, ResolvedViewportCommand::SetTopRow { top_row: 0 }),
        ]
    );
    let selection = app.session.views()[&view].selections().unwrap().primary();
    assert_eq!(
        text_point(&app, editor_cid(), selection.anchor),
        TextPoint { row: 0, col: 2 }
    );
    assert_eq!(
        text_point(&app, editor_cid(), selection.head()),
        TextPoint { row: 2, col: 2 }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_visual_change_and_insert_is_one_undo_unit() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::char('d')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('0')),
            FrontendEvent::Key(KeyEvent::char('v')),
            FrontendEvent::Key(KeyEvent::char('l')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::char('X')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('u')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["abcd"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_normal_h_moves_left_after_insert() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('h')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["ab"]);
    let head = view_at(&app, app.session.focused())
        .selections()
        .unwrap()
        .primary()
        .head();
    assert_eq!(head.char_index, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_insert_accepts_unicode_text() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('中')),
            FrontendEvent::Key(KeyEvent::char('文')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["中文"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn run_renders_after_state_changes() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    app.run().await.unwrap();
    assert!(app.frontend.renders >= 1);
    assert_eq!(text_rows(&app, editor_cid()), vec!["a"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_insert_session_is_one_undo_unit_and_ctrl_r_redoes_it() {
    let mut undo = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('u')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    undo.run().await.unwrap();
    assert_eq!(text_rows(&undo, editor_cid()), vec![""]);

    let mut redo = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('u')),
            FrontendEvent::Key(KeyEvent::ctrl('r')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    redo.run().await.unwrap();

    assert_eq!(text_rows(&redo, editor_cid()), vec!["ab"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_undo_restores_edit_start_and_redo_restores_edit_end() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("abcdef".to_string())),
        view,
        content: editor_cid(),
    })
    .unwrap();
    app.session
        .view_mut(view)
        .unwrap()
        .state_mut()
        .replace_selections(Selections::single(Selection::collapsed(TextOffset {
            char_index: 1,
        })))
        .unwrap();

    for key in [
        KeyEvent::char('i'),
        KeyEvent::char('X'),
        KeyEvent::plain(KeyCode::Escape),
        KeyEvent::char('$'),
        KeyEvent::char('u'),
    ] {
        app.handle_event(FrontendEvent::Key(key)).await.unwrap();
    }

    assert_eq!(text_rows(&app, editor_cid()), vec!["abcdef"]);
    assert_eq!(
        app.session.views()[&view]
            .selections()
            .unwrap()
            .primary()
            .head(),
        TextOffset { char_index: 1 }
    );

    app.handle_event(FrontendEvent::Key(KeyEvent::ctrl('r')))
        .await
        .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["aXbcdef"]);
    assert_eq!(
        app.session.views()[&view]
            .selections()
            .unwrap()
            .primary()
            .head(),
        TextOffset { char_index: 2 }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_insert_mode_u_is_text_not_undo() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('u')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["u"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_delete_operator_accepts_word_line_end_and_line_start_motions() {
    let mut word = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('o')),
            FrontendEvent::Key(KeyEvent::char('n')),
            FrontendEvent::Key(KeyEvent::char('e')),
            FrontendEvent::Key(KeyEvent::char(' ')),
            FrontendEvent::Key(KeyEvent::char('t')),
            FrontendEvent::Key(KeyEvent::char('w')),
            FrontendEvent::Key(KeyEvent::char('o')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('0')),
            FrontendEvent::Key(KeyEvent::char('d')),
            FrontendEvent::Key(KeyEvent::char('w')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    word.run().await.unwrap();
    assert_eq!(text_rows(&word, editor_cid()), vec!["two"]);

    let mut line_end = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('0')),
            FrontendEvent::Key(KeyEvent::char('d')),
            FrontendEvent::Key(KeyEvent::char('$')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    line_end.run().await.unwrap();
    assert_eq!(text_rows(&line_end, editor_cid()), vec![""]);

    let mut line_start = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('d')),
            FrontendEvent::Key(KeyEvent::char('0')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    line_start.run().await.unwrap();
    assert_eq!(text_rows(&line_start, editor_cid()), vec!["c"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_word_operators_distinguish_word_start_and_inclusive_word_end() {
    async fn run(operator: char, motion: char) -> Vec<String> {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('i')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::char('n')),
                FrontendEvent::Key(KeyEvent::char('e')),
                FrontendEvent::Key(KeyEvent::char(' ')),
                FrontendEvent::Key(KeyEvent::char('t')),
                FrontendEvent::Key(KeyEvent::char('w')),
                FrontendEvent::Key(KeyEvent::char('o')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::char('0')),
                FrontendEvent::Key(KeyEvent::char(operator)),
                FrontendEvent::Key(KeyEvent::char(motion)),
                FrontendEvent::Key(KeyEvent::char('X')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        app.run().await.unwrap();
        text_rows(&app, editor_cid())
    }

    assert_eq!(run('d', 'w').await, vec!["two"]);
    assert_eq!(run('d', 'e').await, vec![" two"]);
    assert_eq!(run('c', 'w').await, vec!["X two"]);
    assert_eq!(run('c', 'e').await, vec!["X two"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_cw_on_whitespace_stops_at_the_next_word_start() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('o')),
            FrontendEvent::Key(KeyEvent::char('n')),
            FrontendEvent::Key(KeyEvent::char('e')),
            FrontendEvent::Key(KeyEvent::char(' ')),
            FrontendEvent::Key(KeyEvent::char(' ')),
            FrontendEvent::Key(KeyEvent::char(' ')),
            FrontendEvent::Key(KeyEvent::char('t')),
            FrontendEvent::Key(KeyEvent::char('w')),
            FrontendEvent::Key(KeyEvent::char('o')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('0')),
            FrontendEvent::Key(KeyEvent::char('3')),
            FrontendEvent::Key(KeyEvent::char('l')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::char('w')),
            FrontendEvent::Key(KeyEvent::char('X')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["oneXtwo"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_change_word_from_inline_whitespace_preserves_the_line_break() {
    async fn run(text: &str, count: Option<char>) -> Vec<String> {
        let mut events = vec![
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('3')),
            FrontendEvent::Key(KeyEvent::char('l')),
            FrontendEvent::Key(KeyEvent::char('c')),
        ];
        if let Some(count) = count {
            events.push(FrontendEvent::Key(KeyEvent::char(count)));
        }
        events.extend([
            FrontendEvent::Key(KeyEvent::char('w')),
            FrontendEvent::Key(KeyEvent::char('X')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ]);
        let mut app = make_app(events, None);
        let view = view_id(&app, app.session.focused());
        app.execute_command(DispatchCommand::ContentWithView {
            command: ContentCommand::Edit(EditCommand::InsertText(text.to_string())),
            view,
            content: editor_cid(),
        })
        .unwrap();

        app.run().await.unwrap();
        text_rows(&app, editor_cid())
    }

    assert_eq!(run("one   \ntwo", None).await, vec!["oneX", "two"]);
    assert_eq!(
        run("one   two\nthree", Some('2')).await,
        vec!["oneX", "three"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_counted_dw_preserves_the_break_after_the_last_word() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('d')),
            FrontendEvent::Key(KeyEvent::char('2')),
            FrontendEvent::Key(KeyEvent::char('w')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("one two\nthree".to_string())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["", "three"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_cw_on_an_empty_line_preserves_the_line_break() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('j')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::char('w')),
            FrontendEvent::Key(KeyEvent::char('X')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("one\n\ntwo".to_string())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["one", "X", "two"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_counted_cw_on_an_empty_line_covers_the_next_word() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('j')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::char('2')),
            FrontendEvent::Key(KeyEvent::char('w')),
            FrontendEvent::Key(KeyEvent::char('X')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("one\n\ntwo".to_string())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["one", "X"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_counted_cw_counts_blank_lines_and_stops_at_the_next_word() {
    async fn run(text: &str) -> Vec<String> {
        let mut app = make_app(
            vec![
                FrontendEvent::Key(KeyEvent::char('g')),
                FrontendEvent::Key(KeyEvent::char('g')),
                FrontendEvent::Key(KeyEvent::char('j')),
                FrontendEvent::Key(KeyEvent::char('c')),
                FrontendEvent::Key(KeyEvent::char('2')),
                FrontendEvent::Key(KeyEvent::char('w')),
                FrontendEvent::Key(KeyEvent::char('X')),
                FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
                FrontendEvent::Key(KeyEvent::ctrl('q')),
            ],
            None,
        );
        let view = view_id(&app, app.session.focused());
        app.execute_command(DispatchCommand::ContentWithView {
            command: ContentCommand::Edit(EditCommand::InsertText(text.to_string())),
            view,
            content: editor_cid(),
        })
        .unwrap();

        app.run().await.unwrap();
        text_rows(&app, editor_cid())
    }

    assert_eq!(run("one\n\n\ntwo").await, vec!["one", "X", "two"]);
    assert_eq!(run("one\n\ntwo three").await, vec!["one", "Xthree"]);
    assert_eq!(
        run("one\n\ntwo\n   \nthree").await,
        vec!["one", "X", "   ", "three"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_dw_on_an_empty_line_deletes_only_its_line_break() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('j')),
            FrontendEvent::Key(KeyEvent::char('d')),
            FrontendEvent::Key(KeyEvent::char('w')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("one\n\n   \ntwo".to_string())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["one", "   ", "two"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_single_dw_crossing_a_line_preserves_the_break() {
    async fn run(text: &str, right: usize) -> Vec<String> {
        let mut events = vec![
            FrontendEvent::Key(KeyEvent::char('g')),
            FrontendEvent::Key(KeyEvent::char('g')),
        ];
        if right > 0 {
            events.push(FrontendEvent::Key(KeyEvent::char(
                char::from_digit(right as u32, 10).unwrap(),
            )));
            events.push(FrontendEvent::Key(KeyEvent::char('l')));
        }
        events.extend([
            FrontendEvent::Key(KeyEvent::char('d')),
            FrontendEvent::Key(KeyEvent::char('w')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ]);
        let mut app = make_app(events, None);
        let view = view_id(&app, app.session.focused());
        app.execute_command(DispatchCommand::ContentWithView {
            command: ContentCommand::Edit(EditCommand::InsertText(text.to_string())),
            view,
            content: editor_cid(),
        })
        .unwrap();

        app.run().await.unwrap();
        text_rows(&app, editor_cid())
    }

    assert_eq!(run("one\ntwo", 0).await, vec!["", "two"]);
    assert_eq!(run("one! \ntwo", 3).await, vec!["one", "two"]);
    assert_eq!(run("one! \ntwo", 4).await, vec!["one!", "two"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_counted_cw_at_a_word_end_counts_that_character_first() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('o')),
            FrontendEvent::Key(KeyEvent::char('n')),
            FrontendEvent::Key(KeyEvent::char('e')),
            FrontendEvent::Key(KeyEvent::char(' ')),
            FrontendEvent::Key(KeyEvent::char('t')),
            FrontendEvent::Key(KeyEvent::char('w')),
            FrontendEvent::Key(KeyEvent::char('o')),
            FrontendEvent::Key(KeyEvent::char(' ')),
            FrontendEvent::Key(KeyEvent::char('t')),
            FrontendEvent::Key(KeyEvent::char('h')),
            FrontendEvent::Key(KeyEvent::char('r')),
            FrontendEvent::Key(KeyEvent::char('e')),
            FrontendEvent::Key(KeyEvent::char('e')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('0')),
            FrontendEvent::Key(KeyEvent::char('2')),
            FrontendEvent::Key(KeyEvent::char('l')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::char('2')),
            FrontendEvent::Key(KeyEvent::char('w')),
            FrontendEvent::Key(KeyEvent::char('X')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["onX three"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_change_operator_multiplies_counts_and_commits_one_undo_unit() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('o')),
            FrontendEvent::Key(KeyEvent::char('n')),
            FrontendEvent::Key(KeyEvent::char('e')),
            FrontendEvent::Key(KeyEvent::char(' ')),
            FrontendEvent::Key(KeyEvent::char('t')),
            FrontendEvent::Key(KeyEvent::char('w')),
            FrontendEvent::Key(KeyEvent::char('o')),
            FrontendEvent::Key(KeyEvent::char(' ')),
            FrontendEvent::Key(KeyEvent::char('t')),
            FrontendEvent::Key(KeyEvent::char('h')),
            FrontendEvent::Key(KeyEvent::char('r')),
            FrontendEvent::Key(KeyEvent::char('e')),
            FrontendEvent::Key(KeyEvent::char('e')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('0')),
            FrontendEvent::Key(KeyEvent::char('2')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::char('2')),
            FrontendEvent::Key(KeyEvent::char('w')),
            FrontendEvent::Key(KeyEvent::char('X')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('u')),
            FrontendEvent::Key(KeyEvent::ctrl('r')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["X"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_cc_preserves_a_blank_line_for_insert_mode() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('k')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::char('c')),
            FrontendEvent::Key(KeyEvent::char('X')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["X", "b"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_normal_horizontal_motion_never_lands_on_or_deletes_newline() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::char('a')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Enter)),
            FrontendEvent::Key(KeyEvent::char('b')),
            FrontendEvent::Key(KeyEvent::plain(KeyCode::Escape)),
            FrontendEvent::Key(KeyEvent::char('k')),
            FrontendEvent::Key(KeyEvent::char('l')),
            FrontendEvent::Key(KeyEvent::char('x')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["", "b"]);
}

#[test]
fn editing_shared_content_reconciles_other_view_selections() {
    let mut app = make_app(vec![], None);
    let left = app.session.focused();
    let left_view = view_id(&app, left);
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("abc".to_string())),
        view: left_view,
        content: editor_cid(),
    })
    .unwrap();
    let right = app
        .split_space(left, editor_cid(), true, SplitDirection::Right, false)
        .unwrap()
        .new_space;
    let right_view = view_id(&app, right);
    let right_revision = app.session.views()[&right_view].revision();
    app.session
        .view_mut(left_view)
        .unwrap()
        .state_mut()
        .replace_selections(Selections::single(Selection {
            anchor: TextOffset::origin(),
            head: TextOffset { char_index: 3 },
        }))
        .unwrap();
    app.session
        .view_mut(right_view)
        .unwrap()
        .state_mut()
        .replace_selections(Selections::single(Selection::collapsed(TextOffset {
            char_index: 3,
        })))
        .unwrap();

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::Delete(-1)),
        view: left_view,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec![""]);
    assert_eq!(
        app.session.views()[&right_view]
            .selections()
            .unwrap()
            .primary()
            .head(),
        TextOffset::origin()
    );
    assert!(app.session.views()[&right_view].revision() > right_revision);
}

#[test]
fn shared_view_positions_follow_text_change_affinity() {
    let mut app = make_app(vec![], None);
    let left = app.session.focused();
    let left_view = view_id(&app, left);
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("abc".to_string())),
        view: left_view,
        content: editor_cid(),
    })
    .unwrap();
    let right = app
        .split_space(left, editor_cid(), true, SplitDirection::Right, false)
        .unwrap()
        .new_space;
    let right_view = view_id(&app, right);
    for view in [left_view, right_view] {
        app.session
            .view_mut(view)
            .unwrap()
            .state_mut()
            .replace_selections(Selections::single(Selection::collapsed(TextOffset {
                char_index: 1,
            })))
            .unwrap();
    }

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("X".to_string())),
        view: left_view,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["aXbc"]);
    assert_eq!(
        app.session.views()[&right_view]
            .selections()
            .unwrap()
            .primary()
            .head()
            .char_index,
        2
    );
}

#[test]
fn shared_view_positions_follow_undo_and_redo_changes() {
    let mut app = make_app(vec![], None);
    let left = app.session.focused();
    let left_view = view_id(&app, left);
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("abc".to_string())),
        view: left_view,
        content: editor_cid(),
    })
    .unwrap();
    let right = app
        .split_space(left, editor_cid(), true, SplitDirection::Right, false)
        .unwrap()
        .new_space;
    let right_view = view_id(&app, right);
    app.session
        .view_mut(right_view)
        .unwrap()
        .state_mut()
        .replace_selections(Selections::single(Selection::collapsed(TextOffset {
            char_index: 3,
        })))
        .unwrap();

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Undo,
        view: left_view,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec![""]);
    for view in [left_view, right_view] {
        assert_eq!(
            app.session.views()[&view]
                .selections()
                .unwrap()
                .primary()
                .head(),
            TextOffset::origin()
        );
    }

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Redo,
        view: left_view,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["abc"]);
    for view in [left_view, right_view] {
        assert_eq!(
            app.session.views()[&view]
                .selections()
                .unwrap()
                .primary()
                .head()
                .char_index,
            3
        );
    }
}

#[test]
fn shared_view_positions_remain_grapheme_boundaries_through_history() {
    let mut app = make_app(vec![], None);
    let left = app.session.focused();
    let left_view = view_id(&app, left);
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("e\u{301}x".to_owned())),
        view: left_view,
        content: editor_cid(),
    })
    .unwrap();
    let right = app
        .split_space(left, editor_cid(), true, SplitDirection::Right, false)
        .unwrap()
        .new_space;
    let right_view = view_id(&app, right);
    for view in [left_view, right_view] {
        app.session
            .view_mut(view)
            .unwrap()
            .state_mut()
            .replace_selections(Selections::single(Selection::collapsed(TextOffset {
                char_index: 2,
            })))
            .unwrap();
    }

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("\u{302}".to_owned())),
        view: left_view,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["e\u{301}\u{302}x"]);
    for view in [left_view, right_view] {
        assert_eq!(
            app.session.views()[&view]
                .selections()
                .unwrap()
                .primary()
                .head()
                .char_index,
            3
        );
    }

    for (command, text, expected) in [
        (ContentCommand::Undo, "e\u{301}x", 2),
        (ContentCommand::Redo, "e\u{301}\u{302}x", 3),
    ] {
        app.execute_command(DispatchCommand::ContentWithView {
            command,
            view: left_view,
            content: editor_cid(),
        })
        .unwrap();
        assert_eq!(text_rows(&app, editor_cid()), vec![text]);
        for view in [left_view, right_view] {
            assert_eq!(
                app.session.views()[&view]
                    .selections()
                    .unwrap()
                    .primary()
                    .head()
                    .char_index,
                expected
            );
        }
    }
}

#[test]
fn closed_source_view_does_not_break_content_undo() {
    let mut app = make_app(vec![], None);
    let left = app.session.focused();
    let left_view = view_id(&app, left);
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("abc".to_string())),
        view: left_view,
        content: editor_cid(),
    })
    .unwrap();
    let right = app
        .split_space(left, editor_cid(), true, SplitDirection::Right, false)
        .unwrap()
        .new_space;
    let right_view = view_id(&app, right);

    app.close_space(left).unwrap();
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Undo,
        view: right_view,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec![""]);
    assert_eq!(
        app.session.views()[&right_view]
            .selections()
            .unwrap()
            .primary()
            .head(),
        TextOffset::origin()
    );
}

#[test]
fn content_action_without_view_participant_is_undoable() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    let change = TextChangeSet::from_edits(0, vec![TextEdit::new(0..0, "x")]).unwrap();

    app.execute_command(DispatchCommand::ModeContentOperations {
        operations: vec![content_action(ContentAction::Text(change))],
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["x"]);

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Undo,
        view,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec![""]);
}

#[test]
fn raw_view_mode_content_action_maps_its_source_view() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("abc".to_string())),
        view,
        content: editor_cid(),
    })
    .unwrap();
    let change = TextChangeSet::from_edits(3, vec![TextEdit::new(0..3, "")]).unwrap();

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![view_content(ContentAction::Text(change))],
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec![""]);
    assert_eq!(
        app.session.views()[&view]
            .selections()
            .unwrap()
            .primary()
            .head(),
        TextOffset::origin()
    );
}

#[test]
fn content_scoped_origin_cannot_smuggle_view_operations() {
    let mut app = make_app(vec![], None);
    let request = crate::operation::OperationRequest::View {
        target: crate::operation::ViewTarget::Current,
        operation: crate::operation::ViewOperation::Apply(ViewAction::SetSelections(
            Selections::single(Selection::collapsed(TextOffset::origin())),
        )),
    };

    let error = app
        .execute_command(DispatchCommand::ModeContentOperations {
            operations: vec![request],
            content: editor_cid(),
        })
        .unwrap_err();

    assert!(error.to_string().contains("view-scoped origin"));

    let error = app
        .execute_command(DispatchCommand::ModeContentOperations {
            operations: vec![OperationRequest::ViewLifecycle(
                ViewLifecycleOperation::Switch {
                    spec: ViewSpec::buffer(editor_cid()),
                },
            )],
            content: editor_cid(),
        })
        .unwrap_err();

    assert!(error.to_string().contains("view-scoped origin"));
}

#[test]
fn mode_operations_reject_invalid_or_stale_view_state() {
    let mut invalid = make_app(vec![], None);
    let invalid_view = view_id(&invalid, invalid.session.focused());
    let error = invalid
        .execute_command(DispatchCommand::ModeOperations {
            operations: vec![view_action(ViewAction::SetSelections(Selections::single(
                Selection::collapsed(TextOffset { char_index: 99 }),
            )))],
            view: invalid_view,
            content: editor_cid(),
        })
        .unwrap_err();
    assert!(error.to_string().contains("invalid view action"));
    assert_eq!(
        invalid.session.views()[&invalid_view]
            .selections()
            .unwrap()
            .primary()
            .head(),
        TextOffset::origin()
    );

    let mut stale = make_app(vec![], None);
    let stale_view = view_id(&stale, stale.session.focused());
    stale
        .execute_command(DispatchCommand::ContentWithView {
            command: ContentCommand::Edit(EditCommand::InsertText("a".to_string())),
            view: stale_view,
            content: editor_cid(),
        })
        .unwrap();
    let error = stale
        .execute_command(DispatchCommand::ModeOperations {
            operations: vec![OperationRequest::View {
                target: ViewTarget::Current,
                operation: ViewOperation::ApplyPlan(ViewEditPlan {
                    expected: ViewPrecondition::Selections(Selections::single(
                        Selection::collapsed(TextOffset::origin()),
                    )),
                    content: None,
                    view: Some(ViewAction::SetSelections(Selections::single(
                        Selection::collapsed(TextOffset::origin()),
                    ))),
                }),
            }],
            view: stale_view,
            content: editor_cid(),
        })
        .unwrap_err();
    assert!(error.to_string().contains("stale resolved view edit"));
    assert_eq!(
        stale.session.views()[&stale_view]
            .selections()
            .unwrap()
            .primary()
            .head()
            .char_index,
        1
    );
}

#[test]
fn deferred_mode_edits_plan_after_history_operations() {
    let setup = || {
        let mut app = make_app(vec![], None);
        let view = view_id(&app, app.session.focused());
        app.execute_command(DispatchCommand::ContentWithView {
            command: ContentCommand::Edit(EditCommand::InsertText("a".to_string())),
            view,
            content: editor_cid(),
        })
        .unwrap();
        (app, view)
    };

    let (mut undo, undo_view) = setup();
    undo.execute_command(DispatchCommand::ModeOperations {
        operations: vec![
            history(TransactionIntent::Undo),
            view_edit(EditCommand::InsertText("b".to_string())),
        ],
        view: undo_view,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(text_rows(&undo, editor_cid()), vec!["b"]);

    let (mut redo, redo_view) = setup();
    redo.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Undo,
        view: redo_view,
        content: editor_cid(),
    })
    .unwrap();
    redo.execute_command(DispatchCommand::ModeOperations {
        operations: vec![
            history(TransactionIntent::Redo),
            view_edit(EditCommand::InsertText("b".to_string())),
        ],
        view: redo_view,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(text_rows(&redo, editor_cid()), vec!["ab"]);

    let (mut rollback, rollback_view) = setup();
    for command in [
        ContentCommand::Transaction(TransactionCommand::Begin),
        ContentCommand::Edit(EditCommand::InsertText("b".to_string())),
    ] {
        rollback
            .execute_command(DispatchCommand::ContentWithView {
                command,
                view: rollback_view,
                content: editor_cid(),
            })
            .unwrap();
    }
    rollback
        .execute_command(DispatchCommand::ModeOperations {
            operations: vec![
                history(TransactionIntent::Rollback),
                view_edit(EditCommand::InsertText("c".to_string())),
            ],
            view: rollback_view,
            content: editor_cid(),
        })
        .unwrap();
    assert_eq!(text_rows(&rollback, editor_cid()), vec!["ac"]);
}

#[test]
fn app_history_streams_are_isolated_by_content() {
    let mut app = make_app(vec![], None);
    let first_view = view_id(&app, app.session.focused());
    let second = ContentId(2);
    app.kernel
        .contents_mut()
        .insert(second, Content::Buffer(Buffer::new()))
        .unwrap();
    let second_space = app
        .split_space(
            app.session.focused(),
            second,
            true,
            SplitDirection::Right,
            false,
        )
        .unwrap()
        .new_space;
    let second_view = view_id(&app, second_space);

    for (view, content, text) in [(first_view, editor_cid(), "a"), (second_view, second, "b")] {
        app.execute_command(DispatchCommand::ContentWithView {
            command: ContentCommand::Edit(EditCommand::InsertText(text.to_string())),
            view,
            content,
        })
        .unwrap();
    }
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Undo,
        view: second_view,
        content: second,
    })
    .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["a"]);
    assert_eq!(text_rows(&app, second), vec![""]);
}

#[test]
fn failed_layout_mutations_do_not_consume_view_ids() {
    let mut app = make_app(vec![], None);
    let next = app.session.next_view_id_for_test();

    assert!(
        app.split_space(
            SpaceId(999),
            editor_cid(),
            true,
            SplitDirection::Right,
            false,
        )
        .is_err()
    );
    assert_eq!(app.session.next_view_id_for_test(), next);
    assert!(
        app.replace_space_content(SpaceId(999), editor_cid(), true)
            .is_err()
    );
    assert_eq!(app.session.next_view_id_for_test(), next);

    app.set_status_bar_placement(StatusBarPlacement::PerPane)
        .unwrap();
    let next = app.session.next_view_id_for_test();
    assert!(
        app.split_space(
            SpaceId(999),
            editor_cid(),
            true,
            SplitDirection::Right,
            false,
        )
        .is_err()
    );
    assert_eq!(app.session.next_view_id_for_test(), next);
}

#[test]
fn frontend_cannot_publish_an_invalid_focus_target() {
    let mut app = make_app(vec![], None);
    let focused = app.session.focused();
    app.frontend.focus_targets.push_back(Some(SpaceId(999)));

    let error = app
        .execute_command(DispatchCommand::App(AppCommand::Focus(
            SplitDirection::Right,
        )))
        .unwrap_err();

    assert!(error.to_string().contains("invalid focus target"));
    assert_eq!(app.session.focused(), focused);
}

#[test]
fn no_op_edit_does_not_advance_content_or_view_revision() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    let view_revision = app.session.views()[&view].revision();
    let content_revision = app.kernel.contents().revision(editor_cid()).unwrap();

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::MoveLeftBy(1)),
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(app.session.views()[&view].revision(), view_revision);
    assert_eq!(
        app.kernel.contents().revision(editor_cid()),
        Some(content_revision)
    );
}

#[test]
fn line_edit_is_one_history_record_and_restores_target_selection() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("one\ntwo".to_owned())),
        view,
        content: editor_cid(),
    })
    .unwrap();
    let before = Selections::single(Selection {
        anchor: TextOffset { char_index: 7 },
        head: TextOffset { char_index: 4 },
    });
    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![view_action(ViewAction::SetSelections(before.clone()))],
        view,
        content: editor_cid(),
    })
    .unwrap();

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::MoveLinesUp),
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["two", "one"]);
    assert_eq!(
        app.session.views()[&view].selections().unwrap().primary(),
        &Selection {
            anchor: TextOffset { char_index: 3 },
            head: TextOffset { char_index: 0 },
        }
    );

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Undo,
        view,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["one", "two"]);
    assert_eq!(app.session.views()[&view].selections().unwrap(), &before);

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Redo,
        view,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["two", "one"]);
    assert_eq!(
        app.session.views()[&view].selections().unwrap().primary(),
        &Selection {
            anchor: TextOffset { char_index: 3 },
            head: TextOffset { char_index: 0 },
        }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn frontend_error_still_waits_for_pending_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("save-on-error.txt");
    std::fs::write(&path, "old").unwrap();
    let mut app = make_app(vec![], path.to_str());
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("new".to_string())),
        view,
        content: editor_cid(),
    })
    .unwrap();
    app.execute_command(DispatchCommand::Content {
        command: ContentCommand::Save,
        content: editor_cid(),
    })
    .unwrap();
    app.frontend.fail_next_event = true;

    assert!(app.run().await.is_err());

    assert_eq!(std::fs::read_to_string(path).unwrap(), "newold");
    assert!(!app.kernel.has_pending_saves());
}

#[tokio::test(flavor = "multi_thread")]
async fn render_error_still_waits_for_pending_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("save-on-render-error.txt");
    std::fs::write(&path, "old").unwrap();
    let mut app = make_app(vec![], path.to_str());
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("new".to_string())),
        view,
        content: editor_cid(),
    })
    .unwrap();
    app.execute_command(DispatchCommand::Content {
        command: ContentCommand::Save,
        content: editor_cid(),
    })
    .unwrap();
    app.frontend.fail_render = true;

    assert!(app.run().await.is_err());

    assert_eq!(std::fs::read_to_string(path).unwrap(), "newold");
    assert!(!app.kernel.has_pending_saves());
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_discards_frontend_events_after_quit() {
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('i')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
            FrontendEvent::Key(KeyEvent::char('x')),
        ],
        None,
    );

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec![""]);
}

#[test]
fn clipboard_cut_paste_crosses_buffers_and_is_undoable() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![
            view_edit(EditCommand::InsertText("alpha".into())),
            view_action(ViewAction::SetSelections(Selections::single(Selection {
                anchor: TextOffset::origin(),
                head: TextOffset { char_index: 5 },
            }))),
            clipboard(ClipboardOperation::Copy {
                kind: ClipboardKind::CharacterWise,
                destination: ClipboardDestination::Internal,
            }),
        ],
        view,
        content: editor_cid(),
    })
    .unwrap();

    let other = app.new_buffer();
    let other_view = switch_focused_view(&mut app, other).unwrap();
    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![clipboard(ClipboardOperation::Paste {
            source: ClipboardSource::Internal,
            placement: PastePlacement::Before,
        })],
        view: other_view,
        content: other,
    })
    .unwrap();
    assert_eq!(
        app.kernel
            .contents()
            .text_snapshot(other)
            .unwrap()
            .to_owned_string(),
        "alpha"
    );

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![
            view_action(ViewAction::SetSelections(Selections::single(Selection {
                anchor: TextOffset::origin(),
                head: TextOffset { char_index: 5 },
            }))),
            clipboard(ClipboardOperation::Cut {
                kind: ClipboardKind::CharacterWise,
                destination: ClipboardDestination::Internal,
            }),
        ],
        view: other_view,
        content: other,
    })
    .unwrap();
    assert_eq!(text_rows(&app, other), vec![""]);

    for (intent, expected) in [
        (TransactionIntent::Undo, "alpha"),
        (TransactionIntent::Redo, ""),
    ] {
        app.execute_command(DispatchCommand::ModeOperations {
            operations: vec![history(intent)],
            view: other_view,
            content: other,
        })
        .unwrap();
        assert_eq!(
            app.kernel
                .contents()
                .text_snapshot(other)
                .unwrap()
                .to_owned_string(),
            expected
        );
    }
}

#[test]
fn clipboard_write_failure_keeps_internal_payload_available() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    app.frontend.fail_clipboard_write = true;
    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![
            view_edit(EditCommand::InsertText("safe".into())),
            view_action(ViewAction::SetSelections(Selections::single(Selection {
                anchor: TextOffset::origin(),
                head: TextOffset { char_index: 4 },
            }))),
            clipboard(ClipboardOperation::Cut {
                kind: ClipboardKind::CharacterWise,
                destination: ClipboardDestination::InternalAndSystem,
            }),
        ],
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec![""]);
    assert!(app.frontend.clipboard_writes.is_empty());
    assert!(
        app.runtime_diagnostics()
            .last()
            .unwrap()
            .message
            .contains("clipboard write failed")
    );
    let other = app.new_buffer();
    let other_view = switch_focused_view(&mut app, other).unwrap();
    app.frontend.fail_clipboard_read = true;
    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![clipboard(ClipboardOperation::Paste {
            source: ClipboardSource::System,
            placement: PastePlacement::Before,
        })],
        view: other_view,
        content: other,
    })
    .unwrap();
    assert_eq!(text_rows(&app, other), vec!["safe"]);
    assert!(
        app.runtime_diagnostics()
            .last()
            .unwrap()
            .message
            .contains("using internal clipboard")
    );
}

#[test]
fn failed_frame_rolls_back_cut_and_discards_clipboard_effects() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![
            view_edit(EditCommand::InsertText("old new".into())),
            view_action(ViewAction::SetSelections(Selections::single(Selection {
                anchor: TextOffset::origin(),
                head: TextOffset { char_index: 3 },
            }))),
            clipboard(ClipboardOperation::Copy {
                kind: ClipboardKind::CharacterWise,
                destination: ClipboardDestination::Internal,
            }),
        ],
        view,
        content: editor_cid(),
    })
    .unwrap();

    let result = app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![
            view_action(ViewAction::SetSelections(Selections::single(Selection {
                anchor: TextOffset { char_index: 4 },
                head: TextOffset { char_index: 7 },
            }))),
            clipboard(ClipboardOperation::Cut {
                kind: ClipboardKind::CharacterWise,
                destination: ClipboardDestination::InternalAndSystem,
            }),
            nested_mode(ModeCommand::new(
                ModeName::new("missing"),
                ModeActionName::new("run"),
            )),
        ],
        view,
        content: editor_cid(),
    });
    assert!(result.is_err());
    assert_eq!(text_rows(&app, editor_cid()), vec!["old new"]);
    assert!(app.frontend.clipboard_writes.is_empty());

    let other = app.new_buffer();
    let other_view = switch_focused_view(&mut app, other).unwrap();
    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![clipboard(ClipboardOperation::Paste {
            source: ClipboardSource::Internal,
            placement: PastePlacement::Before,
        })],
        view: other_view,
        content: other,
    })
    .unwrap();
    assert_eq!(text_rows(&app, other), vec!["old"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn bracketed_multiline_paste_is_one_undoable_edit() {
    let mut app = make_app(vec![], None);
    app.handle_event(FrontendEvent::Paste("one\r\ntwo".into()))
        .await
        .unwrap();
    assert_eq!(
        app.kernel
            .contents()
            .text_snapshot(editor_cid())
            .unwrap()
            .to_owned_string(),
        "one\r\ntwo"
    );

    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![history(TransactionIntent::Undo)],
        view,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec![""]);
}

#[test]
fn search_find_updates_only_the_view_and_expands_grapheme_matches() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("zero e\u{301} ONE".into())),
        view,
        content: editor_cid(),
    })
    .unwrap();
    let revision = app.kernel.contents().revision(editor_cid()).unwrap();

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![search(SearchOperation::Find {
            expected_revision: revision,
            start: 0,
            pattern: SearchPattern::Literal("\u{301}".into()),
            options: search_options(SearchDirection::Forward, false),
        })],
        view,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(
        app.session.views()[&view].selections().unwrap().primary(),
        &Selection {
            anchor: TextOffset { char_index: 5 },
            head: TextOffset { char_index: 7 },
        }
    );
    assert_eq!(app.kernel.contents().revision(editor_cid()), Some(revision));

    let mut options = search_options(SearchDirection::Backward, false);
    options.case = CaseSensitivity::Insensitive;
    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![search(SearchOperation::Find {
            expected_revision: revision,
            start: 11,
            pattern: SearchPattern::Literal("one".into()),
            options,
        })],
        view,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(
        app.session.views()[&view].selections().unwrap().primary(),
        &Selection {
            anchor: TextOffset { char_index: 11 },
            head: TextOffset { char_index: 8 },
        }
    );
}

#[test]
fn search_replace_next_and_all_are_undoable() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("a1 a2\r\na3".into())),
        view,
        content: editor_cid(),
    })
    .unwrap();
    let revision = app.kernel.contents().revision(editor_cid()).unwrap();
    let pattern = SearchPattern::Regex(r"a(\d)".into());

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![search(SearchOperation::ReplaceNext {
            expected_revision: revision,
            start: 0,
            pattern: pattern.clone(),
            replacement: "${1}a".into(),
            options: search_options(SearchDirection::Forward, false),
        })],
        view,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(
        app.kernel
            .contents()
            .text_snapshot(editor_cid())
            .unwrap()
            .to_owned_string(),
        "1a a2\r\na3"
    );

    for (intent, expected) in [
        (TransactionIntent::Undo, "a1 a2\r\na3"),
        (TransactionIntent::Redo, "1a a2\r\na3"),
        (TransactionIntent::Undo, "a1 a2\r\na3"),
    ] {
        app.execute_command(DispatchCommand::ModeOperations {
            operations: vec![history(intent)],
            view,
            content: editor_cid(),
        })
        .unwrap();
        assert_eq!(
            app.kernel
                .contents()
                .text_snapshot(editor_cid())
                .unwrap()
                .to_owned_string(),
            expected
        );
    }

    let revision = app.kernel.contents().revision(editor_cid()).unwrap();
    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![search(SearchOperation::ReplaceAll {
            expected_revision: revision,
            pattern,
            replacement: "${1}a".into(),
            case: CaseSensitivity::Sensitive,
        })],
        view,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(
        app.kernel
            .contents()
            .text_snapshot(editor_cid())
            .unwrap()
            .to_owned_string(),
        "1a 2a\r\n3a"
    );

    app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![history(TransactionIntent::Undo)],
        view,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(
        app.kernel
            .contents()
            .text_snapshot(editor_cid())
            .unwrap()
            .to_owned_string(),
        "a1 a2\r\na3"
    );
}

#[test]
fn failed_search_frame_rolls_back_text_and_revision() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("aa".into())),
        view,
        content: editor_cid(),
    })
    .unwrap();
    let revision = app.kernel.contents().revision(editor_cid()).unwrap();

    let result = app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![
            search(SearchOperation::ReplaceAll {
                expected_revision: revision,
                pattern: SearchPattern::Literal("a".into()),
                replacement: "b".into(),
                case: CaseSensitivity::Sensitive,
            }),
            search(SearchOperation::Find {
                expected_revision: Revision(revision.0 + 1),
                start: 0,
                pattern: SearchPattern::Regex("(".into()),
                options: search_options(SearchDirection::Forward, false),
            }),
        ],
        view,
        content: editor_cid(),
    });

    assert!(result.is_err());
    assert_eq!(text_rows(&app, editor_cid()), vec!["aa"]);
    assert_eq!(app.kernel.contents().revision(editor_cid()), Some(revision));
}

#[test]
fn search_rejects_stale_content_revision_without_mutation() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    let stale = app.kernel.contents().revision(editor_cid()).unwrap();
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("current".into())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    let result = app.execute_command(DispatchCommand::ModeOperations {
        operations: vec![search(SearchOperation::ReplaceAll {
            expected_revision: stale,
            pattern: SearchPattern::Literal("current".into()),
            replacement: "stale".into(),
            case: CaseSensitivity::Sensitive,
        })],
        view,
        content: editor_cid(),
    });

    assert!(result.is_err());
    assert_eq!(text_rows(&app, editor_cid()), vec!["current"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_product_editing_uses_language_and_line_primitives() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("editing.rs");
    let mut app = make_app(vec![], path.to_str());

    send_key(&mut app, KeyEvent::char('i')).await;
    send_text(&mut app, "fn main() {").await;
    send_key(&mut app, KeyEvent::plain(KeyCode::Enter)).await;
    send_key(&mut app, KeyEvent::plain(KeyCode::Tab)).await;
    send_key(&mut app, KeyEvent::plain(KeyCode::BackTab)).await;
    send_text(&mut app, "(x)").await;
    send_key(&mut app, KeyEvent::plain(KeyCode::Escape)).await;
    send_text(&mut app, "gc>><<").await;

    assert_eq!(
        text_rows(&app, editor_cid()),
        vec!["fn main() {", "    // (x)", "}"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_duplicate_and_move_commands_compose_line_primitives() {
    let mut app = make_app(vec![], None);
    send_key(&mut app, KeyEvent::char('i')).await;
    send_text(&mut app, "one").await;
    send_key(&mut app, KeyEvent::plain(KeyCode::Enter)).await;
    send_text(&mut app, "two").await;
    send_key(&mut app, KeyEvent::plain(KeyCode::Escape)).await;
    send_text(&mut app, "gggdgj").await;

    assert_eq!(text_rows(&app, editor_cid()), vec!["one", "two", "one"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_register_yank_delete_and_paste_use_the_clipboard_path() {
    let mut app = make_app(vec![], None);
    send_key(&mut app, KeyEvent::char('i')).await;
    send_text(&mut app, "one").await;
    send_key(&mut app, KeyEvent::plain(KeyCode::Enter)).await;
    send_text(&mut app, "two").await;
    send_key(&mut app, KeyEvent::plain(KeyCode::Escape)).await;
    send_text(&mut app, "gg\"+yypddP").await;

    assert_eq!(text_rows(&app, editor_cid()), vec!["one", "one", "two"]);
    assert_eq!(app.frontend.clipboard_writes, vec!["one\n"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_search_repeat_word_and_replace_commands_use_search_primitives() {
    let mut app = make_app(vec![], None);
    send_key(&mut app, KeyEvent::char('i')).await;
    send_text(&mut app, "one two one").await;
    send_key(&mut app, KeyEvent::plain(KeyCode::Escape)).await;
    send_text(&mut app, "gg/one").await;
    send_key(&mut app, KeyEvent::plain(KeyCode::Enter)).await;
    send_key(&mut app, KeyEvent::char('n')).await;
    assert_eq!(
        app.session.views()[&view_id(&app, app.session.focused())]
            .selections()
            .unwrap()
            .primary(),
        &Selection {
            anchor: TextOffset { char_index: 8 },
            head: TextOffset { char_index: 10 },
        }
    );

    send_vim_command(&mut app, "%s/one/ONE/g").await;
    assert_eq!(text_rows(&app, editor_cid()), vec!["ONE two ONE"]);

    send_text(&mut app, "gg*").await;
    assert_eq!(
        app.session.views()[&view_id(&app, app.session.focused())]
            .selections()
            .unwrap()
            .primary(),
        &Selection {
            anchor: TextOffset::origin(),
            head: TextOffset { char_index: 3 },
        }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_command_line_calls_formal_commands_and_registered_shortcuts() {
    let mut app = make_app(vec![], None);

    send_vim_command(
        &mut app,
        "ts function createContent() { return content.create(); }\
         editor.commands.shortcut('make-buffer', tail => {\
         if (tail !== 'one two') throw new Error('wrong tail'); content.create(); })",
    )
    .await;
    send_vim_command(&mut app, "make-buffer   one two  ").await;

    assert_eq!(app.buffers().len(), 2);
    assert!(app.runtime_diagnostics().is_empty());

    send_vim_command(
        &mut app,
        "view.switch({ type: 'core.buffer', content: createContent() })",
    )
    .await;

    assert_eq!(app.buffers().len(), 3);
    let view = view_id(&app, app.session.focused());
    assert_ne!(app.session.view(view).unwrap().content(), editor_cid());
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_save_call_uses_the_formal_async_command() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("vim-save-call.txt");
    std::fs::write(&path, "before").unwrap();
    let mut app = make_app(vec![], path.to_str());
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("after ".to_owned())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    send_vim_command(&mut app, "content.save()").await;

    assert_eq!(app.pending_commands.len(), 1);
    let message = app.kernel.receive_message().await.unwrap();
    app.handle_app_message(message).unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "after before");
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_ts_without_a_tail_executes_the_current_top_level_statement() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText(
            "const made = content.create();\n".to_owned(),
        )),
        view,
        content: editor_cid(),
    })
    .unwrap();
    app.session
        .view_mut(view)
        .unwrap()
        .set_selections(Selections::single(Selection::collapsed(TextOffset {
            char_index: 8,
        })));

    send_vim_command(&mut app, "ts").await;

    assert_eq!(app.buffers().len(), 2);
    assert!(app.runtime_diagnostics().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_command_line_rejects_plain_typescript_without_ts() {
    let mut events = vim_command_events("const hidden = 1");
    events.push(FrontendEvent::Key(KeyEvent::ctrl('q')));
    let mut app = make_app(events, None);

    app.run().await.unwrap();

    let message = &app.runtime_diagnostics().last().unwrap().message;
    assert!(message.contains("use ':ts'"), "{message}");
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_wq_waits_for_save_before_quitting() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("vim-wq.txt");
    std::fs::write(&path, "before").unwrap();
    let mut app = make_app(vec![], path.to_str());
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("saved ".to_owned())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    send_vim_command(&mut app, "wq").await;

    assert!(!app.kernel.is_cancelled());
    assert_eq!(app.pending_commands.len(), 1);
    let message = app.kernel.receive_message().await.unwrap();
    app.handle_app_message(message).unwrap();

    assert!(app.kernel.is_cancelled());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "saved before");
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_q_dispatches_the_registered_quit_shortcut() {
    let mut app = make_app(vec![], None);

    send_vim_command(&mut app, "q").await;

    assert!(app.kernel.is_cancelled());
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_shortcut_discards_its_deferred_mode_operation() {
    let mut events = vim_command_events(
        "ts editor.commands.shortcut('fail-new', () => {\
         invokeMode('vim.new'); throw new Error('shortcut failed'); })",
    );
    events.extend(vim_command_events("fail-new"));
    events.push(FrontendEvent::Key(KeyEvent::ctrl('q')));
    let mut app = make_app(events, None);

    app.run().await.unwrap();

    assert_eq!(app.buffers().len(), 1);
    assert!(
        app.runtime_diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("shortcut failed"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_commands_drive_buffer_lifecycle_and_save_as() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("command-save.txt");
    let mut app = make_app(vec![], None);

    send_key(&mut app, KeyEvent::char('i')).await;
    send_text(&mut app, "saved").await;
    send_key(&mut app, KeyEvent::plain(KeyCode::Escape)).await;
    send_vim_command(&mut app, &format!("saveas {}", path.to_string_lossy())).await;
    while app.kernel.has_pending_save(editor_cid()) {
        let message = app.kernel.receive_message().await.unwrap();
        app.handle_app_message(message).unwrap();
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "saved");

    send_vim_command(&mut app, "new").await;
    let created = app.session.views()[&view_id(&app, app.session.focused())].content();
    assert_ne!(created, editor_cid());
    send_vim_command(&mut app, "buffers").await;
    send_vim_command(&mut app, "b 0").await;
    send_vim_command(&mut app, &format!("bd! {}", created.0)).await;

    assert!(!app.kernel.contents().contains(created));
    assert!(
        app.runtime_diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("command-save.txt"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_open_and_force_reload_commands_use_async_file_lifecycle() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("command-open.txt");
    std::fs::write(&path, "opened").unwrap();
    let mut app = make_app(vec![], None);

    send_vim_command(&mut app, &format!("e {}", path.to_string_lossy())).await;
    let message = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        app.kernel.receive_message(),
    )
    .await
    .expect("open command queued a completion")
    .unwrap();
    app.handle_app_message(message).unwrap();
    let view = view_id(&app, app.session.focused());
    let opened = app.session.views()[&view].content();
    assert_eq!(text_rows(&app, opened), vec!["opened"]);

    std::fs::write(&path, "reloaded").unwrap();
    send_vim_command(&mut app, "reload!").await;
    assert_eq!(text_rows(&app, opened), vec!["reloaded"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_invalid_regex_is_reported_in_the_status_bar() {
    let mut app = make_app(vim_command_events("%s/(/x/g"), None);

    app.run().await.unwrap();

    let message = &app.runtime_diagnostics().last().unwrap().message;
    assert!(message.contains("invalid regex"), "{message}");
    assert_eq!(status_center(&app), *message);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_command_parse_errors_are_reported_in_the_status_bar() {
    let mut app = make_app(vec![], None);

    send_key(&mut app, KeyEvent::char(':')).await;
    send_text(&mut app, "%s/a/b/z").await;
    assert_eq!(status_texts(&app).0.concat(), ":%s/a/b/z");
    send_key(&mut app, KeyEvent::plain(KeyCode::Enter)).await;

    assert!(status_texts(&app).0.concat().contains("E488"));
    assert!(app.runtime_diagnostics().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_dirty_buffer_close_is_rejected_without_losing_text() {
    let mut app = make_app(vec![], None);
    send_key(&mut app, KeyEvent::char('i')).await;
    send_text(&mut app, "keep me").await;
    send_key(&mut app, KeyEvent::plain(KeyCode::Escape)).await;
    app.frontend.events.extend(vim_command_events("bd"));

    app.run().await.unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["keep me"]);
    let message = &app.runtime_diagnostics().last().unwrap().message;
    assert!(message.contains("unsaved changes"), "{message}");
    assert_eq!(status_center(&app), *message);
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_save_conflict_preserves_external_file_and_reports_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("vim-save-conflict.txt");
    std::fs::write(&path, "base").unwrap();
    let mut app = make_app(vec![], path.to_str());
    send_key(&mut app, KeyEvent::char('i')).await;
    send_text(&mut app, "local ").await;
    send_key(&mut app, KeyEvent::plain(KeyCode::Escape)).await;
    std::fs::write(&path, "external").unwrap();
    app.frontend.events.extend(vim_command_events("w"));

    app.run().await.unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "external");
    let message = &app.runtime_diagnostics().last().unwrap().message;
    assert!(message.contains("external changes"), "{message}");
    assert_eq!(status_center(&app), *message);
}

#[test]
fn registered_content_result_can_feed_nested_view_switch() {
    let mut app = make_app(vec![], None);
    let test_id = CommandId::new("test.newAndSwitch").unwrap();
    app.register_command(CommandEntry::new(
        test_id.clone(),
        |host: &mut dyn CommandHost, arguments: Vec<CommandValue>| {
            if !arguments.is_empty() {
                return Err(CommandError::InvalidArguments(
                    "expected no arguments".to_owned(),
                ));
            }
            let created = host.invoke_command(CommandInvocation::new(
                CommandId::new("content.create").unwrap(),
                Vec::new(),
            ))?;
            let content = match created {
                CommandCompletion::Ready(content) => content,
                CommandCompletion::Pending(pending) => {
                    return Ok(CommandCompletion::Pending(pending));
                }
            };
            host.invoke_command(CommandInvocation::new(
                CommandId::new("view.switch").unwrap(),
                vec![serde_json::json!({
                    "type": "core.buffer",
                    "content": content,
                })],
            ))
        },
    ));
    let view = view_id(&app, app.session.focused());

    app.execute_command(DispatchCommand::Registered {
        invocation: CommandInvocation::new(test_id, Vec::new()),
        view,
        content: editor_cid(),
    })
    .unwrap();

    let focused_view = view_id(&app, app.session.focused());
    let focused_content = app.session.view(focused_view).unwrap().content();
    assert_ne!(focused_content, editor_cid());
    assert_eq!(app.buffers().len(), 2);
}

#[test]
fn script_command_calls_native_commands_in_the_same_frame() {
    let mut loaded = vell_plugin_v8::load_typescript_modes(
        "file:///commands.ts",
        r#"
editor.commands.register("test.newAndSwitch", () => {
  const created = content.create();
  view.switch({ type: "core.buffer", content: created });
});
"#,
    )
    .unwrap();
    loaded
        .install_native_commands(&crate::native_command_ids())
        .unwrap();
    let mut app = App::new(None, 40, 5, ScriptedFrontend::new(Vec::new())).unwrap();
    for command in loaded.commands {
        app.register_command(command);
    }
    let view = view_id(&app, app.session.focused());

    app.execute_command(DispatchCommand::Registered {
        invocation: CommandInvocation::new(CommandId::new("test.newAndSwitch").unwrap(), vec![]),
        view,
        content: editor_cid(),
    })
    .unwrap();

    let focused_view = view_id(&app, app.session.focused());
    assert_ne!(
        app.session.view(focused_view).unwrap().content(),
        editor_cid()
    );
    assert_eq!(app.buffers().len(), 2);
}

#[test]
fn persistent_global_script_calls_native_commands_in_its_frame() {
    let mut loaded =
        vell_plugin_v8::load_typescript_modes("file:///empty.ts", "globalThis.loaded = true;")
            .unwrap();
    loaded
        .install_native_commands(&crate::native_command_ids())
        .unwrap();
    let mut app = App::new(None, 40, 5, ScriptedFrontend::new(Vec::new())).unwrap();
    for command in loaded.commands {
        app.register_command(command);
    }
    let view = view_id(&app, app.session.focused());

    app.execute_command(DispatchCommand::Registered {
        invocation: vell_plugin_v8::GlobalScriptRequest::Interactive {
            source: r#"
function newAndSwitch() {
  const created = content.create();
  view.switch({ type: "core.buffer", content: created });
}
"#
            .to_owned(),
        }
        .into_invocation(),
        view,
        content: editor_cid(),
    })
    .unwrap();
    app.execute_command(DispatchCommand::Registered {
        invocation: vell_plugin_v8::GlobalScriptRequest::Interactive {
            source: "newAndSwitch();".to_owned(),
        }
        .into_invocation(),
        view,
        content: editor_cid(),
    })
    .unwrap();

    let focused_view = view_id(&app, app.session.focused());
    assert_ne!(
        app.session.view(focused_view).unwrap().content(),
        editor_cid()
    );
    assert_eq!(app.buffers().len(), 2);
}

fn install_command_script(
    app: &mut App<ScriptedFrontend>,
    source: &str,
    extra_native_commands: &[CommandId],
) {
    let mut loaded =
        vell_plugin_v8::load_typescript_modes("file:///async-commands.ts", source).unwrap();
    let mut native_commands = crate::native_command_ids();
    native_commands.extend_from_slice(extra_native_commands);
    loaded.install_native_commands(&native_commands).unwrap();
    for command in loaded.commands {
        app.register_command(command);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn native_save_command_stays_pending_until_the_write_completes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("native-command-save.txt");
    std::fs::write(&path, "before").unwrap();
    let mut app = App::new(path.to_str(), 40, 5, ScriptedFrontend::new(Vec::new())).unwrap();
    let view = view_id(&app, app.session.focused());

    app.execute_command(DispatchCommand::Registered {
        invocation: CommandInvocation::new(CommandId::new("content.save").unwrap(), Vec::new()),
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(app.pending_commands.len(), 1);
    let message = app.kernel.receive_message().await.unwrap();
    app.handle_app_message(message).unwrap();
    assert!(app.pending_commands.is_empty());
    assert_eq!(save_state(&app, editor_cid()), SaveState::Saved);
}

#[tokio::test(flavor = "multi_thread")]
async fn returned_save_promise_resumes_before_quit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("async-save-quit.txt");
    std::fs::write(&path, "before").unwrap();
    let mut app = App::new(path.to_str(), 40, 5, ScriptedFrontend::new(Vec::new())).unwrap();
    install_command_script(
        &mut app,
        r#"
editor.commands.register("test.saveThenQuit", async () => {
  await content.save();
  quit();
});
"#,
        &[],
    );
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("changed ".to_owned())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    app.execute_command(DispatchCommand::Registered {
        invocation: CommandInvocation::new(
            CommandId::new("test.saveThenQuit").unwrap(),
            Vec::new(),
        ),
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(app.pending_commands.len(), 1);
    assert!(!app.kernel.is_cancelled());
    let message = app.kernel.receive_message().await.unwrap();
    app.handle_app_message(message).unwrap();

    assert!(app.kernel.is_cancelled());
    assert!(app.pending_commands.is_empty());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "changed before");
}

#[tokio::test(flavor = "multi_thread")]
async fn async_command_resume_keeps_its_original_target_after_focus_changes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("async-fixed-target.txt");
    std::fs::write(&path, "before").unwrap();
    let captured = Rc::new(RefCell::new(Vec::new()));
    let captured_for_command = Rc::clone(&captured);
    let capture_id = CommandId::new("test.captureCurrent").unwrap();
    let mut app = App::new(path.to_str(), 40, 5, ScriptedFrontend::new(Vec::new())).unwrap();
    app.register_command(CommandEntry::new(
        capture_id.clone(),
        move |host: &mut dyn CommandHost, _arguments: Vec<CommandValue>| {
            let result = host.request(CommandRequest::Query(CommandQuery::CurrentContent))?;
            if let CommandCompletion::Ready(value) = &result {
                captured_for_command.borrow_mut().push(value.clone());
            }
            Ok(result)
        },
    ));
    install_command_script(
        &mut app,
        r#"
editor.commands.register("test.saveAndCapture", async () => {
  await content.save();
  return test.captureCurrent();
});
"#,
        std::slice::from_ref(&capture_id),
    );
    let original_view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::Registered {
        invocation: CommandInvocation::new(
            CommandId::new("test.saveAndCapture").unwrap(),
            Vec::new(),
        ),
        view: original_view,
        content: editor_cid(),
    })
    .unwrap();
    app.execute_command(DispatchCommand::Registered {
        invocation: CommandInvocation::new(CommandId::new("splitHorizontal").unwrap(), Vec::new()),
        view: original_view,
        content: editor_cid(),
    })
    .unwrap();
    let other = app.new_buffer();
    switch_focused_view(&mut app, other).unwrap();

    let message = app.kernel.receive_message().await.unwrap();
    app.handle_app_message(message).unwrap();

    assert_eq!(&*captured.borrow(), &[CommandValue::from(editor_cid().0)]);
    assert_eq!(
        app.session
            .view_for_space(app.session.focused())
            .and_then(|view| app.session.view(view))
            .map(|view| view.content()),
        Some(other)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn text_change_while_awaiting_cancels_the_original_continuation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("async-stale-target.txt");
    std::fs::write(&path, "before").unwrap();
    let calls = Rc::new(RefCell::new(0));
    let calls_for_command = Rc::clone(&calls);
    let capture_id = CommandId::new("test.captureResume").unwrap();
    let mut app = App::new(path.to_str(), 40, 5, ScriptedFrontend::new(Vec::new())).unwrap();
    app.register_command(CommandEntry::new(
        capture_id.clone(),
        move |_host: &mut dyn CommandHost, _arguments: Vec<CommandValue>| {
            *calls_for_command.borrow_mut() += 1;
            Ok(CommandValue::Null.into())
        },
    ));
    install_command_script(
        &mut app,
        r#"
editor.commands.register("test.saveAndResume", async () => {
  await content.save();
  test.captureResume();
});
"#,
        std::slice::from_ref(&capture_id),
    );
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::Registered {
        invocation: CommandInvocation::new(
            CommandId::new("test.saveAndResume").unwrap(),
            Vec::new(),
        ),
        view,
        content: editor_cid(),
    })
    .unwrap();
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("new ".to_owned())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert!(app.pending_commands.is_empty());

    let message = app.kernel.receive_message().await.unwrap();
    app.handle_app_message(message).unwrap();

    assert_eq!(*calls.borrow(), 0);
    assert!(app.pending_commands.is_empty());
    assert!(
        app.runtime_diagnostics()
            .last()
            .unwrap()
            .message
            .contains("target changed")
    );
    assert_eq!(text_rows(&app, editor_cid()), ["new before"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn post_await_exception_rolls_back_only_the_resumed_frame() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("async-frame-rollback.txt");
    std::fs::write(&path, "before").unwrap();
    let mut app = App::new(path.to_str(), 40, 5, ScriptedFrontend::new(Vec::new())).unwrap();
    install_command_script(
        &mut app,
        r#"
editor.commands.register("test.failAfterSave", async () => {
  await content.save();
  const created = content.create();
  view.switch({ type: "core.buffer", content: created });
  throw new Error("after await");
});
"#,
        &[],
    );
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("saved ".to_owned())),
        view,
        content: editor_cid(),
    })
    .unwrap();
    app.execute_command(DispatchCommand::Registered {
        invocation: CommandInvocation::new(
            CommandId::new("test.failAfterSave").unwrap(),
            Vec::new(),
        ),
        view,
        content: editor_cid(),
    })
    .unwrap();

    let message = app.kernel.receive_message().await.unwrap();
    app.handle_app_message(message).unwrap();

    assert_eq!(std::fs::read_to_string(path).unwrap(), "saved before");
    assert_eq!(app.buffers().len(), 1);
    assert_eq!(
        app.session.view(view).map(|view| view.content()),
        Some(editor_cid())
    );
    assert!(
        app.runtime_diagnostics()
            .last()
            .unwrap()
            .message
            .contains("after await")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn save_failure_rejects_the_script_promise() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("async-save-error.txt");
    std::fs::write(&path, "before").unwrap();
    let captured = Rc::new(RefCell::new(Vec::new()));
    let captured_for_command = Rc::clone(&captured);
    let capture_id = CommandId::new("test.captureError").unwrap();
    let mut app = App::new(path.to_str(), 40, 5, ScriptedFrontend::new(Vec::new())).unwrap();
    app.register_command(CommandEntry::new(
        capture_id.clone(),
        move |_host: &mut dyn CommandHost, arguments: Vec<CommandValue>| {
            captured_for_command.borrow_mut().extend(arguments);
            Ok(CommandValue::Null.into())
        },
    ));
    install_command_script(
        &mut app,
        r#"
editor.commands.register("test.catchSave", async () => {
  try {
    await content.save();
  } catch (error) {
    test.captureError(String(error));
  }
});
"#,
        std::slice::from_ref(&capture_id),
    );
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::Registered {
        invocation: CommandInvocation::new(CommandId::new("test.catchSave").unwrap(), Vec::new()),
        view,
        content: editor_cid(),
    })
    .unwrap();
    let revision = app.command_tasks.values().next().unwrap().revision;
    let state = app.pending_commands[0].expected_state;

    app.handle_app_message(AppMessage::SaveCompleted {
        content: editor_cid(),
        revision,
        state,
        result: Err(io::Error::other("disk full")),
    })
    .unwrap();

    let captured = captured.borrow();
    let [CommandValue::String(message)] = captured.as_slice() else {
        panic!("save rejection was not caught: {captured:?}");
    };
    assert!(message.contains("save failed: disk full"), "{message}");
    assert!(app.pending_commands.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn promise_all_correlates_multiple_save_tasks_to_one_continuation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("async-save-all.txt");
    std::fs::write(&path, "before").unwrap();
    let calls = Rc::new(RefCell::new(0));
    let calls_for_command = Rc::clone(&calls);
    let capture_id = CommandId::new("test.captureAll").unwrap();
    let mut app = App::new(path.to_str(), 40, 5, ScriptedFrontend::new(Vec::new())).unwrap();
    app.register_command(CommandEntry::new(
        capture_id.clone(),
        move |_host: &mut dyn CommandHost, _arguments: Vec<CommandValue>| {
            *calls_for_command.borrow_mut() += 1;
            Ok(CommandValue::Null.into())
        },
    ));
    install_command_script(
        &mut app,
        r#"
editor.commands.register("test.saveAll", async () => {
  await Promise.all([content.save(), content.save()]);
  test.captureAll();
});
"#,
        std::slice::from_ref(&capture_id),
    );
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::Registered {
        invocation: CommandInvocation::new(CommandId::new("test.saveAll").unwrap(), Vec::new()),
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(app.command_tasks.len(), 2);
    let message = app.kernel.receive_message().await.unwrap();
    app.handle_app_message(message).unwrap();

    assert_eq!(*calls.borrow(), 1);
    assert!(app.command_tasks.is_empty());
    assert!(app.pending_commands.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn unreturned_save_promise_does_not_suspend_the_command() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("async-unreturned-save.txt");
    std::fs::write(&path, "before").unwrap();
    let mut app = App::new(path.to_str(), 40, 5, ScriptedFrontend::new(Vec::new())).unwrap();
    install_command_script(
        &mut app,
        r#"
editor.commands.register("test.fireAndForget", () => {
  content.save();
});
"#,
        &[],
    );
    let view = view_id(&app, app.session.focused());

    app.execute_command(DispatchCommand::Registered {
        invocation: CommandInvocation::new(
            CommandId::new("test.fireAndForget").unwrap(),
            Vec::new(),
        ),
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert!(app.pending_commands.is_empty());
    assert_eq!(app.command_tasks.len(), 1);
    let message = app.kernel.receive_message().await.unwrap();
    app.handle_app_message(message).unwrap();
    assert!(app.command_tasks.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_the_original_view_cancels_its_suspended_command() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("async-closed-view.txt");
    std::fs::write(&path, "before").unwrap();
    let calls = Rc::new(RefCell::new(0));
    let calls_for_command = Rc::clone(&calls);
    let capture_id = CommandId::new("test.captureClosed").unwrap();
    let mut app = App::new(path.to_str(), 40, 5, ScriptedFrontend::new(Vec::new())).unwrap();
    app.register_command(CommandEntry::new(
        capture_id.clone(),
        move |_host: &mut dyn CommandHost, _arguments: Vec<CommandValue>| {
            *calls_for_command.borrow_mut() += 1;
            Ok(CommandValue::Null.into())
        },
    ));
    install_command_script(
        &mut app,
        r#"
editor.commands.register("test.resumeClosed", async () => {
  await content.save();
  test.captureClosed();
});
"#,
        std::slice::from_ref(&capture_id),
    );
    let original_view = view_id(&app, app.session.focused());
    let original_space = app.session.body_space_for_view(original_view).unwrap();
    app.execute_command(DispatchCommand::Registered {
        invocation: CommandInvocation::new(CommandId::new("splitHorizontal").unwrap(), Vec::new()),
        view: original_view,
        content: editor_cid(),
    })
    .unwrap();
    app.execute_command(DispatchCommand::Registered {
        invocation: CommandInvocation::new(
            CommandId::new("test.resumeClosed").unwrap(),
            Vec::new(),
        ),
        view: original_view,
        content: editor_cid(),
    })
    .unwrap();
    app.close_space(original_space).unwrap();

    assert!(app.pending_commands.is_empty());

    let message = app.kernel.receive_message().await.unwrap();
    app.handle_app_message(message).unwrap();

    assert_eq!(*calls.borrow(), 0);
    assert!(app.pending_commands.is_empty());
    assert!(
        app.runtime_diagnostics()
            .last()
            .unwrap()
            .message
            .contains("target view was closed")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn command_registered_after_await_is_published_to_the_host_registry() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("async-register.txt");
    std::fs::write(&path, "before").unwrap();
    let mut app = App::new(path.to_str(), 40, 5, ScriptedFrontend::new(Vec::new())).unwrap();
    install_command_script(
        &mut app,
        r#"
editor.commands.register("test.installAfterSave", async () => {
  await content.save();
  editor.commands.register("test.installedAfterSave", () => content.create());
});
"#,
        &[],
    );
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::Registered {
        invocation: CommandInvocation::new(
            CommandId::new("test.installAfterSave").unwrap(),
            Vec::new(),
        ),
        view,
        content: editor_cid(),
    })
    .unwrap();

    let message = app.kernel.receive_message().await.unwrap();
    app.handle_app_message(message).unwrap();
    let installed = CommandId::new("test.installedAfterSave").unwrap();

    assert!(app.kernel.commands().get(&installed).is_some());
    app.execute_command(DispatchCommand::Registered {
        invocation: CommandInvocation::new(installed, Vec::new()),
        view,
        content: editor_cid(),
    })
    .unwrap();
    assert_eq!(app.buffers().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn global_typescript_evaluator_resumes_its_final_promise() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("async-global-script.txt");
    std::fs::write(&path, "before").unwrap();
    let mut loaded =
        vell_plugin_v8::load_typescript_modes("file:///async-global-config.ts", "void 0;").unwrap();
    loaded
        .install_native_commands(&crate::native_command_ids())
        .unwrap();
    let mut app = App::new(path.to_str(), 40, 5, ScriptedFrontend::new(Vec::new())).unwrap();
    for command in loaded.commands {
        app.register_command(command);
    }
    let view = view_id(&app, app.session.focused());

    app.execute_command(DispatchCommand::Registered {
        invocation: vell_plugin_v8::GlobalScriptRequest::Interactive {
            source: r#"
async function saveThenQuitFromGlobal() {
  await content.save();
  quit();
}
saveThenQuitFromGlobal();
"#
            .to_owned(),
        }
        .into_invocation(),
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(app.pending_commands.len(), 1);
    let message = app.kernel.receive_message().await.unwrap();
    app.handle_app_message(message).unwrap();
    assert!(app.kernel.is_cancelled());
}

#[test]
fn save_conflict_is_exposed_to_typescript_as_a_rejected_promise() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("async-save-conflict.txt");
    std::fs::write(&path, "opened").unwrap();
    let captured = Rc::new(RefCell::new(Vec::new()));
    let captured_for_command = Rc::clone(&captured);
    let capture_id = CommandId::new("test.capturePromise").unwrap();
    let mut app = App::new(path.to_str(), 40, 5, ScriptedFrontend::new(Vec::new())).unwrap();
    app.register_command(CommandEntry::new(
        capture_id.clone(),
        move |_host: &mut dyn CommandHost, arguments: Vec<CommandValue>| {
            captured_for_command.borrow_mut().extend(arguments);
            Ok(CommandValue::Null.into())
        },
    ));
    install_command_script(
        &mut app,
        r#"
editor.commands.register("test.captureConflict", async () => {
  const pending = content.save();
  test.capturePromise(pending instanceof Promise);
  try {
    await pending;
  } catch (error) {
    test.capturePromise(String(error));
  }
});
"#,
        std::slice::from_ref(&capture_id),
    );
    std::fs::write(&path, "external").unwrap();
    let view = view_id(&app, app.session.focused());

    app.execute_command(DispatchCommand::Registered {
        invocation: CommandInvocation::new(
            CommandId::new("test.captureConflict").unwrap(),
            Vec::new(),
        ),
        view,
        content: editor_cid(),
    })
    .unwrap();

    let captured = captured.borrow();
    assert_eq!(captured.first(), Some(&CommandValue::Bool(true)));
    let Some(CommandValue::String(error)) = captured.get(1) else {
        panic!("save conflict did not reject its promise: {captured:?}");
    };
    assert!(error.contains("external changes"), "{error}");
    assert!(app.pending_commands.is_empty());
    assert!(app.command_tasks.is_empty());
}

#[test]
fn failed_registered_command_removes_provisional_content() {
    let mut app = make_app(vec![], None);
    let test_id = CommandId::new("test.createThenFail").unwrap();
    app.register_command(CommandEntry::new(
        test_id.clone(),
        |host: &mut dyn CommandHost, _arguments: Vec<CommandValue>| {
            host.invoke_command(CommandInvocation::new(
                CommandId::new("content.create").unwrap(),
                Vec::new(),
            ))?;
            host.invoke_command(CommandInvocation::new(
                CommandId::new("missing").unwrap(),
                Vec::new(),
            ))
        },
    ));
    let view = view_id(&app, app.session.focused());

    let error = app
        .execute_command(DispatchCommand::Registered {
            invocation: CommandInvocation::new(test_id, Vec::new()),
            view,
            content: editor_cid(),
        })
        .unwrap_err();

    assert!(error.to_string().contains("unknown command 'missing'"));
    assert_eq!(app.buffers().len(), 1);
    assert_eq!(app.buffers()[0].content, editor_cid());
}

#[test]
fn registered_query_observes_an_earlier_edit_in_the_same_frame() {
    let mut app = make_app(vec![], None);
    let test_id = CommandId::new("test.editThenRead").unwrap();
    app.register_command(CommandEntry::new(
        test_id.clone(),
        |host: &mut dyn CommandHost, _arguments: Vec<CommandValue>| {
            host.request(CommandRequest::Execute(OperationRequest::View {
                target: ViewTarget::Current,
                operation: ViewOperation::Edit(EditCommand::InsertText("visible".to_owned())),
            }))?;
            match host.request(CommandRequest::Query(CommandQuery::CurrentText))? {
                CommandCompletion::Ready(CommandValue::String(text)) if text == "visible" => {
                    Ok(CommandValue::Null.into())
                }
                _ => Err(CommandError::Failed(
                    "query did not observe the earlier edit".to_owned(),
                )),
            }
        },
    ));
    let view = view_id(&app, app.session.focused());

    app.execute_command(DispatchCommand::Registered {
        invocation: CommandInvocation::new(test_id, Vec::new()),
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), ["visible"]);
}

#[test]
fn nested_registered_failure_rolls_back_host_mutations_and_effects() {
    let mut app = make_app(vec![], None);
    let test_id = CommandId::new("test.mutateThenFail").unwrap();
    app.register_command(CommandEntry::new(
        test_id.clone(),
        |host: &mut dyn CommandHost, _arguments: Vec<CommandValue>| {
            host.request(CommandRequest::Execute(OperationRequest::View {
                target: ViewTarget::Current,
                operation: ViewOperation::Edit(EditCommand::InsertText("rollback".to_owned())),
            }))?;
            host.invoke_command(CommandInvocation::new(
                CommandId::new("forceQuit").unwrap(),
                Vec::new(),
            ))?;
            host.invoke_command(CommandInvocation::new(
                CommandId::new("missing").unwrap(),
                Vec::new(),
            ))
        },
    ));
    let view = view_id(&app, app.session.focused());

    assert!(
        app.execute_command(DispatchCommand::Registered {
            invocation: CommandInvocation::new(test_id, Vec::new()),
            view,
            content: editor_cid(),
        })
        .is_err()
    );

    assert_eq!(text_rows(&app, editor_cid()), [""]);
    assert!(!app.kernel.is_cancelled());
}

#[test]
fn registered_command_depth_budget_stops_recursive_adapters() {
    let mut app = make_app(vec![], None);
    let test_id = CommandId::new("test.recurse").unwrap();
    let recursive_id = test_id.clone();
    app.register_command(CommandEntry::new(
        test_id.clone(),
        move |host: &mut dyn CommandHost, _arguments: Vec<CommandValue>| {
            host.invoke_command(CommandInvocation::new(recursive_id.clone(), Vec::new()))
        },
    ));
    let view = view_id(&app, app.session.focused());

    let error = app
        .execute_command(DispatchCommand::Registered {
            invocation: CommandInvocation::new(test_id, Vec::new()),
            view,
            content: editor_cid(),
        })
        .unwrap_err();

    assert!(error.to_string().contains("recursion limit of 256"));
}

#[test]
fn registered_host_requests_share_the_frame_operation_budget() {
    let mut app = make_app(vec![], None);
    let test_id = CommandId::new("test.exhaustRequests").unwrap();
    app.register_command(CommandEntry::new(
        test_id.clone(),
        |host: &mut dyn CommandHost, _arguments: Vec<CommandValue>| {
            for _ in 0..=vell_mode::operation::MAX_OPERATIONS_PER_FRAME {
                host.invoke_command(CommandInvocation::new(
                    CommandId::new("content.create").unwrap(),
                    Vec::new(),
                ))?;
            }
            Ok(CommandValue::Null.into())
        },
    ));
    let view = view_id(&app, app.session.focused());

    let error = app
        .execute_command(DispatchCommand::Registered {
            invocation: CommandInvocation::new(test_id, Vec::new()),
            view,
            content: editor_cid(),
        })
        .unwrap_err();

    assert!(error.to_string().contains("command chain exceeded"));
    assert_eq!(app.buffers().len(), 1);
}

#[test]
fn native_history_command_uses_the_registered_execution_path() {
    let mut app = make_app(vec![], None);
    let view = view_id(&app, app.session.focused());
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("undo me".to_owned())),
        view,
        content: editor_cid(),
    })
    .unwrap();

    app.execute_command(DispatchCommand::Registered {
        invocation: CommandInvocation::new(CommandId::new("undo").unwrap(), Vec::new()),
        view,
        content: editor_cid(),
    })
    .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), [""]);
}
