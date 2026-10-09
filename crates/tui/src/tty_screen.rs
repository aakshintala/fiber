//! Bounded incremental terminal output: one escape parser across deltas
//! feeding either an 80x24 grid or capped lines (`docs/tui.md`, "Swapped
//! views": a job running under a pseudo-terminal has a live view of its
//! screen).
//!
//! The parser holds its state across deltas, so a sequence split across
//! two deltas completes on the next. A pending sequence over 64 bytes is
//! dropped and the parser returns to the ground, so a runaway sequence
//! cannot grow state or swallow later text past that bound. No raw text is
//! kept and every loop iteration consumes at least one char.

/// The grid's columns: Fiber sets no window size on the pseudo-terminal,
/// and programs then assume 80x24.
pub(crate) const COLUMNS: usize = 80;
/// The grid's rows.
pub(crate) const ROWS: usize = 24;
/// The most pending sequence bytes held: a longer run drops back to the
/// ground. Picked, not measured.
const PENDING_CAP: usize = 64;
/// The most lines a plain job keeps: past it the oldest whole line drops.
/// Picked, not measured.
const LINES_CAP: usize = 1000;
/// The most characters kept on one plain line: past it the tail drops.
/// Picked, not measured.
const LINE_WIDTH: usize = 1024;
/// Tab stops every 8 columns.
const TAB_STOP: usize = 8;

/// What the parser is inside of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Plain text and control bytes.
    Ground,
    /// After `ESC`: the introducer decides.
    Escape,
    /// After `ESC [`: parameter bytes until the final byte.
    Csi,
    /// After `ESC ]`: dropped until `BEL` or `ESC \`.
    Osc,
}

/// One escape parser, holding its state across deltas.
#[derive(Debug, Clone)]
pub(crate) struct Parser {
    /// What the parser is inside of.
    state: State,
    /// The CSI's parameter bytes so far, at most [`PENDING_CAP`].
    params: Vec<u8>,
    /// The OSC's content bytes counted so far, at most [`PENDING_CAP`].
    content: usize,
    /// Whether the OSC saw an `ESC` that may close it.
    osc_esc: bool,
}

impl Parser {
    /// A parser on the ground, with no sequence pending.
    pub(crate) fn new() -> Self {
        Self {
            state: State::Ground,
            params: Vec::new(),
            content: 0,
            osc_esc: false,
        }
    }

    /// Feeds one delta's text: every char is consumed once, either drawn
    /// or dropped as part of a sequence.
    fn feed<S: Sink>(&mut self, text: &str, sink: &mut S) {
        for ch in text.chars() {
            self.step(ch, sink);
        }
    }

    /// Steps one char through the current state.
    fn step<S: Sink>(&mut self, ch: char, sink: &mut S) {
        match self.state {
            State::Ground => self.ground(ch, sink),
            State::Escape => match ch {
                '\x1b' => {}
                '[' => {
                    self.state = State::Csi;
                    self.params.clear();
                }
                ']' => {
                    self.state = State::Osc;
                    self.content = 0;
                    self.osc_esc = false;
                }
                // Any other char completes a dropped two-byte sequence.
                _ => {
                    self.state = State::Ground;
                }
            },
            State::Csi => {
                if ch == '\x1b' {
                    self.state = State::Escape;
                    self.params.clear();
                    return;
                }
                if matches!(ch, '@'..='~') {
                    // The final byte dispatches, whatever the pending
                    // length held: the bound counts the sequence's bytes,
                    // not its terminator.
                    if let Some(byte) = ascii(ch) {
                        dispatch(&self.params, byte, sink);
                    }
                    self.state = State::Ground;
                    self.params.clear();
                    return;
                }
                if self.params.len() >= PENDING_CAP {
                    // Past the bound the sequence is dropped and the byte
                    // reads as ground text, so later text is never
                    // swallowed.
                    self.state = State::Ground;
                    self.params.clear();
                    self.ground(ch, sink);
                    return;
                }
                self.params.push(ascii(ch).unwrap_or_default());
            }
            State::Osc => match ch {
                // `BEL` ends the sequence, dropped.
                '\x07' => {
                    self.state = State::Ground;
                }
                '\x1b' => {
                    self.osc_esc = true;
                }
                '\\' if self.osc_esc => {
                    self.state = State::Ground;
                    self.osc_esc = false;
                }
                _ => {
                    self.osc_esc = false;
                    self.content = self.content.saturating_add(ch.len_utf8());
                    if self.content > PENDING_CAP {
                        self.state = State::Ground;
                        self.content = 0;
                        self.ground(ch, sink);
                    }
                }
            },
        }
    }

