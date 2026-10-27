use serde_json::{Map, Value};

use contract::ErrorCode;

use super::{arg, object_arg};

/// The `host.tool` name: any string, as the Lua half checks. The runner
/// refuses an unknown name as an ordinary return.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn tool_name(args: &[Value]) -> Result<String, (ErrorCode, String)> {
    match arg(args, 0) {
        Value::String(name) => Ok(name.clone()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            Err((
                ErrorCode::InvalidArguments,
                "host.tool: `name` must be a string".to_owned(),
            ))
        }
    }
}

/// The `host.tool` arguments: absent or null reads as no arguments, and
/// an empty table reads the same way, since an empty Lua table is an
/// empty JSON array. A non-empty array stays an array, and the runner
/// refuses it `invalid_arguments` as an ordinary return. Anything else
/// is the caller's error. Past 125 nested containers the parse the Lua
/// half replays would refuse, so deeper values fail naming the limit.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn tool_arguments(args: &[Value]) -> Result<Value, (ErrorCode, String)> {
    const TOOL_DEPTH: usize = 125;
    let arguments = match arg(args, 1) {
        Value::Null => return Ok(Value::Object(Map::new())),
        Value::Array(items) if items.is_empty() => return Ok(Value::Object(Map::new())),
        Value::Array(items) => Value::Array(items.clone()),
        Value::Object(arguments) => Value::Object(arguments.clone()),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.tool: `args` must be a table".to_owned(),
            ));
        }
    };
    if depth(&arguments) > TOOL_DEPTH {
        return Err((
            ErrorCode::InvalidArguments,
            "host.tool: `args` nests deeper than 125".to_owned(),
        ));
    }
    Ok(arguments)
}

/// How deeply `value` nests: a leaf reads 0, and each container adds one.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn depth(value: &Value) -> usize {
    match value {
        Value::Array(items) => items.iter().map(depth).max().unwrap_or(0) + 1,
        Value::Object(map) => map.values().map(depth).max().unwrap_or(0) + 1,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => 0,
    }
}

/// The `host.model` request: the model reference with a slash, the
/// optional system string, the non-empty messages and the positive
/// token limit, with the same sentences as the Lua half.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn model_request(
    args: &[Value],
) -> Result<contract::model::HostModelRequest, (ErrorCode, String)> {
    let opts = object_arg("host.model", args, 0, "opts")?;
    let model = match opts.get("model").unwrap_or(&Value::Null) {
        Value::String(model) if model.contains('/') => model.clone(),
        Value::String(_)
        | Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::Array(_)
        | Value::Object(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.model: `model` must be a model reference, not a role".to_owned(),
            ));
        }
    };
    let system = match opts.get("system").unwrap_or(&Value::Null) {
        Value::Null => None,
        Value::String(system) => Some(system.clone()),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.model: `system` must be a string".to_owned(),
            ));
        }
    };
    let messages = match opts.get("messages").unwrap_or(&Value::Null) {
        Value::Array(messages) if !messages.is_empty() => messages.clone(),
        Value::Array(_)
        | Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Object(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.model: `messages` must be a non-empty list".to_owned(),
            ));
        }
    };
    let mut request_messages = Vec::with_capacity(messages.len());
    for message in &messages {
        request_messages.push(model_message(message)?);
    }
    let max_tokens = match opts.get("max_tokens").unwrap_or(&Value::Null) {
        Value::Number(tokens) if tokens.as_u64().is_some_and(|tokens| tokens > 0) => {
            tokens.as_u64().unwrap_or(0)
        }
        Value::Number(_)
        | Value::Null
        | Value::Bool(_)
        | Value::String(_)
        | Value::Array(_)
        | Value::Object(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.model: `max_tokens` must be a positive integer".to_owned(),
            ));
        }
    };
    Ok(contract::model::HostModelRequest {
        model,
        system,
        messages: request_messages,
        max_tokens,
    })
}

#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn model_message(
    message: &Value,
) -> Result<contract::model::HostMessage, (ErrorCode, String)> {
    const NOT_A_MESSAGE: &str = "host.model: `messages` must hold `{ role, content }` tables";
    let message = message
        .as_object()
        .ok_or((ErrorCode::InvalidArguments, NOT_A_MESSAGE.to_owned()))?;
    let role = match message.get("role").unwrap_or(&Value::Null) {
        Value::String(role) if role == "user" => contract::model::HostRole::User,
        Value::String(role) if role == "assistant" => contract::model::HostRole::Assistant,
        Value::String(_)
        | Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::Array(_)
        | Value::Object(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.model: a message `role` is `user` or `assistant`".to_owned(),
            ));
        }
    };
    let content = model_content(message.get("content").unwrap_or(&Value::Null))?;
    Ok(contract::model::HostMessage { role, content })
}

