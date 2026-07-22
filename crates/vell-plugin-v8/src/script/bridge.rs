use vell_mode::command::ModeValue;
use vell_mode::{
    CursorDomain, ModeContentContext, ModeViewPolicy, NamedStatusBarPresentation,
    NamedStatusBarSegment,
};
use vell_protocol::content_query::{
    BufferBackingState, CursorStyle, DirtyState, FaceName, SaveState, SelectionShape, TextMetrics,
};

use super::{MAX_SCRIPT_INPUT_BYTES, MAX_SCRIPT_JSON_BYTES, ScriptError, ensure_size};

pub(super) fn parse_position(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Object>,
    snapshot: &vell_core::text_snapshot::TextSnapshot,
) -> Result<usize, ScriptError> {
    let line = required_usize(scope, value, "line")?;
    let character = required_usize(scope, value, "character")?;
    snapshot
        .utf16_position_to_char(line, character)
        .ok_or_else(|| ScriptError::new(format!("invalid UTF-16 position {line}:{character}")))
}

pub(super) fn property<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    object: v8::Local<v8::Object>,
    name: &str,
) -> Option<v8::Local<'scope, v8::Value>> {
    let name = v8::String::new(scope, name)?;
    object.get(scope, name.into())
}

pub(super) fn required_object<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    object: v8::Local<v8::Object>,
    name: &str,
) -> Result<v8::Local<'scope, v8::Object>, ScriptError> {
    property(scope, object, name)
        .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
        .ok_or_else(|| ScriptError::new(format!("mode {name} must be an object")))
}

pub(super) fn required_string(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    name: &str,
) -> Result<String, ScriptError> {
    optional_string(scope, object, name)?
        .ok_or_else(|| ScriptError::new(format!("mode {name} must be a string")))
}

pub(super) fn optional_string(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    name: &str,
) -> Result<Option<String>, ScriptError> {
    let Some(value) = property(scope, object, name) else {
        return Ok(None);
    };
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    if !value.is_string() {
        return Err(ScriptError::new(format!("mode {name} must be a string")));
    }
    Ok(Some(value.to_rust_string_lossy(scope)))
}

pub(super) fn required_usize(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    name: &str,
) -> Result<usize, ScriptError> {
    let value = property(scope, object, name)
        .and_then(|value| value.integer_value(scope))
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| ScriptError::new(format!("{name} must be a non-negative integer")))?;
    if value as u64 > 9_007_199_254_740_991_u64 {
        return Err(ScriptError::new(format!(
            "{name} exceeds JavaScript's safe integer range"
        )));
    }
    Ok(value)
}

pub(super) fn json_to_mode_value(value: &serde_json::Value) -> Result<ModeValue, ScriptError> {
    Ok(match value {
        serde_json::Value::Null => ModeValue::Null,
        serde_json::Value::Bool(value) => ModeValue::Bool(*value),
        serde_json::Value::Number(value) => ModeValue::Integer(
            value
                .as_i64()
                .ok_or_else(|| ScriptError::new("mode arguments must use integer numbers"))?,
        ),
        serde_json::Value::String(value) => ModeValue::String(value.clone()),
        serde_json::Value::Array(values) => ModeValue::List(
            values
                .iter()
                .map(json_to_mode_value)
                .collect::<Result<_, _>>()?,
        ),
        serde_json::Value::Object(values) => ModeValue::Map(
            values
                .iter()
                .map(|(key, value)| Ok((key.clone(), json_to_mode_value(value)?)))
                .collect::<Result<_, ScriptError>>()?,
        ),
    })
}

