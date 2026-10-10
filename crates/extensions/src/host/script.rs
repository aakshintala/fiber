//! Scripted `host.http` and `host.exec` calls for extension cases (`docs/testing.md`, "Testing an extension").

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use contract::ErrorCode;
use serde_json::{Map, Value};

use super::{HttpRequest, exec::ExecRequest};

/// One scripted reply to a `host.http` request.
#[derive(Clone)]
pub struct HttpEntry {
    /// The request fields the case expects.
    pub request: Value,
    /// The status and body, or the error `host.http` raises.
    pub reply: Result<(u16, Vec<u8>), (ErrorCode, String)>,
}

/// The result of a scripted `host.exec` run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecReply {
    /// The process exit code.
    pub code: i32,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
}

/// One scripted result for a `host.exec` request.
#[derive(Clone)]
pub struct ExecEntry {
    /// The request fields the case expects.
    pub request: Value,
    /// The process result, or the error `host.exec` raises.
    pub reply: Result<ExecReply, (ErrorCode, String)>,
}

/// Host-call replies available while the case runner drives an extension.
/// Requests consume entries in order, separately for HTTP and exec; a miss
/// consumes nothing (`docs/testing.md`, "Testing an extension").
pub struct HostScript {
    state: Mutex<State>,
}

struct State {
    http: Vec<HttpEntry>,
    exec: Vec<ExecEntry>,
    oauth: Vec<Value>,
    oauth_next: usize,
    http_next: usize,
    exec_next: usize,
    misses: Vec<String>,
}

impl HostScript {
    /// Builds a shared script with the supplied HTTP and exec entries.
    pub fn new(http: Vec<HttpEntry>, exec: Vec<ExecEntry>, oauth: Vec<Value>) -> Arc<Self> {
        let http = http
            .into_iter()
            .map(|mut entry| {
                lowercase_headers(&mut entry.request);
                entry
            })
            .collect();
        Arc::new(Self {
            state: Mutex::new(State {
                http,
                exec,
                oauth,
                oauth_next: 0,
                http_next: 0,
                exec_next: 0,
                misses: Vec::new(),
            }),
        })
    }

    /// Returns every request miss, followed by every unused entry.
    pub fn unmet(&self) -> Vec<String> {
        let state = lock(&self.state);
        let mut unmet = state.misses.clone();
        for (index, entry) in state.http.iter().enumerate().skip(state.http_next) {
            unmet.push(format!(
                "host.http[{}] unused: request {}",
                index.saturating_add(1),
                entry.request
            ));
        }
        for (index, entry) in state.exec.iter().enumerate().skip(state.exec_next) {
            unmet.push(format!(
                "host.exec[{}] unused: request {}",
                index.saturating_add(1),
                entry.request
            ));
        }
        for (index, entry) in state.oauth.iter().enumerate().skip(state.oauth_next) {
            unmet.push(format!(
                "host.oauth[{}] unused: {entry}",
                index.saturating_add(1)
            ));
        }
        unmet
    }

    pub(crate) fn oauth(&self, kind: &str, request: &Value) -> Result<Value, (ErrorCode, String)> {
        let mut state = lock(&self.state);
        let index = state.oauth_next;
        let entry = state
            .oauth
            .get(index)
            .and_then(|entry| entry.get(kind))
            .cloned();
        let Some(entry) = entry else {
            state.misses.push(miss("oauth", index, request));
            return Err(no_reply("oauth"));
        };
        if matches!(kind, "open" | "show") && json_matches(&entry, request).is_err() {
            state.misses.push(miss("oauth", index, request));
            return Err(no_reply("oauth"));
        }
        state.oauth_next = index.saturating_add(1);
        if let Some(error) = entry.get("error") {
            let code = error
                .get("code")
                .cloned()
                .and_then(|code| serde_json::from_value(code).ok())
                .unwrap_or(ErrorCode::IoFailed);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("invalid scripted OAuth error")
                .to_owned();
            return Err((code, message));
        }
        Ok(entry.get("reply").cloned().unwrap_or(entry))
    }

    pub(crate) fn http(&self, mut request: Value) -> Result<(u16, Vec<u8>), (ErrorCode, String)> {
        lowercase_headers(&mut request);
        let mut state = lock(&self.state);
        let index = state.http_next;
        let entry = state.http.get(index).cloned();
        let Some(entry) = entry else {
            state.misses.push(miss("http", index, &request));
            return Err(no_reply("http"));
        };
        if json_matches(&entry.request, &request).is_err() {
            state.misses.push(miss("http", index, &request));
            return Err(no_reply("http"));
        }
        state.http_next = index.saturating_add(1);
        entry.reply
    }

