use modeleaf_mode::command::ModeValue;
use modeleaf_mode::{CursorDomain, ModeContentContext, ModeViewPolicy};
use modeleaf_protocol::content_query::{CursorStyle, FaceName, SelectionShape};

use super::{MAX_SCRIPT_INPUT_BYTES, MAX_SCRIPT_JSON_BYTES, ScriptError, ensure_size};

pub(super) fn parse_position(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Object>,
    snapshot: &modeleaf_core::text_snapshot::TextSnapshot,
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
    })
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
        if let Some(status) = buffer.document_status() {
            set_document_context(scope, argument, "document", status);
        }
    } else if let Some(status) = context
        .status_bar()
        .and_then(|context| context.status_bar_data())
    {
        set_document_context(scope, argument, "status", status);
    }
    Ok(argument)
}

pub(super) fn set_document_context(
    scope: &mut v8::PinScope,
    argument: v8::Local<v8::Object>,
    name: &str,
    status: modeleaf_protocol::content_query::DocumentStatus,
) {
    let document = v8::Object::new(scope);
    if let Some(file_name) = status.file_name {
        set_string(scope, document, "fileName", &file_name);
    }
    let key = v8::String::new(scope, "modified").unwrap();
    let modified = v8::Boolean::new(scope, status.modified);
    document.set(scope, key.into(), modified.into());
    set_object(scope, argument, name, document);
}

pub(super) fn content_change_to_v8<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    change: &modeleaf_core::content::ContentChange,
) -> Result<v8::Local<'scope, v8::Value>, ScriptError> {
    let modeleaf_core::content::ContentChange::Text(change) = change;
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
