//! The `/status` view: what the terminal already holds about the session
//! on screen, one named row each, asking nothing (`docs/tui.md`,
//! "Swapped views").

use contract::SessionId;
use contract::events::SessionStatus;

use crate::format::money;
use crate::swapped::{Frame, Ink, List, Spot, compact};

/// What the view draws from: the session's latest `session_status`, the
/// credential its latest build names, and the terminal's launch.
pub(crate) struct Facts<'a> {
    /// The session on screen.
    pub(crate) session: Option<&'a SessionId>,
    /// Its latest `session_status`.
    pub(crate) status: Option<&'a SessionStatus>,
    /// The credential label of its latest `preamble_built` or
    /// `model_changed`: `None` before either, `Some(None)` when the
    /// provider takes no credential.
    pub(crate) credential: Option<Option<&'a str>>,
    /// The listed hub's name on a remote terminal; `None` on this machine.
    pub(crate) hub: Option<&'a str>,
    /// Fiber's version, from the launch.
    pub(crate) version: Option<&'a str>,
}

/// The rows' name column: the longest name and two spaces.
const NAME: usize = 13;

/// The status rows, one per name, each the name dim and its value, or
/// "not known yet" dim where the value has not arrived.
pub(crate) fn frame(facts: &Facts<'_>, list: List) -> Frame {
    let status = facts.status;
    let rows = vec![
        entry(
            "session",
            facts.session.map(|session| match status {
                Some(status) => format!("{} · {}", session.0, status.name),
                None => session.0.clone(),
            }),
        ),
        entry("workspace", status.map(workspace)),
        entry("model", status.map(model)),
        entry(
            "credential",
            facts
                .credential
                .map(|label| label.unwrap_or("none").to_owned()),
        ),
        entry("permissions", Some("auto".to_owned())),
        entry("hub", Some(facts.hub.unwrap_or("this machine").to_owned())),
        entry("version", facts.version.map(str::to_owned)),
        entry(
            "context",
            status
                .and_then(|status| status.context.as_ref())
                .map(|context| {
                    format!(
                        "{} of {} tokens",
                        compact(context.tokens),
                        compact(context.window)
                    )
                }),
        ),
        entry(
            "spend",
            status.map(|status| {
                let billed = status
                    .spend
                    .cost
                    .map_or_else(|| "unknown".to_owned(), money);
                format!(
                    "{billed} billed · {} subscription",
                    money(status.spend.subscription_cost)
                )
            }),
        ),
        entry("turns", status.map(turns)),
        entry("clients", status.map(|status| status.clients.to_string())),
    ];
    Frame {
        title: "Status".to_owned(),
        command: "/status",
        rows,
        list,
        below: Vec::new(),
        field: None,
        footer: String::new(),
    }
}

/// One row: `name` dim in its column, then `value`, or "not known yet"
/// dim when it has not arrived.
fn entry(name: &str, value: Option<String>) -> Vec<(String, Option<Spot>, Ink)> {
    let name = format!("{name:<NAME$}");
    match value {
        Some(value) => vec![(name, None, Ink::Muted), (value, None, Ink::Plain)],
        None => vec![
            (name, None, Ink::Muted),
            ("not known yet".to_owned(), None, Ink::Muted),
        ],
    }
}

/// The workspace, with its branch in a git repository.
fn workspace(status: &SessionStatus) -> String {
    match &status.git {
        None => status.workspace.clone(),
        Some(git) => match &git.branch {
            Some(branch) => format!("{} · branch {branch}", status.workspace),
            None => format!("{} · detached", status.workspace),
        },
    }
}

/// The model, with its thinking level when it has one.
fn model(status: &SessionStatus) -> String {
    match &status.thinking {
        Some(level) => format!("{} · thinking {level}", status.model),
        None => status.model.clone(),
    }
}

/// The turns and the calls, or the calls alone when the session reports
/// no turn count.
fn turns(status: &SessionStatus) -> String {
    let calls = plural(status.calls, "call");
    match status.turns {
        Some(turns) => format!("{} · {calls}", plural(turns, "turn")),
        None => calls,
    }
}

/// `count` and `noun`, plural unless one.
fn plural(count: u64, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

#[cfg(test)]
#[path = "status_view_tests.rs"]
mod tests;
