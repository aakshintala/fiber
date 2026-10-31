//! Line start and end at the raw terminal (see #3751): the bytes Ghostty
//! sends for ⌘← and ⌘→, Ctrl+A (`\x01`) and Ctrl+E (`\x05`), move on the
//! cursor's line in a multi-line draft.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use serde_json::json;
use support::pty::Run;
use support::{Setup, write_json};

/// A bare home and workspace with a text-only scripted turn: the draft
/// the test types never runs, but the terminal still opens on the input
/// box.
fn configure(setup: &Setup) {
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps":[{"text":["Complete."]}]}),
    );
    let config = json!({"model":"scripted/s.json","hub":{"idle_exit_ms":200},"tui":{"rail":{"shown":false},"panel":{"cards":[]}}});
    write_json(&setup.home().join("config.json"), &config);
}

#[test]
fn ctrl_a_and_ctrl_e_move_on_the_cursor_line() {
    let setup = Setup::new();
    configure(&setup);
    let mut run = Run::spawn(&setup, 160, 48, &[], &[]);
    run.ready(); // The input reader runs before any typing goes out.
    // A two-line draft with the cursor at its end.
    run.write(b"qx\x0ajz");
    run.wait_screen("the two-line draft", |grid| {
        grid.rows.iter().any(|row| row.contains("qx"))
            && grid.rows.iter().any(|row| row.contains("jz"))
    });
    // `\x01` moves to the start of the cursor's line: typing lands
    // before it, not at the draft's start. The cursor keeps the covered
    // character, so the row reads "Xjz" with `j` in the slab colour on
    // the text colour.
    run.write(b"\x01X");
    let grid = run.wait_screen("the line-start edit", |grid| {
        grid.rows.iter().any(|row| row.contains("Xjz"))
            && !grid.rows.iter().any(|row| row.contains("Xqx"))
    });
    // The covered `j` sits one cell past the `X`: its colours swap
    // against its neighbour.
    let (x, y) = grid
        .rows
        .iter()
        .enumerate()
        .find_map(|(y, row)| {
            row.find("Xjz").map(|byte| {
                (
                    u16::try_from(row[..byte].chars().count()).unwrap_or(0),
                    u16::try_from(y).unwrap_or(0),
                )
            })
        })
        .expect("the edited line");
    let (before, covered) = (grid.cell(x, y), grid.cell(x + 1, y));
    assert_eq!(covered.symbol, "j");
    assert_eq!(covered.fg, before.bg, "the cursor swaps its cell");
    assert_eq!(covered.bg, before.fg, "the cursor swaps its cell");
    assert!(
        !grid.rows.iter().any(|row| row.contains("Xqx")),
        "the start edit left the cursor's line: {}",
        grid.contents
    );
    // `\x05` moves to the end of the cursor's line.
    run.write(b"\x05Y");
    run.wait_screen("the line-end edit", |grid| {
        grid.rows.iter().any(|row| row.contains("XjzY"))
    });
    // From the first line, `\x05` stays on it instead of jumping to the
    // draft's end.
    run.write(b"\x1b[A\x05Q");
    let grid = run.wait_screen("the first line's end", |grid| {
        grid.rows.iter().any(|row| row.contains("qxQ"))
            && !grid
                .rows
                .iter()
                .map(|row| row.replace('█', ""))
                .any(|row| row.contains("YQ"))
    });
    let bare: Vec<String> = grid.rows.iter().map(|row| row.replace('█', "")).collect();
    assert!(
        !bare.iter().any(|row| row.contains("YQ")),
        "the end edit left the cursor's line: {}",
        grid.contents
    );
    // One Ctrl+C clears the draft, and the usual sequence quits from
    // the empty box.
    run.write(b"\x03");
    run.wait_screen("the cleared draft", |grid| {
        !grid.rows.iter().any(|row| row.contains("XjzY"))
    });
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored terminal", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    assert_eq!(run.wait().status.code(), Some(0));
}
