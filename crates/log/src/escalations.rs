//! Escalations a person answered allow (`docs/permissions.md`, "Setting up
//! the notes"): one entry per counted escalation, read from every started
//! session's log. It never writes, never locks and never fails: an
//! unreadable log gives what was read before the failure.

mod place;
mod reviewer_shell_calls;

pub use reviewer_shell_calls::{ShellCall, reviewer_shell_calls};

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::Deserialize;

/// One escalation a person answered allow: the `projects/<key>` directory
/// name of the session whose log holds it, the identity that session's
/// first line records, if any, and the request's rule prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedEscalation {
    /// The `projects/<key>` directory holding the session.
    pub project: String,
    /// The session's recorded identity, if its first line records one.
    pub identity: Option<String>,
    /// The request's rule prefix.
    pub prefix: String,
}

/// Every escalation answered allow in every session's log, in session-id
/// order, then request order. A session's log is walked through
/// [`place::sessions`], never [`crate::started_sessions`],
/// [`crate::lines`] or [`crate::SessionStarted`]; the rest of each log is
/// read here through [`crate::open_log`], skipping its first line.
pub fn allowed_escalations(home: &Path) -> Vec<AllowedEscalation> {
    let mut found = Vec::new();
    for session in place::sessions(home) {
        found.extend(escalations_in(&session));
    }
    found
}

/// The counted escalations in one session's log: a `permission_requested`
/// with `step` review, an escalation caused by `consecutive_blocks` or
/// `session_blocks`, and a rule, followed in the same log by a
/// `permission_resolved` with the same request id, decision allow and
/// decided by a person. A request without a rule is skipped, as is a
/// `reviewer_failed` escalation. Each later line is read first for its
/// kind alone; only those two kinds are read again for their payloads. A
/// torn tail keeps what came before it.
fn escalations_in(session: &place::Place) -> Vec<AllowedEscalation> {
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
    let mut pending: HashMap<String, String> = HashMap::new();
    let mut found = Vec::new();
    loop {
        let mut raw = Vec::new();
        if lines.read_until(b'\n', &mut raw).is_err() {
            return found;
        }
        if raw.last() != Some(&b'\n') {
            // The end of the file, or a torn tail: a line with no
            // newline keeps what came before it.
            return found;
        }
        let kind: Kind = match serde_json::from_slice(&raw) {
            Ok(kind) => kind,
            Err(_) => return found,
        };
        match kind.kind.as_str() {
            "permission_requested" => {
                if let Ok(line) = serde_json::from_slice::<RequestLine>(&raw)
                    && let Some((id, prefix)) = counted(&line.payload)
                {
                    pending.insert(id, prefix);
                }
            }
            "permission_resolved" => {
                if let Ok(line) = serde_json::from_slice::<ResolvedLine>(&raw)
                    && let Some(prefix) = answered(&line.payload, &mut pending)
                {
                    found.push(AllowedEscalation {
                        project: session.project.clone(),
                        identity: session.identity.clone(),
                        prefix,
                    });
                }
            }
            _ => {}
        }
    }
}

/// A line's kind alone: every other line is skipped past this.
#[derive(Deserialize)]
struct Kind {
    kind: String,
}

/// A `permission_requested` line, carrying only its payload.
#[derive(Deserialize)]
struct RequestLine {
    payload: Request,
}

/// A `permission_requested` payload, naming only what the count checks:
/// its request id, its step, its escalation's cause and its rule's prefix.
/// `hosts` is never read.
#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    step: Option<String>,
    #[serde(default)]
    escalation: Option<Escalation>,
    #[serde(default)]
    rule: Option<Rule>,
}

/// An escalation, naming only its cause.
#[derive(Deserialize)]
struct Escalation {
    #[serde(default)]
    cause: Option<String>,
}

/// A rule offer, naming only its prefix.
#[derive(Deserialize)]
struct Rule {
    #[serde(default)]
    prefix: Option<String>,
}

/// A `permission_resolved` line, carrying only its payload.
#[derive(Deserialize)]
struct ResolvedLine {
    payload: Resolved,
}

/// A `permission_resolved` payload, naming only what the count checks: the
/// request it answers, its decision and who decided. `hosts` is never read.
#[derive(Deserialize)]
struct Resolved {
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    decided_by: Option<String>,
}

/// The request's id and prefix when it is a counted escalation: `step`
/// review, an escalation caused by `consecutive_blocks` or
/// `session_blocks`, and a rule with a prefix. Anything else is skipped.
fn counted(request: &Request) -> Option<(String, String)> {
    if request.step.as_deref() != Some("review") {
        return None;
    }
    match request
        .escalation
        .as_ref()
        .and_then(|escalation| escalation.cause.as_deref())
    {
        Some("consecutive_blocks" | "session_blocks") => {}
        _ => return None,
    }
    let prefix = request.rule.as_ref().and_then(|rule| rule.prefix.clone())?;
    let id = request.request_id.clone()?;
    Some((id, prefix))
}

/// The resolved request's prefix when the resolution answers a pending
/// request with allow decided by a person. The request leaves the pending
/// set, so a second resolution for it counts nothing more.
fn answered(resolved: &Resolved, pending: &mut HashMap<String, String>) -> Option<String> {
    if resolved.decision.as_deref() != Some("allow") {
        return None;
    }
    if resolved.decided_by.as_deref() != Some("person") {
        return None;
    }
    let id = resolved.request_id.as_deref()?;
    pending.remove(id)
}

#[cfg(test)]
#[path = "escalations_tests.rs"]
mod tests;
