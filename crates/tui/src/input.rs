//! The draft editor: the input box's text, its cursor and its paste
//! tokens (`docs/tui.md`, "The input box").

use ratatui::text::Span;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::approvals::Queue;
use crate::keys::Edit;

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
        /// Its number, the `N` of its label.
        number: usize,
        /// What the draft shows, `[Pasted text #N · L lines]`.
        label: String,
        /// What is sent.
        text: String,
    },
    /// A pasted image, shown as its positional label.
    Image {
        /// The image's bytes, in base64.
        data: Arc<str>,
    },
}

/// The draft: pieces with the cursor between two of them.
#[derive(Debug, Clone)]
pub(crate) struct Draft {
    pieces: Vec<Piece>,
    /// The cursor: the number of pieces before it.
    cursor: usize,
    /// The next paste token's number.
    next: usize,
    /// The draft's serial: a read's result lands only while the box holds
    /// the draft it started for.
    serial: u64,
}

/// The next draft serial: every draft, and every renewal, takes a fresh
/// one, so no two drafts share a serial.
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(0);

/// A serial no draft holds yet.
fn next_serial() -> u64 {
    NEXT_SERIAL.fetch_add(1, Ordering::Relaxed)
}

/// An image's label: images count in draft order from 1, separately from
/// paste tokens, computed when drawn.
fn image_label(number: usize) -> String {
    format!("[Image #{number}]")
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            pieces: Vec::new(),
            cursor: 0,
            next: 1,
            serial: next_serial(),
        }
    }
}

/// Routes one editing key: the request the panel shows takes every edit
/// while it is open; otherwise the draft takes it.
pub(crate) fn route(edit: Edit, draft: &mut Draft, queue: &mut Queue) {
    if queue.open() {
        queue.on_edit(&edit);
    } else {
        draft.edit(edit);
    }
}

/// The wrapped rows at one width, and where each cursor position shows.
struct Layout {
    /// The rows, prefixed.
    rows: Vec<String>,
    /// For each cursor position, 0 to the piece count: row and column.
    at: Vec<(usize, u16)>,
    /// The cells each paste token's label takes, one span per row.
    tokens: Vec<TokenSpan>,
}

/// The cells of one row a paste token's label takes in [`Draft::rows`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TokenSpan {
    /// The token's number.
    pub(crate) number: usize,
    /// The row.
    pub(crate) row: usize,
    /// The first column.
    pub(crate) start: u16,
    /// The column after the last.
    pub(crate) end: u16,
}

