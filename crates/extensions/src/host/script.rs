//! Scripted `host.http` and `host.exec` calls for extension cases (`docs/testing.md`, "Testing an extension").

use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};
use support::lock;

use contract::ErrorCode;
use contract::clock::Clock;
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

/// How a completed scripted `host.exec` run ended: its exit code or the
/// signal that ended it, shaped like a real run's (`host.rs` sets only the
/// present one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecEnd {
    /// The process exit code.
    Code(i32),
    /// The signal number that ended the process.
    Signal(i32),
}

/// The result of a scripted `host.exec` run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecReply {
    /// A process that ended, with its termination reason and streams.
    Completed {
        /// How the run ended.
        end: ExecEnd,
        /// Standard output.
        stdout: String,
        /// Standard error.
        stderr: String,
    },
    /// The process never exits: the scripted run parks on the case clock
    /// until the callback's deadline.
    Hang,
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
    /// Wakes parked hang workers and hang-match waiters. Wake-ups are a
    /// cadence only: every waiter re-reads the case clock after one.
    changed: Condvar,
}

struct State {
    http: Vec<HttpEntry>,
    exec: Vec<ExecEntry>,
    oauth: Vec<Value>,
    oauth_next: usize,
    http_next: usize,
    exec_next: usize,
    misses: Vec<String>,
    hangs: Vec<HangMatch>,
    hang_waiters: Vec<usize>,
}

/// One matched scripted hang: the 1-based exec entry, its redacted request
/// and the callback deadline it parks until on the case clock.
struct HangMatch {
    entry: usize,
    request: Value,
    deadline: Instant,
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
                hangs: Vec::new(),
                hang_waiters: Vec::new(),
            }),
            changed: Condvar::new(),
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
                redact_env(&entry.request)
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

    /// Matches one `host.exec` request against the next scripted entry and
    /// returns its 1-based index with the outcome. A miss consumes nothing.
    pub(crate) fn exec(&self, request: Value) -> (usize, Result<ExecReply, (ErrorCode, String)>) {
        let mut state = lock(&self.state);
        let at = state.exec_next;
        let entry = state.exec.get(at).cloned();
        let Some(entry) = entry else {
            state.misses.push(miss("exec", at, &redact_env(&request)));
            return (at.saturating_add(1), Err(no_reply("exec")));
        };
        if json_matches(&entry.request, &request).is_err() {
            state.misses.push(miss("exec", at, &redact_env(&request)));
            return (at.saturating_add(1), Err(no_reply("exec")));
        }
        state.exec_next = at.saturating_add(1);
        (at.saturating_add(1), entry.reply)
    }

    /// Records a scripted hang match: exec entry `index` (1-based, from
    /// [`HostScript::exec`]) met `request` and parks until `deadline` on the
    /// case clock. Called while the hub lock is held, before the worker
    /// spawns, so the runner's hold cannot miss it. Wakes every waiter in
    /// [`HostScript::wait_hang_matched`].
    pub fn hang_matched(&self, index: usize, request: &Value, deadline: Instant) {
        let mut state = lock(&self.state);
        state.hangs.push(HangMatch {
            entry: index,
            request: redact_env(request),
            deadline,
        });
        self.changed.notify_all();
    }

    /// Waits, bounded by `within` on the process clock, until exec entry
    /// `entry` has matched as a hang.
    pub fn wait_hang_matched(&self, entry: usize, within: Duration) -> Result<(), String> {
        let mut state = lock(&self.state);
        state.hang_waiters.push(entry);
        self.changed.notify_all();
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, within, |state| {
                !state.hangs.iter().any(|hang| hang.entry == entry)
            })
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(waiter) = state
            .hang_waiters
            .iter()
            .position(|waiting| *waiting == entry)
        {
            state.hang_waiters.remove(waiter);
        }
        if state.hangs.iter().any(|hang| hang.entry == entry) {
            Ok(())
        } else {
            Err(format!("host.exec[{entry}] did not hang within {within:?}"))
        }
    }

    /// Waits, bounded by `within` on the process clock, until the runner is
    /// waiting for exec entry `entry` to match as a hang.
    pub fn wait_hang_waiting(&self, entry: usize, within: Duration) -> Result<(), String> {
        let state = lock(&self.state);
        let (state, _) = self
            .changed
            .wait_timeout_while(state, within, |state| !state.hang_waiters.contains(&entry))
            .unwrap_or_else(PoisonError::into_inner);
        if state.hang_waiters.contains(&entry) {
            Ok(())
        } else {
            Err(format!(
                "host.exec[{entry}] did not wait for a hang match within {within:?}"
            ))
        }
    }

    /// Every matched hang still later than `now`: only the runner moves the
    /// case clock, so no other advance can release these.
    pub fn pending_hangs(&self, now: Instant) -> Vec<(usize, Value, Instant)> {
        lock(&self.state)
            .hangs
            .iter()
            .filter(|hang| hang.deadline > now)
            .map(|hang| (hang.entry, hang.request.clone(), hang.deadline))
            .collect()
    }

    /// Parks a scripted hang on the case clock until the callback's
    /// `deadline`, as a real `host.exec` parks on the injected clock. The
    /// worker registers as parked at the deadline, so a runner advance to
    /// it matches; the scheduler answers the callback's timeout, so the
    /// worker delivers no reply. A stop ends the park early and records
    /// nothing: the runner owns the stuck report.
    pub(crate) fn park_hang(&self, clock: &dyn Clock, deadline: Instant, stop: &Receiver<()>) {
        let mut guard = lock(&self.state);
        while clock.now() < deadline && !is_disconnected(stop) {
            // The slice is a wake-up cadence, not a timing judgement: the
            // loop's decision reads only the case clock after each slice.
            let parked = support::clock::park(
                clock,
                Some(deadline),
                Some(support::group::GROUP_POLL),
                &self.changed,
                guard,
                |_| false,
            );
            guard = match parked {
                Some(guard) => guard,
                None => lock(&self.state),
            };
        }
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

/// A request with each `env` value replaced by `<redacted>`: names stay,
/// so a diagnostic names the variable without disclosing its value, which
/// may be a secret. Any other shape is unchanged.
fn redact_env(request: &Value) -> Value {
    let mut redacted = request.clone();
    if let Some(env) = redacted.get_mut("env").and_then(Value::as_object_mut) {
        for value in env.values_mut() {
            *value = Value::String("<redacted>".to_owned());
        }
    }
    redacted
}

/// Whether the stop sender is gone: the run's stop, cancel or dispose
/// dropped it, so the parked worker ends. A sent value never arrives; the
/// channel carries only the drop.
fn is_disconnected(stop: &Receiver<()>) -> bool {
    matches!(stop.try_recv(), Err(TryRecvError::Disconnected))
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
            (
                "env".to_owned(),
                Value::Object(
                    self.env
                        .iter()
                        .map(|(name, value)| (name.clone(), Value::String(value.clone())))
                        .collect(),
                ),
            ),
            (
                // A case that names no `input` matches as today: the
                // subset match ignores this key, so null stdin needs no
                // new expectation in old cases.
                "input".to_owned(),
                self.input.as_ref().map_or(Value::Null, |input| {
                    Value::String(String::from_utf8_lossy(input).into_owned())
                }),
            ),
        ]))
    }
}

#[cfg(test)]
#[path = "script_tests.rs"]
mod tests;
