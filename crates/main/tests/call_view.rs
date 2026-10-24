//! A call's swapped view end to end (`docs/tui.md`, "Swapped views"):
//! the real binary on a 160x48 pty in truecolour, with a scripted
//! provider. A shell call cut at both ends opens its view, which pages
//! to the end through `artifact_page` with both markers; a write call
//! cut at its tail only opens its view with just the end marker. Each
//! case asserts opening, paging to the end, the top row, its markers,
//! the footer, a scoped hit, the copied text, Esc focus, and the grid
//! text and SGR. The SGR pins assert on the grid itself.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use fakes::{ProviderServer, Response};
use insta::assert_snapshot;
use serde_json::json;
use support::journey::grid_text;
use support::pty::{Cell, Colour, Grid, Run};
use support::{Setup, TRUECOLOUR, write_json};

/// A turn card's background in truecolour.
const TURN: Colour = Colour::Rgb(16, 16, 23);

/// The scripted turn: `steps`, then done.
fn script(setup: &Setup, steps: serde_json::Value) {
    write_json(&setup.workspace().join("s.json"), &json!({"steps": steps}));
}

/// The scripted provider as the model, the rail hidden. The hub stays
/// past the ledger browsing: the view pages its artifact through it.
fn config(setup: &Setup) {
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000},
            "tui": {"rail": {"shown": false}}}),
    );
}

/// Standing rules allowing every shell command, edit and write, so no
/// approval panel stops the turns.
fn allow(setup: &Setup) {
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n{}\n{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": ""}),
            json!({"decision": "allow", "tool": "edit", "prefix": ""}),
            json!({"decision": "allow", "tool": "write", "prefix": ""}),
        ),
    )
    .unwrap();
}

/// The binary started and prompted, returning the run and the output
/// length the turn's end is waited on.
fn started(setup: &Setup) -> (Run, usize) {
    let mut run = Run::spawn(setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"go\r");
    let from = run.output().len();
    (run, from)
}

/// Answers any approval, then waits for the reply `done`.
fn finish(run: &mut Run, done: &str) {
    loop {
        let grid = run.wait_screen("approval or done", |grid| {
            grid.contents.contains("Allow once") || grid.contents.contains(done)
        });
        if grid.contents.contains(done) {
            return;
        }
        run.write(b"\r");
    }
}

/// The first row containing `needle`.
fn find_row(rows: &[String], needle: &str) -> Option<u16> {
    rows.iter()
        .position(|row| row.contains(needle))
        .map(|y| y as u16)
}

/// The smallest numbered output row visible in `grid`.
fn first_output_number(grid: &Grid) -> Option<u16> {
    grid.rows
        .iter()
        .filter_map(|row| row.trim_start().get(..3)?.parse().ok())
        .min()
}

/// The row's cells as text, indexed by cell column.
fn row_text(grid: &Grid, y: u16) -> String {
    (0..160).map(|x| grid.cell(x, y).symbol.clone()).collect()
}

/// A click at cell (`x`, `y`).
fn click(run: &mut Run, x: u16, y: u16) {
    run.write(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1).as_bytes());
}

