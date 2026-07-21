use std::cell::RefCell;
use std::rc::Rc;

use vell_core::action::ContentAction;
use vell_core::command::{CharSearchDirection, EditCommand};
use vell_core::content::ContentKind;
use vell_core::motion::{OperatorCommand, TextMotion, TextOperator, TextTarget};
use vell_core::text_snapshot::TextSnapshot;
use vell_core::transaction::{TextChangeSet, TextEdit};
use vell_mode::ModeViewContext;
use vell_mode::command::{AppCommand, ModeCommand, ModeValue};
use vell_mode::mode_name::{ModeActionName, ModeName};
use vell_mode::operation::{
    AppOperation, ContentOperation, ContentTarget, ModeFlowPropagation, ModeInvocation, ModeTarget,
    OperationRequest, ViewOperation, ViewTarget,
};
use vell_protocol::viewport::{
    ViewportAlignment, ViewportCommand, ViewportCursorBehavior, ViewportMoveAmount,
    ViewportMoveDirection,
};

use super::{
    MAX_SCRIPT_OPERATIONS, ScriptError, ensure_count, json_to_mode_value, parse_position, property,
    required_object, required_string, set_number, set_object, throw_script_error, v8_to_json,
};

const OPCODE_BITS: u32 = 8;
const OPCODE_MASK: u64 = (1 << OPCODE_BITS) - 1;
const MAX_INVOCATION_ID: u64 = (1 << (53 - OPCODE_BITS)) - 1;

macro_rules! primitives {
    ($( $variant:ident => ($namespace:literal, $name:literal) ),+ $(,)?) => {
        #[repr(u8)]
        #[derive(Clone, Copy)]
        enum Primitive {
            $( $variant, )+
        }

        const PRIMITIVES: &[(Primitive, &str, &str)] = &[
            $( (Primitive::$variant, $namespace, $name), )+
        ];

        impl Primitive {
            fn from_code(code: u8) -> Option<Self> {
                match code {
                    $( value if value == Self::$variant as u8 => Some(Self::$variant), )+
                    _ => None,
                }
            }
        }
    };
}

