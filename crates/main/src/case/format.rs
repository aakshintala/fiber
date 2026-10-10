//! Parsing for the case format (`docs/testing.md`, "Testing an extension").

use std::path::Path;

use contract::ErrorCode;
use serde_json::{Map, Value};

/// One parsed extension case: a session case or a provider call case.
/// The two kinds are variants, so a case cannot carry both a prompt and a
/// call, and a call case cannot carry both a return and an error
/// (`docs/code-quality.md`, "Types").
pub(crate) enum Case {
    Session(SessionCase),
    Call(CallCase),
}

/// A session case: the script, the prompt and the expected event lines.
pub(crate) struct SessionCase {
    /// The optional display name, defaulted by the caller from the file stem.
    pub(crate) name: Option<String>,
    /// Inline model steps.
    pub(crate) script: Value,
    /// The first prompt.
    pub(crate) prompt: String,
    /// Global configuration written into the case's temporary Fiber home.
    pub(crate) config: Option<Value>,
    /// Scripted extension host calls.
    pub(crate) host: Host,
    /// Advances on the case clock, in order.
    pub(crate) clock: Vec<ClockAdvance>,
    /// The event after which the child closes the session.
    pub(crate) until: Option<Selector>,
    /// Durable event lines expected from the session.
    pub(crate) expect: Vec<Value>,
}

/// A provider call case: the call and its one expected outcome.
pub(crate) struct CallCase {
    /// The optional display name, defaulted by the caller from the file stem.
    pub(crate) name: Option<String>,
    /// Ordered calls and credential barriers sharing one provider.
    pub(crate) steps: Vec<CallStep>,
    /// Stored credentials to seed before the first call.
    pub(crate) credentials: Map<String, Value>,
    /// Stored credentials to check after the last call.
    pub(crate) expect_credentials: Map<String, Value>,
    /// Whether a person is attached to the case.
    pub(crate) attended: bool,
    /// Advances while a call is parked on its clock deadline.
    pub(crate) clock: Vec<ClockAdvance>,
    /// Scripted extension host calls.
    pub(crate) host: Host,
}

pub(crate) struct CallStep {
    pub(crate) operation: CallOperation,
    pub(crate) clock: Vec<ClockAdvance>,
}

pub(crate) enum CallOperation {
    Invoke { call: Call, outcome: CallOutcome },
    AwaitCredentialIdle,
}

/// The expected outcome of a provider call: what it returns or the error
/// it fails with. One variant, so a case cannot expect both
/// (`docs/code-quality.md`, "Types").
pub(crate) enum CallOutcome {
    Returns(Value),
    Error(Value),
}

/// Replies supplied to extension host calls.
#[derive(Default)]
pub(crate) struct Host {
    /// HTTP replies in request order.
    pub(crate) http: Vec<extensions::HttpEntry>,
    /// Exec replies in request order.
    pub(crate) exec: Vec<extensions::ExecEntry>,
    /// OAuth calls in order.
    pub(crate) oauth: Vec<Value>,
}

/// One manual clock advance, optionally anchored to an event line.
pub(crate) struct ClockAdvance {
    /// The durable event that authorizes this advance, when present.
    pub(crate) after: Option<Selector>,
    /// The positive number of milliseconds to advance.
    pub(crate) advance_ms: u64,
}

/// An event kind and its 1-based occurrence.
#[derive(Clone)]
pub(crate) struct Selector {
    /// The event kind to find.
    pub(crate) kind: String,
    /// The occurrence to find, defaulting to the first.
    pub(crate) nth: usize,
}

/// A provider callback invocation.
pub(crate) struct Call {
    /// The provider name registered by the package.
    pub(crate) provider: String,
    /// The supported provider callback name.
    pub(crate) function: String,
    /// The JSON argument passed to the callback.
    pub(crate) arg: Value,
}

