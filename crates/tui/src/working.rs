//! The working line's text (`docs/tui.md`, "The working line"): what a
//! running turn says while it works, laid out for the column it draws in.
//! Pure: the draw site asks the frame for the next change.

use std::ops::Range;

use crate::format;
use crate::turn::PendingRetry;

/// What the working line says: how long the turn runs, or its wait to
/// retry.
pub(crate) struct Working {
    /// The running turn's start, in milliseconds; none before its
    /// `turn_started` arrives.
    pub(crate) started_ms: Option<u64>,
    /// The open turn's wait to retry, while one pends.
    pub(crate) retry: Option<PendingRetry>,
}

/// The line laid out for its column.
pub(crate) struct Laid {
    /// The drawn text, cut at the column.
    pub(crate) text: String,
    /// The cells of "Working", for the glimmer; none while retrying.
    pub(crate) word: Option<Range<usize>>,
    /// The spinner's cell, for its colour; none when cut or retrying.
    pub(crate) spinner: Option<usize>,
    /// The cells of "esc to interrupt", while it shows.
    pub(crate) interrupt: Option<Range<u16>>,
    /// The retry form replaces the whole line.
    pub(crate) retrying: bool,
    /// The wall moment the text next changes; what the draw asks for.
    pub(crate) next_ms: Option<u64>,
}

/// The line's two-column indent (`docs/tui.md`, "The working line").
const INDENT: &str = "  ";
/// The working word the glimmer sweeps (`docs/tui.md`, "The working line").
const WORD: &str = "Working";
/// The spinner's cell: the indent wide (`docs/tui.md`, "The working line").
pub(crate) const SPINNER_AT: usize = 2;

/// Lays `working` out in `width` columns at wall time `now_ms`, with
/// `spinner` in its cell: the full line, or the detail shed first, then
/// the time, with the word cut last. The interrupt target goes with its
/// text.
pub(crate) fn lay(working: &Working, now_ms: Option<u64>, spinner: &str, width: u16) -> Laid {
    if let Some(retry) = &working.retry {
        return retry_laid(retry, now_ms, width);
    }
    let time = match (now_ms, working.started_ms) {
        (Some(now), Some(started)) => Some(format::duration(now.saturating_sub(started))),
        _ => None,
    };
    let head = match &time {
        Some(time) => format!("{INDENT}{spinner} {WORD} {time}"),
        None => format!("{INDENT}{spinner} {WORD}"),
    };
    let tail = " · esc to interrupt";
    let room = usize::from(width);
    let (text, interrupt) = if format::width(&head) + format::width(tail) <= room {
        let start = u16::try_from(format::width(&head) + format::width(" · ")).unwrap_or(u16::MAX);
        let end = u16::try_from(format::width(&head) + format::width(tail)).unwrap_or(u16::MAX);
        (format!("{head}{tail}"), Some(start..end))
    } else if format::width(&head) <= room {
        (head, None)
    } else {
        let bare = format!("{INDENT}{spinner} {WORD}");
        (format::cut(&bare, room), None)
    };
    // The word's cells start past the indent, the spinner and one space.
    let at = format::width(INDENT)
        .saturating_add(format::width(spinner))
        .saturating_add(1);
    let word =
        at..at.saturating_add(format::width(WORD).min(format::width(&text).saturating_sub(at)));
    // The spinner drew whole while the text reaches past its cell.
    let spinner = (format::width(&text) >= SPINNER_AT.saturating_add(format::width(spinner)))
        .then_some(SPINNER_AT);
    Laid {
        text,
        word: Some(word),
        spinner,
        interrupt,
        retrying: false,
        next_ms: next_second(now_ms, working.started_ms),
    }
}

/// The wall moment the elapsed time next ticks over: one second past the
/// second it shows.
fn next_second(now_ms: Option<u64>, started_ms: Option<u64>) -> Option<u64> {
    let (now, started) = (now_ms?, started_ms?);
    let elapsed = now.saturating_sub(started);
    started.checked_add((elapsed / 1000).saturating_add(1).saturating_mul(1000))
}

/// The retry form: the countdown, floored at zero, indented like the
/// working line and cut at the column. It has no spinner, no word and no
/// interrupt target; Esc still interrupts.
fn retry_laid(retry: &PendingRetry, now_ms: Option<u64>, width: u16) -> Laid {
    let wait = retry.retry.delay_ms;
    let end = retry.ts.saturating_add(wait);
    let secs = match now_ms {
        Some(now) => end.saturating_sub(now).div_ceil(1000),
        None => wait.div_ceil(1000),
    };
    let room = usize::from(width).saturating_sub(format::width(INDENT));
    let text = format!(
        "{INDENT}{}",
        format::cut(
            &format::retry(&retry.retry, retry.attempt, secs).to_string(),
            room,
        ),
    );
    Laid {
        text,
        word: None,
        spinner: None,
        interrupt: None,
        retrying: true,
        next_ms: match (now_ms, secs) {
            (Some(_), 1..) => end.checked_sub(secs.saturating_sub(1).saturating_mul(1000)),
            _ => None,
        },
    }
}

#[cfg(test)]
#[path = "working_tests.rs"]
mod tests;
