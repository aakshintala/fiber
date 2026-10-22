//! One approval: the call it asks about, the person's choice so far, and
//! the panel text it draws (`docs/tui.md`, "An approval").

use contract::commands::{Remember, RememberScope, ReplyAnswer};
use contract::events::{AskStep, Decision, Escalation, RuleOffer, RuleScope};
use contract::shapes::Effect;
use ratatui::style::{Modifier, Style};

use super::Mark;
use crate::format;
use crate::keys::{Edit, Key};
use std::ops::Range;

/// A choice on the approval panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Choice {
    /// Allow once.
    Once,
    /// Allow for this session.
    Session,
    /// Always allow in this project.
    Project,
    /// Deny, with the typed feedback.
    Deny,
}

/// An approval: the call it asks about and the person's choice so far.
#[derive(Debug, Clone)]
pub(super) struct Approval {
    /// The asking call's tool and main argument, when its
    /// `tool_call_requested` was seen.
    pub(super) call: Option<(String, String)>,
    pub(super) reversible: bool,
    pub(super) effects: Vec<Effect>,
    pub(super) paths: Vec<String>,
    pub(super) step: AskStep,
    /// What the person typed, kept while the request waits.
    pub(super) feedback: String,
    pub(super) cursor: Choice,
}

/// "Approval k of n": bold in `attention` (`docs/tui.md`, "An approval").
const HEAD: Style = Style::new()
    .fg(crate::theme::Role::Attention.color())
    .add_modifier(Modifier::BOLD);

/// The asking session: in `attention` (`docs/tui.md`, "An approval").
const SESSION: Style = Style::new().fg(crate::theme::Role::Attention.color());

/// "irreversible" in the header: bold, in the error colour, on whatever
/// tint asked (`docs/tui.md`, "An approval").
const LOUD: Style = Style::new()
    .fg(crate::theme::Role::Error.color())
    .add_modifier(Modifier::BOLD);

/// A dim run: the right-aligned hint in the header, the facts line, a
/// choice number, the empty feedback and the hint
/// (`docs/tui.md`, "An approval").
const DIM: Style = Style::new().add_modifier(Modifier::DIM);

/// Bold: a focused choice's label (`docs/tui.md`, "An approval").
const BOLD: Style = Style::new().add_modifier(Modifier::BOLD);

/// A focused choice's label over a dim run: the empty feedback
/// (`docs/tui.md`, "An approval").
const DIM_BOLD: Style = Style::new()
    .add_modifier(Modifier::DIM)
    .add_modifier(Modifier::BOLD);

/// A focused choice's label over an `info` run: a remembering choice's
/// rule prefix and " *" (`docs/tui.md`, "An approval").
const REMEMBER_BOLD: Style = Style::new()
    .fg(crate::theme::Role::Info.color())
    .add_modifier(Modifier::BOLD);

/// The tool's name: bold in `info` (`docs/tui.md`, "An approval").
const TOOL: Style = Style::new()
    .fg(crate::theme::Role::Info.color())
    .add_modifier(Modifier::BOLD);

/// The tool's main argument: bold (`docs/tui.md`, "An approval").
const ARGUMENT: Style = Style::new().add_modifier(Modifier::BOLD);

/// The focused choice's mark: in `attention` (`docs/tui.md`, "An approval").
const FOCUS: Style = Style::new().fg(crate::theme::Role::Attention.color());

/// A remembering choice's rule prefix and " *": in `info`
/// (`docs/tui.md`, "An approval").
const REMEMBER: Style = Style::new().fg(crate::theme::Role::Info.color());

/// The tail kept visible on a choice row past `prefix_cells` cells at
/// `width` columns: the tail's end, like a form's words row, so a long
/// tail never wraps the row and the cursor stays on it
/// (`docs/tui.md`, "An approval").
fn scrolled_tail(tail: &str, prefix_cells: usize, width: usize) -> String {
    let take = width.saturating_sub(prefix_cells);
    let mut kept = String::new();
    let mut taken: usize = 0;
    for ch in tail.chars().rev() {
        let mut encoded = [0; 4];
        let cell = format::width(ch.encode_utf8(&mut encoded));
        if taken.saturating_add(cell) > take {
            break;
        }
        taken = taken.saturating_add(cell);
        kept.insert(0, ch);
    }
    kept
}

/// The hint below the choices (`docs/tui.md`, "An approval").
const HINT: &str = "↑↓ choose · Enter confirms · typing goes to the feedback · click a choice";