/// The ledger row whose kind cell reads `kind` followed by `target`.
fn ledger_row(grid: &Grid, gutter: u16, kind: &str, target: &str) -> u16 {
    let padded = format!("{kind:<9}");
    (0..48)
        .find(|y| {
            let row = row_text(grid, *y);
            let chars: Vec<char> = row.chars().collect();
            chars.get(gutter as usize + 8..gutter as usize + 17)
                == Some(&padded.chars().collect::<Vec<char>>()[..])
                && chars
                    .get(gutter as usize + 17..)
                    .map(|tail| tail.iter().collect::<String>().starts_with(target))
                    .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("the {kind} {target} row"))
}

/// Every scripted step thinks "**Doing**", so its thought row marks
/// the open ledger.
const REASONING: &str = "**Doing**";

/// The turn card's edges on row `y`: the maximal run of card background
/// around the inner column, with blank padding cells at both ends. The
/// edges are derived from the grid, never hard-coded.
fn card_edges(grid: &Grid, y: u16, inner: u16) -> (u16, u16) {
    let mut left = inner;
    while left > 0 && grid.cell(left - 1, y).bg == TURN {
        left -= 1;
    }
    let mut right = inner;
    while grid.cell(right + 1, y).bg == TURN {
        right += 1;
    }
    assert_eq!(grid.cell(left, y).symbol.as_str(), " ");
    assert_eq!(grid.cell(right, y).symbol.as_str(), " ");
    (left, right)
}

/// Opens the ledger and returns the grid with its gutter, derived from
/// the group row.
fn open_ledger(run: &mut Run) -> (Grid, u16) {
    run.write(b"\x0f");
    let grid = run.wait_screen("the open ledger", |grid| {
        grid.rows.iter().any(|row| row.contains("+ Thought: Doing"))
    });
    let y = find_row(&grid.rows, "thought ").expect("the group row");
    let (left, _right) = card_edges(&grid, y, 2);
    (grid, left + 1)
}

/// Clicks the ledger row (`kind`, `target`) and waits for `marker`.
fn open_call(run: &mut Run, gutter: u16, kind: &str, target: &str, marker: &str) -> Grid {
    let grid = run.wait_screen("the open ledger", |grid| {
        grid.rows.iter().any(|row| row.contains("+ Thought: Doing"))
    });
    let row = ledger_row(&grid, gutter, kind, target);
    click(run, gutter + 10, row);
    run.wait_screen("the opened call", |grid| {
        grid.rows.iter().any(|row| row.contains(marker))
    })
}

/// Clicks the more row of the opened call. The click waits on the
/// settled opened call: the turn finished before the ledger opened, so
/// a grid showing the whole opened call (the more row) with the turn's
/// completed paint has nothing left to render, and the more row's
/// position in that grid is final. The row and click coordinates derive
/// from that same grid.
fn open_view_by_click(run: &mut Run) -> Grid {
    run.wait_screen("the more row", |grid| {
        grid.rows.iter().any(|row| row.contains("more lines"))
    });
    let grid = run.wait_screen("the settled opened call", |grid| {
        grid.rows.iter().any(|row| row.contains("more lines"))
            && grid.contents.contains("completed")
    });
    let row = find_row(&grid.rows, "more lines").expect("the more row");
    let text = row_text(&grid, row);
    let x = text.find(|c: char| !c.is_whitespace()).unwrap_or(20) as u16 + 1;
    click(run, x, row);
    grid
}

/// The top row names the capitalised kind bold and its target dim.
fn assert_top_row(grid: &Grid, kind: &str, target: &str) {
    let row = row_text(grid, 0);
    let kind_at = row
        .find(kind)
        .unwrap_or_else(|| panic!("the {kind} title in {row:?}"));
    for (i, _) in kind.char_indices() {
        assert!(
            grid.cell(kind_at as u16 + i as u16, 0).bold,
            "the kind draws bold"
        );
    }
    let target_at = row
        .find(target)
        .unwrap_or_else(|| panic!("the {target} target in {row:?}"));
    for (i, _) in target.char_indices() {
        assert!(
            grid.cell(target_at as u16 + i as u16, 0).dim,
            "the target draws dim"
        );
    }
}

#[test]
fn a_cut_shell_call_pages_to_the_end_with_both_markers() {
    let setup = Setup::new();
    script(
        &setup,
        json!([
            {"reasoning": REASONING, "tool_calls": [
                {"name": "shell", "arguments": {"command": "awk 'BEGIN { for (i = 1; i < 500; i++) print sprintf(\"%03d %220s\", i, \"x\"); print \"TAIL\" }'"}},
            ]},
            {"text": ["Done."]},
        ]),
    );
    config(&setup);
    allow(&setup);
    let (mut run, from) = started(&setup);
    finish(&mut run, "Done.");
    run.turn_finished(from);
    let (_grid, gutter) = open_ledger(&mut run);
    open_call(&mut run, gutter, "shell", "awk", "exit 0");
    // The more row opens the view with one click; following pages to the
    // end through `artifact_page`, where the resume marker draws.
    open_view_by_click(&mut run);
    // Following pages to the end: the resume marker draws at the tail.
    // (The head page's end marker pages out of the three-page window, as
    // scrolling back up shows.)
    let grid = run.wait_screen("the call's view", |grid| {
        grid.rows
            .iter()
            .any(|row| row.contains("── the model's view resumes ──"))
    });
    assert_view_chrome(&grid, "Shell", "awk", true);
    assert_snapshot!("call_view_open", snapshot_text(&setup, &grid, &[]));
    // Search stays scoped to the view: the tail hit counts once, and the
    // ledger's thought behind the view counts never.
    scoped_search(&mut run);
    // `y` copies the call's whole output text as the tool returned it.
    let from = run.output().len();
    run.write(b"y");
    assert_eq!(copied_text(&mut run, from), shell_output());
    // Scrolling back up shows the head page's end marker again.
    let grid = scrolled_to_end_marker(&mut run, grid);
    assert!(
        grid.rows
            .iter()
            .any(|row| row.contains("── end of the model's view ──")),
        "the end marker draws on the head page"
    );
    // Esc returns with the call's row still focused: Enter opens the
    // view again.
    esc_keeps_call_focus(&mut run, "── the model's view resumes ──");
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// The temporary directory's name, redacted from snapshots.
fn temp_id(setup: &Setup) -> String {
    setup
        .workspace()
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// The grid's text with the temporary directory redacted, plus every
/// string in `extra` (a test server's URL, whose port varies).
fn snapshot_text(setup: &Setup, grid: &Grid, extra: &[String]) -> String {
    let mut ids = vec![temp_id(setup)];
    ids.extend(extra.iter().cloned());
    grid_text(&grid.rows, grid.cursor, grid.hide_cursor, &ids)
}

/// The tail the view paged to, the footer and the top row, with the
/// resume marker when the cut kept both ends.
fn assert_view_chrome(grid: &Grid, kind: &str, target: &str, resume: bool) {
    assert!(
        grid.rows.iter().any(|row| row.contains("TAIL")),
        "the tail paged to the end"
    );
    assert_eq!(
        grid.rows
            .iter()
            .any(|row| row.contains("── the model's view resumes ──")),
        resume,
        "the resume marker draws exactly on a both-ends cut"
    );
    assert!(
        grid.rows
            .iter()
            .any(|row| row.contains("y copies the whole call")),
        "the footer names the copy key"
    );
    assert_top_row(grid, kind, target);
}

/// Types into the view's find bar and asserts the scoped count: the
/// tail hit counts once, while the ledger's thought behind the view
/// counts never.
/// Whether the cell draws a scoped match mark: the match roles draw
/// reverse video in the dark theme's match colours, while the selected
/// row draws reverse video in the default colour (`Theme::DARK`).
fn marked(cell: &Cell) -> bool {
    cell.inverse
        && matches!(
            cell.fg,
            Colour::Rgb(0x5a, 0x4a, 0x1a) | Colour::Rgb(0xff, 0x9f, 0x43)
        )
}

/// Whether the tail output row draws marked: a match mark on one of
/// its cells, from the view's scoped match highlight.
fn tail_row_marked(grid: &Grid) -> bool {
    (0..48).any(|y| {
        let row: String = (0..160).map(|x| grid.cell(x, y).symbol.clone()).collect();
        row.trim() == "TAIL" && (0..160).any(|x| marked(&grid.cell(x, y)))
    })
}

/// Whether any cell on screen draws a scoped match mark.
fn any_marked(grid: &Grid) -> bool {
    (0..48).any(|y| (0..160).any(|x| marked(&grid.cell(x, y))))
}

/// Types into the view's find bar and asserts the scoped hit through
/// the rows' match marks: the tail output row marks, while the
/// ledger's thought behind the view marks nothing and its marks clear
/// once the rescan lands.
fn scoped_search(run: &mut Run) {
    run.write(b"\x06TAIL");
    run.wait_screen("the scoped tail hit", tail_row_marked);
    run.write(b"\x7f\x7f\x7f\x7fDoing");
    run.wait_screen("the scoped miss", |grid| !any_marked(grid));
    // Esc closes the find bar; the view stays open on its footer, and
    // the next `y` copies instead of typing into the bar.
    run.write(b"\x1b[27u");
    run.wait_screen("the closed find bar", |grid| {
        grid.rows
            .iter()
            .any(|row| row.contains("y copies the whole call"))
    });
}

/// Decodes standard base64: the OSC 52 payload `y` writes.
fn base64_decode(text: &str) -> Vec<u8> {
    fn six(byte: u8) -> u32 {
        match byte {
            b'A'..=b'Z' => u32::from(byte - b'A'),
            b'a'..=b'z' => u32::from(byte - b'a') + 26,
            b'0'..=b'9' => u32::from(byte - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => 0,
            _ => panic!("bad base64 byte: {byte}"),
        }
    }
    assert!(text.len().is_multiple_of(4), "padded base64 comes in quads");
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for quad in bytes.chunks(4) {
        let pad = quad.iter().rev().take_while(|byte| **byte == b'=').count();
        assert!(pad <= 2, "at most two pad bytes: {text:?}");
        let word = (six(quad[0]) << 18) | (six(quad[1]) << 12) | (six(quad[2]) << 6) | six(quad[3]);
        out.push((word >> 16) as u8);
        if pad < 2 {
            out.push((word >> 8) as u8);
        }
        if pad < 1 {
            out.push(word as u8);
        }
    }
    out
}

/// The clipboard text the child wrote through OSC 52 after `from`.
fn copied_text(run: &mut Run, from: usize) -> String {
    let start = run.wait_bytes(from, b"\x1b]52;c;", "the OSC 52 copy");
    // The wait ends at the needle's end, so the payload stops one byte
    // before the closing bell.
    let end = run.wait_bytes(start, b"\x07", "the end of the OSC 52 copy") - 1;
    let output = run.output();
    let payload = std::str::from_utf8(&output[start..end]).unwrap();
    String::from_utf8(base64_decode(payload)).unwrap()
}

/// Scrolls back up until the head page's end marker draws again.
fn scrolled_to_end_marker(run: &mut Run, grid: Grid) -> Grid {
    let mut grid = grid;
    let mut first = first_output_number(&grid).unwrap_or(u16::MAX);
    for _ in 0..40 {
        if grid
            .rows
            .iter()
            .any(|row| row.contains("── end of the model's view ──"))
        {
            break;
        }
        // The first PageUp moves selection to the top of the visible
        // window; the second scrolls that window upward.
        run.write(b"\x1b[5~");
        run.write(b"\x1b[5~");
        grid = run.wait_screen("the scrolled view", |grid| {
            grid.rows
                .iter()
                .any(|row| row.contains("── end of the model's view ──"))
                || first_output_number(grid).is_some_and(|at| at < first)
        });
        first = first_output_number(&grid).unwrap_or(first);
    }
    grid
}

/// Closes the view with Esc and proves the call's row kept focus:
/// Enter opens the view again on its `marker`.
fn esc_keeps_call_focus(run: &mut Run, marker: &str) {
    run.write(b"\x1b[27u");
    let grid = run.wait_screen("the opened call after Esc", |grid| {
        grid.rows.iter().any(|row| row.contains("more lines"))
    });
    let row = find_row(&grid.rows, "more lines").expect("the more row after Esc");
    assert!(
        (0..grid.size().0).any(|x| grid.cell(x, row).inverse),
        "the call row stays focused after Esc: {grid:?}"
    );
    run.write(b"\r");
    // Enter reopens the view on the focused call row: its top row
    // draws first, then following pages to the tail marker.
    run.wait_screen("the reopened view", |grid| {
        grid.rows
            .iter()
            .any(|row| row.contains("y copies the whole call"))
    });
    run.wait_screen("the reopened tail", |grid| {
        grid.rows.iter().any(|row| row.contains(marker))
    });
}

/// The shell case's whole output text, as the tool returns it: the
/// command's output with the tool's own exit line after it.
fn shell_output() -> String {
    let mut out = String::new();
    for i in 1..500 {
        out.push_str(&format!("{:03} {:>220}\n", i, "x"));
    }
    out.push_str("TAIL\nExit code 0.\n");
    out
}

/// A page server with one 200 answer of `content_type` and `body`.
fn page(content_type: &str, body: &str) -> ProviderServer {
    ProviderServer::start([Response {
        status: 200,
        headers: vec![("content-type".to_owned(), content_type.to_owned())],
        body: body.as_bytes().to_vec(),
        drop_connection: false,
        stall: false,
    }])
    .unwrap()
}

/// Standing rules allowing the shell, edit and write tools and fetching
/// from the test page server, so no approval panel stops the turn.
fn allow_fetch(setup: &Setup, server: &ProviderServer) {
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n{}\n{}\n{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": ""}),
            json!({"decision": "allow", "tool": "edit", "prefix": ""}),
            json!({"decision": "allow", "tool": "write", "prefix": ""}),
            json!({"decision": "allow", "tool": "web_fetch", "prefix": format!("{}/", server.url())}),
        ),
    )
    .unwrap();
}

/// The fetch case's page body, past the loop's 16 KiB cut.
fn fetch_body() -> String {
    let mut out = String::new();
    for i in 0..90 {
        out.push_str(&format!("F{i:04} {}\n", "x".repeat(216)));
    }
    out.push_str("TAIL\n");
    out
}

/// The fetch case's whole output text, as the tool returns it.
fn fetch_output(url: &str, body: &str) -> String {
    format!("{url} 200 text/plain\n\n{body}")
}

#[test]
fn a_head_only_fetch_cut_pages_with_one_marker() {
    let setup = Setup::new();
    let body = fetch_body();
    let site = page("text/plain", &body);
    let url = format!("{}/long", site.url());
    script(
        &setup,
        json!([
            {"reasoning": REASONING, "tool_calls": [
                {"name": "web_fetch", "arguments": {"url": url}},
            ]},
            {"text": ["Done."]},
        ]),
    );
    config(&setup);
    allow_fetch(&setup, &site);
    let (mut run, from) = started(&setup);
    finish(&mut run, "Done.");
    run.turn_finished(from);
    let (_grid, gutter) = open_ledger(&mut run);
    // The inline call opens on its more row: the 90-line body exceeds the
    // 20-line cap, so the head lines may scroll off while the more row
    // at the bottom stays visible.
    open_call(&mut run, gutter, "other", &url, "more lines");
    // The more row opens the view with one click; following pages to the
    // end through `artifact_page`, where the tail lands with no resume
    // marker: only the head was kept.
    open_view_by_click(&mut run);
    let grid = run.wait_screen("the call's view", |grid| {
        grid.rows.iter().any(|row| row.contains("TAIL"))
    });
    assert_view_chrome(&grid, "Other", &url, false);
    assert_snapshot!(
        "call_view_one_end_open",
        snapshot_text(&setup, &grid, std::slice::from_ref(&url))
    );
    // Search stays scoped to the view: the tail hit counts once, and the
    // ledger's thought behind the view counts never.
    scoped_search(&mut run);
    // `y` copies the call's whole output text as the tool returned it.
    let from = run.output().len();
    run.write(b"y");
    assert_eq!(copied_text(&mut run, from), fetch_output(&url, &body));
    // Scrolling back up shows the head page's end marker, and no resume
    // marker: the cut kept the head only.
    let grid = scrolled_to_end_marker(&mut run, grid);
    assert!(
        grid.rows
            .iter()
            .any(|row| row.contains("── end of the model's view ──")),
        "the end marker draws on the head page"
    );
    assert!(
        !grid
            .rows
            .iter()
            .any(|row| row.contains("── the model's view resumes ──")),
        "no resume marker on a head-only cut"
    );
    // Esc returns with the call's row still focused: Enter opens the
    // view again.
    esc_keeps_call_focus(&mut run, "TAIL");
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}