primitives! {
    MoveLeft => ("cursor", "moveLeft"),
    MoveRight => ("cursor", "moveRight"),
    MoveWithinLineLeft => ("cursor", "moveWithinLineLeft"),
    MoveWithinLineRight => ("cursor", "moveWithinLineRight"),
    MoveUp => ("cursor", "moveUp"),
    MoveDown => ("cursor", "moveDown"),
    MoveToLine => ("cursor", "moveToLine"),
    MoveToLinePreservingColumn => ("cursor", "moveToLinePreservingColumn"),
    MoveToCharForward => ("cursor", "moveToCharForward"),
    MoveToCharBackward => ("cursor", "moveToCharBackward"),
    ExtendLeft => ("cursor", "extendLeft"),
    ExtendRight => ("cursor", "extendRight"),
    ExtendWithinLineLeft => ("cursor", "extendWithinLineLeft"),
    ExtendWithinLineRight => ("cursor", "extendWithinLineRight"),
    ExtendUp => ("cursor", "extendUp"),
    ExtendDown => ("cursor", "extendDown"),
    ExtendToLine => ("cursor", "extendToLine"),
    ExtendToLinePreservingColumn => ("cursor", "extendToLinePreservingColumn"),
    ExtendToCharForward => ("cursor", "extendToCharForward"),
    ExtendToCharBackward => ("cursor", "extendToCharBackward"),
    MoveWordForward => ("cursor", "moveWordForward"),
    MoveWordBackward => ("cursor", "moveWordBackward"),
    MoveWordEnd => ("cursor", "moveWordEnd"),
    ExtendWordForward => ("cursor", "extendWordForward"),
    ExtendWordBackward => ("cursor", "extendWordBackward"),
    ExtendWordEnd => ("cursor", "extendWordEnd"),
    MoveToLineStart => ("cursor", "moveToLineStart"),
    MoveToFirstNonBlank => ("cursor", "moveToFirstNonBlank"),
    MoveToLineEnd => ("cursor", "moveToLineEnd"),
    MoveToLastLine => ("cursor", "moveToLastLine"),
    MoveToPrevParagraph => ("cursor", "moveToPrevParagraph"),
    MoveToNextParagraph => ("cursor", "moveToNextParagraph"),
    ExtendToLineStart => ("cursor", "extendToLineStart"),
    ExtendToFirstNonBlank => ("cursor", "extendToFirstNonBlank"),
    ExtendToLineEnd => ("cursor", "extendToLineEnd"),
    ExtendToLastLine => ("cursor", "extendToLastLine"),
    ExtendToPrevParagraph => ("cursor", "extendToPrevParagraph"),
    ExtendToNextParagraph => ("cursor", "extendToNextParagraph"),
    MoveAfterLineEnd => ("cursor", "moveAfterLineEnd"),
    CollapseSelections => ("cursor", "collapseSelections"),
    Insert => ("text", "insert"),
    DeleteBackward => ("text", "deleteBackward"),
    DeleteForward => ("text", "deleteForward"),
    DeleteWordBackward => ("text", "deleteWordBackward"),
    DeleteToLineStart => ("text", "deleteToLineStart"),
    DeleteToLineEnd => ("text", "deleteToLineEnd"),
    JoinLines => ("text", "joinLines"),
    ToggleCase => ("text", "toggleCase"),
    InsertLineBelow => ("text", "insertLineBelow"),
    InsertLineAbove => ("text", "insertLineAbove"),
    DeleteLineContent => ("text", "deleteLineContent"),
    DeleteSelectionInclusive => ("text", "deleteSelectionInclusive"),
    DeleteSelectedLines => ("text", "deleteSelectedLines"),
    DeleteWordMotion => ("text", "deleteWordMotion"),
    DeleteWordEndMotion => ("text", "deleteWordEndMotion"),
    ChangeWordMotion => ("text", "changeWordMotion"),
    DeleteToLineStartMotion => ("text", "deleteToLineStartMotion"),
    DeleteToLineEndMotion => ("text", "deleteToLineEndMotion"),
    DeleteLines => ("text", "deleteLines"),
    ChangeLines => ("text", "changeLines"),
    ApplyEdits => ("text", "applyEdits"),
    Begin => ("history", "begin"),
    Commit => ("history", "commit"),
    Rollback => ("history", "rollback"),
    Undo => ("history", "undo"),
    Redo => ("history", "redo"),
    HalfPageUp => ("viewport", "halfPageUp"),
    HalfPageDown => ("viewport", "halfPageDown"),
    FullPageUp => ("viewport", "fullPageUp"),
    FullPageDown => ("viewport", "fullPageDown"),
    AlignTop => ("viewport", "alignTop"),
    AlignCenter => ("viewport", "alignCenter"),
    AlignBottom => ("viewport", "alignBottom"),
    InvokeMode => ("mode", "invoke"),
    InvokeCommand => ("commands", "invoke"),
    Save => ("app", "save"),
    Quit => ("app", "quit"),
}

struct PrimitiveInvocation {
    id: u64,
    snapshot: Option<TextSnapshot>,
    effects: Vec<OperationRequest>,
}

pub(super) struct PrimitiveRuntime {
    next_id: u64,
    current: Option<PrimitiveInvocation>,
}

impl PrimitiveRuntime {
    pub(super) fn new() -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            next_id: 1,
            current: None,
        }))
    }

    pub(super) fn begin(&mut self, context: &ModeViewContext<'_>) -> Result<u64, ScriptError> {
        if self.current.is_some() {
            return Err(ScriptError::new("nested script actions are not supported"));
        }
        let id = self.next_id;
        self.next_id = if id == MAX_INVOCATION_ID { 1 } else { id + 1 };
        self.current = Some(PrimitiveInvocation {
            id,
            snapshot: context.buffer().and_then(|context| context.text_snapshot()),
            effects: Vec::new(),
        });
        Ok(id)
    }

    pub(super) fn finish(&mut self, id: u64) -> Result<Vec<OperationRequest>, ScriptError> {
        let invocation = self
            .current
            .take()
            .ok_or_else(|| ScriptError::new("script primitive invocation is not active"))?;
        if invocation.id != id {
            return Err(ScriptError::new(
                "script primitive invocation changed unexpectedly",
            ));
        }
        Ok(invocation.effects)
    }
}

