//! The delegate steering journey (#4187): a running fiber delegate's view
//! draws the steering card for its own queue and edits it through the same
//! keys and targets as the conversation (`docs/tui.md`, "Swapped views"
//! and "Steering"). The delegate runs a foreground shell command blocked
//! on a FIFO the test owns, so its queue persists on screen until the
//! test releases the FIFO.

use serde_json::{Value, json};

use super::support::pty::{Grid, Run};
use super::support::{
    Setup, Socket, Subscribed, TRUECOLOUR, accepted, bounded, recv, subscribe, until, until_after,
    write_json,
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

/// The delegate's session id: the `delegate_session_id` of the
/// `delegate_started` line in the parent's log, waited for under one
/// deadline.
fn delegate_session(setup: &Setup) -> String {
    let sessions = log::sessions_dir(&setup.home(), &worktree::project(&setup.workspace()));
    let found = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let seen = std::sync::Arc::clone(&found);
    fakes::wait_until("the delegate_started line", setup.deadline, move || {
        let Ok(entries) = std::fs::read_dir(&sessions) else {
            return false;
        };
        for entry in entries.flatten() {
            let Ok(text) = std::fs::read_to_string(entry.path().join("events.jsonl")) else {
                continue;
            };
            for line in super::support::complete_lines(&text) {
                if line["kind"] == "delegate_started"
                    && let Some(id) = line["payload"]["delegate_session_id"].as_str()
                {
                    *seen.lock().unwrap() = Some(id.to_owned());
                    return true;
                }
            }
        }
        false
    });
    let id = found.lock().unwrap().clone();
    id.unwrap_or_else(|| panic!("a delegate session id"))
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

/// The subscription bound just after `line`: later waits skip every
/// replayed line at or before it. An ephemeral line, such as a
/// `steering_queue`, carries no `seq` and keeps `prev`; the socket hands
/// lines over in order, so a wait after it never sees an earlier copy.
fn fence_after(prev: Subscribed, line: &Value, delegate: &str) -> Subscribed {
    match line["seq"].as_u64() {
        Some(seq) => accepted(
            json!({"payload": {"result": {"last_seq": seq}}}),
            Some(delegate.to_owned()),
        ),
        None => prev,
    }
}

/// The scripts and rules: the parent's `delegate_spawn` goes to the
/// scripted reviewer, and the delegate's one shell call, which blocks
/// reading the FIFO at `hold`, is allowed by its exact command.
fn write_setup(setup: &Setup, command: &str) {
    write_json(
        &setup.workspace().join("d.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "shell", "arguments": {"command": command}}]},
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
            json!({"decision": "allow", "tool": "shell", "prefix": command}),
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
            let hold_path = setup.workspace().join("delegate-steering-hold");
            let command = format!("head -n 1 '{}'", hold_path.display());
            write_setup(&setup, &command);
            let bin = journey_bin();
            let mut run = Run::spawn_journey(&setup, 160, 48, &bin, &[], &TRUECOLOUR);
            // Declared after the run, so a failure drops the hold first:
            // its watchdog reaps the blocked reader before the teardown.
            fakes::fifo(&hold_path);
            let mut delegate_shell_hold = fakes::Hold::on(&hold_path, hold_path.to_str().unwrap());
            run.ready();
            dismiss_first_run_offer(&mut run);
            run.write(b"go\r");
            run.wait_screen("the running delegate row", |grid| shows(grid, DELEGATE_ROW));
            run.click_where("the delegate row", |grid| find_text(grid, DELEGATE_ROW));
            run.wait_screen("the delegate view", |grid| {
                shows(grid, "Delegate  ◆ review the parser")
                    && shows(grid, "› ◆ review the parser")
                    && shows(grid, "steering goes to ◆ review the parser")
            });
            let delegate = delegate_session(&setup);
            // A second client on the delegate hears its lines; each wait
            // below starts after the latest line the test consumed.
            let control = Socket::connect(setup.deadline, &setup.hub_socket());
            assert_eq!(recv(&control, "hub_hello")["kind"], "hub_hello");
            let sub = subscribe(&control, &delegate);
            // The shell call runs, blocked on the FIFO, before any steer
            // goes: its own start is the rendezvous.
            let started = until(&control, "the held shell call starting", |line| {
                line["kind"] == "tool_call_started" && line["session_id"] == delegate.as_str()
            });
            let started = started.last().cloned().unwrap_or_default();
            let shell_call = started["action_id"]
                .as_str()
                .unwrap_or_else(|| panic!("an action id on {started}"))
                .to_owned();
            let mut fence = fence_after(sub, &started, &delegate);

            // Enter on a typed message queues it in the delegate's card
            // above the cleared input.
            run.write(b"use the parser\r");
            let line = queue_after(
                &control,
                &fence,
                "the first queue echo",
                &["use the parser"],
            );
            let first = id_of(&line, "use the parser");
            fence = fence_after(fence, &line, &delegate);
            run.wait_screen("the first queued row", |grid| {
                shows(grid, "Delegate  ◆ review the parser")
                    && shows(grid, "Steering")
                    && shows(grid, "1 queued")
                    && shows(grid, "↳ use the parser")
                    && shows(grid, "⌥↑ edit")
                    && shows(grid, "✕")
                    && shows(grid, "steering goes to ◆ review the parser")
            });
            run.write(b"and the tests\r");
            let line = queue_after(
                &control,
                &fence,
                "the second queue echo",
                &["use the parser", "and the tests"],
            );
            let second = id_of(&line, "and the tests");
            assert_ne!(first, second);
            fence = fence_after(fence, &line, &delegate);
            run.wait_screen("two queued rows", |grid| {
                shows(grid, "2 queued")
                    && shows(grid, "↳ use the parser")
                    && shows(grid, "↳ and the tests")
            });

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
            let line = queue_after(&control, &fence, "the drop echo", &["use the parser"]);
            assert_eq!(id_of(&line, "use the parser"), first);
            fence = fence_after(fence, &line, &delegate);
            run.wait_screen("the selected row dropped", |grid| {
                shows(grid, "1 queued")
                    && shows(grid, "↳ use the parser")
                    && !shows(grid, "and the tests")
                    && !shows(grid, "editing a queued message")
            });

            // Another message, then its ✕ drops it.
            run.write(b"third thing\r");
            let line = queue_after(
                &control,
                &fence,
                "the third queue echo",
                &["use the parser", "third thing"],
            );
            assert_ne!(id_of(&line, "third thing"), first);
            fence = fence_after(fence, &line, &delegate);
            run.wait_screen("the third row", |grid| {
                shows(grid, "2 queued") && shows(grid, "↳ third thing")
            });
            run.click_where("the third row's cross", |grid| {
                find_cross(grid, "↳ third thing")
            });
            let line = queue_after(&control, &fence, "the cross drop echo", &["use the parser"]);
            assert_eq!(id_of(&line, "use the parser"), first);
            fence = fence_after(fence, &line, &delegate);
            run.wait_screen("the third row dropped", |grid| {
                shows(grid, "1 queued")
                    && shows(grid, "↳ use the parser")
                    && !shows(grid, "third thing")
            });

            // Releasing the FIFO ends the shell call; the delegate reaches
            // its step boundary and the kept message joins its turn.
            let mut release = delegate_shell_hold.release().try_clone().unwrap();
            bounded(
                setup.deadline.start_wait(),
                "writing the release",
                move || std::io::Write::write_all(&mut release, b"\n"),
            )
            .unwrap();
            let (mut ended, mut applied, mut emptied) = (false, false, false);
            until_after(
                &control,
                &fence,
                "the shell end, the applied message and the empty queue",
                |line| {
                    match line["kind"].as_str() {
                        Some("tool_call_completed") => {
                            ended |= line["action_id"] == shell_call.as_str();
                        }
                        Some("steering_applied") => {
                            applied |= line["payload"]["content"][0]["text"] == "use the parser";
                        }
                        Some("steering_queue") => emptied |= queued(line).is_empty(),
                        _ => {}
                    }
                    ended && applied && emptied
                },
            );
            delegate_shell_hold.stand_down(setup.deadline.cleanup_phase());
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
