//! The look-diff comparator (`scripts/look-diff`, `docs/testing/screens.md`,
//! "Screens"): replays a pty dump through the same `vt100` parser the
//! driver rebuilds its grid with, compares the grid to a prototype
//! capture row by row, and writes the one verdict line. It is `#[ignore]`,
//! run only through the script with `--run-ignored only`, and it passes
//! whenever it writes a verdict: `match` and `differs` are both verdicts.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use support::pty::Colour;

/// The look tests' size (`docs/testing/screens.md`): the comparator
/// parses both sides at it, and a capture larger than it is an error,
// so an over-wide line cannot wrap into a false match.
const COLS: u16 = 160;
const ROWS: u16 = 48;

/// The value of test-process env `name`: every name the script sets, so
/// a missing one panics instead of comparing against nothing.
fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is set by scripts/look-diff"))
}

/// The cell's symbol, with an empty cell reading as a blank: how the
/// driver reads unwritten cells (`crates/main/tests/support/pty.rs`).
fn symbol(cell: &vt100::Cell) -> &str {
    let contents = cell.contents();
    if contents.is_empty() { " " } else { contents }
}

/// Row `y` of the screen as text, with trailing spaces trimmed: the form
/// the text comparison reads on both sides.
fn row_text(screen: &vt100::Screen, y: u16) -> String {
    let mut row = String::new();
    for x in 0..COLS {
        match screen.cell(y, x) {
            Some(cell) => row.push_str(symbol(cell)),
            None => row.push(' '),
        }
    }
    row.trim_end_matches(' ').to_owned()
}

/// The cell's colour as the diff prints it.
fn colour(color: vt100::Color) -> String {
    format!("{color:?}", color = Colour::from(color))
}

/// The first property where the capture cell and the dump cell differ:
/// its name with both values, for the diff's cell line.
fn first_difference(
    capture: &vt100::Cell,
    dump: &vt100::Cell,
) -> Option<(&'static str, String, String)> {
    if symbol(capture) != symbol(dump) {
        return Some((
            "symbol",
            symbol(capture).to_owned(),
            symbol(dump).to_owned(),
        ));
    }
    if capture.fgcolor() != dump.fgcolor() {
        return Some(("fg", colour(capture.fgcolor()), colour(dump.fgcolor())));
    }
    if capture.bgcolor() != dump.bgcolor() {
        return Some(("bg", colour(capture.bgcolor()), colour(dump.bgcolor())));
    }
    if capture.bold() != dump.bold() {
        return Some(("bold", capture.bold().to_string(), dump.bold().to_string()));
    }
    if capture.italic() != dump.italic() {
        return Some((
            "italic",
            capture.italic().to_string(),
            dump.italic().to_string(),
        ));
    }
    if capture.underline() != dump.underline() {
        return Some((
            "underline",
            capture.underline().to_string(),
            dump.underline().to_string(),
        ));
    }
    if capture.inverse() != dump.inverse() {
        return Some((
            "inverse",
            capture.inverse().to_string(),
            dump.inverse().to_string(),
        ));
    }
    None
}

/// The `.ansi` capture with `tmux capture-pane -e -p` line endings:
/// a bare LF moves the `vt100` cursor down without returning it, so
/// each row after the first parses shifted. Folding every LF to CRLF
/// replays the capture where the dump's CRLF rows land; the extra CR
/// on an existing CRLF is a no-op column reset, so no guard is needed.
fn crlf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for &b in bytes {
        if b == b'\n' {
            out.push(b'\r');
        }
        out.push(b);
    }
    out
}

