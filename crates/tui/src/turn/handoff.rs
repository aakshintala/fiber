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
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::crash::{self, Aside};
use super::group::{dim_span, fit, spans_width};
use super::{Entry, Fold, Turn, open};
use crate::app::{Target, read};
use crate::ext::Input;
use crate::format;
use crate::rows::{RowText, Rows};
use crate::theme::{Role, style};

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
    /// It failed or was cancelled: the context is unchanged.
    Unchanged { cancelled: bool },
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
    /// The `handoff_started` envelope's `ts`, or the `handoff_completed`
    /// envelope's for a band the model started with its tool: the band's
    /// time reads from it.
    ts: u64,
    state: State,
    /// The note the model wrote, as it streamed.
    parts: Vec<Part>,
    /// A hook's note, in place of the model's.
    hook: Option<String>,
    open: bool,
}

impl Band {
    fn new(id: usize, trigger: String, ts: u64) -> Self {
        Self {
            id,
            trigger,
            ts,
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

    /// Its card entry: the band row without the note, whose toggle and
    /// body stay the terminal's. The context sizes ride the done
    /// state, so only a done band carries them.
    pub(crate) fn turn_entry(&self) -> crate::ext::TurnBand<'_> {
        let state = match self.state {
            State::Writing => crate::ext::TurnBandState::Writing,
            State::Done { before, after } => {
                crate::ext::TurnBandState::Done(crate::ext::TurnSizes { before, after })
            }
            State::Unchanged { cancelled } => crate::ext::TurnBandState::Unchanged { cancelled },
        };
        crate::ext::TurnBand {
            trigger: &self.trigger,
            ts: self.ts,
            state,
        }
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

    /// Its lines: the band row, exactly `width` cells, then "▸ note"
    /// once there is a note, and the note under it when open. The band
    /// row is one row on the `handoff` tint with half-block edges; the
    /// note rows sit under its lower edge on the same tint with no
    /// edges (`docs/tui.md`, "Handoff", "Look"). A bound `handoff_band`
    /// renderer replaces the band row only: the note toggle and body
    /// stay the terminal's, with the note's own target. A replacement
    /// of a band still being written spins like the built-in row; its
    /// lines carry no target.
    pub(crate) fn rows(&self, width: u16, zone: &jiff::tz::TimeZone, out: &mut Rows) {
        let max = usize::from(width);
        if max == 0 {
            // The band keeps its row, but zero width leaves no cells for the seam to paint.
            out.push((Line::default(), None));
            let band_at = out.len().saturating_sub(1);
            out.on_surface(band_at, width, Role::Handoff, crate::surface::Edges::BOTH);
            return;
        }
        let flat = out.seam.as_ref().is_some_and(|seam| seam.flat);
        let entry = self.turn_entry();
        let input = crate::ext::HandoffBand {
            trigger: entry.trigger,
            ts: entry.ts,
            state: entry.state,
            flat,
        };
        let wrapped = Input::HandoffBand(input);
        let builtin = |input: &Input<'_>, w: u16| {
            if !matches!(input, Input::HandoffBand(_)) {
                return Vec::new();
            }
            let (row, _) = self.band_row(usize::from(w), zone);
            vec![crate::ext::spans::from_ratatui(&row)]
        };
        let writing = matches!(self.state, State::Writing);
        let rendered = match out.fills_for("handoff_band") {
            crate::ext::SlotSelection::Draw { extension } => {
                out.ext_lines("handoff_band", &extension, &wrapped, width, &builtin)
            }
            crate::ext::SlotSelection::Absent | crate::ext::SlotSelection::Suppressed => None,
        };
        match rendered {
            Some(fitted) => {
                // The replacement owns the band row whole, tint and
                // edges included; the note below keeps its own.
                for line in fitted.iter() {
                    out.push_text(
                        (line.line.clone(), None),
                        RowText {
                            tail: line.pad,
                            links: line.links.clone(),
                            spin: writing.then_some(0),
                            ..RowText::plain()
                        },
                    );
                }
            }
            None => {
                let (row, spin) = self.band_row(max, zone);
                out.push_text(
                    (Line::from(row), None),
                    RowText {
                        spin,
                        ..RowText::plain()
                    },
                );
                let band_at = out.len().saturating_sub(1);
                out.on_surface(band_at, width, Role::Handoff, crate::surface::Edges::BOTH);
            }
        }
        let note = self.note();
        if matches!(self.state, State::Writing) || note.is_empty() {
            return;
        }
        let note_at = out.len();
        out.push((
            Line::raw("  ▸ note".to_owned()),
            Some(Target::Note(self.id)),
        ));
        if out.open_scope(Target::Note(self.id), self.open) {
            format::opened(&note, "    ", out);
        }
        out.end_scope();
        out.on_surface(
            note_at,
            width,
            Role::Handoff,
            crate::surface::Edges {
                top: false,
                bottom: false,
            },
        );
    }

    /// The band's content row, `max` cells, and the spinning column
    /// while the note is being written: the "●" that survives the fit,
    /// or `None` when the cut removes it.
    fn band_row(&self, max: usize, zone: &jiff::tz::TimeZone) -> (Vec<Span<'static>>, Option<u16>) {
        // " ⇄ " and "handoff" read bold in secondary
        // (`docs/tui.md`, "Handoff").
        let handoff = style(Role::Secondary).add_modifier(Modifier::BOLD);
        let left = vec![
            Span::styled(" ⇄ ".to_owned(), handoff),
            Span::styled("handoff".to_owned(), handoff),
            dim_span(format!(" · {}", self.trigger)),
        ];
        let time = crate::local_time::time_of_day(self.ts, zone)
            .map(|time| format!(" · {time} "))
            .unwrap_or_default();
        let (right, spin_at) = match &self.state {
            State::Writing => (
                vec![
                    Span::raw("●".to_owned()),
                    dim_span(format!(" writing the note…{time}")),
                ],
                true,
            ),
            State::Done { before, after } => {
                let after = after.map_or_else(|| "…".to_owned(), size);
                let sizes = format!("{} → {after}", size(*before));
                (
                    vec![
                        Span::styled(sizes, Style::new().add_modifier(Modifier::BOLD)),
                        dim_span(time),
                    ],
                    false,
                )
            }
            State::Unchanged { cancelled } => {
                let outcome = if *cancelled { "cancelled" } else { "failed" };
                (
                    vec![
                        Span::styled(
                            format!("{outcome} · the context is unchanged"),
                            style(Role::Attention),
                        ),
                        dim_span(time),
                    ],
                    false,
                )
            }
        };
        // The row fills `width` exactly; what does not fit is cut from
        // the left part with "…", so the right part keeps its columns.
        let left_width = spans_width(&left);
        let right_width = spans_width(&right);
        if left_width + 1 + right_width <= max {
            let fill = max - left_width - right_width;
            let mut row = left;
            row.push(dim_span(" ".repeat(fill)));
            let spin = spin_at.then(|| left_width + fill).and_then(try_col);
            row.extend(right);
            (row, spin)
        } else if max >= right_width + 2 {
            // One truncation: `fit` emits the row's single "…", so the
            // right part keeps its columns, and the "●" survives.
            let mut row = fit(left, max - right_width - 1);
            row.push(dim_span(" ".to_owned()));
            let spin = spin_at.then(|| max - right_width).and_then(try_col);
            row.extend(right);
            (row, spin)
        } else {
            // Even the right part does not fit: the whole row is cut to
            // `width` with one "…", and nothing spins.
            let mut row = left;
            row.push(dim_span(" ".to_owned()));
            row.extend(right);
            (fit(row, max), None)
        }
    }
}

/// A column that fits in a row's mark, or `None` past it.
fn try_col(col: usize) -> Option<u16> {
    u16::try_from(col).ok()
}

/// The size, for tests of its boundaries.
#[cfg(test)]
pub(super) fn size_for_tests(tokens: u64) -> String {
    size(tokens)
}

/// A context size: the figure of [`format::tokens`] without its unit,
/// and without a zero decimal, so whole thousands read "402k".
fn size(tokens: u64) -> String {
    let text = format::tokens(tokens);
    let text = text
        .strip_suffix(" tokens")
        .or_else(|| text.strip_suffix(" token"))
        .unwrap_or(&text);
    text.strip_suffix(".0k")
        .map(|kept| format!("{kept}k"))
        .or_else(|| text.strip_suffix(".0M").map(|kept| format!("{kept}M")))
        .unwrap_or_else(|| text.to_owned())
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
            | Entry::Aside(_)
            | Entry::Answers(_) => None,
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
            let ts = envelope.ts;
            open(turns).is_some_and(|turn| {
                turn.band(Band::new(id, trigger, ts));
                true
            })
        }),
        "handoff_completed" => read!(envelope, HandoffCompleted).is_some_and(|done| {
            let id = fold.id();
            let ts = envelope.ts;
            open(turns).is_some_and(|turn| {
                if turn.writing().is_none() {
                    turn.band(Band::new(id, BY_MODEL.to_owned(), ts));
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
                        Outcome::Failed => State::Unchanged { cancelled: false },
                        Outcome::Cancelled => State::Unchanged { cancelled: true },
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
/// another session, or an extension's own call, is not this context's, and
/// a call that reported no input tokens measured nothing, so the size waits
/// for the next (`docs/tui.md`, the handoff band).
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
    if context == 0 {
        return false;
    }
    let latest = turns
        .iter_mut()
        .rev()
        .flat_map(|turn| turn.entries.iter_mut().rev())
        .find_map(|entry| match entry {
            Entry::Band(band) => Some(band),
            Entry::Reply { .. }
            | Entry::Steer(_)
            | Entry::Group(_)
            | Entry::Aside(_)
            | Entry::Answers(_) => None,
        });
    match latest.map(|band| &mut band.state) {
        Some(State::Done {
            after: after @ None,
            ..
        }) => {
            *after = Some(context);
            true
        }
        Some(State::Done { .. } | State::Writing | State::Unchanged { .. }) | None => false,
    }
}

#[cfg(test)]
#[path = "handoff_tests.rs"]
mod tests;
