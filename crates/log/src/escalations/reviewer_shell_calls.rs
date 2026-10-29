//! Shell calls a reviewer judged (`docs/permissions.md`, "Setting up the
//! notes"): per-action records of the reviewer's allow and deny decisions
//! on shell calls, joined by `action_id` and read from every started
//! session's log as it stands. It never writes, never locks and never
//! fails: an unreadable log gives what was read before the failure.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::Deserialize;
use serde_json::{Map, Value};

use super::place::Place;

/// One shell call the log attributes: the command the reviewer saw, who
/// decided it, and whether it escalated. The gather classifies `raw`,
/// the reviewed command source, and falls back to it whole-word for
/// calls the classifier cannot parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCall {
    /// The `projects/<key>` directory holding the session.
    pub project: String,
    /// The session's recorded identity, if its first line records one.
    pub identity: Option<String>,
    /// The requested command, `repaired.command` preferred over
    /// `arguments.command`; `None` when neither names one, or when no
    /// shell requester joined the action.
    pub requested: Option<String>,
    /// The reviewed command, from `permission_resolved.arguments`;
    /// `None` when no rewrite was judged.
    pub reviewed: Option<String>,
    /// The reviewed command source: `reviewed` when present, else
    /// `requested`.
    pub raw: Option<String>,
    /// Whether an extension ran the call as an inner call.
    pub ran_by: bool,
    /// The judged subject, when the request carried a rule for it.
    pub rule_subject: Option<String>,
    /// The resolution's decision.
    pub decision: String,
    /// Who or what decided.
    pub decided_by: String,
    /// Whether the request escalated to a person.
    pub escalated: bool,
}

/// Every attributable shell call in every session's log, in session-id
/// order, then log order. A session's log is walked through
/// [`super::place::sessions`], never [`crate::started_sessions`] or
/// [`crate::lines`]; the rest of each log is read here through
/// [`crate::open_log`], skipping its first line. One action's lines join
/// by the envelope's `action_id`: an action with no resolution, a
/// non-shell requester, an allow with no command anywhere, and a deny or
/// escalation with neither a command nor a judged subject give nothing.
/// A torn tail and an unreadable line keep what came before them.
pub fn reviewer_shell_calls(home: &Path) -> Vec<ShellCall> {
    let mut found = Vec::new();
    for session in super::place::sessions(home) {
        found.extend(calls_in(&session));
    }
    found
}

/// One action's joined lines: the first of each kind wins, so a repeated
/// line cannot move the record.
#[derive(Default)]
struct Action {
    /// Whether a `tool_call_requested` joined, and its tool name.
    name: Option<String>,
    /// The requested command, `repaired.command` preferred.
    requested: Option<String>,
    /// Whether an extension ran the call.
    ran_by: bool,
    /// The judged subject, when the request carried a rule for it.
    rule_subject: Option<String>,
    /// Whether any request for the action escalated.
    escalated: bool,
    /// The first resolution, when one joined.
    resolved: Option<Resolved>,
}

/// One action's first `permission_resolved`.
struct Resolved {
    decision: String,
    decided_by: String,
    reviewed: Option<String>,
}