impl Case {
    /// Parses a JSON case file and reports the first malformed field.
    pub(crate) fn parse(path: &Path, bytes: &[u8]) -> Result<Self, String> {
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|error| format!("{}: invalid JSON: {error}", path.display()))?;
        let map = value
            .as_object()
            .ok_or_else(|| format!("{}: case must be an object", path.display()))?;
        only(map, CASE_FIELDS, "case")?;
        if map.contains_key("call") && map.contains_key("calls") {
            return Err("call and calls cannot be used in the same case".to_owned());
        }
        let name = optional_string(map, "name", "case")?;
        let script = map.get("script").cloned();
        let prompt = optional_string(map, "prompt", "case")?;
        let config = map.get("config").cloned();
        if config.as_ref().is_some_and(|value| !value.is_object()) {
            return Err("config: expected an object".to_owned());
        }
        let host = parse_host(map.get("host"))?;
        let clock = parse_clock(map.get("clock"))?;
        let until = map
            .get("until")
            .map(|value| parse_selector(value, "until"))
            .transpose()?;
        let expect = parse_expect(map.get("expect"))?;
        let call = map.get("call").map(parse_call).transpose()?;
        let returns = map.get("returns").cloned();
        let error = map.get("error").cloned();

        if call.is_some() || map.contains_key("calls") {
            if prompt.is_some() {
                return Err("call and prompt cannot be used in the same case".to_owned());
            }
            if ["script", "config", "until", "expect"]
                .iter()
                .any(|field| map.contains_key(*field))
            {
                return Err("call cases take call, host and one of returns or error".to_owned());
            }
            let steps = if let Some(call) = call {
                vec![CallStep {
                    operation: CallOperation::Invoke {
                        outcome: parse_outcome(&call, returns, error)?,
                        call,
                    },
                    clock: Vec::new(),
                }]
            } else {
                if returns.is_some() || error.is_some() {
                    return Err("calls entries carry their own returns or error".to_owned());
                }
                parse_calls(map.get("calls"))?
            };
            let credentials = credential_map(map.get("credentials"), "credentials")?;
            let expect_credentials =
                credential_map(map.get("expect_credentials"), "expect_credentials")?;
            let attended = match map.get("attended") {
                None => true,
                Some(Value::Bool(value)) => *value,
                Some(_) => return Err("attended: expected a boolean".to_owned()),
            };
            if clock.iter().any(|advance| advance.after.is_some()) {
                return Err("clock.after: call cases have no events".to_owned());
            }
            Ok(Case::Call(CallCase {
                name,
                steps,
                host,
                credentials,
                expect_credentials,
                attended,
                clock,
            }))
        } else {
            if ["credentials", "expect_credentials", "attended"]
                .iter()
                .any(|key| map.contains_key(*key))
            {
                return Err(
                    "credentials, expect_credentials and attended are only valid in a call case"
                        .to_owned(),
                );
            }
            if !map.contains_key("expect") {
                return Err("expect: required for a session case".to_owned());
            }
            if returns.is_some() || error.is_some() {
                return Err("returns and error are only valid in a call case".to_owned());
            }
            let script_value = script
                .as_ref()
                .ok_or_else(|| "script: required for a session case".to_owned())?;
            let script_bytes = serde_json::to_vec(script_value)
                .map_err(|error| format!("script: cannot encode: {error}"))?;
            provider::scripted::Script::parse(path, &script_bytes)
                .map_err(|error| format!("script: {error}"))?;
            let prompt = prompt.ok_or_else(|| "prompt: required for a session case".to_owned())?;
            Ok(Case::Session(SessionCase {
                name,
                script: script_value.clone(),
                prompt,
                config,
                host,
                clock,
                until,
                expect,
            }))
        }
    }
}

fn parse_outcome(
    call: &Call,
    returns: Option<Value>,
    error: Option<Value>,
) -> Result<CallOutcome, String> {
    let outcome = match (returns, error) {
        (Some(returns), None) => CallOutcome::Returns(returns),
        (None, Some(error)) => CallOutcome::Error(error),
        _ => {
            return Err("a call case needs exactly one of returns or error".to_owned());
        }
    };
    if call.function == "cost" {
        if let CallOutcome::Returns(value) = &outcome
            && !value.is_null()
            && value
                .as_f64()
                .is_none_or(|number| !number.is_finite() || number < 0.0)
        {
            return Err("returns: expected null or a finite number at or above 0".to_owned());
        }
    } else if call.function == "models"
        && let CallOutcome::Returns(value) = &outcome
        && !value.is_array()
    {
        return Err("returns: models expects a list of models".to_owned());
    }
    if let CallOutcome::Error(value) = &outcome {
        validate_expected_error(value)?;
    }
    Ok(outcome)
}

