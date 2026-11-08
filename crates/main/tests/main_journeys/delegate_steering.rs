//! The delegate steering journey (#4187): a running fiber delegate's view
//! draws the steering card for its own queue and edits it through the same
//! keys and targets as the conversation (`docs/tui.md`, "Swapped views"
//! and "Steering"). The delegate parks on a standing ask, so its queue
//! persists on screen until the test allows the call.

use serde_json::{Value, json};

use super::support::pty::{Grid, Run};
use super::support::{
    Setup, Socket, Subscribed, TRUECOLOUR, recv, subscribe, until, until_after, write_json,
};
use super::{dismiss_first_run_offer, journey_bin, quit_journey, run_journey, shows};

/// Alt+Up in its CSI form.
const ALT_UP: &[u8] = b"\x1b[1;3A";
/// Alt+Down in its CSI form.
const ALT_DOWN: &[u8] = b"\x1b[1;3B";
/// Alt+X in its CSI-u form.
const ALT_X: &[u8] = b"\x1b[120;3u";
/// The panel row of the running delegate.
const DELEGATE_ROW: &str = "● review the parser";
/// The shell command the delegate asks about.
const ASKED: &str = "echo hold";

/// The first column on row `y` where `needle` starts, cell by cell.
fn col_of(grid: &Grid, y: u16, needle: &str) -> Option<u16> {
    let want: Vec<char> = needle.chars().collect();
    let width = u16::try_from(grid.rows.first().map_or(0, |row| row.chars().count())).ok()?;
    (0..width).find(|x| {
        want.iter().enumerate().all(|(dx, want)| {
            let at = x.saturating_add(u16::try_from(dx).unwrap_or(u16::MAX));
            at < width && grid.cell(at, y).symbol == want.to_string()
        })
    })
}

/// The row holding `needle`.
fn row_of(grid: &Grid, needle: &str) -> Option<u16> {
    grid.rows
        .iter()
        .position(|row| row.contains(needle))
        .and_then(|y| u16::try_from(y).ok())
}

/// Where `needle` starts.
fn find_text(grid: &Grid, needle: &str) -> Option<(u16, u16)> {
    let y = row_of(grid, needle)?;
    Some((col_of(grid, y, needle)?, y))
}

/// Where the `✕` of the queued row holding `text` is: the first `✕` right
/// of the row's text.
fn find_cross(grid: &Grid, text: &str) -> Option<(u16, u16)> {
    let (from, y) = find_text(grid, text)?;
    let width = u16::try_from(grid.rows.first()?.chars().count()).ok()?;
    (from..width)
        .find(|x| grid.cell(*x, y).symbol == "✕")
        .map(|x| (x, y))
}

/// The delegate's session id, read from the status card's `session` field.
fn session_on(grid: &Grid) -> String {
    let row = grid
        .rows
        .iter()
        .find(|row| row.contains("session s_"))
        .unwrap_or_else(|| panic!("a session field in\n{}", grid.contents));
    let at = row.find("session s_").unwrap_or_default() + "session ".len();
    row.chars().skip(at).take(18).collect()
}

/// The texts of a `steering_queue` line's messages, oldest first.
fn queued(line: &Value) -> Vec<String> {
    line["payload"]["messages"]
        .as_array()
        .map(|messages| {
            messages
                .iter()
                .map(|message| {
                    message["content"][0]["text"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `command_id` of the queued message holding `text` in `line`.
fn id_of(line: &Value, text: &str) -> String {
    line["payload"]["messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .find(|message| message["content"][0]["text"] == text)
        })
        .and_then(|message| message["command_id"].as_str())
        .unwrap_or_else(|| panic!("{text:?} queued in {line}"))
        .to_owned()
}

/// The first live `steering_queue` after `since` whose texts equal `texts`.
fn queue_after(control: &Socket, since: &Subscribed, what: &str, texts: &[&str]) -> Value {
    let lines = until_after(control, since, what, |line| {
        line["kind"] == "steering_queue" && queued(line) == texts
    });
    lines.last().cloned().unwrap_or_default()
}

/// The standing rules: the parent's `delegate_spawn` is allowed, the
/// delegate's shell call always asks, so its turn parks on the approval.
fn write_setup(setup: &Setup) {
    write_json(
        &setup.workspace().join("d.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "shell", "arguments": {"command": ASKED}}]},
            {"text": ["Reviewed."]},
            {"text": ["Spare."]},
        ]}),
    );
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "delegate_spawn", "arguments": {
                "description": "review the parser", "prompt": "go",
                "model": "fiber:scripted/d.json"}}]},
            {"text": ["Done."]},
            {"text": ["Noted."]},
            {"text": ["Calm."]},
        ]}),
    );
    // The reviewer answers the parent's `delegate_spawn` review.
    write_json(
        &setup.workspace().join("r.json"),
        &json!({"steps": [{"text": ["allow"]}, {"text": ["allow"]}]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({
            "model": "scripted/s.json",
            "reviewer": {"model": "scripted/r.json"},
            "roles": {"delegate": "fiber:scripted/d.json"},
            "hub": {"idle_exit_ms": 1000}
        }),
    );
    std::fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n{}\n",
            json!({"decision": "allow", "tool": "delegate_spawn", "prefix": ""}),
            json!({"decision": "ask", "tool": "shell", "prefix": ASKED}),
        ),
    )
    .unwrap();
}