/// The styled run `bytes` on the panel's `line`, or nothing when the run
/// is empty: an empty run draws nothing, so it leaves no mark. Callers
/// extend the panel's marks with it instead of guarding the push with a
/// comparison, so the guard cannot mutate.
fn marked(line: usize, bytes: Range<usize>, style: Style) -> Option<Mark> {
    if bytes.is_empty() {
        None
    } else {
        Some(Mark { line, bytes, style })
    }
}

impl Approval {
    /// The rule an allow can remember, offered only at review.
    fn rule(&self) -> Option<&RuleOffer> {
        match &self.step {
            AskStep::Review { rule, .. } => rule.as_ref(),
            AskStep::StandingAsk { .. } => None,
        }
    }

    /// The choices it shows: the remembering ones only with a rule.
    fn choices(&self) -> Vec<Choice> {
        if self.rule().is_some() {
            vec![Choice::Once, Choice::Session, Choice::Project, Choice::Deny]
        } else {
            vec![Choice::Once, Choice::Deny]
        }
    }

    /// Why it asked, as the facts line ends with it
    /// (`docs/tui.md`, "An approval").
    fn why(&self) -> String {
        match &self.step {
            AskStep::StandingAsk { standing_rule } => {
                let scope = match standing_rule.scope {
                    RuleScope::Global => "global",
                    RuleScope::Project => "project",
                };
                format!("asked by the {scope} rule {}", standing_rule.prefix)
            }
            AskStep::Review {
                escalation: Some(Escalation::ConsecutiveBlocks { reason }),
                ..
            } => format!("the reviewer blocked the last calls: {reason}"),
            AskStep::Review {
                escalation: Some(Escalation::SessionBlocks { reason }),
                ..
            } => format!("the reviewer has blocked too many calls: {reason}"),
            AskStep::Review {
                escalation: Some(Escalation::ReviewerFailed { .. }),
                ..
            } => "the reviewer failed".to_owned(),
            AskStep::Review {
                escalation: None, ..
            } => "no rule allows this call".to_owned(),
        }
    }