    pub(crate) fn exec(&self, request: Value) -> Result<ExecReply, (ErrorCode, String)> {
        let mut state = lock(&self.state);
        let index = state.exec_next;
        let entry = state.exec.get(index).cloned();
        let Some(entry) = entry else {
            state.misses.push(miss("exec", index, &request));
            return Err(no_reply("exec"));
        };
        if json_matches(&entry.request, &request).is_err() {
            state.misses.push(miss("exec", index, &request));
            return Err(no_reply("exec"));
        }
        state.exec_next = index.saturating_add(1);
        entry.reply
    }
}

/// Matches a case's expected JSON subset against an actual request or event.
/// Objects may omit actual keys; arrays and scalar values match exactly
/// (`docs/testing.md`, "Event streams").
pub fn json_matches(expected: &Value, actual: &Value) -> Result<(), String> {
    compare(expected, actual, "")
}

fn compare(expected: &Value, actual: &Value, path: &str) -> Result<(), String> {
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            for (key, expected_value) in expected {
                let child = child_path(path, key);
                match actual.get(key) {
                    Some(actual_value) => compare(expected_value, actual_value, &child)?,
                    None => {
                        if !expected_value.is_null() {
                            return Err(format!(
                                "{child}: expected {}, found no value",
                                expected_value
                            ));
                        }
                    }
                }
            }
            Ok(())
        }
        (Value::Array(expected), Value::Array(actual)) => {
            if expected.len() != actual.len() {
                return Err(format!(
                    "{}: expected array length {}, got {}",
                    shown_path(path),
                    expected.len(),
                    actual.len()
                ));
            }
            for (index, (expected_value, actual_value)) in
                expected.iter().zip(actual.iter()).enumerate()
            {
                compare(
                    expected_value,
                    actual_value,
                    &format!("{}[{index}]", shown_path(path)),
                )?;
            }
            Ok(())
        }
        (Value::Number(expected), Value::Number(actual)) => {
            let equal = match (expected.is_f64(), actual.is_f64()) {
                (false, false) => expected == actual,
                (false, true) | (true, false) | (true, true) => {
                    expected.as_f64() == actual.as_f64()
                }
            };
            if equal {
                Ok(())
            } else {
                Err(mismatch(path, expected.to_string(), actual.to_string()))
            }
        }
        _ => {
            if expected == actual {
                Ok(())
            } else {
                Err(mismatch(path, expected.to_string(), actual.to_string()))
            }
        }
    }
}

fn child_path(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_owned()
    } else {
        format!("{parent}.{child}")
    }
}

fn shown_path(path: &str) -> &str {
    if path.is_empty() { "$" } else { path }
}

fn mismatch(path: &str, expected: String, actual: String) -> String {
    format!("{}: expected {expected}, got {actual}", shown_path(path))
}

fn lowercase_headers(request: &mut Value) {
    let Some(headers) = request.get_mut("headers").and_then(Value::as_object_mut) else {
        return;
    };
    let normalized: Map<String, Value> = headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.clone()))
        .collect();
    *headers = normalized;
}

fn miss(kind: &str, index: usize, request: &Value) -> String {
    format!(
        "host.{kind}[{}] miss: request {request}",
        index.saturating_add(1)
    )
}

fn no_reply(kind: &str) -> (ErrorCode, String) {
    let code = if kind == "http" {
        ErrorCode::ConnectionFailed
    } else {
        ErrorCode::IoFailed
    };
    (
        code,
        format!("host.{kind}: the case scripts no reply for this request"),
    )
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl HttpRequest {
    /// The JSON shape a case matches, with header names compared without case.
    pub(crate) fn case_value(&self) -> Value {
        let headers: Map<String, Value> = self
            .headers
            .iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), Value::String(value.clone())))
            .collect();
        Value::Object(Map::from_iter([
            ("method".to_owned(), Value::String(self.method.clone())),
            ("url".to_owned(), Value::String(self.url.clone())),
            ("headers".to_owned(), Value::Object(headers)),
            (
                "body".to_owned(),
                self.body
                    .as_ref()
                    .map(|body| Value::String(String::from_utf8_lossy(body).into_owned()))
                    .unwrap_or(Value::Null),
            ),
        ]))
    }
}

impl ExecRequest {
    pub(crate) fn case_value(&self) -> Value {
        Value::Object(Map::from_iter([
            ("program".to_owned(), Value::String(self.program.clone())),
            (
                "args".to_owned(),
                Value::Array(self.args.iter().cloned().map(Value::String).collect()),
            ),
            (
                "cwd".to_owned(),
                Value::String(self.cwd.display().to_string()),
            ),
        ]))
    }
}

#[cfg(test)]
#[path = "script_tests.rs"]
mod tests;