const CASE_FIELDS: &[&str] = &[
    "name",
    "script",
    "prompt",
    "config",
    "host",
    "clock",
    "until",
    "expect",
    "call",
    "returns",
    "error",
    "calls",
    "credentials",
    "expect_credentials",
    "attended",
];

fn parse_calls(value: Option<&Value>) -> Result<Vec<CallStep>, String> {
    let entries = value
        .and_then(Value::as_array)
        .ok_or("calls: expected a non-empty list")?;
    if entries.is_empty() {
        return Err("calls: expected a non-empty list".to_owned());
    }
    entries
        .iter()
        .map(|value| {
            let map = value.as_object().ok_or("calls entry: expected an object")?;
            only(
                map,
                &["call", "returns", "error", "clock", "await"],
                "calls entry",
            )?;
            if let Some(value) = map.get("await") {
                only(map, &["await"], "calls await")?;
                if value.as_str() != Some("credential_idle") {
                    return Err("await: supported value is credential_idle".to_owned());
                }
                return Ok(CallStep {
                    operation: CallOperation::AwaitCredentialIdle,
                    clock: Vec::new(),
                });
            }
            let call = parse_call(map.get("call").ok_or("calls entry.call: required")?)?;
            let outcome = parse_outcome(
                &call,
                map.get("returns").cloned(),
                map.get("error").cloned(),
            )?;
            let clock = parse_clock(map.get("clock"))?;
            if clock.iter().any(|advance| advance.after.is_some()) {
                return Err("calls clock.after: call cases have no events".to_owned());
            }
            Ok(CallStep {
                operation: CallOperation::Invoke { call, outcome },
                clock,
            })
        })
        .collect()
}

fn credential_map(value: Option<&Value>, field: &str) -> Result<Map<String, Value>, String> {
    let Some(value) = value else {
        return Ok(Map::new());
    };
    let map = value
        .as_object()
        .ok_or_else(|| format!("{field}: expected an object"))?;
    for key in map.keys() {
        let (credential, label) = key
            .split_once('/')
            .ok_or_else(|| format!("{field}: expected credential/label"))?;
        config::CredentialFile::new(Path::new("."), credential, label)
            .map_err(|error| format!("{field}.{key}: {error}"))?;
    }
    Ok(map.clone())
}

fn parse_oauth(value: Option<&Value>) -> Result<Vec<Value>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let entries = value.as_array().ok_or("host.oauth: expected a list")?;
    for entry in entries {
        let map = entry
            .as_object()
            .ok_or("host.oauth entry: expected an object")?;
        only(
            map,
            &["pkce", "open", "show", "callback"],
            "host.oauth entry",
        )?;
        if map.len() != 1 {
            return Err("host.oauth entry: expected exactly one operation".to_owned());
        }
        for (kind, value) in map {
            let fields = value
                .as_object()
                .ok_or("host.oauth operation: expected an object")?;
            match kind.as_str() {
                "pkce" => {
                    only(fields, &["verifier", "challenge"], "host.oauth.pkce")?;
                    string(fields, "verifier", "host.oauth.pkce")?;
                    string(fields, "challenge", "host.oauth.pkce")?;
                }
                "open" => {
                    only(fields, &["url"], "host.oauth.open")?;
                    string(fields, "url", "host.oauth.open")?;
                }
                "show" => {
                    only(fields, &["url", "code"], "host.oauth.show")?;
                    string(fields, "url", "host.oauth.show")?;
                    string(fields, "code", "host.oauth.show")?;
                }
                "callback" => {
                    only(fields, &["reply", "error"], "host.oauth.callback")?;
                    match (fields.get("reply"), fields.get("error")) {
                        (Some(reply), None) => {
                            let reply = reply
                                .as_object()
                                .ok_or("host.oauth.callback.reply: expected an object")?;
                            only(reply, &["query"], "host.oauth.callback.reply")?;
                            let query = reply
                                .get("query")
                                .and_then(Value::as_object)
                                .ok_or("host.oauth.callback.reply.query: expected an object")?;
                            if !query.values().all(Value::is_string) {
                                return Err(
                                    "host.oauth.callback.reply.query: expected string values"
                                        .to_owned(),
                                );
                            }
                        }
                        (None, Some(error)) => {
                            validate_expected_error(error)?;
                            let fields = error
                                .as_object()
                                .ok_or("host.oauth.callback.error: expected an object")?;
                            string(fields, "message", "host.oauth.callback.error")?;
                        }
                        _ => {
                            return Err(
                                "host.oauth.callback: expected exactly one of reply or error"
                                    .to_owned(),
                            );
                        }
                    }
                }
                _ => return Err("host.oauth: unsupported operation".to_owned()),
            }
        }
    }
    Ok(entries.clone())
}

