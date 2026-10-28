//! One turn's card as surfaces: the prompt bubble above, the entries and
//! the ending on the turn tint with half-block edges, broken at a
//! handoff's band (`docs/tui.md`, "Look", "Turns", "Handoff"). A child of
//! `turn`, so it reads the card's fields.

use contract::Seq;
use contract::events::{TurnCompleted, TurnOutcome};
use jiff::tz::TimeZone;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{Ending, Entry, Message, MessageSource, Turn};
use crate::ext::{
    CardPrompt, Input, PromptImage, TurnCard, TurnCardState, TurnEnded, TurnEntry, TurnError,
};
use crate::format;
use crate::rows::Rows;
use crate::surface::{self, Edges};
use crate::theme::Role;
use std::borrow::Cow;
use std::collections::HashMap;

/// The whole-turn totals a page's draw supplies for the turn they
/// close: the draw renders them without cloning or mutating the card.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DrawSummary<'a> {
    pub(crate) started: u64,
    pub(crate) calls: u64,
    pub(crate) spend: &'a format::Spend,
    pub(crate) ended: Option<&'a (TurnCompleted, u64)>,
}

/// Whether the card's first piece and its last piece drew any row, edges
/// not counted; one piece with no band. A card with no piece row: both
/// false.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pieces {
    pub(crate) first: bool,
    pub(crate) last: bool,
}

/// Draws one block's rows with `draw` at the card's inner width, padding
/// them past it; a blank row separates it from the block before when both
/// drew, or from the piece's withheld top edge. Whether this block drew.
fn block(
    out: &mut Rows,
    inner: u16,
    pad: u16,
    separates: bool,
    draw: impl FnOnce(&mut Rows),
) -> bool {
    let at = out.len();
    draw(out);
    if out.len() == at {
        return false;
    }
    out.pad(at, pad, inner);
    if separates {
        out.gap(at);
    }
    true
}

impl Turn {
    /// Notes `seq`, the line just folded into the card, as the source of
    /// what it began: the bubble on the card's first line, each entry it
    /// added, and the ending once the card closes. Entries a line with
    /// no `seq` added take the next line's that has one.
    pub(crate) fn note_seq(&mut self, seq: Option<Seq>) {
        if self.first_seq.is_none() {
            self.first_seq = seq;
        }
        self.entry_seqs.resize(self.entries.len(), None);
        if let Some(seq) = seq {
            for slot in self
                .entry_seqs
                .iter_mut()
                .rev()
                .take_while(|slot| slot.is_none())
            {
                *slot = Some(seq);
            }
        }
        if self.ended.is_some() && self.end_seq.is_none() {
            self.end_seq = seq;
        }
    }

    /// The bubble and its time row, then the card's pieces on `turn`,
    /// each broken at a handoff's band; `edges` says whether the first
    /// piece draws its top edge and the last its bottom one
    /// (`docs/tui.md`, "Look", "Turns", "Handoff"). `layout` names
    /// the session's images, so each draws as its one clickable line
    /// (`docs/tui.md`, "Images").
    pub(crate) fn rows(
        &self,
        width: u16,
        zone: &TimeZone,
        edges: Edges,
        layout: &crate::image::Layout,
        out: &mut Rows,
    ) -> Pieces {
        self.rows_with_summary(width, zone, edges, layout, None, out)
    }

