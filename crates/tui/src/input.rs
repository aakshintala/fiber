//! The draft editor: the input box's text, its cursor and its paste
//! tokens (`docs/tui.md`, "The input box").

use ratatui::text::Span;

use crate::approvals::Queue;
use crate::keys::{Edit, Key};

/// A paste of more lines than this shows as one token.
const PASTE_LINES: usize = 10;

/// The first row's prefix; later rows indent by its width.
const PROMPT: &str = "> ";

/// The indent of every row after the first.
const INDENT: &str = "  ";

/// One unit the cursor steps over.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    /// A character; `\n` is a line break.
    Char(char),
    /// A pasted text shown as its label.
    Paste {
        /// What the draft shows, `[Pasted text #N · L lines]`.
        label: String,
        /// What is sent.
        text: String,
    },
}

/// The draft: pieces with the cursor between two of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Draft {
    pieces: Vec<Piece>,
    /// The cursor: the number of pieces before it.
    cursor: usize,
    /// The next paste token's number.
    next: usize,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            pieces: Vec::new(),
            cursor: 0,
            next: 1,
        }
    }
}

/// Routes one editing key: the approval panel, while open, takes a paste
/// as typed feedback, its line breaks as spaces, and no other editing key;
/// otherwise the draft takes it.
pub(crate) fn route(edit: Edit, draft: &mut Draft, queue: &mut Queue) {
    if queue.panel().is_none() {
        draft.edit(edit);
    } else if let Edit::Paste(text) = edit {
        for ch in text.chars() {
            let ch = if ch.is_control() { ' ' } else { ch };
            queue.on_key(&Key::Char(ch));
        }
    }
}

/// The wrapped rows at one width, and where each cursor position shows.
struct Layout {
    /// The rows, prefixed.
    rows: Vec<String>,
    /// For each cursor position, 0 to the piece count: row and column.
    at: Vec<(usize, u16)>,
}

impl Draft {
    /// Applies a key the draft takes: a character, Backspace, or ↑ or ↓
    /// by wrapped row at `width`. `false` for any other key.
    pub(crate) fn key(&mut self, key: &Key, width: u16) -> bool {
        match key {
            Key::Char(ch) => self.insert(*ch),
            Key::Backspace => self.backspace(),
            // debt: ↑ on the first row does nothing, upgrade when prompt
            // recall lands (part 2 of #684).
            Key::Up => drop(self.up(width)),
            Key::Down => drop(self.down(width)),
            Key::Enter
            | Key::Esc
            | Key::CtrlC
            | Key::CtrlO
            | Key::PageUp
            | Key::PageDown
            | Key::End
            | Key::AltA
            | Key::Tab
            | Key::BackTab
            | Key::F1 => return false,
        }
        true
    }

    /// Applies one editing key.
    pub(crate) fn edit(&mut self, edit: Edit) {
        match edit {
            Edit::Left => self.left(),
            Edit::Right => self.right(),
            Edit::ShiftEnter | Edit::CtrlJ => self.line_break(),
            Edit::WordLeft => self.word_left(),
            Edit::WordRight => self.word_right(),
            Edit::DeleteWord => self.delete_word(),
            Edit::LineStart => self.line_start(),
            Edit::LineEnd => self.line_end(),
            Edit::Delete => self.delete(),
            Edit::Paste(text) => self.paste(&text),
        }
    }

    /// Inserts `ch` at the cursor. A control character other than a tab
    /// is not inserted; a line break comes from [`Draft::line_break`].
    pub(crate) fn insert(&mut self, ch: char) {
        if ch == '\t' || !ch.is_control() {
            self.put(Piece::Char(ch));
        }
    }

    /// Inserts a line break at the cursor.
    pub(crate) fn line_break(&mut self) {
        self.put(Piece::Char('\n'));
    }

    /// Inserts pasted text at the cursor. `\r\n` and `\r` become line
    /// breaks and other control characters but tabs go. More than
    /// [`PASTE_LINES`] lines, not counting a trailing line break, become
    /// one token.
    pub(crate) fn paste(&mut self, text: &str) {
        let text: String = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .chars()
            .filter(|ch| matches!(ch, '\n' | '\t') || !ch.is_control())
            .collect();
        if text.is_empty() {
            return;
        }
        let lines = text.strip_suffix('\n').unwrap_or(&text).split('\n').count();
        if lines > PASTE_LINES {
            let label = format!("[Pasted text #{} · {lines} lines]", self.next);
            self.next = self.next.saturating_add(1);
            self.put(Piece::Paste { label, text });
        } else {
            for ch in text.chars() {
                self.put(Piece::Char(ch));
            }
        }
    }

    /// Deletes the piece before the cursor.
    pub(crate) fn backspace(&mut self) {
        if let Some(at) = self.cursor.checked_sub(1) {
            self.pieces.remove(at);
            self.cursor = at;
        }
    }

    /// Deletes the piece after the cursor.
    pub(crate) fn delete(&mut self) {
        if self.cursor < self.pieces.len() {
            self.pieces.remove(self.cursor);
        }
    }