    /// One ground char: carriage return, line feed, backspace and tab act;
    /// every other control byte is dropped; the rest draws.
    fn ground<S: Sink>(&mut self, ch: char, sink: &mut S) {
        match ch {
            '\x1b' => {
                self.state = State::Escape;
            }
            '\r' => sink.carriage_return(),
            '\n' => sink.line_feed(),
            '\x08' => sink.backspace(),
            '\t' => sink.tab(),
            _ if ch.is_control() => {}
            _ => sink.put(ch),
        }
    }
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

/// The parser's output: text and cursor moves. Every escape sequence the
/// parser drops never reaches one.
trait Sink {
    /// Draws one char at the cursor and steps past it.
    fn put(&mut self, ch: char);
    /// Moves to the start of the row.
    fn carriage_return(&mut self);
    /// Moves to the start of the next row, scrolling at the bottom.
    fn line_feed(&mut self);
    /// Steps back one cell.
    fn backspace(&mut self);
    /// Moves to the next tab stop.
    fn tab(&mut self);
    /// Moves up `n` rows, clamped.
    fn up(&mut self, n: usize);
    /// Moves down `n` rows, clamped.
    fn down(&mut self, n: usize);
    /// Moves right `n` cells, clamped.
    fn right(&mut self, n: usize);
    /// Moves left `n` cells, clamped.
    fn left(&mut self, n: usize);
    /// Moves to 1-based row `row` and column `col`, clamped.
    fn position(&mut self, row: usize, col: usize);
    /// Erases in the cursor's line: 0 to its end, 1 from its start, 2 all
    /// of it.
    fn erase_line(&mut self, mode: usize);
    /// Erases in the display: 0 below the cursor, 1 above it, 2 all of it.
    /// The cursor stays where it is.
    fn erase_display(&mut self, mode: usize);
}

/// An ASCII char's byte; `None` past ASCII.
fn ascii(ch: char) -> Option<u8> {
    u8::try_from(ch).ok()
}

/// Dispatches a complete CSI: cursor movement and erasing act, and every
/// other sequence is dropped.
fn dispatch<S: Sink>(params: &[u8], final_byte: u8, sink: &mut S) {
    if !params
        .iter()
        .all(|byte| byte.is_ascii_digit() || *byte == b';')
    {
        return;
    }
    match final_byte {
        b'A' => sink.up(number(params, 0, 1)),
        b'B' => sink.down(number(params, 0, 1)),
        b'C' => sink.right(number(params, 0, 1)),
        b'D' => sink.left(number(params, 0, 1)),
        b'H' | b'f' => sink.position(number(params, 0, 1), number(params, 1, 1)),
        b'K' => sink.erase_line(number(params, 0, 0)),
        b'J' => sink.erase_display(number(params, 0, 0)),
        // Color and every other sequence are dropped.
        _ => {}
    }
}

/// The `at`-th `;`-separated parameter, saturating a huge one; missing or
/// empty reads `default`.
fn number(params: &[u8], at: usize, default: usize) -> usize {
    match params.split(|byte| *byte == b';').nth(at) {
        None | Some([]) => default,
        Some(group) => group.iter().fold(0usize, |value, byte| {
            value
                .saturating_mul(10)
                .saturating_add(usize::from(byte.saturating_sub(b'0')))
        }),
    }
}

/// A `tty` job's screen: cells and a cursor.
#[derive(Debug, Clone)]
pub(crate) struct Grid {
    /// The cells, row by row.
    cells: Vec<Vec<char>>,
    /// The cursor's row.
    row: usize,
    /// The cursor's column.
    col: usize,
}

impl Grid {
    /// A blank screen with the cursor at its first cell.
    pub(crate) fn new() -> Self {
        Self {
            cells: vec![vec![' '; COLUMNS]; ROWS],
            row: 0,
            col: 0,
        }
    }

