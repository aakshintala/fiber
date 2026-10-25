//! Binary-level tests of the approval panel (`docs/tui.md`, "An
//! approval"): the real binary in a pseudo-terminal draws the standing ask
//! and a reviewer's escalation as the prototype draws them, in truecolour
//! and at 256 colours.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test helpers; a failure is the test's; a live test prints its outcome"
)]

mod support;

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use support::pty::{Colour, Grid, Run};
use support::{
    SessionGuard, Setup, Socket, TRUECOLOUR, close_session, connect_hub, connect_hub_within, recv,
    start_session, subscribe, until, write_json,
};

/// The standing-ask tint in truecolour.
const APPROVAL_BG: Colour = Colour::Rgb(26, 26, 34);
/// A reviewer's escalation tint in truecolour.
const ALERT_BG: Colour = Colour::Rgb(80, 28, 32);
/// `attention` in the dark theme.
const ATTENTION: Colour = Colour::Rgb(255, 159, 67);
/// `info` in the dark theme.
const INFO: Colour = Colour::Rgb(125, 211, 252);
/// `error` in the dark theme.
const ERROR: Colour = Colour::Rgb(255, 93, 115);

/// The scripted script making one `shell` call.
fn shell_script(command: &str) -> serde_json::Value {
    json!({"steps": [{"tool_calls": [{"name": "shell", "arguments": {"command": command}}]}]})
}

/// A setup whose scripted model makes the shell call `command`.
fn shell_setup(command: &str) -> Setup {
    let setup = Setup::new();
    write_json(&setup.workspace().join("s.json"), &shell_script(command));
    setup
}

/// The config naming the workspace script, with `extra` beside the model.
fn configure(setup: &Setup, extra: serde_json::Value) {
    let mut config = json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200}});
    if let Some(into) = config.as_object_mut()
        && let Some(from) = extra.as_object()
    {
        for (key, value) in from {
            into.insert(key.clone(), value.clone());
        }
    }
    write_json(&setup.home().join("config.json"), &config);
}

/// A standing project ask for `prefix`: the project's rules live in Fiber
/// home at `projects/<key>/rules` (`docs/state.md`, "Projects"), placed
/// through the canonical project key.
fn ask_rule(setup: &Setup, prefix: &str) {
    let key = log::project_key(&doors::project(&setup.workspace()));
    let rule = setup.home().join("projects").join(key).join("rules");
    fs::create_dir_all(rule.parent().unwrap()).unwrap();
    fs::write(
        rule,
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": prefix})
        ),
    )
    .unwrap();
}

/// Spawns `fiber` on a 160 by 48 terminal with `env` after the cleared
/// environment, so every run keeps today's screen.
fn terminal(setup: &Setup, env: &[(&str, &str)]) -> Run {
    Run::spawn(setup, 160, 48, &[], env)
}

/// The first row holding `needle`, or a panic naming it.
fn row_with(grid: &Grid, needle: &str) -> u16 {
    u16::try_from(
        grid.rows
            .iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} in\n{}", grid.contents)),
    )
    .unwrap_or(u16::MAX)
}

/// The column where `needle` starts on `y`, matching the cells one by
/// one, so a wide cell before it changes nothing.
fn col_at(grid: &Grid, y: u16, needle: &str) -> u16 {
    let want: Vec<char> = needle.chars().collect();
    for x in 0..160u16 {
        let mut ok = !want.is_empty();
        for (dx, want) in want.iter().enumerate() {
            let xi = x.saturating_add(u16::try_from(dx).unwrap_or(u16::MAX));
            if xi >= 160 || grid.cell(xi, y).symbol != want.to_string() {
                ok = false;
                break;
            }
        }
        if ok {
            return x;
        }
    }
    panic!("{needle:?} on row {y}")
}

/// Quits the run: two interrupts and Enter, as the journeys quit.
fn quit(mut run: Run) {
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

/// The rail's ceiling in columns (`crates/tui/src/layout.rs`,
/// `RAIL_CEILING`): the rail never reaches past it, so a click at or
/// right of here missed the rail.
const RAIL_MAX_COLS: u16 = 48;

/// A standing global ask for `prefix`: headless sessions allow shell
/// without asking, so without this the other session would run past its
/// call instead of waiting (`docs/permissions.md`, "Headless").
fn global_ask_rule(setup: &Setup, prefix: &str) {
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": prefix})
        ),
    )
    .unwrap();
}

/// The panel's header row from its own start: the header is inset right
/// of the rail, so the rail's copy of either session's name sits left of
/// it or on another row. Which request a header labels reads here, not
/// anywhere on screen.
fn panel_header_text(grid: &Grid, y: u16, head: &str) -> String {
    let from = col_at(grid, y, head);
    (from..160).map(|x| grid.cell(x, y).symbol).collect()
}