pub(super) fn install_v1(
    scope: &mut v8::PinScope<'_, '_>,
    context: v8::Local<v8::Object>,
    invocation_id: u64,
) {
    for namespace in ["cursor", "text", "history", "viewport", "mode", "app"] {
        let object = v8::Object::new(scope);
        for &(primitive, primitive_namespace, name) in PRIMITIVES {
            if primitive_namespace == namespace {
                let encoded = encode(invocation_id, primitive as u8);
                let data = v8::Number::new(scope, encoded as f64);
                let function = v8::Function::builder(call_primitive)
                    .data(data.into())
                    .build(scope)
                    .expect("primitive function");
                let name = v8::String::new(scope, name).expect("primitive name");
                object.set(scope, name.into(), function.into());
            }
        }
        set_object(scope, context, namespace, object);
    }

    set_flow_function(scope, context, "handled", invocation_id, false);
    set_flow_function(scope, context, "forward", invocation_id, true);
}

pub(super) fn install_v2(
    scope: &mut v8::PinScope<'_, '_>,
    context: v8::Local<v8::Object>,
    invocation_id: u64,
    kind: ContentKind,
) -> v8::Global<v8::Object> {
    let namespaces: &[(&str, &str)] = match kind {
        ContentKind::Buffer => &[
            ("cursor", "cursor"),
            ("text", "edit"),
            ("history", "history"),
            ("viewport", "viewport"),
            ("commands", "commands"),
            ("app", "app"),
        ],
        ContentKind::StatusBar => &[("commands", "commands")],
    };
    for &(primitive_namespace, context_namespace) in namespaces {
        let object = v8::Object::new(scope);
        for &(primitive, candidate, name) in PRIMITIVES {
            if candidate == primitive_namespace {
                let encoded = encode(invocation_id, primitive as u8);
                let data = v8::Number::new(scope, encoded as f64);
                let function = v8::Function::builder(call_primitive)
                    .data(data.into())
                    .build(scope)
                    .expect("primitive function");
                let name = v8::String::new(scope, name).expect("primitive name");
                object.set(scope, name.into(), function.into());
            }
        }
        set_object(scope, context, context_namespace, object);
    }

    let sentinel = v8::Object::new(scope);
    let data = v8::Object::new(scope);
    set_number(scope, data, "invocationId", invocation_id as f64);
    let sentinel_name = v8::String::new(scope, "sentinel").unwrap();
    data.set(scope, sentinel_name.into(), sentinel.into());
    let pass = v8::Function::builder(action_pass)
        .data(data.into())
        .build(scope)
        .expect("pass function");
    let pass_name = v8::String::new(scope, "pass").unwrap();
    context.set(scope, pass_name.into(), pass.into());
    v8::Global::new(scope, sentinel)
}

fn set_flow_function(
    scope: &mut v8::PinScope<'_, '_>,
    context: v8::Local<v8::Object>,
    name: &str,
    invocation_id: u64,
    forward: bool,
) {
    let encoded = invocation_id * 2 + u64::from(forward);
    let data = v8::Number::new(scope, encoded as f64);
    let function = v8::Function::builder(action_flow)
        .data(data.into())
        .build(scope)
        .expect("flow function");
    let name = v8::String::new(scope, name).expect("flow name");
    context.set(scope, name.into(), function.into());
}

fn encode(invocation_id: u64, primitive: u8) -> u64 {
    invocation_id << OPCODE_BITS | u64::from(primitive)
}

