//! An `ask_user` question form on the request panel: one tab per question,
//! then a Submit tab, answered by keys with one `reply` (`docs/tui.md`, "A
//! question form").

use contract::commands::{ReplyAnswer, SentFormAnswer};
use contract::shapes::{Question, True};
use ratatui::style::{Modifier, Style};

use super::builder::Builder;
use super::text::{cells, clean, delete_before, insert_at, move_caret};
use super::{Panel, PanelKey, Title};
use crate::format::{answer_parts, width as measure};
use crate::keys::{Edit, Key};
use crate::theme::Role;

/// "▸ " on the cursor's row, a chosen mark, `Next →`: in secondary.
const SECONDARY: Style = Style::new().fg(Role::Secondary.color());

/// `Next →` under the cursor: bold in secondary.
const SECONDARY_BOLD: Style = Style::new()
    .fg(Role::Secondary.color())
    .add_modifier(Modifier::BOLD);

/// An answered tab, an answer's header, "note": in accent.
const ACCENT: Style = Style::new().fg(Role::Accent.color());

/// An answered tab under the cursor: in accent and reversed.
const ACCENT_REVERSED: Style = Style::new()
    .fg(Role::Accent.color())
    .add_modifier(Modifier::REVERSED);

/// The question, "Review", an answer, typed words: bold.
const BOLD: Style = Style::new().add_modifier(Modifier::BOLD);

/// A chosen label under the cursor: bold and underlined.
const BOLD_UNDERLINED: Style = Style::new()
    .add_modifier(Modifier::BOLD)
    .add_modifier(Modifier::UNDERLINED);

/// A label under the cursor: underlined.
const UNDERLINED: Style = Style::new().add_modifier(Modifier::UNDERLINED);

/// The current tab, " Submit ": reversed.
const REVERSED: Style = Style::new().add_modifier(Modifier::REVERSED);

/// The placeholder's first character with the cursor on its row: dim and
/// reversed.
const FIRST: Style = Style::new()
    .add_modifier(Modifier::DIM)
    .add_modifier(Modifier::REVERSED);

/// A number, the hint, "choose any", a description, an unchosen mark, the
/// Submit tab's trailing words: dim.
const DIM: Style = Style::new().add_modifier(Modifier::DIM);

/// " Chat about this " on the Submit tab: on the rule tint.
const CHAT: Style = Style::new().bg(Role::Rule.color());

/// A question form and what the person has answered so far.
#[derive(Debug, Clone)]
pub(crate) struct Form {
    /// The questions, as the model asked them.
    fields: Vec<Question>,
    /// One per question, in field order.
    answers: Vec<Field>,
    /// The note on the whole form.
    note: String,
    /// The text cursor in the note: the characters before it.
    note_caret: usize,
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

/// A row of a question's tab, top to bottom: an option, the row to answer
/// in words, and, on a multi-choice question only, `Next →` or `Review →`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// An option, by index.
    Option(usize),
    /// The row to answer in words.
    Words,
    /// `Next →`, or `Review →` on the last question.
    Next,
}

/// A row of the Submit tab, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubmitRow {
    Note,
    Send,
}