#[test]
fn a_standing_ask_draws_the_panel_typing_feeds_back_and_esc_puts_it_aside() {
    let setup = shell_setup("cargo mutants -p hub");
    // One session: the rail stays hidden, so the panel's columns are
    // the look pinned.
    configure(&setup, json!({"tui": {"rail": {"shown": false}}}));
    ask_rule(&setup, "cargo");
    let mut run = terminal(&setup, &TRUECOLOUR);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(b"run it\r");
    // The session shows its name, the first prompt: the first frame can
    // still carry the session id, so the wait holds until the named
    // header draws.
    let grid = run.wait_screen("the approval panel", |grid| {
        grid.contents.contains("Approval 1 of 1 · run it")
    });
    let header = row_with(&grid, "Approval 1 of 1");
    // The panel's tinted run: every cell of the panel's text rows takes
    // the approval tint.
    let end = (0..160)
        .rev()
        .find(|x| grid.cell(*x, header).bg == APPROVAL_BG)
        .expect("the tinted run");
    assert!(
        grid.rows[usize::from(header)].contains("Approval 1 of 1 · run it · irreversible"),
        "{}",
        grid.contents
    );
    // Every cell of the panel's text rows takes the approval tint, the
    // stripe its `attention` cell, and the edges draw above and below:
    // header, tool, facts, blank, two choices, blank, hint. Past the
    // grip, which takes column 0.
    for x in 1..end + 1 {
        assert_eq!(grid.cell(x, header - 1).symbol.as_str(), "▄", "top {x}");
        assert_eq!(grid.cell(x, header + 8).symbol.as_str(), "▀", "bottom {x}");
    }
    let text: Vec<u16> = (header..header + 8).collect();
    for y in text.iter().copied() {
        for x in 1..end + 1 {
            assert_eq!(grid.cell(x, y).bg, APPROVAL_BG, "({x}, {y})");
        }
        assert_eq!(grid.cell(1, y).symbol.as_str(), "▌", "row {y}");
        assert_eq!(grid.cell(1, y).fg, ATTENTION, "row {y}");
    }
    // No row holds the call's JSON.
    assert!(
        !grid.rows.iter().any(|row| row.contains("{\"")),
        "{:?}",
        grid.rows
    );
    // The header: "Approval 1 of 1" bold, the session in `attention`,
    // " · irreversible" in `error`, "Esc puts it aside" dim at the end.
    let head = col_at(&grid, header, "Approval 1 of 1");
    for dx in 0..15 {
        let cell = grid.cell(head + dx, header);
        assert_eq!(cell.fg, ATTENTION, "col {dx}");
        assert!(cell.bold, "col {dx}");
    }
    let esc = col_at(&grid, header, "Esc puts it aside");
    for dx in 0..17 {
        let cell = grid.cell(esc + dx, header);
        assert!(cell.dim, "col {dx}");
    }
    // Right-aligned: the tint's last cell is the hint's last one.
    let end = (0..160)
        .rev()
        .find(|x| grid.cell(*x, header).bg == APPROVAL_BG)
        .expect("the tinted run");
    assert_eq!(end, esc + 16);
    let loud = col_at(&grid, header, "irreversible");
    for dx in 0..12 {
        let cell = grid.cell(loud + dx, header);
        assert_eq!(cell.fg, ERROR, "col {dx}");
        assert!(cell.bold, "col {dx}");
    }
    // The session in `attention`, not bold.
    let session = col_at(&grid, header, "run it");
    for dx in 0..6 {
        let cell = grid.cell(session + dx, header);
        assert_eq!(cell.fg, ATTENTION, "col {dx}");
        assert!(!cell.bold, "col {dx}");
    }
    // The tool's name and its main argument, then the facts line: the
    // tool row sits right below the header.
    let tool = header + 1;
    assert!(
        grid.rows[usize::from(tool)].contains("shell cargo mutants -p hub"),
        "{}",
        grid.contents
    );
    let shell = col_at(&grid, tool, "shell");
    assert_eq!(grid.cell(shell, tool).fg, INFO);
    for dx in [0, 4, 6, 25] {
        assert!(grid.cell(shell + dx, tool).bold, "col {dx}");
    }
    let facts = tool + 1;
    assert!(
        grid.rows[usize::from(facts)].contains("runs a command · asked by the project rule cargo"),
        "{}",
        grid.contents
    );
    // The facts line is dim.
    let runs = col_at(&grid, facts, "runs a command");
    for dx in [0, 13] {
        assert!(grid.cell(runs + dx, facts).dim, "col {dx}");
    }
    let cargo = col_at(&grid, facts, "cargo");
    for dx in [0, 4] {
        assert!(grid.cell(cargo + dx, facts).dim, "col {dx}");
    }
    // The choices, numbered, and the hint: the focused mark in
    // `attention", its label bold, the number dim, the hint dim.
    let focused = row_with(&grid, "▸ 1 Allow once");
    let pointed = col_at(&grid, focused, "▸");
    assert_eq!(grid.cell(pointed, focused).fg, ATTENTION);
    let chosen = col_at(&grid, focused, "Allow once");
    for dx in [0, 5, 9] {
        let cell = grid.cell(chosen + dx, focused);
        assert!(cell.bold, "col {dx}");
    }
    let number = col_at(&grid, focused, "1 Allow once");
    assert!(grid.cell(number, focused).dim);
    row_with(&grid, "2 Deny");
    let hint = row_with(&grid, "↑↓ choose");
    let hint_at = col_at(&grid, hint, "↑↓ choose");
    for dx in [0, 10, 30] {
        assert!(grid.cell(hint_at + dx, hint).dim, "col {dx}");
    }
    // Typing "2" is feedback, not a choice: the panel stays open with
    // "2" on deny, and no reply goes out.
    run.write(b"2");
    let grid = run.wait_screen("the feedback", |grid| grid.contents.contains("Deny  2"));
    assert!(grid.contents.contains("Allow once"), "the panel stays open");
    // Esc puts the request aside behind its badge above the working line.
    run.write(b"\x1b[27u");
    let grid = run.wait_screen("the badge", |grid| {
        grid.contents.contains("approvals waiting")
    });
    let badge = row_with(&grid, "approvals waiting");
    assert!(
        grid.rows[usize::from(badge)].contains("1 approvals waiting · click to reopen"),
        "{}",
        grid.contents
    );
    let warning = col_at(&grid, badge, "⚠");
    let mark = grid.cell(warning, badge);
    assert_eq!(mark.fg, ATTENTION);
    assert!(!mark.bold);
    let rest = col_at(&grid, badge, "approvals waiting");
    for dx in [0, 10, 17] {
        let cell = grid.cell(rest + dx, badge);
        assert_eq!(cell.fg, ATTENTION, "col {dx}");
        assert!(cell.bold, "col {dx}");
    }
    quit(run);
}