fn active_runtime(
    scope: &mut v8::PinScope,
    invocation_id: u64,
) -> Result<Rc<RefCell<PrimitiveRuntime>>, ScriptError> {
    let runtime = scope
        .get_slot::<Rc<RefCell<PrimitiveRuntime>>>()
        .cloned()
        .ok_or_else(|| ScriptError::new("script primitive runtime is unavailable"))?;
    let active = runtime
        .borrow()
        .current
        .as_ref()
        .is_some_and(|current| current.id == invocation_id);
    if !active {
        return Err(ScriptError::new(
            "script primitives may only be called by their current action",
        ));
    }
    Ok(runtime)
}

fn action_flow(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let Some(encoded) = callback_data(scope, &arguments) else {
        return;
    };
    let invocation_id = encoded / 2;
    if let Err(error) = active_runtime(scope, invocation_id) {
        throw_script_error(scope, &error.to_string());
        return;
    }
    return_value.set(v8::Boolean::new(scope, encoded % 2 == 1).into());
}

fn action_pass(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let Ok(data) = v8::Local::<v8::Object>::try_from(arguments.data()) else {
        throw_script_error(scope, "invalid script pass binding");
        return;
    };
    let Some(invocation_id) = property(scope, data, "invocationId")
        .and_then(|value| value.integer_value(scope))
        .and_then(|value| u64::try_from(value).ok())
    else {
        throw_script_error(scope, "invalid script pass invocation");
        return;
    };
    if let Err(error) = active_runtime(scope, invocation_id) {
        throw_script_error(scope, &error.to_string());
        return;
    }
    let Some(sentinel) = property(scope, data, "sentinel") else {
        throw_script_error(scope, "script pass sentinel is unavailable");
        return;
    };
    return_value.set(sentinel);
}

fn call_primitive(
    scope: &mut v8::PinScope,
    arguments: v8::FunctionCallbackArguments,
    mut return_value: v8::ReturnValue,
) {
    let Some(encoded) = callback_data(scope, &arguments) else {
        return;
    };
    let invocation_id = encoded >> OPCODE_BITS;
    let Some(primitive) = Primitive::from_code((encoded & OPCODE_MASK) as u8) else {
        throw_script_error(scope, "unknown script primitive");
        return;
    };
    let runtime = match active_runtime(scope, invocation_id) {
        Ok(runtime) => runtime,
        Err(error) => {
            throw_script_error(scope, &error.to_string());
            return;
        }
    };
    match primitive_effects(scope, &arguments, primitive, &runtime) {
        Ok(effects) => {
            let mut runtime = runtime.borrow_mut();
            let invocation = runtime.current.as_mut().expect("active invocation");
            if let Err(error) = ensure_count(
                "operations",
                invocation.effects.len().saturating_add(effects.len()),
                MAX_SCRIPT_OPERATIONS,
            ) {
                throw_script_error(scope, &error.to_string());
                return;
            }
            invocation.effects.extend(effects);
            return_value.set_undefined();
        }
        Err(error) => throw_script_error(scope, &error.to_string()),
    }
}

fn callback_data(
    scope: &mut v8::PinScope,
    arguments: &v8::FunctionCallbackArguments,
) -> Option<u64> {
    arguments
        .data()
        .integer_value(scope)
        .and_then(|value| u64::try_from(value).ok())
        .or_else(|| {
            throw_script_error(scope, "invalid script primitive binding");
            None
        })
}