/// A click target on the form or one-question rows. `Tab(n)`, `n` the
/// question count, is the Submit tab; `Send` is the Submit row, `Chat` the
/// "Chat about this" button on the Submit tab.
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
            note_caret: 0,
            at: Cursor::Submit(SubmitRow::Send),
        };
        form.open_tab(0);
        form
    }

    /// How many questions the form asks.
    pub(super) fn question_count(&self) -> usize {
        self.fields.len()
    }

    /// A key on the form. `None`: not the form's key; it passes through.
    pub(crate) fn on_key(&mut self, key: &Key) -> Option<PanelKey> {
        match key {
            Key::Esc => return Some(PanelKey::Decline),
            Key::Enter => return Some(self.enter()),
            Key::Char(' ') => self.space(),
            Key::Char(ch) => {
                // A digit outside a row that takes text chooses that
                // option; '0' and digits past the words row do nothing.
                let digits = matches!(self.at, Cursor::Question { row, .. } if row != Row::Words)
                    && ch.is_ascii_digit();
                if digits {
                    if *ch != '0'
                        && let Cursor::Question { field, .. } = self.at
                    {
                        self.digit(field, *ch);
                    }
                } else {
                    self.type_char(*ch);
                }
            }
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

    /// A digit `d` in '1'..='9' outside a row that takes text: chooses or
    /// toggles option `d - 1` and moves the cursor to it, goes to the words
    /// row for `d == options + 1`, and does nothing past it.
    fn digit(&mut self, field: usize, d: char) {
        let options = self
            .fields
            .get(field)
            .map_or(0, |question| question.options.len());
        let n = (d as u8).saturating_sub(b'0') as usize;
        if n == 0 || n > options.saturating_add(1) {
            return;
        }
        if n == options.saturating_add(1) {
            self.at = Cursor::Question {
                field,
                row: Row::Words,
            };
            return;
        }
        if let Some(answer) = self.answers.get_mut(field) {
            match &mut answer.picks {
                Picks::One(choice) => *choice = Some(n.saturating_sub(1)),
                Picks::Many(chosen) => {
                    if let Some(chosen) = chosen.get_mut(n.saturating_sub(1)) {
                        *chosen = !*chosen;
                    }
                }
            }
        }
        self.at = Cursor::Question {
            field,
            row: Row::Option(n.saturating_sub(1)),
        };
    }

    /// An editing key: a paste is typed on the words row, or into the note
    /// on the Submit tab, a control character as a space; ← and → move the
    /// text cursor on the words row and on the note row, and move between
    /// the tabs from every other row. Every other edit does nothing.
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
            | Edit::DeleteToLineStart
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
    /// does on a multi-choice one, and `Next →` and `Submit` do what Enter
    /// does there (`docs/tui.md`, "A question form"). "Chat about this"
    /// declines from the Submit tab; a row the shown tab lacks does
    /// nothing.
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
            (Spot::Note, Cursor::Submit(_)) => Cursor::Submit(SubmitRow::Note),
            (Spot::Send, Cursor::Submit(_)) => Cursor::Submit(SubmitRow::Send),
            (Spot::Chat, Cursor::Submit(_)) => return PanelKey::Decline,
            (Spot::Chat, Cursor::Question { .. }) => return PanelKey::Handled,
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
            Spot::Next | Spot::Send => self.enter(),
            Spot::Chat => PanelKey::Handled,
        }
    }

    /// The panel for `title` at `width` columns: the title, the tab line,
    /// then the shown tab's rows (`docs/tui.md`, "A question form").
    pub(crate) fn panel(&self, title: &Title, width: u16) -> Panel {
        let mut built = Builder::new();
        built.title(title, width);
        built.blank();
        match self.at {
            Cursor::Question { field, row } => self.question_panel(field, row, width, &mut built),
            Cursor::Submit(row) => self.submit_panel(row, width, &mut built),
        }
        built.finish()
    }

    /// Question `field`'s tab: the tab line, the question, its options with
    /// their descriptions, the words row, `Next →` on a multi-choice
    /// question, and the hint.
    fn question_panel(&self, field: usize, row: Row, width: u16, built: &mut Builder) {
        let (Some(question), Some(answer)) = (self.fields.get(field), self.answers.get(field))
        else {
            return;
        };
        let multi = self.is_multi(field);
        self.tab_line(width, built);
        built.blank();
        built.row(&[(&question.question, BOLD)]);
        if multi {
            built.row(&[("choose any", DIM)]);
        }
        built.blank();
        for (option, choice) in question.options.iter().enumerate() {
            let here = row == Row::Option(option);
            let tick = match (&answer.picks, answer.chosen(option)) {
                (Picks::One(_), true) => "(•)",
                (Picks::One(_), false) => "( )",
                (Picks::Many(_), true) => "[x]",
                (Picks::Many(_), false) => "[ ]",
            };
            let slot = if here {
                ("▸ ", SECONDARY)
            } else {
                ("  ", Style::default())
            };
            let number = format!("{} ", option.saturating_add(1));
            let mark = if answer.chosen(option) {
                (tick, SECONDARY)
            } else {
                (tick, DIM)
            };
            let label = match (answer.chosen(option), here) {
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
                slot,
                (number.as_str(), DIM),
                (mark.0, mark.1),
                (" ", Style::default()),
                (choice.label.as_str(), label),
            ];
            if let Some(description) = description.as_ref() {
                segments.push((description.as_str(), DIM));
            }
            let line = built.row(&segments);
            built.spot(line, Spot::Option(option));
            if here {
                built.here(line);
            }
        }
        self.words_line(field, answer, row == Row::Words, width, built);
        if multi {
            let here = row == Row::Next;
            let next = if field.saturating_add(1) == self.fields.len() {
                "Review →"
            } else {
                "Next →"
            };
            let slot = if here {
                ("▸ ", SECONDARY)
            } else {
                ("  ", Style::default())
            };
            let line = built.row(&[slot, (next, if here { SECONDARY_BOLD } else { SECONDARY })]);
            built.spot(line, Spot::Next);
            if here {
                built.here(line);
            }
        }
        built.blank();
        let hint = if multi {
            "←→ question · ↑↓ choose · Enter chooses and moves on · Space toggles · type to answer in words"
        } else {
            "←→ question · ↑↓ choose · Enter chooses and moves on · type to answer in words"
        };
        built.row(&[(hint, DIM)]);
    }

    /// One tab per question header, marked when answered, then Submit; the
    /// shown tab reversed. Each tab is a click target over its columns
    /// only while the line fits `width` columns on one row.
    fn tab_line(&self, width: u16, built: &mut Builder) {
        let shown = self.tab();
        let headers: Vec<String> = self
            .fields
            .iter()
            .map(|question| clean(&question.header))
            .collect();
        let mut owned: Vec<String> = Vec::with_capacity(headers.len().saturating_add(1));
        for (field, header) in headers.iter().enumerate() {
            let tick = if self.said(field).is_some() {
                "✔"
            } else {
                "☐"
            };
            owned.push(format!(" {tick} {header} "));
        }
        owned.push(" ✔ Submit ".to_owned());
        let mut segments: Vec<(&str, Style)> = vec![("← ", DIM)];
        let mut spots: Vec<((u16, u16), Spot)> = Vec::new();
        let mut at = cells("← ");
        for (tab, text) in owned.iter().enumerate() {
            if tab > 0 {
                segments.push(("  ", Style::default()));
                at = at.saturating_add(2);
            }
            let answered = self.said(tab).is_some();
            // The Submit tab is never answered; off the cursor it
            // reads dim (`docs/tui.md`, "A question form").
            let style = if tab == shown {
                if answered { ACCENT_REVERSED } else { REVERSED }
            } else if answered {
                ACCENT
            } else if tab == self.fields.len() {
                DIM
            } else {
                Style::default()
            };
            let from = at;
            at = at.saturating_add(cells(text));
            spots.push(((from, at), Spot::Tab(tab)));
            segments.push((text.as_str(), style));
        }
        segments.push((" →", DIM));
        at = at.saturating_add(cells(" →"));
        let line = built.row(&segments);
        if usize::from(width) >= usize::from(at) {
            for (cols, spot) in spots {
                built.spot_cols(line, cols, spot);
            }
        }
    }

    /// Question `field`'s words row, numbered after its options: the
    /// placeholder dim, its first character reversed with the cursor on the
    /// row, or the typed words bold, clipped at `width` columns.
    fn words_line(
        &self,
        field: usize,
        answer: &Field,
        here: bool,
        width: u16,
        built: &mut Builder,
    ) {
        let options = self
            .fields
            .get(field)
            .map_or(0, |question| question.options.len());
        let number = format!("{} ", options.saturating_add(1));
        let slot = if here { "▸ " } else { "  " };
        let slot_style = if here { SECONDARY } else { Style::default() };
        let prefix_cells = measure(slot)
            .saturating_add(measure(&number))
            .saturating_add(measure("✎ "));
        let placeholder = if options == 0 {
            "Type an answer"
        } else {
            "Type an answer, alone or with the options"
        };
        let cols = usize::from(width);
        if answer.words.is_empty() {
            let mut segments: Vec<(&str, Style)> =
                vec![(slot, slot_style), (number.as_str(), DIM), ("✎ ", DIM)];
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
                built.here(line);
                built.caret(line, u16::try_from(prefix_cells).unwrap_or(u16::MAX));
            }
            return;
        }
        let words: Vec<char> = clean(&answer.words).chars().collect();
        let before = |start: usize| -> String {
            words
                .get(start..answer.caret)
                .unwrap_or_default()
                .iter()
                .collect()
        };
        let fits = |start: usize| prefix_cells.saturating_add(measure(&before(start))) < cols;
        let start = if here {
            (0..answer.caret)
                .find(|start| fits(*start))
                .unwrap_or(answer.caret)
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
                (number.as_str(), DIM),
                ("✎ ", DIM),
                (shown.as_str(), BOLD),
            ],
            cols,
        );
        built.spot(line, Spot::Words);
        if let Some(caret) = caret {
            built.here(line);
            built.caret(line, caret);
        } else if here {
            built.here(line);
        }
    }

    /// The Submit tab's rows: "Review", each answer, the note, the buttons
    /// and the hint.
    fn submit_panel(&self, row: SubmitRow, width: u16, built: &mut Builder) {
        built.row(&[("Review", BOLD)]);
        built.blank();
        for (field, question) in self.fields.iter().enumerate() {
            let said = self.said(field);
            let said = said
                .as_ref()
                .map(|(labels, text)| (labels.as_slice(), text.as_deref()));
            let parts = answer_parts(&question.header, said);
            built.row(&[
                (parts.header.as_str(), ACCENT),
                (parts.text.as_str(), if parts.skipped { DIM } else { BOLD }),
            ]);
        }
        let slot = if row == SubmitRow::Note {
            ("▸ ", SECONDARY)
        } else {
            ("  ", Style::default())
        };
        let note = clean(&self.note);
        let (rest, style) = if note.is_empty() {
            (" Add a note on the whole form".to_owned(), DIM)
        } else {
            (format!(" {note}"), BOLD)
        };
        let line = built.row(&[slot, ("note", ACCENT), (rest.as_str(), style)]);
        built.spot(line, Spot::Note);
        if row == SubmitRow::Note {
            built.here(line);
            let before: String = note.chars().take(self.note_caret).collect();
            built.caret(
                line,
                cells(slot.0)
                    .saturating_add(cells("note "))
                    .saturating_add(cells(&before)),
            );
        }
        built.blank();
        self.buttons_row(row, width, built);
        built.blank();
        built.row(&[(
            "←→ question · Enter submits · type to add the note · Esc: chat about this",
            DIM,
        )]);
    }

    /// The Submit tab's buttons at `width` columns: " Submit " and
    /// " Chat about this " share one row with the trailing words while
    /// they fit, or stack one per row below it. A column target sits on
    /// a line that fits one row, so stacking keeps each button a
    /// whole-line target through wrapping and scrolling: a click on the
    /// wrapped label still declines (`docs/tui.md`, "A question form").
    fn buttons_row(&self, row: SubmitRow, width: u16, built: &mut Builder) {
        let send = " Submit ";
        let chat = " Chat about this ";
        let rest = "  declines and ends the turn, so you can answer in your own words";
        let single = cells(send)
            .saturating_add(1)
            .saturating_add(cells(chat))
            .saturating_add(cells(rest));
        if single <= width {
            let send_end = cells(send);
            let chat_at = send_end.saturating_add(1);
            let chat_end = chat_at.saturating_add(cells(chat));
            let line = built.row(&[
                (send, REVERSED),
                (" ", Style::default()),
                (chat, CHAT),
                (rest, DIM),
            ]);
            built.spot_cols(line, (0, send_end), Spot::Send);
            built.spot_cols(line, (chat_at, chat_end), Spot::Chat);
            if row == SubmitRow::Send {
                built.here(line);
            }
            return;
        }
        let send_line = built.row(&[(send, REVERSED)]);
        built.spot(send_line, Spot::Send);
        let chat_line = built.row(&[(chat, CHAT)]);
        built.spot(chat_line, Spot::Chat);
        built.row(&[(rest, DIM)]);
        if row == SubmitRow::Send {
            built.here(send_line);
        }
    }

    /// ← or →: one character on the words row and on the note row,
    /// stopping at either end; from any other row, the previous or next
    /// tab, stopping at the first question and at Submit.
    fn arrow(&mut self, right: bool) {
        if let Cursor::Question {
            field,
            row: Row::Words,
        } = self.at
        {
            if let Some(answer) = self.answers.get_mut(field) {
                move_caret(&answer.words, &mut answer.caret, right);
            }
            return;
        }
        if let Cursor::Submit(SubmitRow::Note) = self.at {
            move_caret(&self.note, &mut self.note_caret, right);
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
    /// option, the words row on a question without options, `Send` on the
    /// Submit tab.
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
    /// any other question row, and answers from the note or `Submit`.
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
            },
            Cursor::Submit(SubmitRow::Note | SubmitRow::Send) => return PanelKey::Answer,
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
            Cursor::Question { row: Row::Next, .. } => {}
        }
    }

    /// Types `ch`: on a question, onto its words row, moving the cursor
    /// there; on the Submit tab, into the note at the note cursor, moving
    /// it after the typed character.
    fn type_char(&mut self, ch: char) {
        match self.at {
            Cursor::Question { field, .. } => {
                if let Some(answer) = self.answers.get_mut(field) {
                    insert_at(&mut answer.words, &mut answer.caret, ch);
                }
                self.at = Cursor::Question {
                    field,
                    row: Row::Words,
                };
            }
            Cursor::Submit(_) => {
                insert_at(&mut self.note, &mut self.note_caret, ch);
                self.at = Cursor::Submit(SubmitRow::Note);
            }
        }
    }

    /// Backspace deletes the character before the text cursor on the words
    /// row and on the note row, and does nothing elsewhere.
    fn backspace(&mut self) {
        match self.at {
            Cursor::Question {
                field,
                row: Row::Words,
            } => {
                if let Some(answer) = self.answers.get_mut(field) {
                    delete_before(&mut answer.words, &mut answer.caret);
                }
            }
            Cursor::Submit(SubmitRow::Note) => {
                delete_before(&mut self.note, &mut self.note_caret);
            }
            Cursor::Question {
                row: Row::Option(_) | Row::Next,
                ..
            }
            | Cursor::Submit(SubmitRow::Send) => {}
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

    /// The shown tab's rows, top to bottom: the options, the words row, and
    /// `Next →` on a multi-choice question; the note and `Send` on Submit.
    fn rows(&self) -> Vec<Cursor> {
        match self.at {
            Cursor::Question { field, .. } => {
                let options = self
                    .fields
                    .get(field)
                    .map_or(0, |question| question.options.len());
                let mut rows: Vec<Cursor> = (0..options)
                    .map(|option| Cursor::Question {
                        field,
                        row: Row::Option(option),
                    })
                    .collect();
                rows.push(Cursor::Question {
                    field,
                    row: Row::Words,
                });
                if self.is_multi(field) {
                    rows.push(Cursor::Question {
                        field,
                        row: Row::Next,
                    });
                }
                rows
            }
            Cursor::Submit(_) => [SubmitRow::Note, SubmitRow::Send]
                .map(Cursor::Submit)
                .to_vec(),
        }
    }

    /// Whether question `field` takes any number of options.
    fn is_multi(&self, field: usize) -> bool {
        self.answers
            .get(field)
            .is_some_and(|answer| matches!(answer.picks, Picks::Many(_)))
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
}

#[cfg(test)]
#[path = "form_tests.rs"]
mod tests;
