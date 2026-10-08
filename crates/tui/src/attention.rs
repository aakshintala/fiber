//! What the terminal does with the hub's `attention` lines (`docs/tui.md`,
//! "Getting the person's attention"; `docs/invocation.md`, "Attention").

use serde_json::{Map, Value};

/// `tui.attention.*`: each defaults to on (`docs/configuration.md`, "Keys").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attention {
    /// Whether the terminal sends an OSC 9 desktop notification.
    pub notification: bool,
    /// Whether it rings the bell where OSC 9 is not supported.
    pub bell: bool,
    /// Whether the terminal title shows the session's state.
    pub title: bool,
}

impl Default for Attention {
    fn default() -> Self {
        Self {
            notification: true,
            bell: true,
            title: true,
        }
    }
}

/// Why a session needs the person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reason {
    /// Waiting on the person, with the request's summary.
    Waiting { summary: String },
    /// Its turn finished.
    Finished,
}

/// One `attention` line, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Line {
    /// The session needing the person.
    pub(crate) session: contract::SessionId,
    /// Its name, or its id when it has none.
    pub(crate) name: String,
    /// Why it needs the person.
    pub(crate) reason: Reason,
}

/// Whether the terminal supports OSC 9, read once at start through `var`
/// (`docs/tui.md`, "Getting the person's attention"): a terminal that
/// speaks it, and no multiplexer in between to swallow the escape.
pub(crate) fn supported(var: impl Fn(&str) -> Option<String>) -> bool {
    let program = var("TERM_PROGRAM").unwrap_or_default();
    let term = var("TERM").unwrap_or_default();
    let terminal = program == "ghostty"
        || program == "iTerm.app"
        || program == "WezTerm"
        || term == "xterm-ghostty"
        || term == "xterm-kitty"
        || var("KITTY_WINDOW_ID").is_some();
    terminal && var("TMUX").is_none() && var("STY").is_none()
}

/// Reads one `attention` line's payload: `None` when it has no string
/// `session_id`, or a `reason` other than `waiting` or `finished`
/// (`docs/tui.md`, "Getting the person's attention"). A missing or empty
/// `name` reads as the session id; a missing `summary` reads as empty.
pub(crate) fn parse(payload: &Map<String, Value>) -> Option<Line> {
    let session = payload
        .get("session_id")
        .and_then(Value::as_str)
        .map(|id| contract::SessionId(id.to_owned()))?;
    let reason = match payload.get("reason").and_then(Value::as_str) {
        Some("waiting") => Reason::Waiting {
            summary: payload
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        Some("finished") => Reason::Finished,
        _ => return None,
    };
    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or(&session.0)
        .to_owned();
    Some(Line {
        session,
        name,
        reason,
    })
}

/// Drops every control character (C0, C1 and DEL) from `text`, then cuts
/// it to `len` characters, never by column width: a name or a summary can
/// never end the escape sequence or start another.
fn scrub(text: &str, len: usize) -> String {
    text.chars()
        .filter(|ch| !ch.is_control())
        .take(len)
        .collect()
}

/// The notification's text (`docs/tui.md`, "Getting the person's
/// attention"). It always starts `Fiber:`, so it never starts with a digit
/// a terminal could read as an OSC 9 progress report.
pub(crate) fn text(line: &Line) -> String {
    let name = scrub(&line.name, 60);
    match &line.reason {
        Reason::Waiting { summary } => {
            let summary = scrub(summary, 120);
            if summary.is_empty() {
                format!("Fiber: {name} needs you")
            } else {
                format!("Fiber: {name} needs you: {summary}")
            }
        }
        Reason::Finished => format!("Fiber: {name} finished"),
    }
}

/// The bytes one `attention` line writes (`docs/tui.md`, "Getting the
/// person's attention"): the OSC 9 notification where supported, else one
/// bell, else nothing.
pub(crate) fn bytes(line: &Line, settings: Attention, osc9: bool) -> Vec<u8> {
    if settings.notification && osc9 {
        crate::osc::notify(&text(line))
    } else if !osc9 && settings.bell {
        vec![0x07]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
#[path = "attention_tests.rs"]
mod tests;