#[test]
fn queued_delegate_steering_journey() {
    run_journey(
        "delegate_steering::queued_delegate_steering_journey",
        std::panic::AssertUnwindSafe(|| {
            let setup = Setup::new();
            write_setup(&setup);
            let bin = journey_bin();
            let mut run = Run::spawn_journey(&setup, 160, 48, &bin, &[], &TRUECOLOUR);
            run.ready();
            dismiss_first_run_offer(&mut run);
            // The parent starts the delegate, whose shell call asks and
            // parks the turn on the approval.
            run.write(b"go\r");
            run.wait_screen("the delegate's approval", |grid| {
                shows(grid, "Approval 1 of 1")
            });
            run.write(b"\x1b[27u");
            run.wait_screen("the approval put aside", |grid| {
                shows(grid, "1 approvals waiting")
            });
            run.wait_screen("the running delegate row", |grid| shows(grid, DELEGATE_ROW));
            run.click_where("the delegate row", |grid| find_text(grid, DELEGATE_ROW));
            let grid = run.wait_screen("the delegate view", |grid| {
                shows(grid, "Delegate  ◆ review the parser")
                    && shows(grid, "› ◆ review the parser")
                    && shows(grid, "steering goes to ◆ review the parser")
            });
            let delegate = session_on(&grid);
            // A second client on the delegate hears its queue and its
            // approval; its subscription bound separates the replay from
            // the live lines the keys below cause.
            let control = Socket::connect(setup.deadline, &setup.hub_socket());
            assert_eq!(recv(&control, "hub_hello")["kind"], "hub_hello");
            let sub = subscribe(&control, &delegate);
            let asked = until(&control, "the delegate's permission_requested", |line| {
                line["kind"] == "permission_requested"
            });
            let request = asked
                .last()
                .and_then(|line| line["payload"]["request_id"].as_str())
                .unwrap_or_else(|| panic!("a request id in {asked:?}"))
                .to_owned();

            // Enter on a typed message queues it in the delegate's card
            // above the cleared input.
            run.write(b"use the parser\r");
            run.wait_screen("the first queued row", |grid| {
                shows(grid, "Steering")
                    && shows(grid, "1 queued")
                    && shows(grid, "↳ use the parser")
                    && shows(grid, "⌥↑ edit")
                    && shows(grid, "✕")
                    && shows(grid, "steering goes to ◆ review the parser")
            });
            let line = queue_after(&control, &sub, "the first queue echo", &["use the parser"]);
            let first = id_of(&line, "use the parser");
            run.write(b"and the tests\r");
            run.wait_screen("two queued rows", |grid| {
                shows(grid, "2 queued")
                    && shows(grid, "↳ use the parser")
                    && shows(grid, "↳ and the tests")
            });
            let line = queue_after(
                &control,
                &sub,
                "the second queue echo",
                &["use the parser", "and the tests"],
            );
            let second = id_of(&line, "and the tests");
            assert_ne!(first, second);

            // Alt+Up selects the newest row, then the older one; Alt+Down
            // goes back to the newer.
            run.write(ALT_UP);
            run.wait_screen("the newest row selected", |grid| {
                shows(grid, "▸ and the tests")
                    && shows(grid, "↳ use the parser")
                    && shows(grid, "editing a queued message")
            });
            run.write(ALT_UP);
            run.wait_screen("the older row selected", |grid| {
                shows(grid, "▸ use the parser") && shows(grid, "↳ and the tests")
            });
            run.write(ALT_DOWN);
            run.wait_screen("the newer row selected again", |grid| {
                shows(grid, "▸ and the tests") && shows(grid, "↳ use the parser")
            });

            // Alt+X drops the selected row from the delegate's queue.
            run.write(ALT_X);
            run.wait_screen("the selected row dropped", |grid| {
                shows(grid, "1 queued")
                    && shows(grid, "↳ use the parser")
                    && !shows(grid, "and the tests")
                    && !shows(grid, "editing a queued message")
            });
            let line = queue_after(&control, &sub, "the drop echo", &["use the parser"]);
            assert_eq!(id_of(&line, "use the parser"), first);

            // Another message, then its ✕ drops it.
            run.write(b"third thing\r");
            run.wait_screen("the third row", |grid| {
                shows(grid, "2 queued") && shows(grid, "↳ third thing")
            });
            let line = queue_after(
                &control,
                &sub,
                "the third queue echo",
                &["use the parser", "third thing"],
            );
            let third = id_of(&line, "third thing");
            assert_ne!(third, first);
            run.click_where("the third row's cross", |grid| {
                find_cross(grid, "↳ third thing")
            });
            run.wait_screen("the third row dropped", |grid| {
                shows(grid, "1 queued")
                    && shows(grid, "↳ use the parser")
                    && !shows(grid, "third thing")
            });
            queue_after(&control, &sub, "the cross drop echo", &["use the parser"]);

            // Allowing the parked call lets the delegate reach its step
            // boundary: the queued message joins its turn.
            control.send(&format!(
                "{{\"id\":\"c_reply\",\"session_id\":\"{delegate}\",\"command\":\"reply\",\"args\":{{\"request_id\":\"{request}\",\"decision\":\"allow\"}}}}"
            ));
            until_after(&control, &sub, "the message applied", |line| {
                line["kind"] == "steering_applied"
                    && line["payload"]["content"][0]["text"] == "use the parser"
            });
            queue_after(&control, &sub, "the emptied queue", &[]);
            run.wait_screen("the joined steer rule and no card", |grid| {
                grid.rows.iter().any(|row| row.contains("steer ·"))
                    && shows(grid, "use the parser")
                    && !shows(grid, "Steering")
                    && !shows(grid, "queued")
            });

            // Back in the conversation the delegate's queue is gone.
            run.write(b"\x1b[27u");
            run.wait_screen("the conversation again", |grid| {
                !shows(grid, "Delegate  ◆ review the parser") && !shows(grid, "Steering")
            });
            drop(control);
            quit_journey(run);
        }),
    );
}