fn primitive_effects(
    scope: &mut v8::PinScope,
    arguments: &v8::FunctionCallbackArguments,
    primitive: Primitive,
    runtime: &Rc<RefCell<PrimitiveRuntime>>,
) -> Result<Vec<OperationRequest>, ScriptError> {
    use Primitive::*;

    let deferred = |command| {
        vec![OperationRequest::View {
            target: ViewTarget::Current,
            operation: ViewOperation::Edit(command),
        }]
    };
    let repeated = |command: EditCommand, count: usize| {
        (0..count)
            .map(|_| OperationRequest::View {
                target: ViewTarget::Current,
                operation: ViewOperation::Edit(command.clone()),
            })
            .collect()
    };
    Ok(match primitive {
        MoveLeft => deferred(EditCommand::MoveLeftBy(count(scope, arguments, 0)?)),
        MoveRight => deferred(EditCommand::MoveRightBy(count(scope, arguments, 0)?)),
        MoveWithinLineLeft => deferred(EditCommand::MoveWithinLineLeftBy(count(
            scope, arguments, 0,
        )?)),
        MoveWithinLineRight => deferred(EditCommand::MoveWithinLineRightBy(count(
            scope, arguments, 0,
        )?)),
        MoveUp => deferred(EditCommand::MoveUpBy(count(scope, arguments, 0)?)),
        MoveDown => deferred(EditCommand::MoveDownBy(count(scope, arguments, 0)?)),
        MoveToLine => deferred(EditCommand::MoveToLine {
            line_index: non_negative_integer(scope, arguments.get(0), "line")?,
        }),
        MoveToLinePreservingColumn => deferred(EditCommand::MoveToLinePreservingColumn {
            line_index: non_negative_integer(scope, arguments.get(0), "line")?,
        }),
        MoveToCharForward => deferred(EditCommand::MoveToChar {
            target: character(scope, arguments.get(0), "character")?,
            direction: CharSearchDirection::Forward,
            occurrence: count(scope, arguments, 1)?,
        }),
        MoveToCharBackward => deferred(EditCommand::MoveToChar {
            target: character(scope, arguments.get(0), "character")?,
            direction: CharSearchDirection::Backward,
            occurrence: count(scope, arguments, 1)?,
        }),
        ExtendLeft => deferred(EditCommand::ExtendLeftBy(count(scope, arguments, 0)?)),
        ExtendRight => deferred(EditCommand::ExtendRightBy(count(scope, arguments, 0)?)),
        ExtendWithinLineLeft => deferred(EditCommand::ExtendWithinLineLeftBy(count(
            scope, arguments, 0,
        )?)),
        ExtendWithinLineRight => deferred(EditCommand::ExtendWithinLineRightBy(count(
            scope, arguments, 0,
        )?)),
        ExtendUp => deferred(EditCommand::ExtendUpBy(count(scope, arguments, 0)?)),
        ExtendDown => deferred(EditCommand::ExtendDownBy(count(scope, arguments, 0)?)),
        ExtendToLine => deferred(EditCommand::ExtendToLine {
            line_index: non_negative_integer(scope, arguments.get(0), "line")?,
        }),
        ExtendToLinePreservingColumn => deferred(EditCommand::ExtendToLinePreservingColumn {
            line_index: non_negative_integer(scope, arguments.get(0), "line")?,
        }),
        ExtendToCharForward => deferred(EditCommand::ExtendToChar {
            target: character(scope, arguments.get(0), "character")?,
            direction: CharSearchDirection::Forward,
            occurrence: count(scope, arguments, 1)?,
        }),
        ExtendToCharBackward => deferred(EditCommand::ExtendToChar {
            target: character(scope, arguments.get(0), "character")?,
            direction: CharSearchDirection::Backward,
            occurrence: count(scope, arguments, 1)?,
        }),
        MoveWordForward => repeated(EditCommand::MoveWordForward, count(scope, arguments, 0)?),
        MoveWordBackward => repeated(EditCommand::MoveWordBackward, count(scope, arguments, 0)?),
        MoveWordEnd => repeated(EditCommand::MoveWordEnd, count(scope, arguments, 0)?),
        ExtendWordForward => repeated(EditCommand::ExtendWordForward, count(scope, arguments, 0)?),
        ExtendWordBackward => {
            repeated(EditCommand::ExtendWordBackward, count(scope, arguments, 0)?)
        }
        ExtendWordEnd => repeated(EditCommand::ExtendWordEnd, count(scope, arguments, 0)?),
        MoveToLineStart => deferred(EditCommand::MoveToLineStart),
        MoveToFirstNonBlank => deferred(EditCommand::MoveToFirstNonBlank),
        MoveToLineEnd => deferred(EditCommand::MoveToLineEnd),
        MoveToLastLine => deferred(EditCommand::MoveToLastLine),
        MoveToPrevParagraph => repeated(
            EditCommand::MoveToPrevParagraph,
            count(scope, arguments, 0)?,
        ),
        MoveToNextParagraph => repeated(
            EditCommand::MoveToNextParagraph,
            count(scope, arguments, 0)?,
        ),
        ExtendToLineStart => deferred(EditCommand::ExtendToLineStart),
        ExtendToFirstNonBlank => deferred(EditCommand::ExtendToFirstNonBlank),
        ExtendToLineEnd => deferred(EditCommand::ExtendToLineEnd),
        ExtendToLastLine => deferred(EditCommand::ExtendToLastLine),
        ExtendToPrevParagraph => repeated(
            EditCommand::ExtendToPrevParagraph,
            count(scope, arguments, 0)?,
        ),
        ExtendToNextParagraph => repeated(
            EditCommand::ExtendToNextParagraph,
            count(scope, arguments, 0)?,
        ),
        MoveAfterLineEnd => deferred(EditCommand::MoveAfterLineEnd),
        CollapseSelections => deferred(EditCommand::CollapseSelections),
        Insert => deferred(EditCommand::InsertText(string(
            scope,
            arguments.get(0),
            "text",
        )?)),
        DeleteBackward => deferred(EditCommand::Delete(negative_count(scope, arguments, 0)?)),
        DeleteForward => deferred(EditCommand::Delete(positive_count(scope, arguments, 0)?)),
        DeleteWordBackward => deferred(EditCommand::DeleteWordBackward),
        DeleteToLineStart => deferred(EditCommand::DeleteToLineStart),
        DeleteToLineEnd => deferred(EditCommand::DeleteToLineEnd),
        JoinLines => deferred(EditCommand::JoinLines),
        ToggleCase => deferred(EditCommand::ToggleCase),
        InsertLineBelow => deferred(EditCommand::InsertNewLineBelow),
        InsertLineAbove => deferred(EditCommand::InsertNewLineAbove),
        DeleteLineContent => deferred(EditCommand::DeleteLineContent),
        DeleteSelectionInclusive => deferred(EditCommand::DeleteInclusiveSelection),
        DeleteSelectedLines => deferred(EditCommand::DeleteSelectedLines),
        DeleteWordMotion => deferred(delete_operator(TextTarget::Motion {
            motion: TextMotion::WordForward,
            count: count(scope, arguments, 0)?,
        })),
        DeleteWordEndMotion => deferred(delete_operator(TextTarget::Motion {
            motion: TextMotion::WordEnd,
            count: count(scope, arguments, 0)?,
        })),
        ChangeWordMotion => deferred(delete_operator(TextTarget::Motion {
            motion: TextMotion::ChangeWordForward,
            count: count(scope, arguments, 0)?,
        })),
        DeleteToLineStartMotion => deferred(delete_operator(TextTarget::Motion {
            motion: TextMotion::LineStart,
            count: count(scope, arguments, 0)?,
        })),
        DeleteToLineEndMotion => deferred(delete_operator(TextTarget::Motion {
            motion: TextMotion::LineEnd,
            count: count(scope, arguments, 0)?,
        })),
        DeleteLines => deferred(delete_operator(TextTarget::Lines {
            count: count(scope, arguments, 0)?,
        })),
        ChangeLines => deferred(EditCommand::ChangeLines {
            lines: count(scope, arguments, 0)?,
        }),
        ApplyEdits => vec![OperationRequest::View {
            target: ViewTarget::Current,
            operation: ViewOperation::ApplyContent(apply_edits(scope, arguments, runtime)?),
        }],
        Begin => vec![history(vell_mode::action::TransactionIntent::Begin)],
        Commit => vec![history(vell_mode::action::TransactionIntent::Commit)],
        Rollback => vec![history(vell_mode::action::TransactionIntent::Rollback)],
        Undo => vec![history(vell_mode::action::TransactionIntent::Undo)],
        Redo => vec![history(vell_mode::action::TransactionIntent::Redo)],
        HalfPageUp => viewport(
            ViewportMoveDirection::Up,
            ViewportMoveAmount::HalfPage,
            extend_selection(scope, arguments)?,
        ),
        HalfPageDown => viewport(
            ViewportMoveDirection::Down,
            ViewportMoveAmount::HalfPage,
            extend_selection(scope, arguments)?,
        ),
        FullPageUp => viewport(
            ViewportMoveDirection::Up,
            ViewportMoveAmount::FullPage,
            extend_selection(scope, arguments)?,
        ),
        FullPageDown => viewport(
            ViewportMoveDirection::Down,
            ViewportMoveAmount::FullPage,
            extend_selection(scope, arguments)?,
        ),
        AlignTop => viewport_alignment(ViewportAlignment::Top),
        AlignCenter => viewport_alignment(ViewportAlignment::Center),
        AlignBottom => viewport_alignment(ViewportAlignment::Bottom),
        InvokeMode => {
            let mode = ModeName::new(string(scope, arguments.get(0), "mode")?);
            let action = ModeActionName::new(string(scope, arguments.get(1), "action")?);
            let value = arguments.get(2);
            let arguments = if value.is_null_or_undefined() {
                ModeValue::Null
            } else {
                json_to_mode_value(&v8_to_json(scope, value, "mode arguments")?)?
            };
            nested(OperationRequest::Mode {
                target: ModeTarget::CurrentView,
                invocation: ModeInvocation {
                    command: ModeCommand::new(mode, action).with_arguments(arguments),
                    nested: true,
                    flow: ModeFlowPropagation::Propagate,
                },
            })
        }
        InvokeCommand => {
            let qualified = string(scope, arguments.get(0), "command")?;
            let (mode, action) = qualified.rsplit_once('.').ok_or_else(|| {
                ScriptError::new("command must use the qualified name 'mode.command'")
            })?;
            if mode.is_empty() || action.is_empty() {
                return Err(ScriptError::new(
                    "command must use the qualified name 'mode.command'",
                ));
            }
            let value = arguments.get(1);
            let arguments = if value.is_null_or_undefined() {
                ModeValue::Null
            } else {
                json_to_mode_value(&v8_to_json(scope, value, "command arguments")?)?
            };
            nested(OperationRequest::Mode {
                target: ModeTarget::CurrentView,
                invocation: ModeInvocation {
                    command: ModeCommand::new(ModeName::new(mode), ModeActionName::new(action))
                        .with_arguments(arguments),
                    nested: true,
                    flow: ModeFlowPropagation::Isolate,
                },
            })
        }
        Save => nested(OperationRequest::Content {
            target: ContentTarget::Current,
            operation: ContentOperation::Save,
        }),
        Quit => nested(OperationRequest::App(AppOperation::Command(
            AppCommand::Quit,
        ))),
    })
}

