//! A one-question interaction on the request panel (`docs/tui.md`, "A
//! question form").

use contract::commands::ReplyAnswer;
use contract::shapes::Choice;
use ratatui::style::{Modifier, Style};

use super::builder::Builder;
use super::form::Spot;
use super::text::{clean, delete_before, insert_at, move_caret};
use super::{Panel, PanelKey, Title};
use crate::format::width as measure;
use crate::keys::{Edit, Key};
use crate::theme::Role;

/// "▸ " on the cursor's row, a chosen mark: in secondary.
const SECONDARY: Style = Style::new().fg(Role::Secondary.color());

/// The question, typed words: bold.
const BOLD: Style = Style::new().add_modifier(Modifier::BOLD);

/// A chosen label under the cursor: bold and underlined.
const BOLD_UNDERLINED: Style = Style::new()
    .add_modifier(Modifier::BOLD)
    .add_modifier(Modifier::UNDERLINED);

/// A label under the cursor: underlined.
const UNDERLINED: Style = Style::new().add_modifier(Modifier::UNDERLINED);

/// " Submit ": reversed.
const REVERSED: Style = Style::new().add_modifier(Modifier::REVERSED);

/// The placeholder's first character with the cursor on its row: dim and
/// reversed.
const FIRST: Style = Style::new()
    .add_modifier(Modifier::DIM)
    .add_modifier(Modifier::REVERSED);

/// A number, "choose any", a description, an unchosen mark: dim.
const DIM: Style = Style::new().add_modifier(Modifier::DIM);