    /// The call's declared effects in words (`docs/tui.md`, "An approval").
    fn effects(&self) -> Vec<&'static str> {
        self.effects
            .iter()
            .map(|effect| match effect {
                Effect::Executes => "runs a command",
                Effect::Writes => "writes files",
                Effect::Reads => "reads files",
                Effect::Network => "uses the network",
            })
            .collect()
    }

    /// The dim facts line: the declared effects joined by ", ", " · " and
    /// the paths it names, and " · " and why it asked
    /// (`docs/tui.md`, "An approval").
    fn facts(&self) -> String {
        let mut parts = Vec::new();
        let effects = self.effects().join(", ");
        if !effects.is_empty() {
            parts.push(effects);
        }
        if !self.paths.is_empty() {
            parts.push(self.paths.join(", "));
        }
        parts.push(self.why());
        parts.join(" · ")
    }

    /// The answer the cursor's choice sends.
    pub(super) fn answer(&self) -> ReplyAnswer {
        let remember = |scope| {
            self.rule().map(|rule| Remember {
                scope,
                prefix: rule.prefix.clone(),
            })
        };
        let (decision, feedback, remember) = match self.cursor {
            Choice::Once => (Decision::Allow, None, None),
            Choice::Session => (Decision::Allow, None, remember(RememberScope::Session)),
            Choice::Project => (Decision::Allow, None, remember(RememberScope::Project)),
            Choice::Deny => {
                let typed = !self.feedback.trim().is_empty();
                (Decision::Deny, typed.then(|| self.feedback.clone()), None)
            }
        };
        ReplyAnswer::Approval {
            decision,
            feedback,
            remember,
        }
    }

    /// One choice's label head and its tail with the tail's style: the
    /// remembering rows end with the rule's prefix and " *" in `info`,
    /// deny with the feedback prompt dim or the feedback as typed
    /// (`docs/tui.md`, "An approval").
    fn label(&self, choice: Choice) -> (String, Option<(String, Style)>) {
        match choice {
            Choice::Once => ("Allow once".to_owned(), None),
            Choice::Session => {
                let prefix = self
                    .rule()
                    .map(|rule| rule.prefix.as_str())
                    .unwrap_or_default();
                (
                    "Allow for this session ".to_owned(),
                    Some((format!("{prefix} *"), REMEMBER)),
                )
            }
            Choice::Project => {
                let prefix = self
                    .rule()
                    .map(|rule| rule.prefix.as_str())
                    .unwrap_or_default();
                (
                    "Always allow in this project ".to_owned(),
                    Some((format!("{prefix} *"), REMEMBER)),
                )
            }
            Choice::Deny if self.feedback.is_empty() => (
                "Deny".to_owned(),
                Some(("  type to add feedback".to_owned(), DIM)),
            ),
            Choice::Deny => (
                "Deny".to_owned(),
                Some((format!("  {}", self.feedback), Style::new())),
            ),
        }
    }

    /// The panel's lines for the request `k` of `n` from `session`, padded
    /// to `width` columns, with the styled runs of each line and the
    /// feedback cursor (`docs/tui.md`, "An approval").
    pub(super) fn render(
        &self,
        k: usize,
        n: usize,
        session: &str,
        width: u16,
    ) -> (Vec<String>, Vec<Mark>, Option<(usize, u16)>) {
        let mut lines: Vec<String> = Vec::new();
        let mut marks: Vec<Mark> = Vec::new();
        // The header: "Approval k of n" and the session on the left,
        // " · irreversible" when the call declares itself so, "Esc puts
        // it aside" right-aligned with spaces. Past the width the right
        // part follows one space after the left and the line wraps.
        let head = format!("Approval {k} of {n}");
        let mut left = format!("{head} · {session}");
        if !self.reversible {
            left.push_str(" · irreversible");
        }
        let right = "Esc puts it aside";
        let pad = usize::from(width)
            .saturating_sub(format::width(&left).saturating_add(format::width(right)))
            .max(1);
        lines.push(format!("{left}{}{right}", " ".repeat(pad)));
        let last = lines.last().map(String::len).unwrap_or_default();
        let session_end = head
            .len()
            .saturating_add(" · ".len())
            .saturating_add(session.len())
            .min(last);
        let irreversible_end = session_end
            .saturating_add(" · irreversible".len())
            .min(last);
        let right_from = last.saturating_sub(right.len());
        if !head.is_empty() {
            marks.push(Mark {
                line: 0,
                bytes: 0..head.len().min(last),
                style: HEAD,
            });
        }
        marks.extend(marked(0, head.len()..session_end, SESSION));
        if !self.reversible {
            marks.extend(marked(0, session_end..irreversible_end, LOUD));
        }
        marks.extend(marked(0, right_from..last, DIM));
        // Then the tool's name bold in `info` and its main argument bold,
        // or the tool alone when the summary is empty.
        if let Some((tool, arguments)) = &self.call {
            let line = lines.len();
            lines.push(format::label(tool, arguments));
            let end = lines.last().map(String::len).unwrap_or_default();
            let tool_end = tool.len().min(end);
            if tool_end > 0 {
                marks.push(Mark {
                    line,
                    bytes: 0..tool_end,
                    style: TOOL,
                });
            }
            if !arguments.is_empty() {
                let argument_from = tool_end.saturating_add(1).min(end);
                marks.extend(marked(line, argument_from..end, ARGUMENT));
            }
        }
        // Then the dim facts line, a blank row, the numbered choices, a
        // blank row and the dim hint.
        let line = lines.len();
        lines.push(self.facts());
        let end = lines.last().map(String::len).unwrap_or_default();
        marks.extend(marked(line, 0..end, DIM));
        lines.push(String::new());
        let mut caret = None;
        for (index, choice) in self.choices().iter().enumerate() {
            let focused = *choice == self.cursor;
            let number = index.saturating_add(1).to_string();
            let (head, tail) = self.label(*choice);
            let seen = if focused { "▸ " } else { "  " };
            // Long feedback scrolls to its visible end, like a form's
            // words row, so the deny row never wraps and the cursor
            // stays on it. Remembering tails stay complete: the rule's
            // prefix always shows (`docs/tui.md`, "An approval").
            let tail = match choice {
                Choice::Deny => tail.map(|(text, style)| {
                    let prefix_cells = format::width(seen)
                        .saturating_add(number.len())
                        .saturating_add(1)
                        .saturating_add(format::width(&head));
                    (
                        scrolled_tail(&text, prefix_cells, usize::from(width)),
                        style,
                    )
                }),
                Choice::Once | Choice::Session | Choice::Project => tail,
            };
            let line = lines.len();
            let number_from = seen.len();
            let head_from = number_from.saturating_add(number.len()).saturating_add(1);
            let mut row = format!("{seen}{number} {head}");
            if let Some((tail, _)) = &tail {
                row.push_str(tail);
            }
            lines.push(row);
            let end = lines.last().map(String::len).unwrap_or_default();
            if focused && !seen.is_empty() {
                marks.push(Mark {
                    line,
                    bytes: 0..seen.len().min(end),
                    style: FOCUS,
                });
            }
            let number_end = head_from.saturating_sub(1).min(end);
            marks.extend(marked(line, number_from..number_end, DIM));
            let tail_from = head_from.saturating_add(head.len()).min(end);
            if focused {
                marks.extend(marked(line, head_from..tail_from, BOLD));
            }
            if let Some((tail, style)) = &tail {
                let tail_end = tail_from.saturating_add(tail.len()).min(end);
                let style = if focused {
                    if style == &DIM {
                        DIM_BOLD
                    } else if style == &REMEMBER {
                        REMEMBER_BOLD
                    } else {
                        BOLD
                    }
                } else {
                    *style
                };
                marks.extend(marked(line, tail_from..tail_end, style));
            }
            if focused && *choice == Choice::Deny && !self.feedback.is_empty() {
                let col = lines
                    .last()
                    .map(|row| u16::try_from(format::width(row)).unwrap_or(u16::MAX))
                    .unwrap_or_default();
                caret = Some((line, col));
            }
        }
        lines.push(String::new());
        let line = lines.len();
        lines.push(HINT.to_owned());
        let end = lines.last().map(String::len).unwrap_or_default();
        marks.extend(marked(line, 0..end, DIM));
        (lines, marks, caret)
    }

    /// A key on the shown approval. Esc and ⌥A act on the queue
    /// ([`super::Queue::on_key`]) before this.
    pub(super) fn on_key(&mut self, key: &Key) -> Option<super::PanelKey> {
        match key {
            Key::Char(ch) => self.type_char(*ch),
            Key::Backspace => {
                self.feedback.pop();
                self.cursor = Choice::Deny;
            }
            Key::Up | Key::Down => {
                let choices = self.choices();
                let now = choices
                    .iter()
                    .position(|choice| *choice == self.cursor)
                    .unwrap_or_default();
                let next = if *key == Key::Up {
                    now.saturating_sub(1)
                } else {
                    now.saturating_add(1)
                };
                if let Some(choice) = choices.get(next) {
                    self.cursor = *choice;
                }
            }
            Key::Enter => return Some(super::PanelKey::Answer),
            // Tab, Shift+Tab, Ctrl+G and Ctrl+R do nothing in the panel,
            // which stands in the input box's place; F1 opens the key map
            // over it.
            Key::Esc
            | Key::AltA
            | Key::Tab
            | Key::BackTab
            | Key::CtrlG
            | Key::CtrlR
            | Key::CtrlV
            | Key::CtrlL => {}
            // The layout's keys reach the screen behind the panel.
            Key::PageUp
            | Key::PageDown
            | Key::End
            | Key::CtrlC
            | Key::CtrlF
            | Key::F1
            | Key::CtrlO
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_) => {
                return None;
            }
            // The steering queue's keys do nothing while the panel is open.
            Key::AltUp | Key::AltDown | Key::AltX => {}
        }
        Some(super::PanelKey::Handled)
    }

    /// An editing key: a paste is typed into the feedback, a control
    /// character as a space; every other edit does nothing.
    pub(super) fn on_edit(&mut self, edit: &Edit) {
        match edit {
            Edit::Paste(text) => {
                for ch in text.chars() {
                    self.type_char(if ch.is_control() { ' ' } else { ch });
                }
            }
            Edit::Left
            | Edit::Right
            | Edit::ShiftEnter
            | Edit::CtrlJ
            | Edit::WordLeft
            | Edit::WordRight
            | Edit::DeleteWord
            | Edit::DeleteToLineStart
            | Edit::LineStart
            | Edit::LineEnd
            | Edit::Delete => {}
        }
    }

    /// Typing goes to the feedback and moves the cursor to deny: the
    /// numbers label the rows, so a digit is feedback like any other key
    /// (`docs/tui.md`, "An approval").
    fn type_char(&mut self, ch: char) {
        self.feedback.push(ch);
        self.cursor = Choice::Deny;
    }
}