    /// Wraps a cursor past the last column onto the next row.
    fn wrap(&mut self) {
        if self.col >= COLUMNS {
            self.col = 0;
            self.scroll_down(1);
        }
    }

    /// Moves down `n` rows, scrolling the top away past the last row. The
    /// scroll clamps at the grid's height, so a huge count costs one
    /// screen, not one row per count.
    fn scroll_down(&mut self, n: usize) {
        let target = self.row.saturating_add(n);
        if target < ROWS {
            self.row = target;
            return;
        }
        let over = target.saturating_sub(ROWS.saturating_sub(1));
        for _ in 0..over {
            self.cells.remove(0);
            self.cells.push(vec![' '; COLUMNS]);
        }
        self.row = ROWS.saturating_sub(1);
    }

    /// Clears one row's cells from `from` to `to`: callers keep `from`
    /// below `to` and both inside the row, and skipping past the end
    /// clears nothing.
    fn clear_row(&mut self, row: usize, from: usize, to: usize) {
        if let Some(cells) = self.cells.get_mut(row) {
            for cell in cells.iter_mut().skip(from).take(to.saturating_sub(from)) {
                *cell = ' ';
            }
        }
    }
}

impl Grid {
    /// The rows drawn in a `width` by `height` body: the grid's rows cut
    /// to the width and its top rows to the height. Each row reads
    /// `width` cells, padded with spaces.
    pub(crate) fn rows(&self, width: u16, height: u16) -> Vec<String> {
        fit(
            self.cells
                .iter()
                .take(usize::from(height))
                .cloned()
                .collect(),
            width,
        )
    }
}

impl Sink for Grid {
    fn put(&mut self, ch: char) {
        self.wrap();
        if let Some(row) = self.cells.get_mut(self.row)
            && let Some(cell) = row.get_mut(self.col)
        {
            *cell = ch;
            self.col = self.col.saturating_add(1);
        }
    }

    fn carriage_return(&mut self) {
        self.col = 0;
    }

    fn line_feed(&mut self) {
        self.col = 0;
        self.scroll_down(1);
    }

    fn backspace(&mut self) {
        self.col = self.col.saturating_sub(1);
    }

    fn tab(&mut self) {
        let next = self
            .col
            .saturating_add(TAB_STOP)
            .saturating_sub(self.col % TAB_STOP);
        self.col = next.min(COLUMNS.saturating_sub(1));
    }

    fn up(&mut self, n: usize) {
        self.row = self.row.saturating_sub(n);
    }

    fn down(&mut self, n: usize) {
        self.row = self.row.saturating_add(n).min(ROWS.saturating_sub(1));
    }

    fn right(&mut self, n: usize) {
        self.col = self.col.saturating_add(n).min(COLUMNS.saturating_sub(1));
    }

    fn left(&mut self, n: usize) {
        self.col = self.col.saturating_sub(n);
    }

    fn position(&mut self, row: usize, col: usize) {
        self.row = row.saturating_sub(1).min(ROWS.saturating_sub(1));
        self.col = col.saturating_sub(1).min(COLUMNS.saturating_sub(1));
    }

    fn erase_line(&mut self, mode: usize) {
        match mode {
            0 => self.clear_row(self.row, self.col, COLUMNS),
            1 => self.clear_row(self.row, 0, self.col.saturating_add(1)),
            2 => self.clear_row(self.row, 0, COLUMNS),
            _ => {}
        }
    }