pub(super) fn view_policy_from_json(
    state: &serde_json::Value,
) -> Result<ModeViewPolicy, ScriptError> {
    let Some(value) = state.get("viewPolicy") else {
        return Ok(ModeViewPolicy::default());
    };
    if value.is_null() {
        return Ok(ModeViewPolicy::default());
    }
    let object = value
        .as_object()
        .ok_or_else(|| ScriptError::new("viewState.viewPolicy must be an object"))?;
    let string = |name: &str| -> Result<Option<&str>, ScriptError> {
        object
            .get(name)
            .map(|value| {
                value.as_str().ok_or_else(|| {
                    ScriptError::new(format!("viewState.viewPolicy.{name} must be a string"))
                })
            })
            .transpose()
    };
    let cursor_style = match string("cursorStyle")? {
        None => None,
        Some("default") => Some(CursorStyle::Default),
        Some("block") => Some(CursorStyle::Block),
        Some("bar") => Some(CursorStyle::Bar),
        Some(value) => return Err(ScriptError::new(format!("invalid cursor style: {value}"))),
    };
    let cursor_domain = match string("cursorDomain")? {
        None => None,
        Some("insertion-point") => Some(CursorDomain::InsertionPoint),
        Some("character") => Some(CursorDomain::Character),
        Some(value) => return Err(ScriptError::new(format!("invalid cursor domain: {value}"))),
    };
    let selection_shape = match string("selectionShape")? {
        None => None,
        Some("character") => Some(SelectionShape::Character),
        Some("character-inclusive") => Some(SelectionShape::CharacterInclusive),
        Some("line") => Some(SelectionShape::Line),
        Some(value) => {
            return Err(ScriptError::new(format!(
                "invalid selection shape: {value}"
            )));
        }
    };
    Ok(ModeViewPolicy {
        cursor_style,
        cursor_domain,
        selection_shape,
        selection_face: string("selectionFace")?.map(FaceName::new),
        status_bar: parse_status_bar_presentation(object)?,
    })
}

fn parse_status_bar_presentation(
    policy: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<NamedStatusBarPresentation>, ScriptError> {
    let Some(value) = policy.get("statusBar") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let object = value
        .as_object()
        .ok_or_else(|| ScriptError::new("viewState.viewPolicy.statusBar must be an object"))?;
    let parse_region = |name: &str| -> Result<Vec<NamedStatusBarSegment>, ScriptError> {
        let Some(value) = object.get(name) else {
            return Ok(Vec::new());
        };
        let values = value.as_array().ok_or_else(|| {
            ScriptError::new(format!(
                "viewState.viewPolicy.statusBar.{name} must be an array"
            ))
        })?;
        values
            .iter()
            .map(|value| {
                let segment = value.as_object().ok_or_else(|| {
                    ScriptError::new(format!(
                        "viewState.viewPolicy.statusBar.{name} segments must be objects"
                    ))
                })?;
                let text = segment.get("text").and_then(serde_json::Value::as_str).ok_or_else(
                    || {
                        ScriptError::new(format!(
                            "viewState.viewPolicy.statusBar.{name} segment text must be a string"
                        ))
                    },
                )?;
                let face = segment
                    .get("face")
                    .map(|value| {
                        value.as_str().map(FaceName::new).ok_or_else(|| {
                            ScriptError::new(format!(
                                "viewState.viewPolicy.statusBar.{name} segment face must be a string"
                            ))
                        })
                    })
                    .transpose()?;
                Ok(NamedStatusBarSegment {
                    text: text.to_owned(),
                    face,
                })
            })
            .collect()
    };
    Ok(Some(NamedStatusBarPresentation {
        left: parse_region("left")?,
        center: parse_region("center")?,
        right: parse_region("right")?,
    }))
}

pub(super) fn json_to_v8<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    value: &serde_json::Value,
) -> Result<v8::Local<'scope, v8::Value>, ScriptError> {
    let json = serde_json::to_string(value)
        .map_err(|error| ScriptError::new(format!("failed to encode mode state: {error}")))?;
    ensure_size("structured input", json.len(), MAX_SCRIPT_INPUT_BYTES)?;
    let json = v8::String::new(scope, &json)
        .ok_or_else(|| ScriptError::new("mode state is too large for V8"))?;
    v8::json::parse(scope, json).ok_or_else(|| ScriptError::new("failed to decode mode state"))
}

pub(super) fn v8_to_json(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
    name: &str,
) -> Result<serde_json::Value, ScriptError> {
    let json = v8::json::stringify(scope, value)
        .ok_or_else(|| ScriptError::new(format!("{name} must contain only structured data")))?;
    ensure_size(name, json.utf8_length(scope), MAX_SCRIPT_JSON_BYTES)?;
    let json = json.to_rust_string_lossy(scope);
    serde_json::from_str(&json)
        .map_err(|error| ScriptError::new(format!("invalid {name}: {error}")))
}

pub(super) fn set_number(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    name: &str,
    value: f64,
) {
    let key = v8::String::new(scope, name).unwrap();
    let value = v8::Number::new(scope, value);
    object.set(scope, key.into(), value.into());
}

pub(super) fn set_string(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    name: &str,
    value: &str,
) {
    let key = v8::String::new(scope, name).unwrap();
    let value = v8::String::new(scope, value).unwrap();
    object.set(scope, key.into(), value.into());
}