#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn model_content(
    content: &Value,
) -> Result<Vec<contract::shapes::ContentPart>, (ErrorCode, String)> {
    match content {
        Value::String(text) => Ok(vec![contract::shapes::ContentPart::Text {
            text: text.clone(),
        }]),
        Value::Array(parts) => {
            let mut content_parts = Vec::with_capacity(parts.len());
            for part in parts {
                content_parts.push(model_part(part)?);
            }
            Ok(content_parts)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => Err((
            ErrorCode::InvalidArguments,
            "host.model: a message `content` is a string or a list of parts".to_owned(),
        )),
    }
}

#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn model_part(
    part: &Value,
) -> Result<contract::shapes::ContentPart, (ErrorCode, String)> {
    let part = part.as_object().ok_or((
        ErrorCode::InvalidArguments,
        "host.model: a content part is a table".to_owned(),
    ))?;
    match part.get("type").unwrap_or(&Value::Null) {
        Value::String(kind) if kind == "text" => match part.get("text") {
            Some(Value::String(text)) => {
                Ok(contract::shapes::ContentPart::Text { text: text.clone() })
            }
            _ => Err((
                ErrorCode::InvalidArguments,
                "host.model: a text part holds `text`, a string".to_owned(),
            )),
        },
        Value::String(kind) if kind == "image" || kind == "pdf" => {
            serde_json::from_value::<contract::shapes::ContentPart>(Value::Object(part.clone()))
                .map_err(|error| {
                    (
                        ErrorCode::InvalidArguments,
                        format!("host.model: a content part does not read: {error}"),
                    )
                })
        }
        Value::String(_)
        | Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::Array(_)
        | Value::Object(_) => Err((
            ErrorCode::InvalidArguments,
            "host.model: a content part `type` is `text`, `image` or `pdf`".to_owned(),
        )),
    }
}

/// The `host.delegate` call's parts: the spec arguments as a table with
/// the per-call timeout and the `run` function marker.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) type DelegateParts = (Map<String, Value>, std::time::Duration, bool);

/// The `host.delegate` call: the spec arguments as a table with the
/// per-call timeout and the `run` function marker.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn delegate_request(args: &[Value]) -> Result<DelegateParts, (ErrorCode, String)> {
    const CALL: &str = "host.delegate";
    // The Lua half snapshots the spec table: an empty table reads as `{}`,
    // which the service refuses for its missing fields, while a non-empty
    // list names no arguments.
    let spec = match arg(args, 0) {
        Value::Array(items) if items.is_empty() => Map::new(),
        Value::Object(spec) => spec.clone(),
        Value::Array(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.delegate: `spec` must be a table of named arguments".to_owned(),
            ));
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                format!("{CALL}: `spec` must be a table"),
            ));
        }
    };
    // The Lua half checks the options with the timeout, so missing
    // options read as the missing timeout.
    let opts = match arg(args, 1) {
        Value::Object(opts) => opts.clone(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.delegate: `timeout` must be a whole number of milliseconds above 0"
                    .to_owned(),
            ));
        }
    };
    let timeout = match opts.get("timeout").unwrap_or(&Value::Null) {
        Value::Number(timeout) if timeout.as_u64().is_some_and(|timeout| timeout > 0) => {
            std::time::Duration::from_millis(timeout.as_u64().unwrap_or(0))
        }
        Value::Number(_)
        | Value::Null
        | Value::Bool(_)
        | Value::String(_)
        | Value::Array(_)
        | Value::Object(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.delegate: `timeout` must be a whole number of milliseconds above 0"
                    .to_owned(),
            ));
        }
    };
    if !matches!(opts.get("run").unwrap_or(&Value::Null), Value::Bool(true)) {
        return Err((
            ErrorCode::InvalidArguments,
            "host.delegate: `run` must be a function".to_owned(),
        ));
    }
    Ok((spec, timeout, true))
}

/// The `host.ask` interaction: the closed kind with its spec.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn ask_interaction(
    args: &[Value],
) -> Result<contract::events::Interaction, (ErrorCode, String)> {
    let kind = match arg(args, 0) {
        Value::String(kind) => kind.clone(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.ask: `kind` must be a string".to_owned(),
            ));
        }
    };
    let spec = arg(args, 1).clone();
    // `interaction` prefixes its own sentences with `host.ask: `, as the
    // Lua half's checker does.
    crate::lua::asks::interaction(&kind, &spec)
        .map_err(|message| (ErrorCode::InvalidArguments, message))
}
