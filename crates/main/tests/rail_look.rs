//! Binary-level tests of the rail's look (`docs/tui.md`, "The rail",
//! "The panel"): the real binary on a 160x48 pty in truecolour, with
//! three live sessions in two projects. The project header draws the
//! name bold in accent with the spend right-aligned dim, a card's branch
//! row carries a dim ⎇, and the context bar's track is rule. With the
//! rail hidden and sessions waiting, the Session card ends with the
//! waiting line, and a click on it shows the rail again.

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
use std::sync::{Arc, Mutex};

use fakes::ProviderServer;
use serde_json::json;
use support::pty::{Colour, Run};
use support::{
    SessionGuard, Setup, TRUECOLOUR, connect_hub, git, hello, recv, start_session, write_json,
};

/// The dark theme's accent, rule and attention in truecolour
/// (`crates/tui/src/theme.rs`).
const ACCENT: Colour = Colour::Rgb(110, 170, 254);
const RULE: Colour = Colour::Rgb(58, 58, 74);
const ATTENTION: Colour = Colour::Rgb(255, 159, 67);

/// Alt+R as the kitty keyboard protocol encodes it
/// (`crates/tui/src/keys.rs`).
const ALT_R: &[u8] = b"\x1b[114;3u";

/// The first row containing `needle`.
fn find_row(rows: &[String], needle: &str) -> u16 {
    rows.iter()
        .position(|row| row.contains(needle))
        .map(|y| y as u16)
        .unwrap_or_else(|| panic!("a row containing {needle:?}"))
}

/// The cell index of `needle` in `row`, counting cells.
fn locate(row: &str, needle: &str) -> u16 {
    let chars: Vec<char> = row.chars().collect();
    let need: Vec<char> = needle.chars().collect();
    chars
        .windows(need.len().max(1))
        .position(|window| window == need.as_slice())
        .map(|at| at as u16)
        .unwrap_or_else(|| panic!("{needle:?} in {row:?}"))
}

/// A git repository with one commit on `main`, so a session in `dir`
/// reports its branch.
fn commit_on_main(setup: &Setup, dir: &std::path::Path) {
    git(setup.deadline, dir, &["init", "--quiet"]);
    fs::write(dir.join("file.txt"), "hi\n").unwrap();
    git(setup.deadline, dir, &["add", "."]);
    git(setup.deadline, dir, &["commit", "--quiet", "-m", "first"]);
}