    /// The card's renderer input: its prompts, every entry inlined, and
    /// its ending. `summary` names the whole-turn totals the closing
    /// turn draws in place of the card's own, as [`card_rows`] does, so
    /// a bound `turn_card` renderer sees the same rows it decorates.
    /// `running` is the event-derived turn still running, not click-set
    /// row state. A group entry's `running` is the turn running with
    /// that group as its open one.
    pub(crate) fn card_input<'a>(
        &'a self,
        summary: Option<DrawSummary<'a>>,
        flat: bool,
    ) -> TurnCard<'a> {
        self.card_input_with_names(summary, flat, &std::collections::HashMap::new())
    }

    pub(crate) fn card_input_with_names<'a>(
        &'a self,
        summary: Option<DrawSummary<'a>>,
        flat: bool,
        names: &std::collections::HashMap<contract::SessionId, String>,
    ) -> TurnCard<'a> {
        let ended = summary.as_ref().and_then(|view| view.ended);
        let running = ended.is_none() && self.ended.is_none();
        let state = match ended {
            Some((done, ts)) => TurnCardState::Ended(ended_of(done, *ts)),
            None => match &self.ended {
                Some((Ending::Done(done), ts)) => TurnCardState::Ended(ended_of(done, *ts)),
                Some((Ending::CutShort, ts)) => {
                    TurnCardState::Ended(TurnEnded::CutShort { ended_ts: *ts })
                }
                None => TurnCardState::Running,
            },
        };
        TurnCard {
            prompts: self
                .prompts
                .iter()
                .map(|prompt| CardPrompt {
                    text: &prompt.text,
                    images: prompt
                        .images
                        .iter()
                        .map(|part| PromptImage {
                            path: &part.path,
                            width: part.width,
                            height: part.height,
                        })
                        .collect(),
                })
                .collect(),
            entries: self
                .entries
                .iter()
                .filter_map(|entry| match entry {
                    Entry::Reply { reply, .. } => Some(TurnEntry::Reply { text: reply.text() }),
                    Entry::Steer(steered) => Some(steered.turn_entry()),
                    Entry::Group(at) => self.groups.get(*at).map(|group| {
                        TurnEntry::Group(group.turn_entry(running && self.open_group == Some(*at)))
                    }),
                    Entry::Aside(aside) => Some(TurnEntry::Aside(aside.turn_input())),
                    Entry::Band(band) => Some(TurnEntry::Band(band.turn_entry())),
                    Entry::Answers(answered) => Some(TurnEntry::Answers(answered.turn_entry())),
                    Entry::Message(message) => {
                        let source = match &message.source {
                            MessageSource::Fiber => "fiber",
                            MessageSource::Session => "session",
                            MessageSource::Extension(_) => "extension",
                        };
                        Some(TurnEntry::Message {
                            source,
                            label: message.label(names),
                            text: &message.text,
                            images: message
                                .images
                                .iter()
                                .map(|part| PromptImage {
                                    path: &part.path,
                                    width: part.width,
                                    height: part.height,
                                })
                                .collect(),
                            ts: message.ts,
                        })
                    }
                })
                .collect(),
            started: summary.as_ref().map_or(self.started, |view| view.started),
            calls: summary.as_ref().map_or(self.calls, |view| view.calls),
            usage: summary
                .as_ref()
                .map_or_else(|| self.spend.usage(), |view| view.spend.usage()),
            state,
            flat,
        }
    }

    /// The same card with the whole-turn totals `summary` names drawn in
    /// place of the card's own, for the turn the summary closes; any
    /// other turn draws its own fields. The view is borrowed, so recounts
    /// neither clone nor mutate the open turn. A bound `turn_card`
    /// renderer draws the card whole: no nested slot runs inside a
    /// replacement, and the built-in the decorator calls draws with
    /// none bound, so it never re-enters the seam.
    pub(crate) fn rows_with_summary(
        &self,
        width: u16,
        zone: &TimeZone,
        edges: Edges,
        layout: &crate::image::Layout,
        summary: Option<DrawSummary<'_>>,
        out: &mut Rows,
    ) -> Pieces {
        match out.fills_for("turn_card") {
            crate::ext::SlotSelection::Draw { extension } => {
                let flat = out.seam.as_ref().is_some_and(|seam| seam.flat);
                let input = if out.session_names.is_empty() {
                    self.card_input(summary, flat)
                } else {
                    self.card_input_with_names(summary, flat, &out.session_names)
                };
                let wrapped = Input::TurnCard(input);
                let builtin = |input: &Input<'_>, w: u16| {
                    if !matches!(input, Input::TurnCard(_)) {
                        return Vec::new();
                    }
                    let mut scratch = Rows::default();
                    scratch.session_names = std::sync::Arc::clone(&out.session_names);
                    self.card_rows(w, zone, edges, layout, summary, &mut scratch);
                    let (rows, _) = scratch.into_parts();
                    rows.into_iter()
                        .map(|(line, _)| crate::ext::spans::from_ratatui(&line.spans))
                        .collect()
                };
                if let Some(fitted) =
                    out.ext_lines("turn_card", &extension, &wrapped, width, &builtin)
                {
                    let drew = !fitted.is_empty();
                    for line in fitted.iter() {
                        out.push_fitted(line, None);
                    }
                    return Pieces {
                        first: drew,
                        last: drew,
                    };
                }
            }
            crate::ext::SlotSelection::Absent | crate::ext::SlotSelection::Suppressed => {}
        }
        self.card_rows(width, zone, edges, layout, summary, out)
    }

    /// The card's built-in rows, drawn when no `turn_card` replacement
    /// stands: the bubble and its time row, then the pieces on `turn`.
    fn card_rows(
        &self,
        width: u16,
        zone: &TimeZone,
        edges: Edges,
        layout: &crate::image::Layout,
        summary: Option<DrawSummary<'_>>,
        out: &mut Rows,
    ) -> Pieces {
        crate::work::add(|work| work.turn_rows += 1);
        let started = summary.as_ref().map_or(self.started, |view| view.started);
        let calls = summary.as_ref().map_or(self.calls, |view| view.calls);
        let spend: &format::Spend = summary.as_ref().map_or(&self.spend, |view| view.spend);
        let open = summary.as_ref().and_then(|view| view.ended).is_none() && self.is_open();
        let prompts_at = out.len();
        out.set_source(self.first_seq);
        for prompt in &self.prompts {
            let before = out.len();
            super::images::bubble_rows(prompt, width, layout, out);
            if out.len() > before {
                out.gap(before);
                if let Some(time) = crate::local_time::time_of_day(started, zone) {
                    out.push((format::dim(format!("you {time} ")).right_aligned(), None));
                }
            }
        }
        let card_at = out.len();
        let led = card_at > prompts_at;
        let inner = surface::inset(width);
        let pad = width.saturating_sub(inner) / 2;
        let mut from = out.len();
        let mut top = edges.top;
        let mut first = false;
        let mut broken = false;
        // Whether the next drawn block separates from the block before
        // or from the piece's withheld top edge with a blank row.
        let mut separates = !top;
        let mut piece_drew = false;
        // Whether a band drew since the last block: no blank row goes
        // next to a handoff band (`docs/tui.md`, "Turns", "Handoff").
        let mut after_band = false;
        for (index, entry) in self.entries.iter().enumerate() {
            out.set_source(self.entry_seqs.get(index).copied().flatten());
            if let Entry::Band(band) = entry {
                if !broken {
                    first = piece_drew;
                    broken = true;
                }
                out.on_surface(from, width, Role::Turn, Edges { top, bottom: true });
                band.rows(width, zone, out);
                from = out.len();
                top = true;
                separates = false;
                piece_drew = false;
                after_band = true;
                continue;
            }
            let gap_before = separates;
            let drew = block(out, inner, pad, gap_before, |out| match entry {
                Entry::Reply { reply, .. } => reply.rows(inner, out),
                Entry::Steer(steered) => steered.rows(inner, zone, layout, out),
                Entry::Group(at) => {
                    if let Some(group) = self.groups.get(*at) {
                        group.rows(open && self.open_group == Some(*at), inner, layout, out);
                    }
                }
                Entry::Aside(aside) => aside.rows(inner, out),
                Entry::Band(_) => {}
                Entry::Answers(answered) => answered.rows(inner, zone, out),
                Entry::Message(message) => message_rows(message, inner, zone, layout, out),
            });
            piece_drew = piece_drew || drew;
            if drew {
                separates = true;
                after_band = false;
            }
        }
        let summary_ended = summary.as_ref().and_then(|view| view.ended);
        // The summary names the closing turn's ending; any other turn
        // draws its own. One selection, so the error and outcome
        // formatting below runs once.
        let done = match summary_ended {
            Some((done, _)) => Some(done),
            None => match &self.ended {
                Some((Ending::Done(done), _)) => Some(done),
                _ => None,
            },
        };
        if summary_ended.is_some() || self.ended.is_some() {
            out.set_source(self.end_seq);
            let drew = block(out, inner, pad, separates, |out| {
                let head = match done {
                    Some(done) => {
                        if let (TurnOutcome::Failed, Some(error)) = (done.outcome, &done.error) {
                            format::failure(error, inner, out);
                        }
                        match done.outcome {
                            TurnOutcome::Completed => "▣ completed",
                            TurnOutcome::Interrupted => "▣ interrupted",
                            TurnOutcome::Failed => "▣ failed",
                        }
                    }
                    None => "▣ cut short: Fiber stopped",
                };
                let ts = summary_ended.map_or_else(
                    || self.ended.as_ref().map_or(0, |(_, ts)| *ts),
                    |(_, ts)| *ts,
                );
                let ms = ts.saturating_sub(started);
                let closing = format::closing(head, ms, calls, &spend.usage());
                out.push((format::dim(closing), None));
            });
            piece_drew = piece_drew || drew;
            if drew {
                after_band = false;
            }
        }
        let last = piece_drew;
        if !broken {
            first = last;
        }
        out.on_surface(
            from,
            width,
            Role::Turn,
            Edges {
                top,
                bottom: edges.bottom,
            },
        );
        if led && (first || last) {
            out.gap(card_at);
        }
        // A bubble whose card draws nothing yet, on a turn still
        // running: the blank row one fold has ahead of its card, in the
        // same place the blank before the card takes once it draws
        // (`docs/tui.md`, "Turns", "History and paging").
        if led && !last && !after_band {
            out.gap(out.len());
        }
        out.set_source(None);
        Pieces { first, last }
    }
}