/// " Chat about this ": on the rule tint.
const CHAT: Style = Style::new().bg(Role::Rule.color());

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
                // A digit chooses that option: it toggles a multi-choice
                // option and moves the cursor to a confirm or select one,
                // sending nothing. '0' and digits past the options do
                // nothing.
                Kind::Confirm | Kind::Select | Kind::MultiSelect => {
                    if ch.is_ascii_digit() && *ch != '0' {
                        self.digit(*ch);
                    }
                }
                Kind::TextInput => self.type_char(*ch),
            },
            Key::Backspace => {
                if self.at == Row::Words {
                    delete_before(&mut self.words, &mut self.caret);
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

    /// A digit `d` in '1'..='9': toggles a multi-choice option and moves
    /// the cursor to it, or moves the cursor to a confirm or select option.
    /// A digit past the options does nothing.
    fn digit(&mut self, d: char) {
        let n = (d as u8).saturating_sub(b'0') as usize;
        let option = n.checked_sub(1);
        let Some(option) = option.filter(|option| *option < self.options.len()) else {
            return;
        };
        match self.kind {
            Kind::MultiSelect => {
                if let Some(toggled) = self.toggled.get_mut(option) {
                    *toggled = !*toggled;
                }
            }
            Kind::Confirm | Kind::Select | Kind::TextInput => {}
        }
        self.at = Row::Option(option);
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
                if self.at == Row::Words {
                    move_caret(&self.words, &mut self.caret, *edit == Edit::Right);
                }
            }
            Edit::ShiftEnter
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
        match row {
            Row::Option(_) => match self.kind {
                Kind::Confirm | Kind::Select => self.enter(),
                Kind::MultiSelect => {
                    self.toggle();
                    PanelKey::Handled
                }
                Kind::TextInput => PanelKey::Handled,
            },
            Row::Words => {
                self.caret = self.words.chars().count();
                PanelKey::Handled
            }
            Row::Submit | Row::Chat => self.enter(),
        }
    }

    /// Draws the question panel for `title` at `width` columns: the title,
    /// the question, then its rows, ending with "Chat about this"
    /// (`docs/tui.md`, "A question form").
    pub(crate) fn panel(&self, title: &Title, width: u16) -> Panel {
        let mut built = Builder::new();
        built.title(title, width);
        built.blank();
        built.row(&[(self.prompt.as_str(), BOLD)]);
        if self.kind == Kind::MultiSelect {
            built.row(&[("choose any", DIM)]);
        }
        built.blank();
        for row in self.rows() {
            let here = row == self.at;
            let slot = if here {
                ("▸ ", SECONDARY)
            } else {
                ("  ", Style::default())
            };
            let line = match row {
                Row::Option(option) => {
                    let Some(choice) = self.options.get(option) else {
                        continue;
                    };
                    let chosen = self.kind == Kind::MultiSelect
                        && self.toggled.get(option).copied().unwrap_or_default();
                    let tick = match (self.kind, chosen) {
                        (Kind::MultiSelect, true) => "[x]",
                        (Kind::MultiSelect, false) => "[ ]",
                        (Kind::Confirm | Kind::Select | Kind::TextInput, _) => "( )",
                    };
                    let number = format!("{} ", option.saturating_add(1));
                    let label = match (chosen, here) {
                        (true, true) => BOLD_UNDERLINED,
                        (true, false) => BOLD,
                        (false, true) => UNDERLINED,
                        (false, false) => Style::default(),
                    };
                    let description = choice
                        .description
                        .as_ref()
                        .map(|description| format!("  {description}"));
                    let mut segments: Vec<(&str, Style)> = vec![
                        (slot.0, slot.1),
                        (number.as_str(), DIM),
                        (tick, if chosen { SECONDARY } else { DIM }),
                        (" ", Style::default()),
                        (choice.label.as_str(), label),
                    ];
                    if let Some(description) = description.as_ref() {
                        segments.push((description.as_str(), DIM));
                    }
                    let line = built.row(&segments);
                    built.spot(line, Spot::Option(option));
                    line
                }
                Row::Words => self.words_line(here, width, &mut built),
                Row::Submit => {
                    let line = built.row(&[slot, (" Submit ", REVERSED)]);
                    built.spot(line, Spot::Send);
                    line
                }
                Row::Chat => {
                    let line = built.row(&[slot, (" Chat about this ", CHAT)]);
                    built.spot(line, Spot::Chat);
                    line
                }
            };
            if here {
                built.here(line);
            }
        }
        built.finish()
    }

    /// The words row, numbered 1: the placeholder dim, its first character
    /// reversed with the cursor on the row, or the typed words bold,
    /// clipped at `width` columns. Returns its line index.
    fn words_line(&self, here: bool, width: u16, built: &mut Builder) -> usize {
        let slot = if here { "▸ " } else { "  " };
        let slot_style = if here { SECONDARY } else { Style::default() };
        let prefix_cells = measure(slot)
            .saturating_add(measure("1 "))
            .saturating_add(measure("✎ "));
        let cols = usize::from(width);
        if self.words.is_empty() {
            let placeholder = "Type an answer";
            let mut segments: Vec<(&str, Style)> =
                vec![(slot, slot_style), ("1 ", DIM), ("✎ ", DIM)];
            if here {
                let mut first = placeholder;
                let mut rest = "";
                if let Some(at) = placeholder.char_indices().nth(1).map(|(at, _)| at) {
                    (first, rest) = placeholder.split_at(at);
                }
                segments.push((first, FIRST));
                segments.push((rest, DIM));
            } else {
                segments.push((placeholder, DIM));
            }
            let line = built.clipped_row(&segments, cols);
            built.spot(line, Spot::Words);
            if here {
                built.caret(line, u16::try_from(prefix_cells).unwrap_or(u16::MAX));
            }
            return line;
        }
        let words: Vec<char> = clean(&self.words).chars().collect();
        let before = |start: usize| -> String {
            words
                .get(start..self.caret)
                .unwrap_or_default()
                .iter()
                .collect()
        };
        let fits = |start: usize| prefix_cells.saturating_add(measure(&before(start))) < cols;
        let start = if here {
            (0..self.caret)
                .find(|start| fits(*start))
                .unwrap_or(self.caret)
        } else {
            0
        };
        let shown: String = words.get(start..).unwrap_or_default().iter().collect();
        let caret = here.then(|| {
            u16::try_from(prefix_cells.saturating_add(measure(&before(start)))).unwrap_or(u16::MAX)
        });
        let line = built.clipped_row(
            &[
                (slot, slot_style),
                ("1 ", DIM),
                ("✎ ", DIM),
                (shown.as_str(), BOLD),
            ],
            cols,
        );
        built.spot(line, Spot::Words);
        if let Some(caret) = caret {
            built.caret(line, caret);
        }
        line
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
        let at = match kind {
            Kind::Confirm | Kind::Select => options.first().map_or(Row::Chat, |_| Row::Option(0)),
            Kind::MultiSelect => options.first().map_or(Row::Submit, |_| Row::Option(0)),
            Kind::TextInput => Row::Words,
        };
        let toggled = vec![false; options.len()];
        Self {
            kind,
            prompt,
            options,
            toggled,
            words: String::new(),
            caret: 0,
            at,
        }
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
            (Kind::Confirm | Kind::Select | Kind::MultiSelect | Kind::TextInput, Row::Chat) => {
                PanelKey::Decline
            }
            (Kind::Confirm | Kind::Select, Row::Words | Row::Submit)
            | (Kind::MultiSelect, Row::Words)
            | (Kind::TextInput, Row::Option(_)) => PanelKey::Handled,
        }
    }

    /// Types one character and takes the cursor to the words row.
    fn type_char(&mut self, ch: char) {
        insert_at(&mut self.words, &mut self.caret, ch);
        self.at = Row::Words;
    }

    /// Toggles the selected multi-choice option when the cursor is on one.
    fn toggle(&mut self) {
        match self.at {
            Row::Option(option) => {
                if let Some(toggled) = self.toggled.get_mut(option) {
                    *toggled = !*toggled;
                }
            }
            Row::Words | Row::Submit | Row::Chat => {}
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
