//! An `ask_user` question form on the request panel: one tab per question,
//! then a Submit tab, answered by keys with one `reply` (`docs/tui.md`, "A
//! question form").

use contract::commands::{ReplyAnswer, SentFormAnswer};
use contract::shapes::{Question, True};

use super::PanelKey;
use crate::keys::{Edit, Key};

/// A question form and what the person has answered so far.
#[derive(Debug, Clone)]
pub(crate) struct Form {
    /// The questions, as the model asked them.
    fields: Vec<Question>,
    /// One per question, in field order.
    answers: Vec<Field>,
    /// The note on the whole form.
    note: String,
    /// Where the cursor is.
    at: Cursor,
}

/// One question's answer so far.
#[derive(Debug, Clone)]
struct Field {
    picks: Picks,
    /// The words typed on the question's words row.
    words: String,
}

impl Field {
    /// Whether option `option` is chosen.
    fn chosen(&self, option: usize) -> bool {
        match &self.picks {
            Picks::One(choice) => *choice == Some(option),
            Picks::Many(chosen) => chosen.get(option).copied().unwrap_or_default(),
        }
    }
}

/// The options chosen: one at most on a single-choice question (free text
/// included), any number on a multi-choice one.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Picks {
    One(Option<usize>),
    Many(Vec<bool>),
}

/// The cursor: a row of a question's tab, or a row of the Submit tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cursor {
    Question { field: usize, row: Row },
    Submit(SubmitRow),
}

/// A row of a question's tab, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// An option, by index.
    Option(usize),
    /// The row to answer in words.
    Words,
    /// `Next →`, or `Review →` on the last question.
    Next,
    /// "Chat about this".
    Chat,
}

/// A row of the Submit tab, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubmitRow {
    Note,
    Send,
    Chat,
}

impl Form {
    /// A form over `fields`, nothing chosen, on the first question's
    /// landing row; on the Submit tab when there is no question.
    pub(crate) fn new(fields: Vec<Question>) -> Self {
        let answers = fields
            .iter()
            .map(|question| Field {
                picks: if question.multi_select == Some(true) {
                    Picks::Many(vec![false; question.options.len()])
                } else {
                    Picks::One(None)
                },
                words: String::new(),
            })
            .collect();
        let mut form = Self {
            fields,
            answers,
            note: String::new(),
            at: Cursor::Submit(SubmitRow::Send),
        };
        form.open_tab(0);
        form
    }

    /// A key on the form. `None`: not the form's key; it passes through.
    pub(crate) fn on_key(&mut self, key: &Key) -> Option<PanelKey> {
        match key {
            Key::Esc => return Some(PanelKey::Decline),
            Key::Enter => return self.enter(),
            Key::Char(' ') => self.space(),
            Key::Char(ch) => self.type_char(*ch),
            Key::Backspace => self.backspace(),
            Key::Up => self.step(false),
            Key::Down => self.step(true),
            Key::Tab => self.open_tab(self.tab().saturating_add(1)),
            Key::BackTab => self.open_tab(self.tab().saturating_sub(1)),
            // ⌥A acts on the queue before the form sees it.
            Key::AltA => {}
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
            | Key::AltDigit(_) => return None,
            // The input box's and the steering queue's keys do nothing.
            Key::CtrlG | Key::CtrlR | Key::AltUp | Key::AltDown | Key::AltX => {}
        }
        Some(PanelKey::Handled)
    }

