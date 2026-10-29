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
                {"name": "shell", "arguments": {"command": "awk 'BEGIN { for (i = 1; i < 500; i++) print sprintf(\"%03d %220s\", i, \"x\"); print \"TA\" \"IL\" }'"}},
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
    // Search stays scoped to the view: the tail hit counts once, and
    // the ledger's thought behind the view never does.
    scoped_search(&mut run, "1 of 1");
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
/// string in `extra` (a test server's URL, whose port varies). Normalize
/// the truncated workspace path too: its visible prefix depends on whether
/// the runner's temp directory is `/tmp` or a longer path, so any prefix
/// before `[session]/w` folds to `[workspace]/w`. The title row's pad
/// before the `auto` status and the live speed value also vary by runner.
fn snapshot_text(setup: &Setup, grid: &Grid, extra: &[String]) -> String {
    let mut ids = vec![temp_id(setup)];
    ids.extend(extra.iter().cloned());
    let text = grid_text(&grid.rows, grid.cursor, grid.hide_cursor, &ids);
    text.lines()
        .map(|row| {
            let mut row = if let Some(path) = row.find("[session]/w") {
                let path_end = path + "[session]/w".len();
                let path_start = row[..path].rfind(' ').map_or(0, |space| space + 1);
                format!("{}[workspace]/w{}", &row[..path_start], &row[path_end..])
            } else {
                row.to_owned()
            };
            if let Some(marker) = row.find("[workspace]/w") {
                let after = marker + "[workspace]/w".len();
                if let Some(status) = row[after..].find("auto") {
                    let gap = &row[after..after + status];
                    if !gap.is_empty() && gap.chars().all(|cell| cell == ' ') {
                        row = format!(
                            "{}[workspace]/w auto{}",
                            &row[..marker],
                            &row[after + status + 4..]
                        );
                    }
                }
            }
            if let Some(speed) = row.find("speed, last reply") {
                row = format!("{}speed, last reply            —", &row[..speed]);
            }
            row
        })
        .collect::<Vec<_>>()
        .join("\n")
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
/// the rows' match marks and the find-bar count `hit`: the tail output
/// row marks, while the ledger's thought behind the view marks nothing
/// and its miss counts `no matches`, with its marks clearing once the
/// rescan lands. Both fixtures contain `TAIL` only in the output, so
/// the count stays one even though the shell command is also in the view.
fn scoped_search(run: &mut Run, hit: &str) {
    run.write(b"\x06TAIL");
    let grid = run.wait_screen("the scoped tail hit", |grid| {
        tail_row_marked(grid) && grid.rows.iter().any(|row| row.contains(hit))
    });
    assert!(
        grid.rows.iter().any(|row| row.contains(hit)),
        "the tail hit counts {hit}"
    );
    run.write(b"\x7f\x7f\x7f\x7fDoing");
    let grid = run.wait_screen("the scoped miss", |grid| {
        !any_marked(grid) && grid.rows.iter().any(|row| row.contains("no matches"))
    });
    assert!(
        grid.rows.iter().any(|row| row.contains("no matches")),
        "the ledger thought counts never"
    );
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

/// One wheel step up over the conversation column at 160x48.
const WHEEL_UP: &[u8] = b"\x1b[<64;21;41M";
/// One wheel step down over the conversation column at 160x48.
const WHEEL_DOWN: &[u8] = b"\x1b[<65;21;41M";

/// The thought row's marker: the turn ran one command, so the finished
/// turn names it once.
const THOUGHT: &str = "Ran 1 command";

/// The wheel over a call's view scrolls the view, never the hidden
/// conversation (`docs/tui.md:576`, "Swapped views"; see #3578): with the
/// conversation scrolled up to its thought row, three wheel-downs over
/// the open view move its first visible row nine rows while the
/// conversation's top marker stays exactly where the wheel found it.
#[test]
fn wheel_over_a_call_view_scrolls_the_view_not_the_conversation() {
    let setup = Setup::new();
    // Enough reply filler that the finished turn is taller than the
    // screen: the conversation can scroll, and the thought row can leave
    // the followed bottom. Reply lines wrap, so the filler stays one
    // space-joined paragraph.
    let mut filler: Vec<String> = (1..=800).map(|n| format!("F{n:04}")).collect();
    filler.push("Done.".to_owned());
    script(
        &setup,
        json!([
            {"reasoning": REASONING, "tool_calls": [
                {"name": "shell", "arguments": {"command": "awk 'BEGIN { for (i = 1; i < 500; i++) print sprintf(\"%03d %220s\", i, \"x\"); print \"TA\" \"IL\" }'"}},
            ]},
            {"text": [filler.join(" ")]},
        ]),
    );
    config(&setup);
    allow(&setup);
    let (mut run, from) = started(&setup);
    finish(&mut run, "F0800");
    run.turn_finished(from);
    // Scroll the conversation up until its thought row shows: each step
    // must move the rows, so a step that would sit on the top clamp fails
    // the wait instead of settling short. The marker itself is recorded
    // after the call opens below.
    let mut scrolled = false;
    for _ in 0..6 {
        let prev = run.screen();
        run.write(WHEEL_UP);
        let grid = run.wait_screen("the thought row scrolling into view", |next| {
            match (find_row(&prev.rows, THOUGHT), find_row(&next.rows, THOUGHT)) {
                (None, Some(_)) => true,
                (Some(before), Some(after)) => after > before,
                (Some(_), None) | (None, None) => false,
            }
        });
        if find_row(&grid.rows, THOUGHT).is_some() {
            scrolled = true;
            break;
        }
    }
    assert!(
        scrolled,
        "the conversation never scrolled its thought row into view"
    );
    // Open the ledger and the call. The marker is recorded after the call
    // opens: opening flips the thought row from its collapsed to its
    // expanded mark, and the expansion lands below it, so the row keeps
    // its screen row.
    let (_grid, gutter) = open_ledger(&mut run);
    let grid = open_call(&mut run, gutter, "shell", "awk", "exit 0");
    let top_y = find_row(&grid.rows, THOUGHT).expect("the thought row above the opened call");
    let top_marker = grid.rows[usize::from(top_y)].clone();
    // Click the more row of the opened call. The turn is taller than the
    // screen, so the shared helper's settled wait (which also wants the
    // completed paint on screen) never lands: the turn already finished
    // before the ledger opened, so the first grid showing the more row
    // has its final position.
    let grid = run.wait_screen("the more row", |grid| {
        grid.rows.iter().any(|row| row.contains("more lines"))
    });
    // Dismiss the reviewer notice first: while it is up, the click on the
    // more row below misses, so the view never opens.
    let notice_row = find_row(&grid.rows, "no_model").expect("the reviewer notice");
    let notice_text = row_text(&grid, notice_row);
    let dismiss_x = notice_text.find('✕').unwrap_or(159) as u16 + 1;
    click(&mut run, dismiss_x, notice_row);
    let grid = run.wait_screen("the dismissed notice", |grid| {
        grid.rows.iter().all(|row| !row.contains("no_model"))
    });
    let row = find_row(&grid.rows, "more lines").expect("the more row");
    let text = row_text(&grid, row);
    let x = text.find("more lines").unwrap_or(20) as u16 + 1;
    click(&mut run, x, row);
    // The view follows to the resume marker at the tail.
    run.wait_screen("the call's view", |grid| {
        grid.rows
            .iter()
            .any(|row| row.contains("── the model's view resumes ──"))
    });
    // PageUp twice: the ups page past the held tail window, so the tail
    // pages drop and the window re-anchors to the held head segment,
    // whose last output rows are 036 and 037 above the end marker. The
    // refetches behind that window fill the rows below without moving its
    // top again, so waiting for 36 proves both ups and the refetch settle
    // landed, in order.
    run.write(b"\x1b[5~");
    run.write(b"\x1b[5~");
    let grid = run.wait_screen("the settled paged-up view", |grid| {
        first_output_number(grid) == Some(36)
    });
    let settled = first_output_number(&grid).expect("the settled first output number");
    // The marker row breaks the output-number/top-row one-to-one. Aim
    // the wheel at the view's fixed interior instead of clicking a
    // grid-derived output row: page replies may still fill the view, but
    // cannot change which surface owns this wheel coordinate.
    run.write(WHEEL_DOWN);
    let grid = run.wait_screen("the view past the end marker", |grid| {
        first_output_number(grid).is_some_and(|first| first > settled)
    });
    let start = first_output_number(&grid).expect("the view moved past the end marker");
    // Three wheel-downs over the view move its first visible row nine
    // rows: each step shifts the viewport three displayed rows, as
    // promised in `docs/tui.md:576`. The
    // conversation stays hidden, so the Esc check below is the one
    // that proves its scroll is unchanged.
    run.write(WHEEL_DOWN);
    run.write(WHEEL_DOWN);
    run.write(WHEEL_DOWN);
    let grid = run.wait_screen("the wheeled view", |grid| {
        first_output_number(grid) == Some(start + 9)
    });
    assert_eq!(
        first_output_number(&grid),
        Some(start + 9),
        "three wheel steps moved the view's first visible row nine rows"
    );
    // Esc returns to the opened call with its row still focused: the
    // conversation's scroll is exactly where the wheel found it.
    run.write(b"\x1b[27u");
    let grid = run.wait_screen("the opened call after Esc", |grid| {
        grid.rows.iter().any(|row| row.contains("more lines"))
    });
    assert_eq!(
        grid.rows[usize::from(top_y)],
        top_marker,
        "the wheel over the view left the conversation's scroll"
    );
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
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
    scoped_search(&mut run, "1 of 1");
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