/// Quits the run's terminal and waits for it.
fn quit(mut run: Run) {
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn rail_cards_match_the_doc() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "hub": {"idle_exit_ms": 200}}),
    );
    // One reply each: all three turns complete, leaving their sessions
    // live and idle with their usage recorded, so their cards draw
    // their context bars.
    let hub = Arc::new(Mutex::new(None));
    let (watch, _) = connect_hub(&setup, &hub);
    let other = setup.root.path().join("v");
    fs::create_dir_all(&other).unwrap();
    commit_on_main(&setup, &setup.workspace());
    commit_on_main(&setup, &other);
    let here = setup.workspace().to_string_lossy().into_owned();
    let away = other.to_string_lossy().into_owned();
    let first_guard = SessionGuard::arm(setup.deadline, &here);
    let second_guard = SessionGuard::arm(setup.deadline, &away);
    let control = support::Socket::connect(setup.deadline, &setup.hub_socket());
    assert_eq!(recv(&control, "hub_hello")["kind"], "hub_hello");
    start_session(&control, &here, "first");
    start_session(&control, &away, "second");
    assert!(server.await_requests(2, setup.deadline), "both turns run");
    // The terminal's own session is the third live session, in the
    // launch project's workspace: two projects on the rail.
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"go\r");
    assert!(
        server.await_requests(3, setup.deadline),
        "all three turns run"
    );
    let grid = run.wait_screen("the rail with three sessions", |grid| {
        // Both project headers with the styled cells the assertions
        // read: the name bold in accent, the "+" chip, and the dim
        // spend. Provider requests do not prove the terminal received
        // both projects' status updates, so a frame with one header
        // must not satisfy the wait.
        let header = |name: &str| {
            grid.rows.iter().enumerate().any(|(y, row)| {
                let y = y as u16;
                row.starts_with(name)
                    && grid.cell(2, y).symbol.as_str() == &name[2..3]
                    && grid.cell(2, y).fg == ACCENT
                    && grid.cell(2, y).bold
                    && grid.cell(4, y).symbol.as_str() == "+"
                    && row.find("$0.00").is_some_and(|byte| {
                        let at = row[..byte].chars().count() as u16;
                        (at..at + 5).all(|x| grid.cell(x, y).dim)
                    })
            })
        };
        // The branch row with the dim prefix and plain branch the
        // assertions read.
        let branch = grid.rows.iter().enumerate().any(|(y, row)| {
            let y = y as u16;
            row.find('⎇').is_some_and(|byte| {
                let at = row[..byte].chars().count() as u16;
                grid.cell(at, y).dim
                    && grid.cell(at + 1, y).dim
                    && ["m", "a", "i", "n"]
                        .iter()
                        .enumerate()
                        .all(|(n, expected)| {
                            let cell = grid.cell(at + 2 + n as u16, y);
                            cell.symbol.as_str() == *expected && !cell.dim
                        })
            })
        });
        // The panel's context bar beside its percentage: `▆` cells
        // with the track in rule. The rail's cards are below 30 columns
        // here, so their bars are gone and the panel holds the track.
        let bar = grid.rows.iter().any(|row| row.contains('%'))
            && (0..48).any(|y| {
                (0..160).any(|x| {
                    grid.cell(x, y).symbol.as_str() == "\u{2586}" && grid.cell(x, y).fg == RULE
                })
            });
        header("  w +") && header("  v +") && branch && bar
    });

    // Both project headers draw: the name, then " +", with the spend
    // right-aligned.
    for name in ["  w +", "  v +"] {
        let y = grid
            .rows
            .iter()
            .position(|row| row.starts_with(name))
            .map(|y| y as u16)
            .unwrap_or_else(|| panic!("the {name:?} header"));
        let cell = grid.cell(2, y);
        assert_eq!(cell.symbol.as_str(), &name[2..3]);
        assert_eq!(cell.fg, ACCENT, "the {name:?} name's colour");
        assert!(cell.bold, "the {name:?} name is bold");
        assert_eq!(grid.cell(4, y).symbol.as_str(), "+");
        let row = &grid.rows[y as usize];
        let at = locate(row, "$0.00");
        for x in at..at + 5 {
            assert!(grid.cell(x, y).dim, "spend cell ({x}, {y}) is dim");
        }
    }

    // A card's branch row is a dim "⎇ " followed by the branch.
    let y = find_row(&grid.rows, "⎇ main");
    let row = grid.rows[y as usize].clone();
    let at = locate(&row, "⎇");
    for x in at..at + 2 {
        assert!(grid.cell(x, y).dim, "prefix cell ({x}, {y}) is dim");
    }
    for (n, expected) in ["m", "a", "i", "n"].iter().enumerate() {
        let cell = grid.cell(at + 2 + n as u16, y);
        assert_eq!(cell.symbol.as_str(), *expected);
        assert!(
            !cell.dim,
            "branch cell ({}, {y}) is plain",
            at + 2 + n as u16
        );
    }

    // Empty bar cells: the rail's `░` track where its bars draw, and
    // the panel's `▆` track in rule.
    let mut track = 0;
    for y in 0..48 {
        for x in 0..160 {
            let cell = grid.cell(x, y);
            if cell.symbol.as_str() == "\u{2591}" {
                assert_eq!(cell.fg, RULE, "track cell ({x}, {y})");
            }
            if cell.symbol.as_str() == "\u{2586}" && cell.fg == RULE {
                track += 1;
            }
        }
    }
    assert!(track > 0, "the panel bar's track draws");

    // Dropping the hub's clients leaves it idle once the terminal
    // quits, so it exits on its own.
    drop((watch, control));
    quit(run);
    drop((first_guard, second_guard));
    // The hub exits once nothing runs; reap it after its sessions.
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.wait();
}

/// A scripted turn that ends at once with `text`.
fn done_script(text: &str) -> serde_json::Value {
    json!({"steps": [{"text": [text]}, {"text": [text]}]})
}

/// A scripted turn that calls shell, which asks approval and waits.
fn waiting_script() -> serde_json::Value {
    json!({"steps": [
        {"tool_calls": [{"name": "shell", "arguments": {"command": "echo hi"}}]},
        {"text": ["done."]},
        {"text": ["done."]},
    ]})
}