    /// Deletes back to the start of the previous word, stopping at the
    /// line's start unless the cursor is already there.
    pub(crate) fn delete_word(&mut self) {
        let line = self.line_start_at();
        let mut target = self.word_left_at();
        if self.cursor != line {
            target = target.max(line);
        }
        self.pieces.drain(target..self.cursor);
        self.cursor = target;
    }

    /// Moves left one piece.
    pub(crate) fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves right one piece.
    pub(crate) fn right(&mut self) {
        self.cursor = self.cursor.saturating_add(1).min(self.pieces.len());
    }

    /// Moves to the start of the previous word. A token is a word alone.
    pub(crate) fn word_left(&mut self) {
        self.cursor = self.word_left_at();
    }

    /// Moves to the end of the next word. A token is a word alone.
    pub(crate) fn word_right(&mut self) {
        let mut at = self.cursor;
        while self.pieces.get(at).is_some_and(Piece::is_gap) {
            at = at.saturating_add(1);
        }
        if let Some(Piece::Paste { .. }) = self.pieces.get(at) {
            self.cursor = at.saturating_add(1);
            return;
        }
        while self.pieces.get(at).is_some_and(Piece::is_word) {
            at = at.saturating_add(1);
        }
        self.cursor = at;
    }

    /// Moves to the start of the logical line.
    pub(crate) fn line_start(&mut self) {
        self.cursor = self.line_start_at();
    }

    /// Moves to the end of the logical line.
    pub(crate) fn line_end(&mut self) {
        let after = self.pieces.get(self.cursor..).unwrap_or_default();
        let offset = after
            .iter()
            .position(|piece| *piece == Piece::Char('\n'))
            .unwrap_or(after.len());
        self.cursor = self.cursor.saturating_add(offset);
    }

    /// Moves up one wrapped row at `width`, keeping the column where it
    /// fits. `false` when already on the first row: nothing moved.
    pub(crate) fn up(&mut self, width: u16) -> bool {
        let layout = self.layout(width);
        let Some(&(row, col)) = layout.at.get(self.cursor) else {
            return false;
        };
        let Some(target) = row.checked_sub(1) else {
            return false;
        };
        self.move_to(&layout, target, col)
    }

    /// Moves down one wrapped row at `width`, keeping the column where it
    /// fits. `false` when already on the last row: nothing moved.
    pub(crate) fn down(&mut self, width: u16) -> bool {
        let layout = self.layout(width);
        let Some(&(row, col)) = layout.at.get(self.cursor) else {
            return false;
        };
        self.move_to(&layout, row.saturating_add(1), col)
    }

    /// Whether the draft holds nothing.
    pub(crate) fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// Empties the draft; token numbers start from 1 again.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Replaces the draft with `text`, typed, the cursor at its end.
    pub(crate) fn set(&mut self, text: &str) {
        self.clear();
        for ch in text.chars() {
            self.put(Piece::Char(ch));
        }
    }

    /// The cursor: the number of pieces before it.
    pub(crate) fn position(&self) -> usize {
        self.cursor
    }

    /// Whether the cursor is at the draft's start or after whitespace.
    pub(crate) fn after_space(&self) -> bool {
        match self.before(self.cursor) {
            None => true,
            Some(Piece::Char(ch)) => ch.is_whitespace(),
            Some(Piece::Paste { .. }) => false,
        }
    }

    /// The `@` file query whose `@` is the piece at `anchor`: the
    /// characters after it up to whitespace, a token or the end, and the
    /// position where they end. `None` when that piece is no `@`, or the
    /// cursor is not after the `@` and within the query.
    pub(crate) fn mention(&self, anchor: usize) -> Option<(String, usize)> {
        if self.pieces.get(anchor) != Some(&Piece::Char('@')) {
            return None;
        }
        let start = anchor.saturating_add(1);
        let mut end = start;
        let mut query = String::new();
        while let Some(Piece::Char(ch)) = self.pieces.get(end)
            && !ch.is_whitespace()
        {
            query.push(*ch);
            end = end.saturating_add(1);
        }
        (start..=end).contains(&self.cursor).then_some((query, end))
    }

    /// Replaces the pieces in `range` with `text`, typed, the cursor after
    /// it.
    pub(crate) fn replace(&mut self, range: std::ops::Range<usize>, text: &str) {
        let start = range.start.min(self.pieces.len());
        let end = range.end.clamp(start, self.pieces.len());
        self.pieces.drain(start..end);
        self.cursor = start;
        for ch in text.chars() {
            self.put(Piece::Char(ch));
        }
    }

    /// The text to send: every token as its full text.
    pub(crate) fn expand(&self) -> String {
        let mut out = String::new();
        for piece in &self.pieces {
            match piece {
                Piece::Char(ch) => out.push(*ch),
                Piece::Paste { text, .. } => out.push_str(text),
            }
        }
        out
    }