#[test]
fn a_reviewer_failure_escalates_to_an_alert_panel() {
    let setup = shell_setup("cargo mutants -p hub");
    // An empty reviewer script: the reviewer's first request fails, which
    // reads as a reviewer failure. No rule asks first.
    write_json(&setup.workspace().join("empty.json"), &json!({"steps": []}));
    configure(
        &setup,
        json!({"reviewer": {"model": "scripted/empty.json"},
            "tui": {"rail": {"shown": false}}}),
    );
    let mut run = terminal(&setup, &TRUECOLOUR);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(b"run it\r");
    let grid = run.wait_screen("the approval panel", |grid| {
        grid.contents.contains("Approval 1 of 1")
    });
    let header = row_with(&grid, "Approval 1 of 1");
    let end = (0..160)
        .rev()
        .find(|x| grid.cell(*x, header).bg == ALERT_BG)
        .expect("the tinted run");
    // Every panel row takes the alert tint with an error stripe: ten
    // text rows from the header to the bottom edge.
    let bottom = (header..48)
        .find(|y| {
            grid.rows[usize::from(*y)]
                .chars()
                .skip(1)
                .take(usize::from(end))
                .all(|cell| cell == '▀')
        })
        .expect("the bottom edge");
    assert_eq!(bottom - header, 10, "ten text rows to the edge");
    for y in header..bottom {
        for x in 1..end + 1 {
            assert_eq!(grid.cell(x, y).bg, ALERT_BG, "({x}, {y})");
        }
        assert_eq!(grid.cell(1, y).symbol.as_str(), "▌", "row {y}");
        assert_eq!(grid.cell(1, y).fg, ERROR, "row {y}");
    }
    let facts = row_with(&grid, "the reviewer failed");
    assert!(
        grid.rows[usize::from(facts)]
            .trim_end()
            .ends_with("the reviewer failed"),
        "{}",
        grid.contents
    );
    // Esc puts it aside; quitting then lands on the primary screen.
    run.write(b"\x1b[27u");
    run.wait_screen("the badge", |grid| {
        grid.contents.contains("approvals waiting")
    });
    quit(run);
}