fn parse_host(value: Option<&Value>) -> Result<Host, String> {
    let Some(value) = value else {
        return Ok(Host::default());
    };
    let map = value.as_object().ok_or("host: expected an object")?;
    only(map, &["http", "exec", "oauth"], "host")?;
    let http = match map.get("http") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or("host.http: expected a list")?
            .iter()
            .enumerate()
            .map(|(index, value)| parse_http(value, index))
            .collect::<Result<_, _>>()?,
    };
    let exec = match map.get("exec") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or("host.exec: expected a list")?
            .iter()
            .enumerate()
            .map(|(index, value)| parse_exec(value, index))
            .collect::<Result<_, _>>()?,
    };
    let oauth = parse_oauth(map.get("oauth"))?;
    Ok(Host { http, exec, oauth })
}

fn parse_http(value: &Value, index: usize) -> Result<extensions::HttpEntry, String> {
    let prefix = format!("host.http[{}]", index.saturating_add(1));
    let map = value
        .as_object()
        .ok_or_else(|| format!("{prefix}: expected an object"))?;
    only(map, &["request", "reply"], &prefix)?;
    let request = map
        .get("request")
        .filter(|request| request.is_object())
        .cloned()
        .ok_or_else(|| format!("{prefix}.request: expected an object"))?;
    let reply = map
        .get("reply")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{prefix}.reply: expected an object"))?;
    let scripted = if let Some(error) = reply.get("error") {
        only(reply, &["error"], &format!("{prefix}.reply"))?;
        let error = error
            .as_object()
            .ok_or_else(|| format!("{prefix}.reply.error: expected an object"))?;
        only(
            error,
            &["code", "message"],
            &format!("{prefix}.reply.error"),
        )?;
        let code = error_code(error, "code", &format!("{prefix}.reply.error"))?;
        let message = string(error, "message", &format!("{prefix}.reply.error"))?;
        Err((code, message))
    } else {
        only(reply, &["status", "body"], &format!("{prefix}.reply"))?;
        let status = unsigned(reply, "status", &format!("{prefix}.reply"))?;
        let status = u16::try_from(status)
            .ok()
            .filter(|status| (100..=599).contains(status))
            .ok_or_else(|| format!("{prefix}.reply.status: expected a status from 100 to 599"))?;
        let body = string(reply, "body", &format!("{prefix}.reply"))?;
        Ok((status, body.into_bytes()))
    };
    Ok(extensions::HttpEntry {
        request,
        reply: scripted,
    })
}

fn parse_exec(value: &Value, index: usize) -> Result<extensions::ExecEntry, String> {
    let prefix = format!("host.exec[{}]", index.saturating_add(1));
    let map = value
        .as_object()
        .ok_or_else(|| format!("{prefix}: expected an object"))?;
    only(map, &["request", "reply"], &prefix)?;
    let request = map
        .get("request")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{prefix}.request: expected an object"))?;
    only(
        request,
        &["program", "args", "cwd"],
        &format!("{prefix}.request"),
    )?;
    string(request, "program", &format!("{prefix}.request"))?;
    if let Some(args) = request.get("args")
        && !args
            .as_array()
            .is_some_and(|args| args.iter().all(Value::is_string))
    {
        return Err(format!("{prefix}.request.args: expected a list of strings"));
    }
    if let Some(cwd) = request.get("cwd")
        && !cwd.is_string()
        && !cwd.is_null()
    {
        return Err(format!("{prefix}.request.cwd: expected a string or null"));
    }
    let reply = map
        .get("reply")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{prefix}.reply: expected an object"))?;
    only(
        reply,
        &["code", "stdout", "stderr"],
        &format!("{prefix}.reply"),
    )?;
    let code = signed(reply, "code", &format!("{prefix}.reply"))?;
    let code = i32::try_from(code)
        .map_err(|_| format!("{prefix}.reply.code: outside the process exit-code range"))?;
    let stdout = string(reply, "stdout", &format!("{prefix}.reply"))?;
    let stderr = string(reply, "stderr", &format!("{prefix}.reply"))?;
    Ok(extensions::ExecEntry {
        request: Value::Object(request.clone()),
        reply: Ok(extensions::ExecReply {
            code,
            stdout,
            stderr,
        }),
    })
}