#[test]
fn the_hidden_rail_waiting_line_shows_the_rail() {
    let setup = Setup::new();
    // The scripted model reads its script against each session's
    // workspace: the terminal's session ends at once, the other two
    // wait on approval.
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200}}),
    );
    // Headless sessions allow shell without asking; a standing ask
    // rule keeps the other two waiting on approval
    // (`docs/permissions.md`, "Headless").
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "echo hi"})
        ),
    )
    .unwrap();
    let second = setup.root.path().join("b");
    let third = setup.root.path().join("c");
    for dir in [&second, &third] {
        fs::create_dir_all(dir).unwrap();
    }
    write_json(&setup.workspace().join("s.json"), &done_script("all done."));
    write_json(&second.join("s.json"), &waiting_script());
    write_json(&third.join("s.json"), &waiting_script());
    let hub = Arc::new(Mutex::new(None));
    let (watch, _) = connect_hub(&setup, &hub);
    let away = second.to_string_lossy().into_owned();
    let elsewhere = third.to_string_lossy().into_owned();
    let second_guard = SessionGuard::arm(setup.deadline, &away);
    let third_guard = SessionGuard::arm(setup.deadline, &elsewhere);
    let control = support::Socket::connect(setup.deadline, &setup.hub_socket());
    assert_eq!(recv(&control, "hub_hello")["kind"], "hub_hello");
    let b = start_session(&control, &away, "second");
    let c = start_session(&control, &elsewhere, "third");
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"go\r");
    // The terminal's session is done while the other two wait: the rail
    // shows both waiting cards.
    run.wait_screen("two sessions waiting", |grid| {
        grid.rows
            .iter()
            .filter(|row| row.contains("NEEDS INPUT"))
            .count()
            >= 2
            && grid.contents.contains("all done.")
    });

    // Hiding the rail moves its waiting count to the Session card's last
    // row.
    run.write(ALT_R);
    let grid = run.wait_screen("the session card's waiting line", |grid| {
        // The styled cells the assertions read: the bang bold in
        // attention and the rest of the line in attention. A frame
        // with the text but without its style must not satisfy the
        // wait.
        let line = "! 2 waiting · hidden · show";
        grid.rows.iter().enumerate().any(|(y, row)| {
            let y = y as u16;
            row.find(line).is_some_and(|byte| {
                let at = row[..byte].chars().count() as u16;
                let bang = grid.cell(at, y);
                bang.symbol.as_str() == "!"
                    && bang.fg == ATTENTION
                    && bang.bold
                    && line.chars().skip(1).enumerate().all(|(n, expected)| {
                        let cell = grid.cell(at + 1 + n as u16, y);
                        cell.symbol == expected.to_string() && cell.fg == ATTENTION
                    })
            })
        })
    });
    assert!(
        !grid.rows.iter().any(|row| row.contains("NEEDS INPUT")),
        "the rail is hidden"
    );
    let line = "! 2 waiting · hidden · show";
    let y = find_row(&grid.rows, line);
    let row = grid.rows[y as usize].clone();
    let at = locate(&row, "! 2 waiting");
    let bang = grid.cell(at, y);
    assert_eq!(bang.symbol.as_str(), "!");
    assert_eq!(bang.fg, ATTENTION);
    assert!(bang.bold, "the bang is bold");
    for (n, expected) in line.chars().skip(1).enumerate() {
        let cell = grid.cell(at + 1 + n as u16, y);
        assert_eq!(cell.symbol, expected.to_string(), "cell {n}");
        assert_eq!(cell.fg, ATTENTION, "cell {n}'s colour");
    }

    // A click on the line shows the rail again.
    run.write(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", at + 1, y + 1, at + 1, y + 1).as_bytes());
    run.wait_screen("the rail shown again", |grid| {
        grid.rows.iter().any(|row| row.contains("NEEDS INPUT"))
    });

    // Ending the waiting sessions leaves the terminal's idle one: the
    // hub exits on its own once the terminal quits.
    for id in [&b, &c] {
        support::close_session(&support::Socket::connect(
            setup.deadline,
            &setup.session_socket(id),
        ));
    }
    second_guard.wait_gone();
    third_guard.wait_gone();
    drop((watch, control));
    quit(run);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.wait();
}
