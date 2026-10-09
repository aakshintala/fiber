//! A one-question interaction on the request panel (`docs/tui.md`, "A
//! question form").

use contract::commands::ReplyAnswer;
use contract::shapes::Choice;

use super::form::{self, Spot};
use super::{Panel, PanelKey};
use crate::keys::{Edit, Key};

/// One non-form interaction and the answer the person is composing.
#[derive(Debug, Clone)]
pub(crate) struct OneQuestion {
    kind: Kind,
    prompt: String,
    options: Vec<Choice>,
    toggled: Vec<bool>,
    words: String,
    caret: usize,
    at: Row,
}

/// An interaction with its own answer rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Confirm,
    Select,
    MultiSelect,
    TextInput,
}

/// A row in the question panel, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Option(usize),
    Words,
    Submit,
    Chat,
}

impl OneQuestion {
    /// A yes-or-no question.
    pub(crate) fn confirm(prompt: String) -> Self {
        Self::new(
            Kind::Confirm,
            prompt,
            vec![
                Choice {
                    label: "yes".to_owned(),
                    description: None,
                },
                Choice {
                    label: "no".to_owned(),
                    description: None,
                },
            ],
        )
    }

    /// A question with one offered answer.
    pub(crate) fn select(prompt: String, options: Vec<Choice>) -> Self {
        Self::new(Kind::Select, prompt, options)
    }

    /// A question with any number of offered answers.
    pub(crate) fn multi_select(prompt: String, options: Vec<Choice>) -> Self {
        Self::new(Kind::MultiSelect, prompt, options)
    }

    /// A question answered with typed words.
    pub(crate) fn text_input(prompt: String) -> Self {
        Self::new(Kind::TextInput, prompt, Vec::new())
    }

    /// Handles a key on the one-question panel. `None` lets the screen
    /// behind it handle the key.
    pub(crate) fn on_key(&mut self, key: &Key) -> Option<PanelKey> {
        match key {
            Key::Esc => return Some(PanelKey::Decline),
            Key::Enter => return Some(self.enter()),
            Key::Char(' ') => match self.kind {
                Kind::MultiSelect => self.toggle(),
                Kind::TextInput => self.type_char(' '),
                Kind::Confirm | Kind::Select => {}
            },
            Key::Char(ch) => match self.kind {
                Kind::TextInput => self.type_char(*ch),
                Kind::Confirm | Kind::Select | Kind::MultiSelect => {}
            },
            Key::Backspace => {
                if self.kind == Kind::TextInput && self.at == Row::Words {
                    form::delete_before(&mut self.words, &mut self.caret);
                }
            }
            Key::Up => self.step(false),
            Key::Down => self.step(true),
            Key::Tab | Key::BackTab | Key::AltA => {}
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
            Key::CtrlG
            | Key::CtrlR
            | Key::CtrlV
            | Key::CtrlL
            | Key::AltUp
            | Key::AltDown
            | Key::AltX => {}
        }
        Some(PanelKey::Handled)
    }

