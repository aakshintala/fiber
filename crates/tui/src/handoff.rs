//! The handoff band and the nudge (`docs/tui.md`, "Handoff").
//!
//! A handoff breaks the turn's card: the band stands where it ran, and the
//! rows after it are the second card. While the note is being written, the
//! assistant's text is the note and goes into the band. A child of `turn`,
//! so it places the band in a card.

use contract::Envelope;
use contract::events::{
    ContextNudged, HandoffCompleted, HandoffStarted, HandoffTrigger, Note, Outcome, PreambleBuilt,
    UsageRecorded,
};
use ratatui::style::{Color, Style};
use ratatui::text::Line;

use super::crash::{self, Aside};
use super::{Entry, Fold, Turn, open};
use crate::app::{Target, read};
use crate::format;
use crate::turn::Row;

/// The band's tint.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
const TINT: Style = Style::new().bg(Color::Indexed(23));

/// What the trigger reads for a handoff the model started with its tool,
/// which writes no `handoff_started`.
const BY_MODEL: &str = "the model handed off";

/// How a handoff stands.
#[derive(Debug, Clone)]
enum State {
    /// The note is being written.
    Writing,
    /// It completed: the size before, and after once the first request
    /// after it returns.
    Done { before: u64, after: Option<u64> },
    /// It failed or was cancelled: the context is unchanged; the error's
    /// message on a failure.
    Unchanged(Option<String>),
}

/// One text part of the note.
#[derive(Debug, Clone)]
struct Part {
    action: String,
    text: String,
    done: bool,
}

/// A handoff's band.
#[derive(Debug, Clone)]
pub(crate) struct Band {
    id: usize,
    trigger: String,
    state: State,
    /// The note the model wrote, as it streamed.
    parts: Vec<Part>,
    /// A hook's note, in place of the model's.
    hook: Option<String>,
    open: bool,
}

impl Band {
    fn new(id: usize, trigger: String) -> Self {
        Self {
            id,
            trigger,
            state: State::Writing,
            parts: Vec::new(),
            hook: None,
            open: false,
        }
    }