pub(super) fn content_context_object<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    context: &ModeContentContext<'_>,
    include_text: bool,
    include_legacy_document: bool,
) -> Result<v8::Local<'scope, v8::Object>, ScriptError> {
    let argument = v8::Object::new(scope);
    set_number(scope, argument, "contentId", context.content_id().0 as f64);
    if let Some(revision) = context.content_revision() {
        set_number(scope, argument, "revision", revision.0 as f64);
    }
    if let Some(buffer) = context.buffer() {
        if include_text && let Some(snapshot) = buffer.text_snapshot() {
            set_string(scope, argument, "text", &snapshot.to_owned_string());
        }
        if include_legacy_document {
            set_legacy_document_context(
                scope,
                argument,
                buffer.resource_name(),
                buffer.dirty_state(),
            );
        }
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
    }
    Ok(argument)
}

fn set_legacy_document_context(
    scope: &mut v8::PinScope,
    argument: v8::Local<v8::Object>,
    resource_name: Option<String>,
    dirty_state: Option<DirtyState>,
) {
    let document = v8::Object::new(scope);
    if let Some(name) = resource_name {
        set_string(scope, document, "fileName", &name);
    }
    let key = v8::String::new(scope, "modified").unwrap();
    let modified = v8::Boolean::new(scope, dirty_state == Some(DirtyState::Modified));
    document.set(scope, key.into(), modified.into());
    set_object(scope, argument, "document", document);
}

pub(super) fn set_resource_facts(
    scope: &mut v8::PinScope,
    argument: v8::Local<v8::Object>,
    resource_name: Option<String>,
    resource_path: Option<String>,
    backing_state: Option<BufferBackingState>,
    dirty_state: Option<DirtyState>,
    text_metrics: Option<TextMetrics>,
) {
    if let Some(name) = resource_name {
        set_string(scope, argument, "resourceName", &name);
    }
    if let Some(path) = resource_path {
        set_string(scope, argument, "resourcePath", &path);
    }
    if let Some(state) = backing_state {
        let state = match state {
            BufferBackingState::Untitled => "untitled",
            BufferBackingState::Unmaterialized => "unmaterialized",
            BufferBackingState::Materialized => "materialized",
        };
        set_string(scope, argument, "backingState", state);
    }
    if let Some(state) = dirty_state {
        let key = v8::String::new(scope, "dirty").unwrap();
        let dirty = v8::Boolean::new(scope, state == DirtyState::Modified);
        argument.set(scope, key.into(), dirty.into());
    }
    if let Some(metrics) = text_metrics {
        let value = v8::Object::new(scope);
        set_number(scope, value, "lineCount", metrics.line_count as f64);
        set_number(scope, value, "characterCount", metrics.char_count as f64);
        set_object(scope, argument, "textMetrics", value);
    }
}

pub(super) fn set_save_state(
    scope: &mut v8::PinScope,
    argument: v8::Local<v8::Object>,
    state: Option<SaveState>,
) {
    let Some(state) = state else {
        return;
    };
    let state = match state {
        SaveState::Idle => "idle",
        SaveState::Saved => "saved",
        SaveState::Failed => "failed",
    };
    set_string(scope, argument, "saveState", state);
}

pub(super) fn content_change_to_v8<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    change: &vell_core::content::ContentChange,
) -> Result<v8::Local<'scope, v8::Value>, ScriptError> {
    let vell_core::content::ContentChange::Text(change) = change;
    let edits = change
        .to_edits()
        .map_err(|error| ScriptError::new(format!("invalid content change: {error:?}")))?;
    let values = v8::Array::new(scope, i32::try_from(edits.len()).unwrap_or(i32::MAX));
    for (index, edit) in edits.into_iter().enumerate() {
        let value = v8::Object::new(scope);
        set_number(scope, value, "startCharacter", edit.range.start as f64);
        set_number(scope, value, "endCharacter", edit.range.end as f64);
        set_string(scope, value, "text", &edit.insert);
        values.set_index(
            scope,
            u32::try_from(index).expect("edit index overflow"),
            value.into(),
        );
    }
    Ok(values.into())
}

pub(super) fn set_object(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    name: &str,
    value: v8::Local<v8::Object>,
) {
    let key = v8::String::new(scope, name).unwrap();
    object.set(scope, key.into(), value.into());
}

pub(super) fn set_value(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    name: &str,
    value: v8::Local<v8::Value>,
) {
    let key = v8::String::new(scope, name).unwrap();
    object.set(scope, key.into(), value);
}

pub(super) fn throw_script_error(scope: &mut v8::PinScope, message: &str) {
    if let Some(message) = v8::String::new(scope, message) {
        scope.throw_exception(message.into());
    }
}