fn parse_clock(value: Option<&Value>) -> Result<Vec<ClockAdvance>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or("clock: expected a list")?
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let prefix = format!("clock[{}]", index.saturating_add(1));
            let map = value
                .as_object()
                .ok_or_else(|| format!("{prefix}: expected an object"))?;
            only(map, &["advance_ms", "after"], &prefix)?;
            let advance_ms = unsigned(map, "advance_ms", &prefix)?;
            if advance_ms == 0 {
                return Err(format!("{prefix}.advance_ms: must be positive"));
            }
            let after = map
                .get("after")
                .map(|value| parse_selector(value, &format!("{prefix}.after")))
                .transpose()?;
            Ok(ClockAdvance { after, advance_ms })
        })
        .collect()
}

fn parse_selector(value: &Value, field: &str) -> Result<Selector, String> {
    let map = value
        .as_object()
        .ok_or_else(|| format!("{field}: expected an object"))?;
    only(map, &["kind", "nth"], field)?;
    let kind = string(map, "kind", field)?;
    let nth = match map.get("nth") {
        None => 1,
        Some(_) => {
            let value = unsigned(map, "nth", field)?;
            usize::try_from(value)
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| format!("{field}.nth: must be a positive whole number"))?
        }
    };
    Ok(Selector { kind, nth })
}

fn parse_expect(value: Option<&Value>) -> Result<Vec<Value>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or("expect: expected a list")?
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let prefix = format!("expect[{}]", index.saturating_add(1));
            let map = value
                .as_object()
                .ok_or_else(|| format!("{prefix}: expected an object"))?;
            string(map, "kind", &prefix)?;
            Ok(value.clone())
        })
        .collect()
}

fn parse_call(value: &Value) -> Result<Call, String> {
    let map = value.as_object().ok_or("call: expected an object")?;
    only(map, &["provider", "function", "arg"], "call")?;
    let provider = string(map, "provider", "call")?;
    let function = string(map, "function", "call")?;
    if !["cost", "models", "login", "credential", "sign"].contains(&function.as_str()) {
        return Err("call.function: supported functions are `cost`, `models`, `login`, `credential` and `sign`".to_owned());
    }
    let arg = map.get("arg").cloned().ok_or("call.arg: required")?;
    Ok(Call {
        provider,
        function,
        arg,
    })
}

fn validate_expected_error(value: &Value) -> Result<(), String> {
    let map = value.as_object().ok_or("error: expected an object")?;
    only(map, &["code", "message"], "error")?;
    error_code(map, "code", "error")?;
    if let Some(message) = map.get("message")
        && !message.is_string()
    {
        return Err("error.message: expected a string".to_owned());
    }
    Ok(())
}

fn error_code(map: &Map<String, Value>, key: &str, field: &str) -> Result<ErrorCode, String> {
    let value = map
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{field}.{key}: expected an error code string"))?;
    serde_json::from_value(Value::String(value.to_owned()))
        .map_err(|_| format!("{field}.{key}: unknown error code `{value}`"))
}

fn only(map: &Map<String, Value>, fields: &[&str], field: &str) -> Result<(), String> {
    match map.keys().find(|key| !fields.contains(&key.as_str())) {
        Some(key) => Err(format!("{field}.{key}: unknown field")),
        None => Ok(()),
    }
}

fn optional_string(
    map: &Map<String, Value>,
    key: &str,
    field: &str,
) -> Result<Option<String>, String> {
    map.get(key).map(|_| string(map, key, field)).transpose()
}

fn string(map: &Map<String, Value>, key: &str, field: &str) -> Result<String, String> {
    map.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{field}.{key}: expected a string"))
}

fn unsigned(map: &Map<String, Value>, key: &str, field: &str) -> Result<u64, String> {
    map.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{field}.{key}: expected a whole number at or above 0"))
}

fn signed(map: &Map<String, Value>, key: &str, field: &str) -> Result<i64, String> {
    map.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("{field}.{key}: expected a whole number"))
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