    /// The note's text.
    fn note(&self) -> String {
        match &self.hook {
            Some(text) => text.clone(),
            None => self
                .parts
                .iter()
                .map(|part| part.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    /// A note delta: it joins the action's part still streaming.
    fn delta(&mut self, action: &str, text: &str) {
        match self
            .parts
            .iter_mut()
            .rev()
            .find(|part| part.action == action && !part.done)
        {
            Some(part) => part.text.push_str(text),
            None => self.parts.push(Part {
                action: action.to_owned(),
                text: text.to_owned(),
                done: false,
            }),
        }
    }

    /// A note part completed: it replaces what streamed for it.
    fn completed(&mut self, action: &str, text: String) {
        match self
            .parts
            .iter_mut()
            .find(|part| part.action == action && !part.done)
        {
            Some(part) => {
                part.text = text;
                part.done = true;
            }
            None => self.parts.push(Part {
                action: action.to_owned(),
                text,
                done: true,
            }),
        }
    }

    /// Sets its note's state; false when `target` is not this band's.
    pub(crate) fn set_open(&mut self, target: &Target, open: bool) -> bool {
        let mine = target == &Target::Note(self.id);
        if mine {
            self.open = open;
        }
        mine
    }

    /// Toggles its note, returning the new state when `target` is this band's.
    pub(crate) fn toggle(&mut self, target: Target) -> Option<bool> {
        if target == Target::Note(self.id) {
            self.open = !self.open;
            Some(self.open)
        } else {
            None
        }
    }

    /// Its lines: the band, then "▸ note" once there is a note, and the
    /// note under it when open.
    pub(crate) fn rows(&self, out: &mut Vec<Row>) {
        let state = match &self.state {
            State::Writing => "writing the note…".to_owned(),
            State::Done { before, after } => {
                let after = after.map_or_else(|| "…".to_owned(), size);
                format!("{} → {after}", size(*before))
            }
            State::Unchanged(None) => "context unchanged".to_owned(),
            State::Unchanged(Some(message)) => format!("context unchanged · {message}"),
        };
        let band = format!("⇄ Handoff · {} · {state}", self.trigger);
        out.push((Line::styled(band, TINT), None));
        let note = self.note();
        if matches!(self.state, State::Writing) || note.is_empty() {
            return;
        }
        out.push((
            Line::styled("  ▸ note".to_owned(), TINT),
            Some(Target::Note(self.id)),
        ));
        if self.open {
            format::opened(&note, "    ", out);
        }
    }
}

/// A context size: the figure of [`format::tokens`] without its unit.
fn size(tokens: u64) -> String {
    let text = format::tokens(tokens);
    text.strip_suffix(" tokens")
        .or_else(|| text.strip_suffix(" token"))
        .unwrap_or(&text)
        .to_owned()
}

/// What started a handoff, as the band says it; `at` is the automatic
/// trigger's size, when automatic handoff is on.
fn trigger(trigger: HandoffTrigger, at: Option<u64>) -> String {
    match (trigger, at) {
        (HandoffTrigger::Auto, Some(at)) => format!("automatic at {}", size(at)),
        (HandoffTrigger::Auto, None) => "automatic".to_owned(),
        (HandoffTrigger::Person, _) => "you asked with /handoff".to_owned(),
        (HandoffTrigger::Overflow, _) => "the request did not fit".to_owned(),
    }
}

impl Turn {
    /// The band whose note is being written, if any.
    pub(super) fn writing(&mut self) -> Option<&mut Band> {
        self.entries.iter_mut().rev().find_map(|entry| match entry {
            Entry::Band(band) if matches!(band.state, State::Writing) => Some(band),
            Entry::Band(_)
            | Entry::Reply { .. }
            | Entry::Steer(_)
            | Entry::Group(_)
            | Entry::Aside(_) => None,
        })
    }

    /// Assistant text while a note is being written: the note's, not a
    /// reply; false when no note is being written.
    pub(super) fn note_text(&mut self, action: &str, text: &str, completed: bool) -> bool {
        let Some(band) = self.writing() else {
            return false;
        };
        if completed {
            band.completed(action, text.to_owned());
        } else {
            band.delta(action, text);
        }
        true
    }

    /// A new band, which ends the open group: what follows is the second
    /// card.
    fn band(&mut self, band: Band) {
        self.open_group = None;
        self.entries.push(Entry::Band(band));
    }
}

/// Folds `preamble_built`, `handoff_started`, `handoff_completed` and
/// `context_nudged`; false when no card changed.
pub(crate) fn fold(turns: &mut [Turn], fold: &mut Fold, envelope: &Envelope) -> bool {
    match envelope.kind.as_str() {
        "preamble_built" => {
            if let Some(built) = read!(envelope, PreambleBuilt) {
                fold.trigger_at = built.trigger_at;
            }
            false
        }
        "handoff_started" => read!(envelope, HandoffStarted).is_some_and(|started| {
            let trigger = trigger(started.trigger, fold.trigger_at);
            let id = fold.id();
            open(turns).is_some_and(|turn| {
                turn.band(Band::new(id, trigger));
                true
            })
        }),
        "handoff_completed" => read!(envelope, HandoffCompleted).is_some_and(|done| {
            let id = fold.id();
            open(turns).is_some_and(|turn| {
                if turn.writing().is_none() {
                    turn.band(Band::new(id, BY_MODEL.to_owned()));
                }
                if let Some(band) = turn.writing() {
                    if let Some(Note::Hook { note_text, .. }) = done.note {
                        band.hook = Some(note_text);
                    }
                    band.state = match done.outcome {
                        Outcome::Completed => State::Done {
                            before: done.tokens_before,
                            after: None,
                        },
                        Outcome::Failed => State::Unchanged(done.error.map(|error| error.message)),
                        Outcome::Cancelled => State::Unchanged(None),
                    };
                }
                true
            })
        }),
        "context_nudged" => read!(envelope, ContextNudged).is_some_and(|nudged| {
            let line = format::dim(format!(
                "◔ Context at {}, two thirds of the way to the {} handoff. The model was told \
                 a handoff keeps the work going.",
                size(nudged.tokens),
                size(nudged.trigger_at)
            ));
            crash::place(turns, fold, Aside::Line(line));
            true
        }),
        _ => false,
    }
}

/// A new model call's `usage_recorded`: the latest completed handoff still
/// waiting for its size after takes the call's context size. A copy from
/// another session, or an extension's own call, is not this context's.
pub(crate) fn sized(turns: &mut [Turn], line: &UsageRecorded) -> bool {
    if line.origin_session_id.is_some() || line.extension.is_some() {
        return false;
    }
    let tokens = &line.tokens;
    let context = tokens
        .cache_write
        .values()
        .fold(tokens.input, |sum, n| sum.saturating_add(*n))
        .saturating_add(tokens.cache_read);
    let latest = turns
        .iter_mut()
        .rev()
        .flat_map(|turn| turn.entries.iter_mut().rev())
        .find_map(|entry| match entry {
            Entry::Band(band) => Some(band),
            Entry::Reply { .. } | Entry::Steer(_) | Entry::Group(_) | Entry::Aside(_) => None,
        });
    match latest.map(|band| &mut band.state) {
        Some(State::Done {
            after: after @ None,
            ..
        }) => {
            *after = Some(context);
            true
        }
        Some(State::Done { .. } | State::Writing | State::Unchanged(_)) | None => false,
    }
}

#[cfg(test)]
#[path = "handoff_tests.rs"]
mod tests;
