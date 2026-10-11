//! Binary-level wiring for wheel scrolling (`docs/testing.md`, "Levels"):
//! the built `fiber` at 160x48 scrolls one turn with a 150-item reply
//! and an opened 300-line `read` exactly 90 rows over 30 wheel-up
//! reports, with every byte inside matched mode-2026 pairs. No frame
//! count and no timing: at this level a single pty write may arrive in
//! one read or several. The reply's items keep 90 rows to scroll above
//! the bottom, since an opened call draws only its first 20 lines
//! (`docs/tui.md`, "Tool groups and the ledger").

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

/// The idle title, written only when the named idle row lands before
/// the finished attention latches.
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

/// The reply's item number in `row`: items read `T` plus four digits.
fn output_line(row: &str) -> usize {
    let at = row
        .find('T')
        .unwrap_or_else(|| panic!("no reply item in {row:?}"));
    row.get(at + 1..at + 5)
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| panic!("no reply item in {row:?}"))
}

#[test]
fn thirty_wheel_ups_scroll_the_opened_read_ninety_rows() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("notes.txt"), notes()).unwrap();
    // The reply of 150 list items precedes the read call, so 90 rows
    // stay above the bottom once the opened call draws only its first
    // 20 lines.
    let reply = (1..=150)
        .map(|n| format!("- T{n:04}"))
        .collect::<Vec<_>>()
        .join("\n");
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"text": [reply],
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
        grid.contents.contains("notes.txt")
    });
    let row = ledger
        .rows
        .iter()
        .position(|line| line.contains("notes.txt"))
        .unwrap_or_else(|| panic!("no ledger row in {:?}", ledger.rows));
    let col = ledger.rows[row]
        .find("notes.txt")
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
    // The opened call draws its first 20 lines, then the more row.
    let opened = run.wait_screen("the read's more row", |grid| {
        grid.contents.contains("\u{2026} 280 more lines")
    });
    assert!(
        opened.rows.iter().any(|row| row.contains("L0020")),
        "the first twenty lines draw"
    );
    assert!(
        !opened.rows.iter().any(|row| row.contains("L0021")),
        "only twenty lines draw"
    );
    let more = opened
        .rows
        .iter()
        .position(|row| row.contains("\u{2026} 280 more lines"))
        .expect("the more row");
    // The more row's own cells are dim.
    let row = &opened.rows[more];
    let at = row.find("\u{2026} 280 more lines").expect("the more row");
    let start = u16::try_from(row[..at].chars().count()).unwrap();
    let end = start + u16::try_from("\u{2026} 280 more lines".chars().count()).unwrap();
    let y = u16::try_from(more).unwrap();
    for x in start..end {
        assert!(opened.cell(x, y).dim, "the more row's cell {x} is dim");
    }
    // The top row's reply item, then 30 wheel-ups in one write: 3 rows
    // a step (`docs/tui.md`, "Turns").
    let top = run.screen();
    let first = output_line(&top.rows[0]);
    assert!(first >= 90, "top row holds T{first:04}, too few rows above");
    let target = format!("T{:04}", first - 90);
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

/// Harness-level fixture for one ordering of the turn's titles: the named
/// idle row lands after the finished attention title latched, so
/// `✓ say hi · fiber` is never written (#2032). A wait on the finished
/// title returns in this ordering; a wait on the idle title never would.
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
        "the idle title was written, so this is not the ordering under test"
    );
}