/// The verdict for look test `test`, writing the readable diff to `out`
/// when the grids differ: `match` when no row differs, `differs` with
/// the differing row and cell counts, `error` for a missing dump or
/// capture or an over-size capture.
fn compare(test: &str, out: &str) -> String {
    let dump_path = var("LOOK_DIFF_GRID");
    let capture_path = var("LOOK_DIFF_CAPTURE");
    let capture = match std::fs::read_to_string(&capture_path) {
        Ok(text) => text,
        Err(_) => return format!("look: error {test} capture missing: {capture_path}"),
    };
    let lines: Vec<&str> = capture.lines().collect();
    let width = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    if lines.len() > usize::from(ROWS) || width > usize::from(COLS) {
        return format!(
            "look: error {test} size mismatch: capture {}x{width}, look 160x48",
            lines.len()
        );
    }
    let dump = match std::fs::read(&dump_path) {
        Ok(bytes) => bytes,
        Err(_) => return format!("look: error {test} dump missing: {dump_path}"),
    };
    let mut dump_parser = vt100::Parser::new(ROWS, COLS, 0);
    dump_parser.process(&dump);
    let dump_screen = dump_parser.screen().clone();
    let ansi = std::env::var("LOOK_DIFF_ANSI")
        .ok()
        .filter(|s| !s.is_empty());
    let ansi_screen = match ansi {
        None => None,
        Some(path) => match std::fs::read(&path) {
            Ok(bytes) => {
                let mut parser = vt100::Parser::new(ROWS, COLS, 0);
                parser.process(&crlf(&bytes));
                Some(parser.screen().clone())
            }
            Err(_) => return format!("look: error {test} ansi missing: {path}"),
        },
    };
    // A row differs when its trimmed text differs; with the `.ansi`
    // capture, when any of its cells differs too. The counts stay
    // consistent: trimmed text that differs always shows a symbol
    // difference in its row, and padding with blanks makes equal trimmed
    // rows symbol-equal, so an empty row set always pairs with no cells.
    let mut differing: Vec<u16> = Vec::new();
    let mut row_lines: Vec<String> = Vec::new();
    for y in 0..ROWS {
        let expected = lines.get(usize::from(y)).copied().unwrap_or("");
        let actual = row_text(&dump_screen, y);
        if expected.trim_end_matches(' ') != actual {
            differing.push(y);
            row_lines.push(format!("row {y} expected {expected} actual {actual}"));
        }
    }
    let mut cells = 0;
    let mut cell_lines: Vec<String> = Vec::new();
    for y in 0..ROWS {
        for x in 0..COLS {
            let dump_cell = dump_screen.cell(y, x).expect("the dump parses at 160x48");
            let difference = match ansi_screen.as_ref() {
                Some(ansi_screen) => {
                    let capture_cell = ansi_screen
                        .cell(y, x)
                        .expect("the capture parses at 160x48");
                    first_difference(capture_cell, dump_cell)
                }
                None => {
                    let expected = lines
                        .get(usize::from(y))
                        .and_then(|line| line.chars().nth(usize::from(x)))
                        .unwrap_or(' ');
                    if expected.to_string() != symbol(dump_cell) {
                        Some(("symbol", expected.to_string(), symbol(dump_cell).to_owned()))
                    } else {
                        None
                    }
                }
            };
            if let Some((property, expected, actual)) = difference {
                cells += 1;
                if !differing.contains(&y) {
                    differing.push(y);
                }
                cell_lines.push(format!(
                    "cell {x},{y} {property} expected {expected} actual {actual}"
                ));
            }
        }
    }
    if differing.is_empty() {
        return format!("look: match {test}");
    }
    differing.sort_unstable();
    let mut diff = row_lines.join("\n");
    if !cell_lines.is_empty() {
        if !diff.is_empty() {
            diff.push('\n');
        }
        diff.push_str(&cell_lines.join("\n"));
    }
    diff.push('\n');
    std::fs::write(out, diff).expect("the diff path is written");
    format!(
        "look: differs {test} rows={} cells={cells} diff={out}",
        differing.len()
    )
}

/// The one verdict: reads the script's env, compares, and writes the
/// verdict line. Ignored unless the script runs it.
#[ignore]
#[test]
fn look_diff() {
    let test = var("LOOK_DIFF_TEST");
    let out = var("LOOK_DIFF_OUT");
    let verdict = compare(&test, &out);
    let verdict_path = var("LOOK_DIFF_VERDICT");
    std::fs::write(&verdict_path, format!("{verdict}\n")).unwrap();
}