fn history(operation: vell_mode::action::TransactionIntent) -> OperationRequest {
    OperationRequest::History {
        target: ContentTarget::Current,
        operation,
    }
}

fn nested(operation: OperationRequest) -> Vec<OperationRequest> {
    vec![operation]
}

fn delete_operator(target: TextTarget) -> EditCommand {
    EditCommand::Operate(OperatorCommand {
        operator: TextOperator::Delete,
        target,
    })
}

fn viewport(
    direction: ViewportMoveDirection,
    amount: ViewportMoveAmount,
    extend_selection: bool,
) -> Vec<OperationRequest> {
    nested(OperationRequest::View {
        target: ViewTarget::Current,
        operation: ViewOperation::Viewport(ViewportCommand::new(
            direction,
            amount,
            if extend_selection {
                ViewportCursorBehavior::Extend
            } else {
                ViewportCursorBehavior::Move
            },
        )),
    })
}

fn viewport_alignment(alignment: ViewportAlignment) -> Vec<OperationRequest> {
    nested(OperationRequest::View {
        target: ViewTarget::Current,
        operation: ViewOperation::Viewport(ViewportCommand::align(alignment)),
    })
}

fn apply_edits(
    scope: &mut v8::PinScope,
    arguments: &v8::FunctionCallbackArguments,
    runtime: &Rc<RefCell<PrimitiveRuntime>>,
) -> Result<ContentAction, ScriptError> {
    let snapshot = runtime
        .borrow()
        .current
        .as_ref()
        .and_then(|current| current.snapshot.clone())
        .ok_or_else(|| ScriptError::new("text.applyEdits requires editable text content"))?;
    let edits = v8::Local::<v8::Array>::try_from(arguments.get(0))
        .map_err(|_| ScriptError::new("text.applyEdits expects an array"))?;
    let mut parsed = Vec::with_capacity(edits.length() as usize);
    for index in 0..edits.length() {
        let edit = edits
            .get_index(scope, index)
            .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
            .ok_or_else(|| ScriptError::new(format!("content edit {index} must be an object")))?;
        let range = required_object(scope, edit, "range")?;
        let start = required_object(scope, range, "start")?;
        let end = required_object(scope, range, "end")?;
        parsed.push(TextEdit::new(
            parse_position(scope, start, &snapshot)?..parse_position(scope, end, &snapshot)?,
            required_string(scope, edit, "text")?,
        ));
    }
    let change = TextChangeSet::from_edits(snapshot.len_chars(), parsed)
        .map_err(|error| ScriptError::new(format!("invalid content edits: {error:?}")))?;
    Ok(ContentAction::Text(change))
}