#[test]
fn an_escalation_takes_palette_52_at_256_colours() {
    let setup = shell_setup("cargo mutants -p hub");
    write_json(&setup.workspace().join("empty.json"), &json!({"steps": []}));
    configure(
        &setup,
        json!({"reviewer": {"model": "scripted/empty.json"},
            "tui": {"rail": {"shown": false}}}),
    );
    let mut run = terminal(&setup, &[("TERM_PROGRAM", "ghostty")]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(b"run it\r");
    let grid = run.wait_screen("the approval panel", |grid| {
        grid.contents.contains("Approval 1 of 1")
    });
    // The alert tint's 256-colour entry, on every cell of the header row.
    let header = row_with(&grid, "Approval 1 of 1");
    let end = (0..160)
        .rev()
        .find(|x| grid.cell(*x, header).bg == Colour::Indexed(52))
        .expect("the tinted run");
    for y in [header] {
        for x in 1..end + 1 {
            assert_eq!(grid.cell(x, y).bg, Colour::Indexed(52), "({x}, {y})");
        }
    }
    run.write(b"\x1b[27u");
    run.wait_screen("the badge", |grid| {
        grid.contents.contains("approvals waiting")
    });
    quit(run);
}

#[test]
fn two_sessions_approvals_share_one_queue_and_esc_steps_then_badges() {
    let setup = Setup::new();
    configure(&setup, json!({}));
    global_ask_rule(&setup, "echo hi");
    // One waiting turn per workspace: the shell call asks, and the two
    // text steps after it let the turn end once answered.
    let script = json!({"steps": [
        {"tool_calls": [{"name": "shell", "arguments": {"command": "echo hi"}}]},
        {"text": ["done."]},
        {"text": ["done."]},
    ]});
    write_json(&setup.workspace().join("s.json"), &script);
    let second_dir = setup.root.path().join("b");
    fs::create_dir_all(&second_dir).unwrap();
    write_json(&second_dir.join("s.json"), &script);
    let hub = Arc::new(Mutex::new(None));
    let (watch, _) = connect_hub(&setup, &hub);
    let away = second_dir.to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &away);
    let control = Socket::connect(setup.deadline, &setup.hub_socket());
    assert_eq!(recv(&control, "hub_hello")["kind"], "hub_hello");
    let b = start_session(&control, &away, "second");
    let mut run = terminal(&setup, &TRUECOLOUR);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    // B's turn runs headless: by the time A asks it waits on the
    // standing ask, and the rail carries both waiting cards. B's
    // request never folds on this terminal before the switch, so A's
    // panel reads 1 of 1.
    run.write(b"run it\r");
    run.wait_screen("A's approval panel", |grid| {
        grid.contents.contains("Approval 1 of 1 · run it")
    });
    // Both sessions wait: A's card for its open panel, B's headless
    // one. Two NEEDS INPUT rows prove B waits before the switch below
    // sets the arrival its request opens.
    run.wait_screen("both sessions waiting", |grid| {
        grid.rows
            .iter()
            .filter(|row| row.contains("NEEDS INPUT"))
            .count()
            >= 2
            && grid.contents.contains("second")
    });
    // Esc puts A aside behind its badge.
    run.write(b"\x1b[27u");
    let grid = run.wait_screen("the badge for one", |grid| {
        grid.contents
            .contains("1 approvals waiting · click to reopen")
    });
    // Click B's waiting card: the row below a NEEDS INPUT row holding
    // B's name. A's own card waits too, so the name picks B's.
    let card = grid
        .rows
        .iter()
        .enumerate()
        .find(|(y, _)| {
            grid.rows
                .get(y + 1)
                .is_some_and(|below| below.contains("second"))
                && grid.rows[*y].contains("NEEDS INPUT")
        })
        .map(|(y, _)| u16::try_from(y).unwrap())
        .expect("B's waiting card");
    let at = col_at(&grid, card + 1, "second");
    assert!(at < RAIL_MAX_COLS, "the click lands on the rail");
    run.write(
        format!(
            "\x1b[<0;{};{}M\x1b[<0;{};{}m",
            at + 1,
            card + 2,
            at + 1,
            card + 2
        )
        .as_bytes(),
    );
    // The switch opens B's request, queued behind A's: 2 of 2.
    run.wait_screen("B's approval panel", |grid| {
        grid.contents.contains("Approval 2 of 2 · second")
    });
    // B is the last waiting request, so one Esc closes the panel.
    run.write(b"\x1b[27u");
    run.wait_screen("the badge for both", |grid| {
        grid.contents
            .contains("2 approvals waiting · click to reopen")
    });
    // `/approvals` reopens the queue at its first request: A's.
    run.write(b"/approvals\r");
    let grid = run.wait_screen("the queue back at A", |grid| {
        grid.contents.contains("Approval 1 of 2")
    });
    let header = row_with(&grid, "Approval 1 of 2");
    let text = panel_header_text(&grid, header, "Approval 1 of 2");
    assert!(
        text.contains("run it"),
        "the first header names A:\n{}",
        grid.contents
    );
    assert!(
        !text.contains("second"),
        "the first header names only A:\n{}",
        grid.contents
    );
    // Esc steps to the next waiting request: B's.
    run.write(b"\x1b[27u");
    run.wait_screen("the queue stepped to B", |grid| {
        grid.contents.contains("Approval 2 of 2 · second")
    });
    // A second Esc puts both aside behind the badge.
    run.write(b"\x1b[27u");
    run.wait_screen("the badge for both again", |grid| {
        grid.contents
            .contains("2 approvals waiting · click to reopen")
    });
    run.write(b"/approvals\r");
    let grid = run.wait_screen("the queue at A again", |grid| {
        grid.contents.contains("Approval 1 of 2")
    });
    let header = row_with(&grid, "Approval 1 of 2");
    let text = panel_header_text(&grid, header, "Approval 1 of 2");
    assert!(
        text.contains("run it"),
        "the first header names A:\n{}",
        grid.contents
    );
    assert!(
        !text.contains("second"),
        "the first header names only A:\n{}",
        grid.contents
    );
    // A's id is the feed's other session: the first status that is not
    // B's. A `full` subscribe folds A's stream, so its resolution
    // reaches this test as evidence the answer below landed.
    control.send(r#"{"id":"c_feed","command":"feed"}"#);
    let fed = until(&control, "A's status on the feed", |line| {
        line["kind"] == "session_status" && line["session_id"].as_str().is_some_and(|id| id != b)
    });
    let a = fed.last().unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    subscribe(&control, &a);
    // Allow A: the queue drops it and shows B alone.
    run.write(b"\r");
    let grid = run.wait_screen("B's request alone", |grid| {
        grid.contents.contains("Approval 1 of 1 · second")
    });
    let header = row_with(&grid, "Approval 1 of 1");
    let text = panel_header_text(&grid, header, "Approval 1 of 1");
    assert!(
        text.contains("second"),
        "the last header names B:\n{}",
        grid.contents
    );
    until(&control, "A's permission_resolved", |line| {
        line["kind"] == "permission_resolved"
            && line["payload"]["decision"] == "allow"
            && line["payload"]["decided_by"] == "person"
    });
    // Allow B: the queue empties and the panel closes.
    run.write(b"\r");
    run.wait_screen("the queue emptied", |grid| {
        !grid.contents.contains("Approval 1 of 1")
    });
    // Quitting waits for the hub to exit, which needs no client open:
    // drop this test's hub clients first. Closing B goes over its own
    // session socket, so it needs no hub.
    drop((watch, control));
    quit(run);
    close_session(&Socket::connect(setup.deadline, &setup.session_socket(&b)));
    guard.wait_gone();
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.wait();
}

/// A hub that cannot start reports its exit status and its `hub.log` tail
/// through the connect wait: the configuration is broken, so the hub the
/// wait starts exits at once and the socket never binds.
#[test]
fn hub_start_failure_reports_status_and_log_tail() {
    let setup = Setup::new();
    let marker = "hub-start-marker-2664";
    let logs = setup.home().join("logs");
    fs::create_dir_all(&logs).unwrap();
    fs::write(logs.join("hub.log"), format!("{marker}\n")).unwrap();
    fs::write(setup.home().join("config.json"), "{invalid-2664").unwrap();
    let hub = Arc::new(Mutex::new(None));
    // The connect's five-second bind wait spans ten seconds here, three
    // short of this wait, so its `did not bind` error arrives before this
    // wait's own timeout.
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        connect_hub_within(&setup, &hub, Duration::from_secs(13))
    }));
    let message = failed
        .err()
        .and_then(|panic| panic.downcast::<String>().ok().map(|boxed| *boxed))
        .expect("a hub that cannot start fails the connect wait");
    assert!(
        message.contains("for the hub to start and say hub_hello"),
        "the failure keeps the connect wait's message: {message}"
    );
    assert!(
        message.contains("did not bind"),
        "the failure is the connect's bind error, not another failure: {message}"
    );
    assert!(
        message.contains("exited (exit status: 1)"),
        "the failure names the hub child's exit status: {message}"
    );
    assert!(
        message.contains(marker),
        "the failure shows the hub.log tail: {message}"
    );
}