/// The card's ending input for one `turn_completed`: the outcome with
/// its timestamp, and on a failure the error the ✗ lines draw.
fn ended_of(done: &TurnCompleted, ts: u64) -> TurnEnded<'_> {
    match done.outcome {
        TurnOutcome::Completed => TurnEnded::Completed { ended_ts: ts },
        TurnOutcome::Interrupted => TurnEnded::Interrupted { ended_ts: ts },
        TurnOutcome::Failed => TurnEnded::Failed {
            ended_ts: ts,
            error: done.error.as_ref().map(|error| TurnError {
                code: error.code.clone(),
                message: &error.message,
            }),
        },
    }
}

impl Message {
    fn label<'a>(&'a self, names: &HashMap<contract::SessionId, String>) -> Cow<'a, str> {
        match &self.source {
            MessageSource::Fiber => Cow::Borrowed("Fiber"),
            MessageSource::Session => self
                .sender_id
                .as_ref()
                .and_then(|id| names.get(id))
                .filter(|name| !name.is_empty())
                .cloned()
                .map(Cow::Owned)
                .or_else(|| {
                    self.sender_id
                        .as_ref()
                        .map(|id| Cow::Borrowed(id.0.as_str()))
                })
                .unwrap_or_default(),
            MessageSource::Extension(name) => Cow::Borrowed(name),
        }
    }
}

