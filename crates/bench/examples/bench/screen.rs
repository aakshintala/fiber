//! The terminal screen behind the bench harness's waits: a grid of cells
//! fed the pty's raw bytes, so a wait sees what the terminal holds rather
//! than the bytes its last frame drew. Ratatui redraws only changed cells,
//! so a frame can skip letters that are already on screen; searching the
//! byte stream for them cannot work.
//!
//! [`Screen::feed`] tracks only what the waits need: printable characters
//! at the cursor (which advances, clamping at the last column without
//! wrapping), carriage return, line feed, cursor positioning (`ESC[r;cH`,
//! a missing parameter meaning 1), entering or leaving the alternate
//! screen (`ESC[?1049h` and `ESC[?1049l`, which clear the grid and home
//! the cursor), erase in display and erase in line (`ESC[2J` clears the
//! grid, `ESC[K` clears to the end of the row). Every other CSI sequence,
//! SGR included, every OSC sequence (`ESC ]` to `BEL` or `ESC \` ) and
//! every other two-byte escape sequence is skipped: a skipped sequence
//! never touches a cell. Each character, wide or not, takes one cell.

/// The terminal screen: its cells, its cursor, and the unfinished input
/// carried to the next [`Screen::feed`], a split UTF-8 character or a
/// split escape sequence.
pub(crate) struct Screen {
    cols: usize,
    rows: usize,
    cells: Vec<Vec<char>>,
    row: usize,
    col: usize,
    state: State,
    undecoded: Vec<u8>,
}

/// Where the next fed character goes: plain text, inside an escape, inside
/// a control sequence, or inside an operating-system-command sequence.
enum State {
    Ground,
    Esc,
    Csi(Vec<u8>),
    Osc,
    OscEsc,
}

/// One `ESC[r;cH` parameter: digits, empty or zero meaning 1, anything
/// else meaning 1.
fn cup_param(part: &[u8]) -> usize {
    let mut value: usize = 0;
    let mut digits: usize = 0;
    for byte in part {
        let Some(digit) = byte.checked_sub(b'0').filter(|digit| *digit <= 9) else {
            return 1;
        };
        value = value.saturating_mul(10).saturating_add(usize::from(digit));
        digits = digits.saturating_add(1);
    }
    if digits == 0 { 1 } else { value.max(1) }
}