fn count(
    scope: &mut v8::PinScope,
    arguments: &v8::FunctionCallbackArguments,
    index: i32,
) -> Result<usize, ScriptError> {
    let value = arguments.get(index);
    if value.is_undefined() {
        return Ok(1);
    }
    let count = non_negative_integer(scope, value, "count")?;
    if count == 0 {
        return Err(ScriptError::new("count must be greater than zero"));
    }
    Ok(count)
}

fn positive_count(
    scope: &mut v8::PinScope,
    arguments: &v8::FunctionCallbackArguments,
    index: i32,
) -> Result<isize, ScriptError> {
    isize::try_from(count(scope, arguments, index)?)
        .map_err(|_| ScriptError::new("count is too large"))
}

fn negative_count(
    scope: &mut v8::PinScope,
    arguments: &v8::FunctionCallbackArguments,
    index: i32,
) -> Result<isize, ScriptError> {
    positive_count(scope, arguments, index)?
        .checked_neg()
        .ok_or_else(|| ScriptError::new("count is too large"))
}

fn non_negative_integer(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
    name: &str,
) -> Result<usize, ScriptError> {
    if !value.is_number() {
        return Err(ScriptError::new(format!("{name} must be an integer")));
    }
    let number = value
        .number_value(scope)
        .ok_or_else(|| ScriptError::new(format!("{name} must be an integer")))?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > usize::MAX as f64 {
        return Err(ScriptError::new(format!(
            "{name} must be a non-negative integer"
        )));
    }
    Ok(number as usize)
}

fn character(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
    name: &str,
) -> Result<char, ScriptError> {
    let value = string(scope, value, name)?;
    let mut characters = value.chars();
    match (characters.next(), characters.next()) {
        (Some(character), None) => Ok(character),
        _ => Err(ScriptError::new(format!(
            "{name} must contain exactly one Unicode character"
        ))),
    }
}

fn string(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
    name: &str,
) -> Result<String, ScriptError> {
    if !value.is_string() {
        return Err(ScriptError::new(format!("{name} must be a string")));
    }
    Ok(value.to_rust_string_lossy(scope))
}

fn extend_selection(
    scope: &mut v8::PinScope,
    arguments: &v8::FunctionCallbackArguments,
) -> Result<bool, ScriptError> {
    let value = arguments.get(0);
    if value.is_undefined() {
        return Ok(false);
    }
    if !value.is_boolean() {
        return Err(ScriptError::new("extendSelection must be a boolean"));
    }
    Ok(value.boolean_value(scope))
}
