use std::io;

use super::App;
use super::bootstrap::create_editor_session;
use super::command_resolver::default_global_keymap;
use super::dispatcher::{DispatchCommand, Dispatcher};
use super::layout::{LayoutError, NewView, resolve_focus, view_for_space};
use super::message::AppMessage;
use super::query::AppQuery;
use super::view::View;
use crate::app::action::{TransactionIntent, ViewAction};
use crate::app::command::{
    AppCommand, Command, ContentCommand, ModeCommand, ModeValue, TransactionCommand,
};
use crate::app::mode::{
    Mode, ModeActionScope, ModeContentContext, ModeEffect, ModeError, ModeResult, ModeState,
    ModeViewContext, ModeViewInstance, ModeViewPolicy, ResolvedViewEdit,
};
use crate::app::mode_name::{ModeActionName, ModeName};
use crate::core::action::ContentAction;
use crate::core::buffer::Buffer;
use crate::core::command::EditCommand;
use crate::core::content::{Content, ContentChange};
use crate::core::keymap::Keymap;
use crate::core::transaction::{TextChangeSet, TextEdit};
use crate::frontend::Frontend;
use crate::protocol::content_query::{
    Color, ContentData, ContentQuery, CursorStyle, DocumentStatus, Face, FaceName,
    NamedTextDecoration, RenderQuery, RowRange, TextPresentation, ViewData, ViewPresentation,
};
use crate::protocol::frontend_event::{FrontendEvent, ResizeEvent};
use crate::protocol::ids::{ContentId, SpaceId, ViewId};
use crate::protocol::key_event::{ArrowKey, KeyCode, KeyEvent};
use crate::protocol::revision::Revision;
use crate::protocol::scene::Scene;
use crate::protocol::selection::{Selection, Selections, TextOffset, TextPoint};
use crate::protocol::space::{Sizing, SpaceKind, SplitDirection};
use crate::protocol::status::StatusMessage;
use crate::protocol::viewport::{ViewportCommand, ViewportCursorBehavior};
use std::collections::VecDeque;

struct ScriptedFrontend {
    events: VecDeque<FrontendEvent>,
    renders: usize,
    scene_revisions: Vec<Revision>,
    fail_next_event: bool,
    fail_render: bool,
    viewport_height: usize,
    viewport_commands: Vec<(ViewId, ViewportCommand)>,
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

#[derive(Clone)]
struct SharedContentState {
    executions: u8,
}

#[derive(Clone)]
struct SharedViewState {
    awaiting: bool,
}

struct ChainProbeMode {
    name: ModeName,
    actions: Vec<ModeActionName>,
    keymap: Keymap<Command>,
    effects: Vec<ModeEffect>,
    continue_input: bool,
}

struct HighlightMode {
    name: ModeName,
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

impl Mode for HighlightMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &[]
    }

    fn create_content_state(
        &self,
        context: &ModeContentContext<'_>,
    ) -> Result<Box<dyn ModeState>, ModeError> {
        assert_eq!(context.content_id(), editor_cid());
        assert_eq!(
            context.query_content(ContentQuery::Text),
            ContentData::Text(String::new())
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
                FaceName::new("syntax.test"),
                Face {
                    foreground: Some(Color::Rgb {
                        red: 1,
                        green: 2,
                        blue: 3,
                    }),
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
            ..ModeViewPolicy::default()
        }
    }

    fn decorations(
        &self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        _visible_rows: RowRange,
    ) -> Vec<NamedTextDecoration> {
        vec![NamedTextDecoration {
            start: TextOffset { char_index: 0 },
            end: TextOffset { char_index: 1 },
            face: FaceName::new("syntax.test"),
        }]
    }
}

impl Mode for FaultingHighlightMode {
    fn name(&self) -> &ModeName {
        &self.name
    }

    fn actions(&self) -> &[ModeActionName] {
        &[]
    }

    fn decorations(
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

    fn decorations(
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
        Ok(ModeResult::operations(vec![ModeEffect::DeferredEdit(
            EditCommand::InsertText(text.clone()),
        )]))
    }
}

impl ChainProbeMode {
    fn new(name: &str, effects: Vec<ModeEffect>, continue_input: bool) -> Self {
        Self::with_sequence(name, vec![KeyEvent::char('q')], effects, continue_input)
    }

