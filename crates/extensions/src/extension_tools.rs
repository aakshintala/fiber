//! A Lua extension's tool on the tool seam (`docs/extensions.md`,
//! "Registering", and `docs/tools.md`, "What a tool declares"): its
//! definition, its effects as the extension declares them, and its run, in
//! the extension's VM.

use std::sync::Arc;

use contract::ErrorCode;
use contract::emit::Emit;
use contract::events::Control;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, Failure};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value};

use crate::lua::{DeclaredTool, effects_from};
use crate::{Error, LuaExtension};

/// One tool a Lua extension registered with `fiber.tool`.
pub struct LuaTool {
    extension: Arc<LuaExtension>,
    declared: DeclaredTool,
}

impl LuaTool {
    pub(crate) fn new(extension: Arc<LuaExtension>, declared: DeclaredTool) -> Self {
        Self {
            extension,
            declared,
        }
    }

    /// The extension that registered it.
    pub fn extension(&self) -> &str {
        self.extension.name()
    }

    /// Why a call failed, worded for the model and the log.
    fn failed(&self, why: &str) -> Failure {
        failure(
            ErrorCode::ToolError,
            format!(
                "the tool `{}` of `{}` returned {why}",
                self.declared.name,
                self.extension()
            ),
        )
    }
}

impl Tool for LuaTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.declared.name.clone(),
            description: self.declared.description.clone(),
            input_schema: self.declared.input_schema.clone(),
            deferred: false,
            hosted: None,
        }
    }

    /// The static effects, or what the effects function returns for these
    /// arguments. Its error, timeout or bad return fails the call before it
    /// runs (`docs/tools.md`, "Before a call runs"). An extension's tool
    /// has no primary argument, so a rule matches it by name
    /// (`docs/permissions.md`, "What a rule matches").
    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        let name = &self.declared.name;
        let declared = match &self.declared.effects {
            Some(declared) => declared.clone(),
            None => {
                let value = self
                    .extension
                    .tool_effects(name, Value::Object(arguments.clone()))
                    .map_err(|e| {
                        EffectsError::Tool(format!(
                            "the effects function of the tool `{name}` failed: {e}"
                        ))
                    })?;
                effects_from(&value).map_err(|why| {
                    EffectsError::Tool(format!(
                        "the effects function of the tool `{name}` of `{}` returned effects that do not read: {why}",
                        self.extension()
                    ))
                })?
            }
        };
        Ok(Effects {
            declared,
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
        })
    }

    /// Runs `run`. A callback past its timeout fails `timeout`; any other
    /// failure, a raised error or a return that does not read, fails
    /// `tool_error` (`docs/errors.md`). A call `cancel` stopped returns no
    /// content and no error, so the loop completes it `cancelled`
    /// (`docs/tools.md`, "Cancellation").
    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, _emit: &dyn Emit) -> Output {
        let returned = self.extension.tool_run(
            &self.declared.name,
            Value::Object(arguments.clone()),
            cancel,
        );
        let value = match returned {
            Ok(Some(value)) => value,
            Ok(None) => return Output::default(),
            Err(e @ Error::Timeout { .. }) => {
                return failed_output(failure(ErrorCode::Timeout, e.to_string()));
            }
            Err(e) => return failed_output(failure(ErrorCode::ToolError, e.to_string())),
        };
        output_from(value).unwrap_or_else(|why| failed_output(self.failed(&why)))
    }
}

fn failure(code: ErrorCode, message: String) -> Failure {
    Failure {
        code,
        message,
        retry_after_ms: None,
        provider: None,
    }
}

fn failed_output(error: Failure) -> Output {
    Output {
        error: Some(error),
        ..Output::default()
    }
}

/// What `run` returned, as a result (`docs/extensions.md`, "Registering"):
/// a string is one text part; a table whose `type` is set is one text part;
/// any other table holds `content` (a string or a list of text parts) and
/// may hold `details`, `error = { code, message }` and `control`. Anything
/// else says what was wrong, for the caller's message.
fn output_from(value: Value) -> Result<Output, String> {
    let mut map = match value {
        Value::String(text) => return Ok(text_output(vec![ContentPart::Text { text }])),
        Value::Object(map) if map.contains_key("type") => {
            return Ok(text_output(vec![part(&Value::Object(map))?]));
        }
        Value::Object(map) => map,
        Value::Null => return Err("nothing".to_owned()),
        Value::Bool(_) => return Err("a boolean".to_owned()),
        Value::Number(_) => return Err("a number".to_owned()),
        Value::Array(_) => return Err("a list with no `content`".to_owned()),
    };
    if let Some(key) = map
        .keys()
        .find(|key| !matches!(key.as_str(), "content" | "details" | "error" | "control"))
    {
        return Err(format!(
            "`{key}`, which is not `content`, `details`, `error` or `control`"
        ));
    }
    let content = match map.remove("content") {
        Some(Value::String(text)) => vec![ContentPart::Text { text }],
        Some(Value::Array(parts)) => parts.iter().map(part).collect::<Result<_, _>>()?,
        Some(_) => return Err("a `content` that is not a string or a list of parts".to_owned()),
        None => return Err("no `content`".to_owned()),
    };
    let error = map.remove("error").map(error_from).transpose()?;
    let control = map
        .remove("control")
        .map(|value| {
            serde_json::from_value::<Control>(value)
                .map_err(|e| format!("a `control` that does not read: {e}"))
        })
        .transpose()?;
    Ok(Output {
        content,
        error,
        details: map.remove("details"),
        control,
        ..Output::default()
    })
}

fn text_output(content: Vec<ContentPart>) -> Output {
    Output {
        content,
        ..Output::default()
    }
}

/// One text part: `{ type = "text", text = <string> }` and nothing else.
fn part(value: &Value) -> Result<ContentPart, String> {
    let Value::Object(map) = value else {
        return Err(format!("{value} where a part belongs"));
    };
    match map.get("type").and_then(Value::as_str) {
        Some("text") => {}
        Some(kind) => return Err(format!("a `{kind}` part; a tool returns only text parts")),
        None => return Err("a part with no `type`".to_owned()),
    }
    if let Some(key) = map
        .keys()
        .find(|key| !matches!(key.as_str(), "type" | "text"))
    {
        return Err(format!("a text part holding `{key}`"));
    }
    match map.get("text") {
        Some(Value::String(text)) => Ok(ContentPart::Text { text: text.clone() }),
        _ => Err("a text part whose `text` is not a string".to_owned()),
    }
}

/// `error = { code, message }`: any code string, a known one or not.
fn error_from(value: Value) -> Result<Failure, String> {
    let wrong = || "an `error` that is not `{ code, message }` of two strings".to_owned();
    let Value::Object(mut map) = value else {
        return Err(wrong());
    };
    let (Some(Value::String(code)), Some(Value::String(message))) =
        (map.remove("code"), map.remove("message"))
    else {
        return Err(wrong());
    };
    if !map.is_empty() {
        return Err(wrong());
    }
    let code = serde_json::from_value::<ErrorCode>(Value::String(code)).map_err(|_| wrong())?;
    Ok(failure(code, message))
}

#[cfg(test)]
#[path = "extension_tools_tests.rs"]
mod tests;