impl Screen {
    /// A blank `cols` by `rows` screen with its cursor home.
    pub(crate) fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            cells: vec![vec![' '; cols]; rows],
            row: 0,
            col: 0,
            state: State::Ground,
            undecoded: Vec::new(),
        }
    }

    /// Draws `bytes` onto the screen, carrying a split UTF-8 character or
    /// a split escape sequence to the next call.
    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        let mut pending = std::mem::take(&mut self.undecoded);
        pending.extend_from_slice(bytes);
        let mut next = 0;
        while next < pending.len() {
            let rest = pending.get(next..).unwrap_or_default();
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    for ch in text.chars() {
                        self.handle_char(ch);
                    }
                    next = pending.len();
                }
                Err(invalid) => {
                    let valid = invalid.valid_up_to();
                    if valid > 0 {
                        let end = next.saturating_add(valid);
                        if let Some(chunk) = pending.get(next..end) {
                            for ch in String::from_utf8_lossy(chunk).chars() {
                                self.handle_char(ch);
                            }
                            next = end;
                        } else {
                            break;
                        }
                    }
                    match invalid.error_len() {
                        // Invalid bytes are skipped: they touch no cell.
                        Some(len) => next = next.saturating_add(len),
                        // An unfinished character waits for more bytes.
                        None => break,
                    }
                }
            }
        }
        self.undecoded = pending.get(next..).unwrap_or_default().to_vec();
    }

    /// Whether any one row's text, its cells joined, holds `needle`. A
    /// needle never spans rows, and an empty one matches nothing: a space
    /// matches a space cell only.
    pub(crate) fn holds(&self, needle: &str) -> bool {
        if needle.is_empty() {
            return false;
        }
        self.cells
            .iter()
            .any(|row| row.iter().collect::<String>().contains(needle))
    }

    /// The rows as text for a timeout note: trailing blanks trimmed,
    /// joined by newlines.
    pub(crate) fn text(&self) -> String {
        self.cells
            .iter()
            .map(|row| {
                let line: String = row.iter().collect();
                line.trim_end().to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Draws one character: plain text, a cursor move, or one step of an
    /// escape sequence a skipped sequence leaves the cells alone.
    fn handle_char(&mut self, ch: char) {
        match std::mem::replace(&mut self.state, State::Ground) {
            State::Ground => match ch {
                '\x1b' => self.state = State::Esc,
                '\r' => self.col = 0,
                '\n' => self.row = self.row.saturating_add(1).min(self.rows.saturating_sub(1)),
                c if c.is_control() => {}
                c => self.put(c),
            },
            // Any other byte ends a two-byte sequence, touching nothing.
            State::Esc => match ch {
                '\x1b' => self.state = State::Esc,
                '[' => self.state = State::Csi(Vec::new()),
                ']' => self.state = State::Osc,
                _ => {}
            },
            State::Csi(mut params) => {
                if ('\u{40}'..='\u{7e}').contains(&ch) {
                    if let Ok(final_byte) = u8::try_from(ch as u32) {
                        self.handle_csi(&params, final_byte);
                    }
                } else if ch.is_ascii()
                    && let Ok(byte) = u8::try_from(ch as u32)
                {
                    params.push(byte);
                    self.state = State::Csi(params);
                }
                // A non-ASCII character inside a sequence abandons it.
            }
            State::Osc => match ch {
                '\x07' => {}
                '\x1b' => self.state = State::OscEsc,
                _ => self.state = State::Osc,
            },
            State::OscEsc => match ch {
                '\\' | '\x07' => {}
                '\x1b' => self.state = State::OscEsc,
                _ => self.state = State::Osc,
            },
        }
    }

    /// Runs one finished CSI sequence's final byte: what the waits track
    /// moves or clears, everything else is skipped.
    fn handle_csi(&mut self, params: &[u8], final_byte: u8) {
        match final_byte {
            b'H' => {
                if params.first() == Some(&b'?') {
                    return;
                }
                let mut parts = params.split(|byte| *byte == b';');
                let row = cup_param(parts.next().unwrap_or_default());
                let col = cup_param(parts.next().unwrap_or_default());
                self.row = row.saturating_sub(1).min(self.rows.saturating_sub(1));
                self.col = col.saturating_sub(1).min(self.cols.saturating_sub(1));
            }
            b'h' | b'l' => {
                if params == b"?1049" {
                    self.clear();
                    self.row = 0;
                    self.col = 0;
                }
            }
            b'J' => {
                if params == b"2" {
                    self.clear();
                }
            }
            b'K' if params.is_empty() || params == b"0" => {
                self.clear_eol();
            }
            _ => {}
        }
    }

    /// Draws one character at the cursor, clamping at the last column
    /// without wrapping: drawing past it overwrites it.
    fn put(&mut self, ch: char) {
        if let Some(row) = self.cells.get_mut(self.row)
            && let Some(cell) = row.get_mut(self.col)
        {
            *cell = ch;
        }
        self.col = self.col.saturating_add(1).min(self.cols.saturating_sub(1));
    }

    /// Blanks every cell, leaving the cursor where it is.
    fn clear(&mut self) {
        for row in &mut self.cells {
            for cell in row.iter_mut() {
                *cell = ' ';
            }
        }
    }

    /// Blanks the cursor's cell to the end of its row.
    fn clear_eol(&mut self) {
        if let Some(row) = self.cells.get_mut(self.row) {
            for cell in row.iter_mut().skip(self.col) {
                *cell = ' ';
            }
        }
    }
}

#[cfg(test)]
#[path = "screen_tests.rs"]
mod tests;