    fn erase_display(&mut self, mode: usize) {
        match mode {
            0 => {
                self.erase_line(0);
                for row in self.row.saturating_add(1)..ROWS {
                    self.clear_row(row, 0, COLUMNS);
                }
            }
            1 => {
                for row in 0..self.row {
                    self.clear_row(row, 0, COLUMNS);
                }
                self.erase_line(1);
            }
            2 => {
                for row in 0..ROWS {
                    self.clear_row(row, 0, COLUMNS);
                }
            }
            _ => {}
        }
    }
}

impl Default for Grid {
    fn default() -> Self {
        Self::new()
    }
}

/// Any other job's tail: at most [`LINES_CAP`] lines of at most
/// [`LINE_WIDTH`] characters. Carriage return moves to the start of the
/// row so the next text overwrites it, and every escape sequence is
/// dropped before it arrives.
#[derive(Debug, Clone)]
pub(crate) struct Lines {
    /// The lines, oldest first.
    lines: Vec<Vec<char>>,
    /// The cursor into the last line, where the next char overwrites.
    col: usize,
}

impl Lines {
    /// No lines yet.
    pub(crate) fn new() -> Self {
        Self {
            lines: vec![Vec::new()],
            col: 0,
        }
    }
}

impl Sink for Lines {
    fn put(&mut self, ch: char) {
        if let Some(line) = self.lines.last_mut() {
            if self.col < line.len() {
                if let Some(cell) = line.get_mut(self.col) {
                    *cell = ch;
                }
            } else {
                line.push(ch);
            }
            self.col = self.col.saturating_add(1);
            if line.len() > LINE_WIDTH {
                line.truncate(LINE_WIDTH);
            }
        }
    }

    fn carriage_return(&mut self) {
        self.col = 0;
    }

    fn line_feed(&mut self) {
        self.lines.push(Vec::new());
        if self.lines.len() > LINES_CAP {
            self.lines.remove(0);
        }
        self.col = 0;
    }

    fn backspace(&mut self) {
        self.col = self.col.saturating_sub(1);
    }

    fn tab(&mut self) {
        let next = self
            .col
            .saturating_add(TAB_STOP)
            .saturating_sub(self.col % TAB_STOP);
        while self.col < next {
            self.put(' ');
        }
    }

    fn up(&mut self, _n: usize) {}

    fn down(&mut self, _n: usize) {}

    fn right(&mut self, _n: usize) {}

    fn left(&mut self, _n: usize) {}

    fn position(&mut self, _row: usize, _col: usize) {}

    fn erase_line(&mut self, _mode: usize) {}

    fn erase_display(&mut self, _mode: usize) {}
}

impl Lines {
    /// The rows drawn in a `width` by `height` body: the last lines that
    /// fit, each cut to the width and padded with spaces.
    pub(crate) fn rows(&self, width: u16, height: u16) -> Vec<String> {
        let skip = self.lines.len().saturating_sub(usize::from(height));
        fit(self.lines.iter().skip(skip).cloned().collect(), width)
    }
}

impl Default for Lines {
    fn default() -> Self {
        Self::new()
    }
}

/// A job's bounded output: a grid for a `tty` job, lines for any other.
#[derive(Debug, Clone)]
pub(crate) enum Output {
    /// A `tty` job's screen with its parser.
    Grid(Parser, Grid),
    /// Any other job's tail with its parser.
    Lines(Parser, Lines),
}

impl Output {
    /// A `tty` job's output: a grid.
    pub(crate) fn tty() -> Self {
        Self::Grid(Parser::new(), Grid::new())
    }

    /// Any other job's output: capped lines.
    pub(crate) fn plain() -> Self {
        Self::Lines(Parser::new(), Lines::new())
    }

    /// Feeds one delta's text.
    pub(crate) fn feed(&mut self, text: &str) {
        match self {
            Self::Grid(parser, grid) => parser.feed(text, grid),
            Self::Lines(parser, lines) => parser.feed(text, lines),
        }
    }

    /// The rows drawn in a `width` by `height` body: the grid's rows cut
    /// to the width and its top rows to the height, or the last lines
    /// that fit. Each row reads `width` cells, padded with spaces.
    pub(crate) fn rows(&self, width: u16, height: u16) -> Vec<String> {
        match self {
            Self::Grid(_, grid) => grid.rows(width, height),
            Self::Lines(_, lines) => lines.rows(width, height),
        }
    }
}

/// Cuts each row to `width` cells and pads it out with spaces.
fn fit(cells: Vec<Vec<char>>, width: u16) -> Vec<String> {
    let width = usize::from(width);
    cells
        .into_iter()
        .map(|row| {
            let mut text: String = row.into_iter().take(width).collect();
            let len = text.chars().count();
            for _ in len..width {
                text.push(' ');
            }
            text
        })
        .collect()
}

#[cfg(test)]
#[path = "tty_screen_tests.rs"]
mod tests;