impl Draft {
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
        let text = clean(text);
        if text.is_empty() {
            return;
        }
        if line_count(&text) > PASTE_LINES {
            let number = self.next;
            self.next = self.next.saturating_add(1);
            self.put(token(number, text));
        } else {
            for ch in text.chars() {
                self.put(Piece::Char(ch));
            }
        }
    }

    /// The number of the paste token directly before the cursor, else of
    /// the one directly after it. An image beside the cursor is no token.
    pub(crate) fn token_at_cursor(&self) -> Option<usize> {
        [self.before(self.cursor), self.pieces.get(self.cursor)]
            .into_iter()
            .find_map(|piece| match piece {
                Some(Piece::Paste { number, .. }) => Some(*number),
                Some(Piece::Char(_) | Piece::Image { .. }) | None => None,
            })
    }

    /// The full text of paste token `number`.
    pub(crate) fn token_text(&self, number: usize) -> Option<&str> {
        self.pieces.iter().find_map(|piece| match piece {
            Piece::Paste {
                number: found,
                text,
                ..
            } if *found == number => Some(text.as_str()),
            Piece::Paste { .. } | Piece::Char(_) | Piece::Image { .. } => None,
        })
    }

    /// Replaces the text of paste token `number`, read as a paste is: the
    /// token keeps its number and its label counts the new lines, or, at
    /// [`PASTE_LINES`] lines or fewer, the text replaces the token inline.
    /// The cursor stays beside the same pieces.
    pub(crate) fn set_token(&mut self, number: usize, text: &str) {
        let Some(at) = self.pieces.iter().position(
            |piece| matches!(piece, Piece::Paste { number: found, .. } if *found == number),
        ) else {
            return;
        };
        let text = clean(text);
        if line_count(&text) > PASTE_LINES {
            if let Some(piece) = self.pieces.get_mut(at) {
                *piece = token(number, text);
            }
            return;
        }
        let chars: Vec<Piece> = text.chars().map(Piece::Char).collect();
        let len = chars.len();
        self.pieces.splice(at..=at, chars);
        if self.cursor > at {
            self.cursor = self.cursor.saturating_add(len).saturating_sub(1);
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

    /// Moves to the start of the previous word. A token or an image is a
    /// word alone.
    pub(crate) fn word_left(&mut self) {
        self.cursor = self.word_left_at();
    }

    /// Moves to the end of the next word. A token or an image is a word
    /// alone.
    pub(crate) fn word_right(&mut self) {
        let mut at = self.cursor;
        while self.pieces.get(at).is_some_and(Piece::is_gap) {
            at = at.saturating_add(1);
        }
        if let Some(Piece::Paste { .. } | Piece::Image { .. }) = self.pieces.get(at) {
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

    /// Replaces the draft with `text`, typed, the cursor at its end: line
    /// breaks read as a paste's do, and no token.
    pub(crate) fn set(&mut self, text: &str) {
        self.clear();
        for ch in clean(text).chars() {
            self.put(Piece::Char(ch));
        }
    }

    /// Inserts the pasted image's base64 at the cursor, as one piece.
    pub(crate) fn insert_image(&mut self, data: Arc<str>) {
        self.put(Piece::Image { data });
    }

    /// Whether the draft holds an image.
    pub(crate) fn has_image(&self) -> bool {
        self.pieces
            .iter()
            .any(|piece| matches!(piece, Piece::Image { .. }))
    }

    /// The draft's serial.
    pub(crate) fn serial(&self) -> u64 {
        self.serial
    }

    /// The draft's parts in order: each run of characters and paste-token
    /// text is one text part, each image one image part; an empty text
    /// part is left out. A draft without images sends exactly one text
    /// part, or none when empty.
    pub(crate) fn content(&self) -> Vec<contract::commands::SentPart> {
        use contract::commands::SentPart;
        let mut parts = Vec::new();
        let mut text = String::new();
        for piece in &self.pieces {
            match piece {
                Piece::Char(ch) => text.push(*ch),
                Piece::Paste { text: pasted, .. } => text.push_str(pasted),
                Piece::Image { data } => {
                    if !text.is_empty() {
                        parts.push(SentPart::Text {
                            text: std::mem::take(&mut text),
                        });
                    }
                    parts.push(SentPart::Image {
                        data: data.to_string(),
                        mime_type: crate::paste_image::MIME_TYPE.to_owned(),
                    });
                }
            }
        }
        if !text.is_empty() {
            parts.push(SentPart::Text { text });
        }
        parts
    }

    /// The editor returned `text` for the whole draft: each `[Image #N]`
    /// naming one of the draft's images, 1 to its count, becomes that
    /// image again, first occurrence only; an image whose label is gone is
    /// dropped, and other text stays text. Line breaks read as a paste's
    /// do, and control characters but tabs go, as in [`Draft::set`]. The
    /// cursor ends at the end, under a fresh serial.
    pub(crate) fn edited(&mut self, text: &str) {
        let images: Vec<Arc<str>> = self
            .pieces
            .iter()
            .filter_map(|piece| match piece {
                Piece::Image { data } => Some(Arc::clone(data)),
                Piece::Char(_) | Piece::Paste { .. } => None,
            })
            .collect();
        let mut used = vec![false; images.len()];
        let mut pieces = Vec::new();
        let cleaned = clean(text);
        let mut rest = cleaned.as_str();
        while let Some(ch) = rest.chars().next() {
            if ch == '['
                && let Some((image, tail)) = take_label(rest, &images, &mut used)
            {
                pieces.push(image);
                rest = tail;
                continue;
            }
            pieces.push(Piece::Char(ch));
            rest = rest.get(ch.len_utf8()..).unwrap_or_default();
        }
        let next = self.next;
        let cursor = pieces.len();
        *self = Self {
            pieces,
            cursor,
            next,
            serial: next_serial(),
        };
    }

    /// Puts the moved draft `back` in the box: its pieces under a fresh
    /// serial, the cursor at its end.
    pub(crate) fn put_back(&mut self, back: Draft) {
        let len = back.pieces.len();
        let next = self.next.max(back.next);
        *self = Self {
            pieces: back.pieces,
            cursor: len,
            next,
            serial: next_serial(),
        };
    }

    /// The cursor: the number of pieces before it.
    pub(crate) fn position(&self) -> usize {
        self.cursor
    }

    /// Whether the cursor is at the draft's start or after whitespace. An
    /// image before the cursor is no space, so `@` typed after one opens
    /// no file panel.
    pub(crate) fn after_space(&self) -> bool {
        match self.before(self.cursor) {
            None => true,
            Some(Piece::Char(ch)) => ch.is_whitespace(),
            Some(Piece::Paste { .. } | Piece::Image { .. }) => false,
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

    /// The text to send: every token as its full text, every image as its
    /// label, which the editor shows and [`Draft::edited`] reads back.
    pub(crate) fn expand(&self) -> String {
        let mut out = String::new();
        let mut image = 0usize;
        for piece in &self.pieces {
            match piece {
                Piece::Char(ch) => out.push(*ch),
                Piece::Paste { text, .. } => out.push_str(text),
                Piece::Image { .. } => {
                    image = image.saturating_add(1);
                    out.push_str(&image_label(image));
                }
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

    /// The cells each paste token's label takes at `width`, in
    /// [`Draft::rows`]' rows and columns: one span per row a label covers.
    pub(crate) fn token_spans(&self, width: u16) -> Vec<TokenSpan> {
        self.layout(width).tokens
    }

    /// Inserts `piece` at the cursor and moves past it.
    fn put(&mut self, piece: Piece) {
        self.pieces.insert(self.cursor, piece);
        self.cursor = self.cursor.saturating_add(1);
    }

    /// Where the previous word starts: back over gaps, then over one
    /// token, one image, or a run of word characters.
    fn word_left_at(&self) -> usize {
        let mut at = self.cursor;
        while self.before(at).is_some_and(Piece::is_gap) {
            at = at.saturating_sub(1);
        }
        if let Some(Piece::Paste { .. } | Piece::Image { .. }) = self.before(at) {
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
        let mut tokens: Vec<TokenSpan> = Vec::new();
        let mut col = 0usize;
        let mut image = 0usize;
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
                Piece::Paste { number, label, .. } => {
                    for (index, ch) in label.chars().enumerate() {
                        let w = char_width(ch);
                        wrap(&mut rows, &mut col, w);
                        let (row, start) = place(&rows, col);
                        if index == 0 {
                            at.push((row, start));
                        }
                        push(&mut rows, ch);
                        col = col.saturating_add(w);
                        let end = place(&rows, col).1;
                        match tokens.last_mut() {
                            Some(span) if index > 0 && span.row == row => span.end = end,
                            _ => tokens.push(TokenSpan {
                                number: *number,
                                row,
                                start,
                                end,
                            }),
                        }
                    }
                }
                // An image draws as its positional label, with no click
                // target: `token_spans` leaves it out.
                Piece::Image { .. } => {
                    image = image.saturating_add(1);
                    for (index, ch) in image_label(image).chars().enumerate() {
                        let w = char_width(ch);
                        wrap(&mut rows, &mut col, w);
                        let (row, start) = place(&rows, col);
                        if index == 0 {
                            at.push((row, start));
                        }
                        push(&mut rows, ch);
                        col = col.saturating_add(w);
                    }
                }
            }
        }
        wrap(&mut rows, &mut col, 1);
        at.push(place(&rows, col));
        Layout { rows, at, tokens }
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

/// Pasted text as the draft holds it: `\r\n` and `\r` become line breaks,
/// and control characters but line breaks and tabs go.
fn clean(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|ch| matches!(ch, '\n' | '\t') || !ch.is_control())
        .collect()
}

/// The lines of `text`, not counting a trailing line break.
fn line_count(text: &str) -> usize {
    text.strip_suffix('\n').unwrap_or(text).split('\n').count()
}

/// Paste token `number` holding `text`.
fn token(number: usize, text: String) -> Piece {
    let label = format!("[Pasted text #{number} · {} lines]", line_count(&text));
    Piece::Paste {
        number,
        label,
        text,
    }
}

/// The image `text` names at its start: `[Image #N]` for one of `images`,
/// 1 to their count, each relinking once through `used`. `None` for any
/// other text, which stays text.
fn take_label<'a>(
    text: &'a str,
    images: &[Arc<str>],
    used: &mut [bool],
) -> Option<(Piece, &'a str)> {
    let tail = text.strip_prefix("[Image #")?;
    let digits = tail.bytes().take_while(|byte| byte.is_ascii_digit()).count();
    let (number, rest) = tail.split_at(digits);
    let rest = rest.strip_prefix(']')?;
    let at = number.parse::<usize>().ok()?.checked_sub(1)?;
    let data = Arc::clone(images.get(at)?);
    if used.get(at) == Some(&true) {
        return None;
    }
    if let Some(slot) = used.get_mut(at) {
        *slot = true;
    }
    Some((Piece::Image { data }, rest))
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