    /// The rows the input box shows at `width`: wrapped, tokens as their
    /// labels, the first row prefixed `> ` and the rest indented.
    pub(crate) fn rows(&self, width: u16) -> Vec<String> {
        self.layout(width).rows
    }

    /// The cursor's row and column in [`Draft::rows`] at `width`.
    pub(crate) fn cursor(&self, width: u16) -> (usize, u16) {
        self.layout(width)
            .at
            .get(self.cursor)
            .copied()
            .unwrap_or_default()
    }

    /// Inserts `piece` at the cursor and moves past it.
    fn put(&mut self, piece: Piece) {
        self.pieces.insert(self.cursor, piece);
        self.cursor = self.cursor.saturating_add(1);
    }

    /// Where the previous word starts: back over gaps, then over one
    /// token or a run of word characters.
    fn word_left_at(&self) -> usize {
        let mut at = self.cursor;
        while self.before(at).is_some_and(Piece::is_gap) {
            at = at.saturating_sub(1);
        }
        if let Some(Piece::Paste { .. }) = self.before(at) {
            return at.saturating_sub(1);
        }
        while self.before(at).is_some_and(Piece::is_word) {
            at = at.saturating_sub(1);
        }
        at
    }

    /// Where the cursor's logical line starts.
    fn line_start_at(&self) -> usize {
        let mut at = self.cursor;
        while self
            .before(at)
            .is_some_and(|piece| *piece != Piece::Char('\n'))
        {
            at = at.saturating_sub(1);
        }
        at
    }

    /// The piece before position `at`.
    fn before(&self, at: usize) -> Option<&Piece> {
        self.pieces.get(at.checked_sub(1)?)
    }

    /// Moves to the position on `row` whose column is the largest not past
    /// `col`, or the row's first. `false` when no position is on `row`.
    fn move_to(&mut self, layout: &Layout, row: usize, col: u16) -> bool {
        let on_row = || {
            layout
                .at
                .iter()
                .enumerate()
                .filter(move |(_, at)| at.0 == row)
        };
        let found = on_row()
            .rev()
            .find(|(_, at)| at.1 <= col)
            .or_else(|| on_row().next());
        match found {
            Some((index, _)) => {
                self.cursor = index;
                true
            }
            None => false,
        }
    }

    /// Wraps the pieces at `width`. Every position needs a free column: a
    /// piece, or the cursor before a line break or at the end, that does
    /// not fit starts a new row. A wide character never splits.
    fn layout(&self, width: u16) -> Layout {
        let inner = usize::from(width)
            .saturating_sub(PROMPT.chars().count())
            .max(1);
        let mut rows = vec![PROMPT.to_owned()];
        let mut at = Vec::with_capacity(self.pieces.len().saturating_add(1));
        let mut col = 0usize;
        // Starts a new row when `need` columns do not fit after `col`.
        let wrap = |rows: &mut Vec<String>, col: &mut usize, need: usize| {
            if *col > 0 && col.saturating_add(need) > inner {
                rows.push(INDENT.to_owned());
                *col = 0;
            }
        };
        let place = |rows: &Vec<String>, col: usize| {
            let col = col.saturating_add(PROMPT.chars().count());
            (
                rows.len().saturating_sub(1),
                u16::try_from(col).unwrap_or(u16::MAX),
            )
        };
        for piece in &self.pieces {
            match piece {
                Piece::Char('\n') => {
                    wrap(&mut rows, &mut col, 1);
                    at.push(place(&rows, col));
                    rows.push(INDENT.to_owned());
                    col = 0;
                }
                Piece::Char(ch) => {
                    let ch = if *ch == '\t' { ' ' } else { *ch };
                    let w = char_width(ch);
                    wrap(&mut rows, &mut col, w);
                    at.push(place(&rows, col));
                    push(&mut rows, ch);
                    col = col.saturating_add(w);
                }
                Piece::Paste { label, .. } => {
                    for (index, ch) in label.chars().enumerate() {
                        let w = char_width(ch);
                        wrap(&mut rows, &mut col, w);
                        if index == 0 {
                            at.push(place(&rows, col));
                        }
                        push(&mut rows, ch);
                        col = col.saturating_add(w);
                    }
                }
            }
        }
        wrap(&mut rows, &mut col, 1);
        at.push(place(&rows, col));
        Layout { rows, at }
    }
}

impl Piece {
    /// A character no word holds: space, punctuation, a line break.
    fn is_gap(&self) -> bool {
        matches!(self, Self::Char(ch) if !is_word_char(*ch))
    }

    /// A character of a word.
    fn is_word(&self) -> bool {
        matches!(self, Self::Char(ch) if is_word_char(*ch))
    }
}

/// A word is a run of alphanumeric characters or `_`.
fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Appends `ch` to the last row.
fn push(rows: &mut [String], ch: char) {
    if let Some(row) = rows.last_mut() {
        row.push(ch);
    }
}

/// The columns `ch` takes on screen, as ratatui measures it.
fn char_width(ch: char) -> usize {
    let mut buf = [0u8; 4];
    Span::raw(&*ch.encode_utf8(&mut buf)).width()
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
