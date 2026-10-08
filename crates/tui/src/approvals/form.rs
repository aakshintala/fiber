//! An `ask_user` question form on the request panel: one tab per question,
//! then a Submit tab, answered by keys with one `reply` (`docs/tui.md`, "A
//! question form").

use contract::commands::{ReplyAnswer, SentFormAnswer};
use contract::shapes::{Question, True};

use super::{Panel, PanelKey, PanelSpot};
use crate::format::{cut, width};
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
    /// The text cursor on the words row: the characters before it.
    caret: usize,
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

/// A click target on the form: a tab, or a row of the shown tab. `Tab(n)`,
/// `n` the question count, is the Submit tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spot {
    Tab(usize),
    Option(usize),
    Words,
    Next,
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
                caret: 0,
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
            Key::Enter => return Some(self.enter()),
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
    /// on the Submit tab, a control character as a space; ← and → move the
    /// words row's text cursor there, and move between the tabs from every
    /// other row. Every other edit does nothing.
    pub(crate) fn on_edit(&mut self, edit: &Edit) {
        match edit {
            Edit::Paste(text) => {
                for ch in text.chars() {
                    self.type_char(if ch.is_control() { ' ' } else { ch });
                }
            }
            Edit::Left => self.arrow(false),
            Edit::Right => self.arrow(true),
            Edit::ShiftEnter
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

    /// A click on `spot`: a tab opens it; a row takes the cursor, then an
    /// option does what Enter does on a single-choice option and what Space
    /// does on a multi-choice one, and `Next →`, `Submit` and "Chat about
    /// this" do what Enter does there (`docs/tui.md`, "A question form").
    /// A row the shown tab lacks does nothing.
    pub(crate) fn click(&mut self, spot: Spot) -> PanelKey {
        let at = match (spot, self.at) {
            (Spot::Tab(tab), _) => {
                self.open_tab(tab);
                return PanelKey::Handled;
            }
            (Spot::Option(option), Cursor::Question { field, .. }) => Cursor::Question {
                field,
                row: Row::Option(option),
            },
            (Spot::Words, Cursor::Question { field, .. }) => Cursor::Question {
                field,
                row: Row::Words,
            },
            (Spot::Next, Cursor::Question { field, .. }) => Cursor::Question {
                field,
                row: Row::Next,
            },
            (Spot::Chat, Cursor::Question { field, .. }) => Cursor::Question {
                field,
                row: Row::Chat,
            },
            (Spot::Note, Cursor::Submit(_)) => Cursor::Submit(SubmitRow::Note),
            (Spot::Send, Cursor::Submit(_)) => Cursor::Submit(SubmitRow::Send),
            (Spot::Chat, Cursor::Submit(_)) => Cursor::Submit(SubmitRow::Chat),
            (Spot::Option(_) | Spot::Words | Spot::Next, Cursor::Submit(_))
            | (Spot::Note | Spot::Send, Cursor::Question { .. }) => return PanelKey::Handled,
        };
        if !self.rows().contains(&at) {
            return PanelKey::Handled;
        }
        self.at = at;
        match spot {
            Spot::Option(_) => match self.answers.get(self.tab()).map(|field| &field.picks) {
                Some(Picks::Many(_)) => {
                    self.space();
                    PanelKey::Handled
                }
                Some(Picks::One(_)) | None => self.enter(),
            },
            Spot::Words => {
                let field = self.tab();
                if let Some(answer) = self.answers.get_mut(field) {
                    answer.caret = answer.words.chars().count();
                }
                PanelKey::Handled
            }
            Spot::Tab(_) | Spot::Note => PanelKey::Handled,
            Spot::Next | Spot::Send | Spot::Chat => self.enter(),
        }
    }

    /// The panel under `header` at `width` columns: the tab line, then the
    /// shown tab's rows, each row a click target. Text from the model has
    /// its control characters drawn as spaces.
    pub(crate) fn panel(&self, header: String, width: u16) -> Panel {
        let mut panel = Panel {
            lines: Vec::new(),
            alert: false,
            spots: Vec::new(),
            cursor: None,
            caret: None,
        };
        push(&mut panel, &header, None, false);
        self.tab_line(width, &mut panel);
        match self.at {
            Cursor::Question { field, row } => self.question_lines(field, row, width, &mut panel),
            Cursor::Submit(row) => self.submit_lines(row, &mut panel),
        }
        panel
    }

    /// ← or →: one character on the words row, stopping at either end;
    /// from any other row, the previous or next tab, stopping at the first
    /// question and at Submit.
    fn arrow(&mut self, right: bool) {
        if let Cursor::Question {
            field,
            row: Row::Words,
        } = self.at
        {
            if let Some(answer) = self.answers.get_mut(field) {
                answer.caret = if right {
                    answer
                        .caret
                        .saturating_add(1)
                        .min(answer.words.chars().count())
                } else {
                    answer.caret.saturating_sub(1)
                };
            }
            return;
        }
        let tab = self.tab();
        self.open_tab(if right {
            tab.saturating_add(1)
        } else {
            tab.saturating_sub(1)
        });
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
    fn enter(&mut self) -> PanelKey {
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
                Row::Chat => return PanelKey::Decline,
            },
            Cursor::Submit(SubmitRow::Note | SubmitRow::Send) => return PanelKey::Answer,
            Cursor::Submit(SubmitRow::Chat) => return PanelKey::Decline,
        }
        PanelKey::Handled
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
                    let at = byte_at(&answer.words, answer.caret);
                    answer.words.insert(at, ch);
                    answer.caret = answer.caret.saturating_add(1);
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

    /// Backspace deletes the character before the text cursor on the words
    /// row and the note's last character on the note row, and does nothing
    /// elsewhere.
    fn backspace(&mut self) {
        match self.at {
            Cursor::Question {
                field,
                row: Row::Words,
            } => {
                if let Some(answer) = self.answers.get_mut(field)
                    && let Some(before) = answer.caret.checked_sub(1)
                {
                    answer.words.remove(byte_at(&answer.words, before));
                    answer.caret = before;
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
    /// shown tab in brackets. Each tab is a click target over its columns
    /// only while the line fits `cols` columns on one row.
    fn tab_line(&self, cols: u16, panel: &mut Panel) {
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
                clean(&format!("{}{mark}", question.header))
            })
            .collect();
        tabs.push("Submit".to_owned());
        if let Some(tab) = tabs.get_mut(shown) {
            *tab = format!("[{tab}]");
        }
        let text = tabs.join(TAB_GAP);
        let line = panel.lines.len();
        if width(&text) <= usize::from(cols) {
            let mut from = 0u16;
            for (tab, text) in tabs.iter().enumerate() {
                let to = from.saturating_add(cells(text));
                panel.spots.push(PanelSpot {
                    line,
                    cols: Some((from, to)),
                    spot: Spot::Tab(tab),
                });
                from = to.saturating_add(cells(TAB_GAP));
            }
        }
        panel.lines.push(text);
    }

    /// Question `field`'s rows: the question, its options with their
    /// descriptions, the words row, `Next →` or `Review →`, and "Chat
    /// about this".
    fn question_lines(&self, field: usize, at: Row, cols: u16, panel: &mut Panel) {
        let (Some(question), Some(answer)) = (self.fields.get(field), self.answers.get(field))
        else {
            return;
        };
        let mark = |row: Row| if row == at { '›' } else { ' ' };
        push(panel, &question.question, None, false);
        for (option, choice) in question.options.iter().enumerate() {
            let tick = match (&answer.picks, answer.chosen(option)) {
                (Picks::One(_), true) => "(•)",
                (Picks::One(_), false) => "( )",
                (Picks::Many(_), true) => "[x]",
                (Picks::Many(_), false) => "[ ]",
            };
            let row = Row::Option(option);
            let mut line = format!("{} {tick} {}", mark(row), choice.label);
            if let Some(description) = &choice.description {
                line.push_str(" · ");
                line.push_str(description);
            }
            push(panel, &line, Some(Spot::Option(option)), row == at);
        }
        let here = at == Row::Words;
        let (line, caret) = words_row(answer, here, cols);
        if let Some(caret) = caret {
            panel.caret = Some((panel.lines.len(), caret));
        }
        push(panel, &line, Some(Spot::Words), here);
        let next = if field.saturating_add(1) == self.fields.len() {
            "Review →"
        } else {
            "Next →"
        };
        let line = format!("{} {next}", mark(Row::Next));
        push(panel, &line, Some(Spot::Next), at == Row::Next);
        let line = format!("{} Chat about this", mark(Row::Chat));
        push(panel, &line, Some(Spot::Chat), at == Row::Chat);
    }

    /// The Submit tab's rows: each answer as the result writes it, the
    /// note, `Submit`, and "Chat about this".
    fn submit_lines(&self, at: SubmitRow, panel: &mut Panel) {
        for (field, question) in self.fields.iter().enumerate() {
            let said = self.said(field);
            let said = said
                .as_ref()
                .map(|(labels, text)| (labels.as_slice(), text.as_deref()));
            push(
                panel,
                &crate::format::answer_row(&question.header, said),
                None,
                false,
            );
        }
        let mark = |row: SubmitRow| if row == at { '›' } else { ' ' };
        let note = if self.note.is_empty() {
            format!("{} note · type to add a note", mark(SubmitRow::Note))
        } else {
            let note = crate::format::note_row(&self.note);
            format!("{} {note}", mark(SubmitRow::Note))
        };
        push(panel, &note, Some(Spot::Note), at == SubmitRow::Note);
        let line = format!("{} Submit", mark(SubmitRow::Send));
        push(panel, &line, Some(Spot::Send), at == SubmitRow::Send);
        let line = format!("{} Chat about this", mark(SubmitRow::Chat));
        push(panel, &line, Some(Spot::Chat), at == SubmitRow::Chat);
    }
}

/// The space between two tabs on the tab line.
const TAB_GAP: &str = "  ";

/// Pushes `text` as the panel's next line, its control characters as
/// spaces, with `spot` over the whole line and the cursor on it when
/// `here`.
fn push(panel: &mut Panel, text: &str, spot: Option<Spot>, here: bool) {
    let line = panel.lines.len();
    panel.lines.push(clean(text));
    if let Some(spot) = spot {
        panel.spots.push(PanelSpot {
            line,
            cols: None,
            spot,
        });
    }
    if here {
        panel.cursor = Some(line);
    }
}

/// The words row at `cols` columns and, when the cursor is on it (`here`),
/// the text cursor's column. The row never wraps: it shows the words from
/// their first character, clipped at the width, unless the text cursor
/// would reach the width, when it shows them from the first character that
/// puts the text cursor on the last column.
fn words_row(answer: &Field, here: bool, cols: u16) -> (String, Option<u16>) {
    let prefix = format!("{} ✎ ", if here { '›' } else { ' ' });
    if answer.words.is_empty() {
        return (
            format!("{prefix}answer in words"),
            here.then(|| cells(&prefix)),
        );
    }
    let words: Vec<char> = clean(&answer.words).chars().collect();
    let caret = answer.caret;
    let before =
        |start: usize| -> String { words.get(start..caret).unwrap_or_default().iter().collect() };
    let fits = |start: usize| width(&prefix) + width(&before(start)) < usize::from(cols);
    let start = if here {
        (0..caret).find(|start| fits(*start)).unwrap_or(caret)
    } else {
        0
    };
    let shown: String = words.get(start..).unwrap_or_default().iter().collect();
    let line = cut(&format!("{prefix}{shown}"), usize::from(cols));
    let caret = here.then(|| cells(&prefix).saturating_add(cells(&before(start))));
    (line, caret)
}

/// The byte where the character `chars` characters into `text` starts, or
/// the end.
fn byte_at(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(at, _)| at)
}

/// How many display cells `text` takes, as a screen column.
fn cells(text: &str) -> u16 {
    u16::try_from(width(text)).unwrap_or(u16::MAX)
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