    fn with_sequence(
        name: &str,
        sequence: Vec<KeyEvent>,
        effects: Vec<ModeEffect>,
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
            effects,
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

    fn keymap(&self, _state: &dyn ModeState, _context: &ModeViewContext<'_>) -> &Keymap<Command> {
        &self.keymap
    }

    fn execute_view(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        action: &ModeActionName,
    ) -> Result<ModeResult, ModeError> {
        if action != &self.actions[0] {
            return Err(ModeError::UnknownAction {
                mode: self.name.clone(),
                action: action.clone(),
            });
        }
        Ok(if self.continue_input {
            ModeResult::continue_with(self.effects.clone())
        } else {
            ModeResult::operations(self.effects.clone())
        })
    }
}

impl SharedContentMode {
    fn new() -> Self {
        let name = ModeName::new("shared-content");
        let actions = vec![ModeActionName::new("advance")];
        let mut keymap = Keymap::new();
        keymap.bind(
            KeyEvent::char('z'),
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

    fn action_scope(&self, _action: &ModeActionName) -> ModeActionScope {
        ModeActionScope::Content
    }

    fn new_content_state(&self) -> Box<dyn ModeState> {
        Box::new(SharedContentState { executions: 0 })
    }

    fn new_view_state(&self) -> Box<dyn ModeState> {
        Box::new(SharedViewState { awaiting: false })
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
    ) -> crate::core::input::InputStatus {
        if view_state
            .as_any()
            .downcast_ref::<SharedViewState>()
            .expect("shared mode owns its view state")
            .awaiting
        {
            crate::core::input::InputStatus::Awaiting(crate::core::input::TimeoutPolicy::Never)
        } else {
            crate::core::input::InputStatus::Ready
        }
    }

    fn input_capture(
        &self,
        _content_state: &mut dyn ModeState,
        view_state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> crate::core::input::InputDecision<Command> {
        if key != KeyEvent::char('x') {
            return crate::core::input::InputDecision::Pass;
        }
        view_state
            .as_any_mut()
            .downcast_mut::<SharedViewState>()
            .expect("shared mode owns its view state")
            .awaiting = true;
        crate::core::input::InputDecision::Consumed
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

    fn execute_content(
        &self,
        state: &mut dyn ModeState,
        context: &ModeContentContext<'_>,
        _action: &ModeActionName,
    ) -> Result<ModeResult, ModeError> {
        assert_eq!(context.content_id(), editor_cid());
        let count = state
            .as_any_mut()
            .downcast_mut::<SharedContentState>()
            .expect("shared mode owns its content state");
        count.executions += 1;
        Ok(ModeResult::operations(vec![match count.executions {
            1 => ModeEffect::Transaction(TransactionIntent::Undo),
            2 => ModeEffect::Transaction(TransactionIntent::Redo),
            _ => ModeEffect::Save,
        }]))
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
            KeyEvent::char('z'),
            Command::Mode(ModeCommand {
                mode: name.clone(),
                action: actions[0].clone(),
                arguments: Default::default(),
            }),
        );
        let mut nonempty_keymap = Keymap::new();
        nonempty_keymap.bind(KeyEvent::char('z'), Command::Noop);
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

    fn new_view_state(&self) -> Box<dyn ModeState> {
        Box::new(false)
    }

    fn keymap(&self, _state: &dyn ModeState, _context: &ModeViewContext<'_>) -> &Keymap<Command> {
        &self.keymap
    }

    fn typing(
        &self,
        _state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        _key: KeyEvent,
    ) -> Option<Command> {
        None
    }

    fn input_status(
        &self,
        state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> crate::core::input::InputStatus {
        if *state.as_any().downcast_ref::<bool>().unwrap() {
            crate::core::input::InputStatus::Ready
        } else {
            crate::core::input::InputStatus::Awaiting(crate::core::input::TimeoutPolicy::After(
                std::time::Duration::ZERO,
            ))
        }
    }

    fn capture(
        &self,
        state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        key: KeyEvent,
    ) -> crate::core::input::InputDecision<Command> {
        if key != KeyEvent::char('x') {
            return crate::core::input::InputDecision::Pass;
        }
        *state.as_any_mut().downcast_mut::<bool>().unwrap() = true;
        crate::core::input::InputDecision::Consumed
    }

    fn on_timeout(&self, state: &mut dyn ModeState, _context: &ModeViewContext<'_>) -> ModeResult {
        *state.as_any_mut().downcast_mut::<bool>().unwrap() = true;
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

    fn execute_view(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        action: &ModeActionName,
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

    fn action_scope(&self, _action: &ModeActionName) -> ModeActionScope {
        ModeActionScope::Content
    }

    fn new_content_state(&self) -> Box<dyn ModeState> {
        Box::new(())
    }

    fn input_keymap<'a>(
        &'a self,
        _content_state: &dyn ModeState,
        _view_state: &dyn ModeState,
        context: &ModeViewContext<'_>,
    ) -> &'a Keymap<Command> {
        match context.query_content(ContentQuery::TextRows(RowRange { start: 0, end: 1 })) {
            ContentData::TextRows(rows) if rows.first().is_some_and(String::is_empty) => {
                &self.empty_keymap
            }
            ContentData::TextRows(_) => &self.nonempty_keymap,
            _ => unreachable!("content-aware mode is bound to text content"),
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

    fn execute_content(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeContentContext<'_>,
        action: &ModeActionName,
    ) -> Result<ModeResult, ModeError> {
        if action != &self.actions[0] {
            return Err(ModeError::UnknownAction {
                mode: self.name.clone(),
                action: action.clone(),
            });
        }
        Ok(ModeResult::operations(vec![ModeEffect::Content(
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

    fn new_view_state(&self) -> Box<dyn ModeState> {
        Box::new(0_u8)
    }

    fn keymap(&self, _state: &dyn ModeState, context: &ModeViewContext<'_>) -> &Keymap<Command> {
        assert_eq!(context.content_id(), editor_cid());
        &self.keymap
    }

    fn typing(
        &self,
        _state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
        _key: KeyEvent,
    ) -> Option<Command> {
        None
    }

    fn input_status(
        &self,
        _state: &dyn ModeState,
        _context: &ModeViewContext<'_>,
    ) -> crate::core::input::InputStatus {
        crate::core::input::InputStatus::Awaiting(crate::core::input::TimeoutPolicy::After(
            std::time::Duration::ZERO,
        ))
    }

    fn capture(
        &self,
        state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
        _key: KeyEvent,
    ) -> crate::core::input::InputDecision<Command> {
        assert_eq!(context.view_id(), ViewId(0));
        *state
            .as_any_mut()
            .downcast_mut::<u8>()
            .expect("capture failure mode owns its state") = 1;
        crate::core::input::InputDecision::Emit(Command::Mode(ModeCommand {
            mode: ModeName::new("missing"),
            action: ModeActionName::new("missing"),
            arguments: Default::default(),
        }))
    }

    fn on_timeout(&self, state: &mut dyn ModeState, context: &ModeViewContext<'_>) -> ModeResult {
        assert_eq!(context.view_id(), ViewId(0));
        *state
            .as_any_mut()
            .downcast_mut::<u8>()
            .expect("capture failure mode owns its state") = 1;
        ModeResult::operations(vec![ModeEffect::Mode(ModeCommand {
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

    fn execute_view(
        &self,
        _state: &mut dyn ModeState,
        _context: &ModeViewContext<'_>,
        action: &ModeActionName,
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

    fn new_view_state(&self) -> Box<dyn ModeState> {
        Box::new(0_u16)
    }

    fn keymap(&self, _state: &dyn ModeState, _context: &ModeViewContext<'_>) -> &Keymap<Command> {
        &self.keymap
    }

    fn typing(
        &self,
        _state: &dyn ModeState,
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

    fn execute_view(
        &self,
        state: &mut dyn ModeState,
        context: &ModeViewContext<'_>,
        _action: &ModeActionName,
    ) -> Result<ModeResult, ModeError> {
        *state
            .as_any_mut()
            .downcast_mut::<u16>()
            .expect("loop mode owns its state") += 1;
        assert_eq!(context.content_id(), editor_cid());
        let _ = context.view_id();
        let _ = context.selections();
        let _ = context.query_content(ContentQuery::DocumentStatus);
        let ContentData::TextRows(rows) =
            context.query_content(ContentQuery::TextRows(RowRange { start: 0, end: 1 }))
        else {
            unreachable!("loop mode is bound to a text content")
        };
        let offset = rows[0].chars().count();
        let change = TextChangeSet::from_edits(offset, vec![TextEdit::new(offset..offset, "x")])
            .expect("loop mode creates a valid insertion");
        Ok(ModeResult::operations(vec![
            ModeEffect::Content(ContentAction::Text(change)),
            ModeEffect::Mode(ModeCommand {
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
            renders: 0,
            scene_revisions: Vec::new(),
            fail_next_event: false,
            fail_render: false,
            viewport_height: 4,
            viewport_commands: Vec::new(),
        }
    }
}

impl Frontend for ScriptedFrontend {
    async fn next_event(&mut self) -> io::Result<Option<FrontendEvent>> {
        if self.fail_next_event {
            self.fail_next_event = false;
            return Err(io::Error::other("scripted frontend failure"));
        }
        Ok(self.events.pop_front())
    }

    fn render(
        &mut self,
        _scene: &Scene,
        scene_revision: Revision,
        _query: &dyn RenderQuery,
        _focused: SpaceId,
    ) -> io::Result<()> {
        self.renders += 1;
        self.scene_revisions.push(scene_revision);
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
        _view: ViewId,
        command: ViewportCommand,
    ) -> io::Result<usize> {
        Ok(match command.amount {
            crate::protocol::viewport::ViewportMoveAmount::HalfPage => {
                (self.viewport_height / 2).max(1)
            }
            crate::protocol::viewport::ViewportMoveAmount::FullPage => self.viewport_height,
        })
    }

    fn apply_viewport_command(&mut self, view: ViewId, command: ViewportCommand, _lines: usize) {
        self.viewport_commands.push((view, command));
    }
}

fn make_app(events: Vec<FrontendEvent>, path: Option<&str>) -> App<ScriptedFrontend> {
    App::new(path, 40, 5, ScriptedFrontend::new(events)).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn rust_highlighting_is_parsed_and_updated_in_background() {
    let file = tempfile::Builder::new().suffix(".rs").tempfile().unwrap();
    std::fs::write(file.path(), "fn main() {}\n").unwrap();
    let mut app = make_app(vec![], file.path().to_str());
    let view = view_id(&app, app.session.focused());
    let revision_before = app.session.view(view).unwrap().revision();

    app.kernel.schedule_mode_jobs();
    let message = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        app.kernel.receive_message(),
    )
    .await
    .unwrap()
    .unwrap();
    app.handle_app_message(message).unwrap();

    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        view_modes: app.session.view_modes(),
        mode_contents: app.kernel.content_modes(),
        faces: app.session.faces(),
    };
    let decorations = query.decorations(view, RowRange { start: 0, end: 1 });
    assert!(decorations.iter().any(|decoration| {
        decoration.start.char_index == 0
            && decoration.end.char_index == 2
            && decoration.face.foreground == Some(Color::Ansi(204))
    }));
    assert!(app.session.view(view).unwrap().revision() > revision_before);

    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("// 中\n".to_string())),
        view,
        content: editor_cid(),
    })
    .unwrap();
    let message = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        app.kernel.receive_message(),
    )
    .await
    .unwrap()
    .unwrap();
    app.handle_app_message(message).unwrap();

    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        view_modes: app.session.view_modes(),
        mode_contents: app.kernel.content_modes(),
        faces: app.session.faces(),
    };
    let decorations = query.decorations(view, RowRange { start: 0, end: 1 });
    assert!(decorations.iter().any(|decoration| {
        decoration.start.char_index == 0
            && decoration.end.char_index == 4
            && decoration.face.foreground == Some(Color::Ansi(244))
            && decoration.face.italic == Some(true)
    }));
}

fn editor_cid() -> ContentId {
    ContentId(0)
}

fn view_id(app: &App<ScriptedFrontend>, space: SpaceId) -> ViewId {
    view_for_space(app.session.scene(), space).expect("space hosts a view")
}

fn view_at(app: &App<ScriptedFrontend>, space: SpaceId) -> &View {
    &app.session.views()[&view_id(app, space)]
}

fn replace_view_mode_for_test(
    app: &mut App<ScriptedFrontend>,
    view: ViewId,
    mode: ModeViewInstance,
) {
    let content = app.session.views()[&view].content();
    let removed = app.session.view_modes_mut_for_test().remove(view);
    let (contents, mode_contents) = app.kernel.mode_runtime_parts();
    let _ = contents;
    for mode_id in removed {
        mode_contents.detach_view(content, mode_id);
    }
    mode_contents.attach_view(content, &mode);
    app.session.view_modes_mut_for_test().insert(view, mode);
}

fn text_presentation(view: &ViewData) -> &TextPresentation {
    match &view.presentation {
        ViewPresentation::Text(text) => text,
        ViewPresentation::StatusBar => panic!("expected text presentation"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_sharing_one_kernel_keep_client_state_independent() {
    let mut app = make_app(vec![], None);
    let (contents, modes, mode_contents) = app.kernel.mode_attachment_parts();
    let mut second = create_editor_session(
        contents,
        modes,
        mode_contents,
        80,
        20,
        editor_cid(),
        ContentId(1),
    );
    let first_view = view_id(&app, app.session.focused());
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
fn production_content_paths_have_no_dynamic_type_probes() {
    let app = [
        include_str!("application.rs"),
        include_str!("kernel.rs"),
        include_str!("layout.rs"),
        include_str!("query.rs"),
        include_str!("runtime.rs"),
        include_str!("save.rs"),
    ]
    .concat();
    let content = include_str!("../core/content.rs");
    let content_view_state = include_str!("../core/content_view_state.rs");
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
    for concrete_content in ["Buffer", "StatusBar"] {
        assert!(!content_view_state.contains(concrete_content));
    }
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
) -> crate::protocol::selection::TextPoint {
    match app
        .kernel
        .contents()
        .query(content, ContentQuery::TextPoints(vec![offset]))
    {
        ContentData::TextPoints(mut points) => points.remove(0),
        _ => panic!("expected text point"),
    }
}

fn document_status(app: &App<ScriptedFrontend>, content: ContentId) -> DocumentStatus {
    match app
        .kernel
        .contents()
        .query(content, ContentQuery::DocumentStatus)
    {
        ContentData::DocumentStatus(status) => status,
        data => panic!("expected document status, got {data:?}"),
    }
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
        view_modes: app.session.view_modes(),
        mode_contents: app.kernel.content_modes(),
        faces: app.session.faces(),
    };
    assert_eq!(
        query.content(
            editor_cid(),
            ContentQuery::TextRows(RowRange { start: 0, end: 5 })
        ),
        ContentData::TextRows(vec!["hi".to_string()])
    );
    let view = query.view(focused_view);
    let text = text_presentation(&view);
    assert_eq!(text.selections.primary().head().char_index, 2);
    assert_eq!(text.cursor_style, CursorStyle::Block);
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
    let mode = {
        let modes = app.kernel.modes_mut();
        modes.register(LoopMode::new());
        modes.instantiate(&mode_name).unwrap()
    };
    let state = app
        .kernel
        .contents()
        .create_view_state(editor_cid())
        .unwrap();
    let focused = app.session.focused();
    let (contents, content_modes) = app.kernel.mode_runtime_parts();
    app.session
        .replace_space_content(
            focused,
            NewView {
                view: View::new(editor_cid(), state),
                modes: vec![mode],
            },
            true,
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
        view_modes: app.session.view_modes(),
        mode_contents: app.kernel.content_modes(),
        faces: app.session.faces(),
    };
    assert_eq!(
        text_presentation(&query.view(view)).cursor_style,
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
        .execute_command(DispatchCommand::ModeEffects {
            operations: vec![
                ModeEffect::Save,
                ModeEffect::Mode(ModeCommand {
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
        .execute_command(DispatchCommand::ModeEffects {
            operations: vec![
                ModeEffect::App(AppCommand::Quit),
                ModeEffect::Mode(ModeCommand {
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
        crate::protocol::viewport::ViewportMoveDirection::Down,
        crate::protocol::viewport::ViewportMoveAmount::HalfPage,
        ViewportCursorBehavior::Move,
    );

    let error = app
        .execute_command(DispatchCommand::ModeEffects {
            operations: vec![
                ModeEffect::Viewport(command),
                ModeEffect::Mode(ModeCommand {
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

    app.execute_command(DispatchCommand::ModeEffects {
        operations: vec![
            ModeEffect::Transaction(TransactionIntent::Undo),
            ModeEffect::DeferredEdit(EditCommand::InsertText("c".to_string())),
            ModeEffect::Mode(ModeCommand {
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
        modes.register(CaptureFailureMode::new());
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
        view_modes: app.session.view_modes(),
        mode_contents: app.kernel.content_modes(),
        faces: app.session.faces(),
    };
    assert_eq!(
        text_presentation(&query.view(view)).cursor_style,
        CursorStyle::Default
    );
}

#[test]
fn failed_timeout_output_restores_the_pre_timeout_mode_state() {
    let mut app = make_app(vec![], None);
    let mode = {
        let modes = app.kernel.modes_mut();
        modes.register(CaptureFailureMode::new());
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
        view_modes: app.session.view_modes(),
        mode_contents: app.kernel.content_modes(),
        faces: app.session.faces(),
    };
    assert_eq!(
        text_presentation(&query.view(view)).cursor_style,
        CursorStyle::Default
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mutable_view_mode_callbacks_advance_revision_after_success() {
    let setup = || {
        let mut app = make_app(vec![], None);
        let mode = {
            let modes = app.kernel.modes_mut();
            modes.register(PresentationMutationMode::new());
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

#[test]
fn status_bar_view_data_has_no_text_selection_or_mode_cursor() {
    let app = make_app(vec![], None);
    let status_view = app
        .session
        .views()
        .iter()
        .find_map(|(id, view)| (view.content() == ContentId(1)).then_some(*id))
        .expect("status bar view exists");
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        view_modes: app.session.view_modes(),
        mode_contents: app.kernel.content_modes(),
        faces: app.session.faces(),
    };

    let view = query.view(status_view);
    assert_eq!(view.presentation, ViewPresentation::StatusBar);
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
    let right = app
        .split_space(left, editor_cid(), true, SplitDirection::Right, true)
        .unwrap()
        .new_space;
    assert_eq!(app.session.focused(), right);
    assert!(app.session.views()[&left_id].revision() > left_revision);

    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        view_modes: app.session.view_modes(),
        mode_contents: app.kernel.content_modes(),
        faces: app.session.faces(),
    };
    let right_id = view_id(&app, right);
    let left_view = query.view(left_id);
    let right_view = query.view(right_id);
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
    assert_eq!(right_text.selections.primary().head(), TextOffset::origin());
}

#[tokio::test(flavor = "multi_thread")]
async fn content_mode_binding_is_shared_and_coexists_with_view_modes() {
    let mut app = make_app(vec![], None);
    let mode = ModeName::new("shared-content");
    app.kernel.modes_mut().register(SharedContentMode::new());
    let existing_view = view_id(&app, app.session.focused());
    let existing_revision = app.session.views()[&existing_view].revision();
    assert!(app.attach_mode_to_content(editor_cid(), &mode));
    assert!(app.session.views()[&existing_view].revision() > existing_revision);

    let left = app.session.focused();
    let right = app
        .split_space(left, editor_cid(), true, SplitDirection::Right, true)
        .unwrap()
        .new_space;
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        view_modes: app.session.view_modes(),
        mode_contents: app.kernel.content_modes(),
        faces: app.session.faces(),
    };
    for space in [left, right] {
        let view = query.view(view_id(&app, space));
        assert_eq!(text_presentation(&view).cursor_style, CursorStyle::Block);
    }

    let command = ModeCommand {
        mode: mode.clone(),
        action: ModeActionName::new("advance"),
        arguments: Default::default(),
    };
    app.handle_event(FrontendEvent::Key(KeyEvent::char('z')))
        .await
        .unwrap();
    assert_eq!(
        app.kernel
            .execute_mode_content_action(editor_cid(), &command)
            .unwrap(),
        ModeResult::operations(vec![ModeEffect::Transaction(TransactionIntent::Redo)])
    );

    app.close_space(left).unwrap();
    assert_eq!(
        app.kernel
            .execute_mode_content_action(editor_cid(), &command)
            .unwrap(),
        ModeResult::operations(vec![ModeEffect::Save])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn content_mode_keymap_tracks_current_content() {
    let mut app = make_app(vec![], None);
    let mode = ModeName::new("content-aware-keymap");
    app.kernel
        .modes_mut()
        .register(ContentAwareKeymapMode::new());
    assert!(app.attach_mode_to_content(editor_cid(), &mode));

    app.handle_event(FrontendEvent::Key(KeyEvent::char('z')))
        .await
        .unwrap();
    app.handle_event(FrontendEvent::Key(KeyEvent::char('z')))
        .await
        .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["a"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn mode_can_handle_input_then_continue_to_the_next_mode() {
    let mut app = make_app(vec![], None);
    app.kernel.modes_mut().register(ChainProbeMode::new(
        "first-probe",
        vec![ModeEffect::DeferredEdit(EditCommand::InsertText(
            "a".to_string(),
        ))],
        true,
    ));
    app.kernel.modes_mut().register(ChainProbeMode::new(
        "second-probe",
        vec![ModeEffect::DeferredEdit(EditCommand::InsertText(
            "b".to_string(),
        ))],
        false,
    ));
    assert!(app.attach_mode_to_content(editor_cid(), &ModeName::new("first-probe")));
    assert!(app.attach_mode_to_content(editor_cid(), &ModeName::new("second-probe")));

    app.handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();

    assert_eq!(text_rows(&app, editor_cid()), vec!["ab"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn later_mode_prefix_does_not_delay_an_earlier_exact_binding() {
    let mut app = make_app(vec![], None);
    app.kernel.modes_mut().register(ChainProbeMode::new(
        "first-probe",
        vec![ModeEffect::DeferredEdit(EditCommand::InsertText(
            "a".to_string(),
        ))],
        true,
    ));
    app.kernel
        .modes_mut()
        .register(ChainProbeMode::with_sequence(
            "second-probe",
            vec![KeyEvent::char('q'), KeyEvent::char('z')],
            vec![ModeEffect::DeferredEdit(EditCommand::InsertText(
                "b".to_string(),
            ))],
            false,
        ));
    assert!(app.attach_mode_to_content(editor_cid(), &ModeName::new("first-probe")));
    assert!(app.attach_mode_to_content(editor_cid(), &ModeName::new("second-probe")));

    app.handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["a"]);

    app.handle_event(FrontendEvent::Key(KeyEvent::char('z')))
        .await
        .unwrap();
    assert_eq!(text_rows(&app, editor_cid()), vec!["ba"]);
}

#[test]
fn mode_decorations_are_resolved_through_named_faces() {
    let mut app = make_app(vec![], None);
    app.kernel.modes_mut().register(HighlightMode {
        name: ModeName::new("highlight-probe"),
    });
    assert!(app.attach_mode_to_content(editor_cid(), &ModeName::new("highlight-probe"),));
    let view = view_id(&app, app.session.focused());
    let query = AppQuery {
        contents: app.kernel.contents(),
        views: app.session.views(),
        view_modes: app.session.view_modes(),
        mode_contents: app.kernel.content_modes(),
        faces: app.session.faces(),
    };

    let view_data = query.view(view);
    let presentation = text_presentation(&view_data);
    let decorations = query.decorations(view, RowRange { start: 0, end: 1 });

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
}

#[test]
fn passive_mode_failure_does_not_rollback_text_and_suspends_presentation() {
    let mut app = make_app(vec![], None);
    let mode = ModeName::new("faulting-highlight");
    app.kernel
        .modes_mut()
        .register(FaultingHighlightMode { name: mode.clone() });
    assert!(app.attach_mode_to_content(editor_cid(), &mode));
    let view = view_id(&app, app.session.focused());
    {
        let query = AppQuery {
            contents: app.kernel.contents(),
            views: app.session.views(),
            view_modes: app.session.view_modes(),
            mode_contents: app.kernel.content_modes(),
            faces: app.session.faces(),
        };
        assert_eq!(
            query.decorations(view, RowRange { start: 0, end: 1 }).len(),
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
        view_modes: app.session.view_modes(),
        mode_contents: app.kernel.content_modes(),
        faces: app.session.faces(),
    };
    assert!(
        query
            .decorations(view, RowRange { start: 0, end: 1 })
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
        app.kernel.modes_mut().register(FactoryFaultMode {
            name: mode.clone(),
            fail_content,
        });
        assert!(app.attach_mode_to_content(editor_cid(), &mode));
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
        view_modes: app.session.view_modes(),
        mode_contents: app.kernel.content_modes(),
        faces: app.session.faces(),
    };
    assert!(
        query
            .decorations(view, RowRange { start: 0, end: 1 })
            .is_empty()
    );
}

#[test]
fn mode_command_delivers_owned_language_neutral_arguments() {
    let mut app = make_app(vec![], None);
    let mode = ModeName::new("argument-probe");
    let action = ModeActionName::new("insert");
    app.kernel.modes_mut().register(ArgumentProbeMode {
        name: mode.clone(),
        actions: vec![action.clone()],
    });
    assert!(app.attach_mode_to_content(editor_cid(), &mode));
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
    app.kernel.modes_mut().register(ChainProbeMode::new(
        "first-probe",
        vec![ModeEffect::DeferredEdit(EditCommand::InsertText(
            "a".to_string(),
        ))],
        true,
    ));
    app.kernel.modes_mut().register(ChainProbeMode::new(
        "failing-probe",
        vec![ModeEffect::Mode(ModeCommand {
            mode: ModeName::new("missing"),
            action: ModeActionName::new("run"),
            arguments: Default::default(),
        })],
        false,
    ));
    assert!(app.attach_mode_to_content(editor_cid(), &ModeName::new("first-probe")));
    assert!(app.attach_mode_to_content(editor_cid(), &ModeName::new("failing-probe")));

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
            vec![KeyEvent::char('q'), KeyEvent::char('z')],
            vec![ModeEffect::Mode(ModeCommand::new(
                ModeName::new("missing"),
                ModeActionName::new("run"),
            ))],
            false,
        ));
    assert!(app.attach_mode_to_content(editor_cid(), &ModeName::new("failing-sequence"),));

    app.handle_event(FrontendEvent::Key(KeyEvent::char('q')))
        .await
        .unwrap();
    assert!(app.session.input_is_pending_for_test());

    let error = app
        .handle_event(FrontendEvent::Key(KeyEvent::char('z')))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("unknown mode 'missing'"));
    assert!(app.session.input_is_pending_for_test());
}

#[tokio::test(flavor = "multi_thread")]
async fn mode_input_state_is_per_view_while_content_state_is_shared() {
    let mut app = make_app(vec![], None);
    let mode = ModeName::new("shared-content");
    app.kernel.modes_mut().register(SharedContentMode::new());
    assert!(app.attach_mode_to_content(editor_cid(), &mode));

    let left = app.session.focused();
    let right = app
        .split_space(left, editor_cid(), true, SplitDirection::Right, true)
        .unwrap()
        .new_space;
    app.handle_event(FrontendEvent::Key(KeyEvent::char('z')))
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
        ModeResult::operations(vec![ModeEffect::Transaction(TransactionIntent::Redo)])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn leaving_content_cancels_view_input_without_resetting_content_state() {
    let mut app = make_app(vec![], None);
    let mode = ModeName::new("shared-content");
    app.kernel.modes_mut().register(SharedContentMode::new());
    assert!(app.attach_mode_to_content(editor_cid(), &mode));
    app.handle_event(FrontendEvent::Key(KeyEvent::char('z')))
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
        ModeResult::operations(vec![ModeEffect::Transaction(TransactionIntent::Redo)])
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
    assert!(matches!(
        app.kernel
            .contents()
            .query(editor_cid(), ContentQuery::DocumentStatus),
        ContentData::DocumentStatus(DocumentStatus {
            modified: false,
            message: StatusMessage::Saved,
            ..
        }),
    ));
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
    // 绑 'z' 前缀 + 's' → Save（覆盖 Ctrl+S 测试前缀路径）
    let mut app = make_app(
        vec![
            FrontendEvent::Key(KeyEvent::char('z')),
            FrontendEvent::Key(KeyEvent::char('z')),
            FrontendEvent::Key(KeyEvent::ctrl('q')),
        ],
        Some(&path_str),
    );
    let mut global = default_global_keymap();
    global.bind(
        [KeyEvent::char('z'), KeyEvent::char('z')],
        Command::Content(ContentCommand::Save),
    );
    app.session
        .replace_dispatcher_for_test(Dispatcher::new(global));
    app.run().await.unwrap();
    assert_eq!(
        document_status(&app, editor_cid()).message,
        StatusMessage::Saved
    );
}

#[test]
fn save_completed_ok_marks_buffer_saved() {
    let mut app = make_app(vec![], None);
    app.execute_command(DispatchCommand::ContentWithView {
        command: ContentCommand::Edit(EditCommand::InsertText("x".to_string())),
        view: view_id(&app, app.session.focused()),
        content: editor_cid(),
    })
    .unwrap();
    assert!(document_status(&app, editor_cid()).modified);
    app.kernel.track_pending_save_for_test(
        editor_cid(),
        1,
        crate::core::transaction::TextStateId(1),
        None,
    );

    app.handle_app_message(AppMessage::SaveCompleted {
        content: editor_cid(),
        revision: 1,
        state: crate::core::transaction::TextStateId(1),
        result: Ok(()),
    })
    .unwrap();

    assert!(!app.kernel.has_pending_save(editor_cid()));
    let status = document_status(&app, editor_cid());
    assert!(!status.modified);
    assert_eq!(status.message, StatusMessage::Saved);
}

#[test]
fn save_completed_err_marks_buffer_save_failed() {
    let mut app = make_app(vec![], None);
    app.kernel.track_pending_save_for_test(
        editor_cid(),
        0,
        crate::core::transaction::TextStateId(0),
        None,
    );

    app.handle_app_message(AppMessage::SaveCompleted {
        content: editor_cid(),
        revision: 0,
        state: crate::core::transaction::TextStateId(0),
        result: Err(io::Error::other("boom")),
    })
    .unwrap();

    assert!(!app.kernel.has_pending_save(editor_cid()));
    assert_eq!(
        document_status(&app, editor_cid()).message,
        StatusMessage::SaveFailed
    );
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
    assert!(document_status(&app, editor_cid()).modified);
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
    let status = document_status(&app, editor_cid());
    assert!(!status.modified);
    assert_eq!(status.message, StatusMessage::Saved);
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
    other.insert_char(0, 'X');
    app.kernel
        .contents_mut()
        .insert(other_cid, Content::Buffer(other))
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
    assert_eq!(
        document_status(&app, editor_cid()).message,
        StatusMessage::Saved
    );
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
            ViewportCommand::new(
                crate::protocol::viewport::ViewportMoveDirection::Down,
                crate::protocol::viewport::ViewportMoveAmount::HalfPage,
                ViewportCursorBehavior::Extend,
            ),
        )
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
    *app.session
        .view_mut(view)
        .unwrap()
        .state_mut()
        .selections_mut()
        .unwrap() = Selections::single(Selection::collapsed(TextOffset { char_index: 1 }));

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
    *app.session
        .view_mut(left_view)
        .unwrap()
        .state_mut()
        .selections_mut()
        .unwrap() = Selections::single(Selection {
        anchor: TextOffset::origin(),
        head: TextOffset { char_index: 3 },
    });
    *app.session
        .view_mut(right_view)
        .unwrap()
        .state_mut()
        .selections_mut()
        .unwrap() = Selections::single(Selection::collapsed(TextOffset { char_index: 3 }));

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
        *app.session
            .view_mut(view)
            .unwrap()
            .state_mut()
            .selections_mut()
            .unwrap() = Selections::single(Selection::collapsed(TextOffset { char_index: 1 }));
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
    *app.session
        .view_mut(right_view)
        .unwrap()
        .state_mut()
        .selections_mut()
        .unwrap() = Selections::single(Selection::collapsed(TextOffset { char_index: 3 }));

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

    app.execute_command(DispatchCommand::ModeContentEffects {
        effects: vec![ModeEffect::Content(ContentAction::Text(change))],
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

    app.execute_command(DispatchCommand::ModeEffects {
        operations: vec![ModeEffect::Content(ContentAction::Text(change))],
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
fn mode_operations_reject_invalid_or_stale_view_state() {
    let mut invalid = make_app(vec![], None);
    let invalid_view = view_id(&invalid, invalid.session.focused());
    let error = invalid
        .execute_command(DispatchCommand::ModeEffects {
            operations: vec![ModeEffect::View(ViewAction::SetSelections(
                Selections::single(Selection::collapsed(TextOffset { char_index: 99 })),
            ))],
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
        .execute_command(DispatchCommand::ModeEffects {
            operations: vec![ModeEffect::Edit(ResolvedViewEdit {
                content: None,
                view: Some(ViewAction::SetSelections(Selections::single(
                    Selection::collapsed(TextOffset::origin()),
                ))),
                before: Selections::single(Selection::collapsed(TextOffset::origin())),
            })],
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
    undo.execute_command(DispatchCommand::ModeEffects {
        operations: vec![
            ModeEffect::Transaction(TransactionIntent::Undo),
            ModeEffect::DeferredEdit(EditCommand::InsertText("b".to_string())),
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
    redo.execute_command(DispatchCommand::ModeEffects {
        operations: vec![
            ModeEffect::Transaction(TransactionIntent::Redo),
            ModeEffect::DeferredEdit(EditCommand::InsertText("b".to_string())),
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
        .execute_command(DispatchCommand::ModeEffects {
            operations: vec![
                ModeEffect::Transaction(TransactionIntent::Rollback),
                ModeEffect::DeferredEdit(EditCommand::InsertText("c".to_string())),
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