/// The attributable shell calls in one session's log.
fn calls_in(session: &Place) -> Vec<ShellCall> {
    let file = match crate::open_log(&session.dir) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    let mut lines = BufReader::new(file);
    let mut first = Vec::new();
    // The narrow scan path already read the first line: skipping it here
    // never fails the walk when it cannot be read.
    if lines.read_until(b'\n', &mut first).is_err() {
        return Vec::new();
    }
    let mut actions: HashMap<String, Action> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    loop {
        let mut raw = Vec::new();
        if lines.read_until(b'\n', &mut raw).is_err() {
            return finish(session, actions, order);
        }
        if raw.last() != Some(&b'\n') {
            // The end of the file, or a torn tail: a line with no
            // newline keeps what came before it.
            return finish(session, actions, order);
        }
        let narrow: Narrow = match serde_json::from_slice(&raw) {
            Ok(narrow) => narrow,
            Err(_) => return finish(session, actions, order),
        };
        let Some(action_id) = narrow.action_id else {
            continue;
        };
        match narrow.kind.as_str() {
            "tool_call_requested" => {
                if let Ok(requested) = serde_json::from_value::<Requested>(narrow.payload) {
                    let action = action_for(&mut actions, &mut order, &action_id);
                    if action.name.is_none() {
                        action.name = requested.name;
                        action.requested =
                            requested_command(&requested.arguments, &requested.repaired);
                        action.ran_by = requested.ran_by.is_some();
                    }
                }
            }
            "permission_requested" => {
                if let Ok(request) = serde_json::from_value::<Request>(narrow.payload) {
                    let action = action_for(&mut actions, &mut order, &action_id);
                    if action.rule_subject.is_none() {
                        action.rule_subject = request.rule.and_then(|rule| rule.subject);
                    }
                    action.escalated = action.escalated || request.escalation.is_some();
                }
            }
            "permission_resolved" => {
                if let Ok(resolved) = serde_json::from_value::<ResolvedLine>(narrow.payload) {
                    let action = action_for(&mut actions, &mut order, &action_id);
                    if action.resolved.is_none() {
                        let (Some(decision), Some(decided_by)) =
                            (resolved.decision, resolved.decided_by)
                        else {
                            continue;
                        };
                        action.resolved = Some(Resolved {
                            decision,
                            decided_by,
                            reviewed: reviewed_command(&resolved.arguments),
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

/// The action for `action_id`, in first-seen order.
fn action_for<'a>(
    actions: &'a mut HashMap<String, Action>,
    order: &mut Vec<String>,
    action_id: &str,
) -> &'a mut Action {
    if !actions.contains_key(action_id) {
        order.push(action_id.to_owned());
    }
    actions.entry(action_id.to_owned()).or_default()
}

/// The attributable records, in log order: an action with no resolution
/// gives nothing, as does a non-shell requester, an allow with no command
/// anywhere, and a deny or escalation with neither a command nor a judged
/// subject.
fn finish(session: &Place, actions: HashMap<String, Action>, order: Vec<String>) -> Vec<ShellCall> {
    let mut found = Vec::new();
    for action_id in order {
        let Some(action) = actions.get(&action_id) else {
            continue;
        };
        if action.name.as_deref().is_some_and(|name| name != "shell") {
            continue;
        }
        let Some(resolved) = action.resolved.as_ref() else {
            continue;
        };
        let raw = resolved
            .reviewed
            .clone()
            .or_else(|| action.requested.clone());
        let attributable = match resolved.decision.as_str() {
            "allow" => action.name.as_deref() == Some("shell") && raw.is_some(),
            _ => raw.is_some() || action.rule_subject.is_some(),
        };
        if !attributable {
            continue;
        }
        found.push(ShellCall {
            project: session.project.clone(),
            identity: session.identity.clone(),
            requested: action.requested.clone(),
            reviewed: resolved.reviewed.clone(),
            raw,
            ran_by: action.ran_by,
            rule_subject: action.rule_subject.clone(),
            decision: resolved.decision.clone(),
            decided_by: resolved.decided_by.clone(),
            escalated: action.escalated,
        });
    }
    found
}

/// A line's kind, envelope action and body alone: every other line is
/// skipped past this.
#[derive(Deserialize)]
struct Narrow {
    #[serde(default)]
    action_id: Option<String>,
    kind: String,
    #[serde(default)]
    payload: Value,
}

/// A `tool_call_requested` body, naming only what the join keeps: the
/// tool's name, its arguments and repair, and whether an extension ran
/// it. `provider_id` and `provider_item` are never read.
#[derive(Deserialize)]
struct Requested {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<Value>,
    #[serde(default)]
    repaired: Option<Map<String, Value>>,
    #[serde(default)]
    ran_by: Option<serde::de::IgnoredAny>,
}

/// A `permission_requested` body, naming only what the join keeps: the
/// judged subject and whether it escalated. The step, the standing rule
/// and the escalation's cause are never read.
#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    rule: Option<Rule>,
    #[serde(default)]
    escalation: Option<serde::de::IgnoredAny>,
}

/// A request's rule, naming only its subject.
#[derive(Deserialize)]
struct Rule {
    #[serde(default)]
    subject: Option<String>,
}

/// A `permission_resolved` body, naming only what the join keeps: the
/// decision, who decided, and the judged arguments. The request it
/// answers, the reason, the grant and the reviewer are never read.
#[derive(Deserialize)]
struct ResolvedLine {
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    decided_by: Option<String>,
    #[serde(default)]
    arguments: Option<Value>,
}

/// The requested command: `repaired.command` when the requested line
/// carries it, else `arguments.command` when the arguments are an object
/// naming one.
fn requested_command(
    arguments: &Option<Value>,
    repaired: &Option<Map<String, Value>>,
) -> Option<String> {
    if let Some(command) = repaired
        .as_ref()
        .and_then(|repaired| repaired.get("command"))
        .and_then(Value::as_str)
    {
        return Some(command.to_owned());
    }
    command_of(arguments)
}

/// The reviewed command: `arguments.command` when the judged arguments
/// are an object naming one.
fn reviewed_command(arguments: &Option<Value>) -> Option<String> {
    command_of(arguments)
}

/// `value.command` when `value` is an object holding a string under it.
fn command_of(value: &Option<Value>) -> Option<String> {
    value
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|arguments| arguments.get("command"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

#[cfg(test)]
#[path = "reviewer_shell_calls_tests.rs"]
mod tests;
