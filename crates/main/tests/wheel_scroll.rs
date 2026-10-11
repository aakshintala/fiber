//! Binary-level wiring for wheel scrolling (`docs/testing.md`, "Levels"):
//! the built `fiber` at 160x48 scrolls one turn with an opened 300-line
//! `read` exactly 90 rows over 30 wheel-up reports, with every byte inside
//! matched mode-2026 pairs. No frame count and no timing: at this level a
//! single pty write may arrive in one read or several.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::Write;

use serde_json::json;
use support::pty::{FINISHED_TITLE, Run, exact_end};
use support::{Deadline, Setup, write_json};

/// The idle title, as the old wait spelled it.
const IDLE_TITLE: &str = "\u{2713} say hi \u{b7} fiber\x07";

/// Waits for the finished attention title. It is the last title the turn
/// writes before the next key or left press, in every ordering: the idle
/// row naming the session can land before or after attention latches, and
/// after it the idle title `✓ say hi · fiber` is never written
/// (`docs/tui.md`, "Getting the person's attention"). Titles and the bell
/// are direct tty writes, so after this wait none can fall inside the
/// measured window below.
#[track_caller]
fn wait_turn_title(run: &mut Run) {
    run.wait_bytes(0, FINISHED_TITLE, "the turn's finished title");
}

/// The read's output: 300 unique lines.
fn notes() -> String {
    (1..=300).map(|n| format!("L{n:04}\n")).collect()
}

/// Every byte from `from` sits inside matched mode-2026 pairs: blocks
/// never nest and no byte falls outside one.
fn assert_synced(output: &[u8], from: usize) {
    const BEGIN: &[u8] = b"\x1b[?2026h";
    const END: &[u8] = b"\x1b[?2026l";
    let bytes = output.get(from..).unwrap_or_default();
    let mut depth = 0u32;
    let mut at = 0;
    while at < bytes.len() {
        let rest = bytes.get(at..).unwrap_or_default();
        if rest.starts_with(BEGIN) {
            assert!(depth == 0, "a synchronized block nests at byte {at}");
            depth += 1;
            at += BEGIN.len();
        } else if rest.starts_with(END) {
            assert!(
                depth == 1,
                "a synchronized block ends at byte {at} with none open"
            );
            depth -= 1;
            at += END.len();
        } else {
            assert!(
                depth == 1,
                "byte {at} sits outside any synchronized block: {:?}",
                bytes.get(at.saturating_sub(64)..(at + 64).min(bytes.len()))
            );
            at += 1;
        }
    }
    assert!(depth == 0, "a synchronized block never closes");
}

/// The output line number in `row`: lines read `L` plus four digits.
fn output_line(row: &str) -> usize {
    let at = row
        .find('L')
        .unwrap_or_else(|| panic!("no output line in {row:?}"));
    row.get(at + 1..at + 5)
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| panic!("no output line in {row:?}"))
}

#[test]
fn thirty_wheel_ups_scroll_the_opened_read_ninety_rows() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("notes.txt"), notes()).unwrap();
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"text": ["Reading the notes."],
             "tool_calls": [{"name": "read", "arguments": {"path": "notes.txt"}}]},
            {"text": ["Done."]},
        ]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000}}),
    );
    let mut run = Run::spawn(&setup, 160, 48, &[], &[("FIBER_TEST_FAKE_KEY", "sk-test")]);
    // The end of the first frame proves the input reader runs before
    // the prompt goes out.
    run.ready();
    run.write(b"say hi\r");
    run.wait_screen("the turn's reply", |grid| grid.contents.contains("Done."));
    wait_turn_title(&mut run);
    // Open the ledger, then the read's call, then its output.
    run.write(b"\x0f");
    let ledger = run.wait_screen("the read's ledger row", |grid| {
        grid.contents.contains("read notes.txt")
    });
    let row = ledger
        .rows
        .iter()
        .position(|line| line.contains("read notes.txt"))
        .unwrap_or_else(|| panic!("no ledger row in {:?}", ledger.rows));
    let col = ledger.rows[row]
        .find("read notes.txt")
        .unwrap_or_else(|| panic!("no ledger row in {:?}", ledger.rows));
    run.write(
        format!(
            "\x1b[<0;{};{}M\x1b[<0;{};{}m",
            col + 1,
            row + 1,
            col + 1,
            row + 1
        )
        .as_bytes(),
    );
    run.wait_screen("the read's last line", |grid| {
        grid.contents.contains("L0300")
    });
    // The top row's output line, then 30 wheel-ups in one write: 3 rows
    // a step (`docs/tui.md`, "Turns").
    let top = run.screen();
    let first = output_line(&top.rows[0]);
    assert!(first >= 90, "top row holds L{first:04}, too few rows above");
    let target = format!("L{:04}", first - 90);
    let offset = run.output().len();
    run.write("\x1b[<64;21;11M".repeat(30).as_bytes());
    run.wait_screen("the scrolled rows", |grid| {
        grid.rows.first().is_some_and(|row| row.contains(&target))
    });
    assert_synced(&run.output(), offset);
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// The ordering that made the idle-title wait fail under load: the named
/// idle row lands after the finished attention title latched, so
/// `✓ say hi · fiber` is never written (#2032). The titles are fed in the
/// order captured from a failing run.
#[test]
fn the_finished_title_arrives_when_the_idle_title_never_does() {
    let (main, mut terminal) = {
        let pair = support::pty::open(160, 48);
        (fs::File::from(pair.main), pair.terminal)
    };
    let mut run = Run::attach(main, 160, 48, Deadline::start());
    for title in [
        "fiber",
        "\u{2713} \u{b7} fiber",
        "\u{25cf} say hi \u{b7} fiber",
        "\u{2713} fiber \u{b7} finished",
    ] {
        terminal
            .write_all(format!("\x1b]2;{title}\x07").as_bytes())
            .unwrap();
    }
    wait_turn_title(&mut run);
    assert_eq!(
        exact_end(&run.output(), 0, IDLE_TITLE.as_bytes()),
        None,
        "the old wait's title was written, so this is not the failing order"
    );
}