    /// Applies an edit on the panel. Paste control characters become spaces.
    pub(crate) fn on_edit(&mut self, edit: &Edit) {
        match edit {
            Edit::Paste(text) => {
                if self.kind == Kind::TextInput {
                    for ch in text.chars() {
                        self.type_char(if ch.is_control() { ' ' } else { ch });
                    }
                }
            }
            Edit::Left | Edit::Right => {
                if self.kind == Kind::TextInput && self.at == Row::Words {
                    form::move_caret(&self.words, &mut self.caret, *edit == Edit::Right);
                }
            }
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

    /// A click on a row. A missing row is a no-op.
    pub(crate) fn click(&mut self, spot: Spot) -> PanelKey {
        let row = match spot {
            Spot::Option(option) => Row::Option(option),
            Spot::Words => Row::Words,
            Spot::Send => Row::Submit,
            Spot::Chat => Row::Chat,
            Spot::Tab(_) | Spot::Next | Spot::Note => return PanelKey::Handled,
        };
        if !self.rows().contains(&row) {
            return PanelKey::Handled;
        }
        self.at = row;
        match spot {
            Spot::Option(_) => match self.kind {
                Kind::Confirm | Kind::Select => self.enter(),
                Kind::MultiSelect => {
                    self.toggle();
                    PanelKey::Handled
                }
                Kind::TextInput => PanelKey::Handled,
            },
            Spot::Words => {
                self.caret = self.words.chars().count();
                PanelKey::Handled
            }
            Spot::Send => self.enter(),
            Spot::Chat => self.enter(),
            Spot::Tab(_) | Spot::Next | Spot::Note => PanelKey::Handled,
        }
    }

    /// Draws the question panel below `header` at `width` columns.
    pub(crate) fn panel(&self, header: String, width: u16) -> Panel {
        let mut panel = Panel {
            lines: Vec::new(),
            alert: false,
            spots: Vec::new(),
            cursor: None,
            caret: None,
        };
        form::push(&mut panel, &header, None, false);
        form::push(&mut panel, &self.prompt, None, false);
        for row in self.rows() {
            let here = row == self.at;
            match row {
                Row::Option(option) => {
                    let Some(choice) = self.options.get(option) else {
                        continue;
                    };
                    let tick = match self.kind {
                        Kind::Confirm | Kind::Select => "( )",
                        Kind::MultiSelect => {
                            if self.toggled.get(option).copied().unwrap_or_default() {
                                "[x]"
                            } else {
                                "[ ]"
                            }
                        }
                        Kind::TextInput => "( )",
                    };
                    form::push(
                        &mut panel,
                        &form::option_line(if here { '›' } else { ' ' }, tick, choice),
                        Some(Spot::Option(option)),
                        here,
                    );
                }
                Row::Words => {
                    let (line, caret) = form::words_row(&self.words, self.caret, here, width);
                    if let Some(caret) = caret {
                        panel.caret = Some((panel.lines.len(), caret));
                    }
                    form::push(&mut panel, &line, Some(Spot::Words), here);
                }
                Row::Submit => {
                    let line = format!("{} Submit", if here { '›' } else { ' ' });
                    form::push(&mut panel, &line, Some(Spot::Send), here);
                }
                Row::Chat => {
                    let line = format!("{} Chat about this", if here { '›' } else { ' ' });
                    form::push(&mut panel, &line, Some(Spot::Chat), here);
                }
            }
        }
        panel
    }

    /// The answer only when the current row can submit one.
    pub(crate) fn answer(&self) -> Option<ReplyAnswer> {
        match (self.kind, self.at) {
            (Kind::Confirm, Row::Option(0)) => Some(ReplyAnswer::Confirmed { confirmed: true }),
            (Kind::Confirm, Row::Option(1)) => Some(ReplyAnswer::Confirmed { confirmed: false }),
            (Kind::Confirm, Row::Option(2..)) => None,
            (Kind::Select, Row::Option(option)) => {
                let choice = self.options.get(option)?;
                Some(ReplyAnswer::Labels {
                    labels: vec![choice.label.clone()],
                })
            }
            (Kind::MultiSelect, Row::Option(_) | Row::Submit) => Some(ReplyAnswer::Labels {
                labels: self.selected_labels(),
            }),
            (Kind::TextInput, Row::Words | Row::Submit) => Some(ReplyAnswer::Text {
                text: self.words.clone(),
            }),
            (Kind::Confirm | Kind::Select, Row::Words | Row::Submit | Row::Chat)
            | (Kind::MultiSelect | Kind::TextInput, Row::Chat)
            | (Kind::TextInput, Row::Option(_))
            | (Kind::MultiSelect, Row::Words) => None,
        }
    }

    /// Creates a request with its first actionable row selected.
    fn new(kind: Kind, prompt: String, options: Vec<Choice>) -> Self {
        let toggled = vec![false; options.len()];
        let mut question = Self {
            kind,
            prompt,
            options,
            toggled,
            words: String::new(),
            caret: 0,
            at: Row::Chat,
        };
        question.at = match kind {
            Kind::Confirm | Kind::Select => question
                .options
                .first()
                .map_or(Row::Chat, |_| Row::Option(0)),
            Kind::MultiSelect => question
                .options
                .first()
                .map_or(Row::Submit, |_| Row::Option(0)),
            Kind::TextInput => Row::Words,
        };
        question
    }

    /// The rows this kind draws, top to bottom.
    fn rows(&self) -> Vec<Row> {
        let options = (0..self.options.len()).map(Row::Option);
        match self.kind {
            Kind::Confirm | Kind::Select => options.chain([Row::Chat]).collect(),
            Kind::MultiSelect => options.chain([Row::Submit, Row::Chat]).collect(),
            Kind::TextInput => vec![Row::Words, Row::Submit, Row::Chat],
        }
    }

    /// Enter answers an option or submit row, or declines from Chat.
    fn enter(&self) -> PanelKey {
        match (self.kind, self.at) {
            (Kind::Confirm | Kind::Select, Row::Option(_))
            | (Kind::MultiSelect, Row::Option(_) | Row::Submit)
            | (Kind::TextInput, Row::Words | Row::Submit) => PanelKey::Answer,
            (_, Row::Chat) => PanelKey::Decline,
            (Kind::Confirm | Kind::Select, Row::Words | Row::Submit)
            | (Kind::MultiSelect, Row::Words)
            | (Kind::TextInput, Row::Option(_)) => PanelKey::Handled,
        }
    }

    /// Types one character and takes the cursor to the words row.
    fn type_char(&mut self, ch: char) {
        if self.kind == Kind::TextInput {
            form::insert_at(&mut self.words, &mut self.caret, ch);
            self.at = Row::Words;
        }
    }

    /// Toggles the selected multi-choice option when the cursor is on one.
    fn toggle(&mut self) {
        if self.kind == Kind::MultiSelect {
            if let Row::Option(option) = self.at {
                if let Some(toggled) = self.toggled.get_mut(option) {
                    *toggled = !*toggled;
                }
            }
        }
    }

    /// Moves up or down one row, stopping at either end.
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

    /// Toggled labels in option order, with repeated labels sent once.
    fn selected_labels(&self) -> Vec<String> {
        let mut labels = Vec::new();
        for (choice, toggled) in self.options.iter().zip(&self.toggled) {
            if *toggled && !labels.contains(&choice.label) {
                labels.push(choice.label.clone());
            }
        }
        labels
    }
}

#[cfg(test)]
#[path = "one_question_tests.rs"]
mod tests;