    /// An editing key: a paste is typed on the words row, or into the note
    /// on the Submit tab, a control character as a space. Every other edit
    /// does nothing.
    pub(crate) fn on_edit(&mut self, edit: &Edit) {
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
            | Edit::LineStart
            | Edit::LineEnd
            | Edit::Delete => {}
        }
    }

    /// The reply's answer: one per question in field order, then the note
    /// when it is not blank.
    pub(crate) fn answer(&self) -> ReplyAnswer {
        let answers = (0..self.fields.len())
            .map(|field| match self.said(field) {
                None => SentFormAnswer::Skipped { skipped: True },
                Some((labels, text)) => SentFormAnswer::Answered { labels, text },
            })
            .collect();
        let note = (!self.note.trim().is_empty()).then(|| self.note.clone());
        ReplyAnswer::Form { answers, note }
    }

    /// The panel's lines: `header`, the tab line, then the shown tab's rows.
    /// Text from the model has its control characters drawn as spaces.
    pub(crate) fn lines(&self, header: String) -> Vec<String> {
        let mut lines = vec![header, self.tab_line()];
        match self.at {
            Cursor::Question { field, row } => self.question_lines(field, row, &mut lines),
            Cursor::Submit(row) => self.submit_lines(row, &mut lines),
        }
        lines.iter().map(|line| clean(line)).collect()
    }

    /// The tab shown: a question's index, or the question count for Submit.
    fn tab(&self) -> usize {
        match self.at {
            Cursor::Question { field, .. } => field,
            Cursor::Submit(_) => self.fields.len(),
        }
    }

    /// Opens tab `tab`, at most Submit, on its landing row: the first
    /// option, the words row on a question without options, `Submit` on
    /// the Submit tab.
    fn open_tab(&mut self, tab: usize) {
        self.at = match self.fields.get(tab) {
            Some(question) if question.options.is_empty() => Cursor::Question {
                field: tab,
                row: Row::Words,
            },
            Some(_) => Cursor::Question {
                field: tab,
                row: Row::Option(0),
            },
            None => Cursor::Submit(SubmitRow::Send),
        };
    }

    /// Moves on: to the next question, or to Submit after the last.
    fn move_on(&mut self) {
        self.open_tab(self.tab().saturating_add(1));
    }

    /// Enter: chooses a single-choice option and moves on, moves on from
    /// any other question row, sends from the note or `Submit`, and
    /// declines from "Chat about this".
    fn enter(&mut self) -> Option<PanelKey> {
        match self.at {
            Cursor::Question { field, row } => match row {
                Row::Option(option) => {
                    if let Some(Field {
                        picks: Picks::One(choice),
                        ..
                    }) = self.answers.get_mut(field)
                    {
                        *choice = Some(option);
                    }
                    self.move_on();
                }
                Row::Words | Row::Next => self.move_on(),
                Row::Chat => return Some(PanelKey::Decline),
            },
            Cursor::Submit(SubmitRow::Note | SubmitRow::Send) => return Some(PanelKey::Answer),
            Cursor::Submit(SubmitRow::Chat) => return Some(PanelKey::Decline),
        }
        Some(PanelKey::Handled)
    }

    /// Space: toggles a multi-choice option, chooses or clears a
    /// single-choice one, and is typed on the words row and the Submit tab.
    fn space(&mut self) {
        match self.at {
            Cursor::Question {
                field,
                row: Row::Option(option),
            } => match self.answers.get_mut(field).map(|field| &mut field.picks) {
                Some(Picks::Many(chosen)) => {
                    if let Some(chosen) = chosen.get_mut(option) {
                        *chosen = !*chosen;
                    }
                }
                Some(Picks::One(choice)) => {
                    *choice = if *choice == Some(option) {
                        None
                    } else {
                        Some(option)
                    };
                }
                None => {}
            },
            Cursor::Question {
                row: Row::Words, ..
            }
            | Cursor::Submit(_) => self.type_char(' '),
            Cursor::Question {
                row: Row::Next | Row::Chat,
                ..
            } => {}
        }
    }

    /// Types `ch`: on a question, onto its words row, moving the cursor
    /// there; on the Submit tab, into the note.
    fn type_char(&mut self, ch: char) {
        match self.at {
            Cursor::Question { field, .. } => {
                if let Some(answer) = self.answers.get_mut(field) {
                    answer.words.push(ch);
                }
                self.at = Cursor::Question {
                    field,
                    row: Row::Words,
                };
            }
            Cursor::Submit(_) => {
                self.note.push(ch);
                self.at = Cursor::Submit(SubmitRow::Note);
            }
        }
    }

    /// Backspace deletes the last character on the words row and the note
    /// row, and does nothing elsewhere.
    fn backspace(&mut self) {
        match self.at {
            Cursor::Question {
                field,
                row: Row::Words,
            } => {
                if let Some(answer) = self.answers.get_mut(field) {
                    answer.words.pop();
                }
            }
            Cursor::Submit(SubmitRow::Note) => {
                self.note.pop();
            }
            Cursor::Question {
                row: Row::Option(_) | Row::Next | Row::Chat,
                ..
            }
            | Cursor::Submit(SubmitRow::Send | SubmitRow::Chat) => {}
        }
    }

    /// ↑ or ↓: one row, stopping at the first and the last.
    fn step(&mut self, down: bool) {
        let rows = self.rows();
        let now = rows
            .iter()
            .position(|row| *row == self.at)
            .unwrap_or_default();
        let next = if down {
            now.saturating_add(1)
        } else {
            now.saturating_sub(1)
        };
        if let Some(row) = rows.get(next) {
            self.at = *row;
        }
    }

    /// The shown tab's rows, top to bottom.
    fn rows(&self) -> Vec<Cursor> {
        match self.at {
            Cursor::Question { field, .. } => {
                let options = self
                    .fields
                    .get(field)
                    .map_or(0, |question| question.options.len());
                (0..options)
                    .map(Row::Option)
                    .chain([Row::Words, Row::Next, Row::Chat])
                    .map(|row| Cursor::Question { field, row })
                    .collect()
            }
            Cursor::Submit(_) => [SubmitRow::Note, SubmitRow::Send, SubmitRow::Chat]
                .map(Cursor::Submit)
                .to_vec(),
        }
    }

    /// Question `field`'s answer: `None` when skipped, else the chosen
    /// labels in option order and the words when not blank.
    fn said(&self, field: usize) -> Option<(Vec<String>, Option<String>)> {
        let question = self.fields.get(field)?;
        let answer = self.answers.get(field)?;
        let labels: Vec<String> = question
            .options
            .iter()
            .enumerate()
            .filter(|(option, _)| answer.chosen(*option))
            .map(|(_, choice)| choice.label.clone())
            .collect();
        let text = (!answer.words.trim().is_empty()).then(|| answer.words.clone());
        (!labels.is_empty() || text.is_some()).then_some((labels, text))
    }

    /// One tab per question header, ` ✓` when answered, then Submit; the
    /// shown tab in brackets.
    fn tab_line(&self) -> String {
        let shown = self.tab();
        let mut tabs: Vec<String> = self
            .fields
            .iter()
            .enumerate()
            .map(|(field, question)| {
                let mark = if self.said(field).is_some() {
                    " ✓"
                } else {
                    ""
                };
                format!("{}{mark}", question.header)
            })
            .collect();
        tabs.push("Submit".to_owned());
        if let Some(tab) = tabs.get_mut(shown) {
            *tab = format!("[{tab}]");
        }
        tabs.join("  ")
    }

    /// Question `field`'s rows: the question, its options with their
    /// descriptions, the words row, `Next →` or `Review →`, and "Chat
    /// about this".
    fn question_lines(&self, field: usize, at: Row, lines: &mut Vec<String>) {
        let (Some(question), Some(answer)) = (self.fields.get(field), self.answers.get(field))
        else {
            return;
        };
        let mark = |row: Row| if row == at { '›' } else { ' ' };
        lines.push(question.question.clone());
        for (option, choice) in question.options.iter().enumerate() {
            let tick = match (&answer.picks, answer.chosen(option)) {
                (Picks::One(_), true) => "(•)",
                (Picks::One(_), false) => "( )",
                (Picks::Many(_), true) => "[x]",
                (Picks::Many(_), false) => "[ ]",
            };
            let mut line = format!("{} {tick} {}", mark(Row::Option(option)), choice.label);
            if let Some(description) = &choice.description {
                line.push_str(" · ");
                line.push_str(description);
            }
            lines.push(line);
        }
        let words = if answer.words.is_empty() {
            "answer in words"
        } else {
            answer.words.as_str()
        };
        lines.push(format!("{} ✎ {words}", mark(Row::Words)));
        let next = if field.saturating_add(1) == self.fields.len() {
            "Review →"
        } else {
            "Next →"
        };
        lines.push(format!("{} {next}", mark(Row::Next)));
        lines.push(format!("{} Chat about this", mark(Row::Chat)));
    }

    /// The Submit tab's rows: each answer as the result writes it, the
    /// note, `Submit`, and "Chat about this".
    fn submit_lines(&self, at: SubmitRow, lines: &mut Vec<String>) {
        for (field, question) in self.fields.iter().enumerate() {
            let said = self.said(field);
            let said = said
                .as_ref()
                .map(|(labels, text)| (labels.as_slice(), text.as_deref()));
            lines.push(crate::format::answer_row(&question.header, said));
        }
        let mark = |row: SubmitRow| if row == at { '›' } else { ' ' };
        if self.note.is_empty() {
            lines.push(format!(
                "{} note · type to add a note",
                mark(SubmitRow::Note)
            ));
        } else {
            let note = crate::format::note_row(&self.note);
            lines.push(format!("{} {note}", mark(SubmitRow::Note)));
        }
        lines.push(format!("{} Submit", mark(SubmitRow::Send)));
        lines.push(format!("{} Chat about this", mark(SubmitRow::Chat)));
    }
}

/// `text` with every control character as a space, so model text stays on
/// its line and sends the terminal nothing.
fn clean(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

#[cfg(test)]
#[path = "form_tests.rs"]
mod tests;