/// Draws one non-driver message as a labelled rule and its text/images.
fn message_rows(
    message: &Message,
    width: u16,
    zone: &TimeZone,
    layout: &crate::image::Layout,
    out: &mut Rows,
) {
    let muted = matches!(&message.source, MessageSource::Fiber);
    let label = message.label(&out.session_names).into_owned();
    let time = crate::local_time::time_of_day(message.ts, zone)
        .map_or_else(String::new, |time| format!(" · {time} "));
    let text = if matches!(&message.source, MessageSource::Fiber) {
        message
            .text
            .strip_prefix("Fiber: ")
            .unwrap_or(&message.text)
    } else {
        &message.text
    };
    let tail = format!("{label}{time}");
    let dashes = usize::from(width).saturating_sub(crate::format::width(&tail).saturating_add(1));
    let label_style = if muted {
        crate::theme::style(Role::Muted)
    } else {
        crate::theme::style(Role::Secondary)
    };
    let mut spans = vec![Span::styled(label, label_style)];
    if !time.is_empty() {
        spans.push(Span::styled(
            time,
            Style::default().add_modifier(Modifier::DIM),
        ));
    }
    if dashes > 0 {
        spans.push(Span::styled(
            "─".repeat(dashes),
            crate::theme::style(Role::Rule),
        ));
    }
    out.push_text(
        (Line::from(spans), None),
        crate::rows::RowText {
            tail: u16::try_from(dashes).unwrap_or(u16::MAX),
            ..crate::rows::RowText::plain()
        },
    );
    if !text.trim().is_empty() {
        let style = if muted {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default()
        };
        for (row, join) in crate::format::wrap_joined(text, usize::from(width)) {
            out.push_text(
                (Line::from(Span::styled(row, style)), None),
                crate::rows::RowText {
                    join,
                    ..crate::rows::RowText::plain()
                },
            );
        }
    }
    super::images::steer_rows(&message.images, width, layout, out);
}

#[cfg(test)]
#[path = "card_tests.rs"]
mod tests;
