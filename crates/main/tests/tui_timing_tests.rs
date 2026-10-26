//! Binary-level terminal timers (`docs/state.md`, "Diagnostic logs"): the
//! built `fiber resume` on a seeded session over a pty. At `debug` the
//! attach records `tui.first_render`, `tui.log_read` and `tui.transcript`
//! with the exact wire shape, once each; at the default no debug line is
//! written. The seeded tail proves the attach built its preview: the wait
//! below holds seeded text only the tail read can draw.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use serde_json::{Value, json};
use support::pty::Run;
use support::{Setup, session_dir, write_json};

/// The seeded session, from `tui`.
const ID: &str = "s_0000000000000001";
/// The pty size the open draws on.
const COLS: u16 = 160;
const ROWS: u16 = 48;
/// The running turn's streamed reply: the preview's last rows hold it, so
/// a wait holding it proves the tail read built the preview.
const LAST: &str = "streamed text, a few words at a time; ";

/// Seeds the session log and its `recent.jsonl` row: the ten-turn log
/// from `tui` plus a running one, as a listed session seeds its row. The
/// fold refuses a line whose payload does not read as its kind, so the
/// seed carries the schema-valid fixups `attach_first_frame.rs` carries,
/// and reads every envelope back before writing.
fn seed(setup: &Setup, extra: Value) {
    let dir = session_dir(setup, ID);
    fs::create_dir_all(&dir).unwrap();
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    let log = tui::paging_session(1)
        .lines()
        .filter_map(|line| {
            let mut event: serde_json::Value = serde_json::from_str(line).unwrap();
            event.get("seq")?;
            if event.get("kind").and_then(|kind| kind.as_str()) == Some("session_started") {
                event["payload"]["workspace"] =
                    serde_json::Value::String(workspace.to_string_lossy().into_owned());
                // A `session_started` without `variables` does not parse
                // as one, so `resolve` finds no session: the seed writes
                // the schema-valid payload the bench writes in
                // `wheel.rs:create_log`.
                event["payload"]["variables"] = json!({
                    "path": "",
                    "names": [],
                    "source": "inherited",
                });
            }
            // The jig's envelopes name its own session, whose lines the
            // open folds nowhere: the attached session drops every line
            // another session sent, so the pass lands empty and the
            // screen goes blank. The seed renames them to the fixture.
            event["session_id"] = json!(ID);
            if event.get("kind").and_then(|kind| kind.as_str()) == Some("tool_call_started") {
                // The jig's `tool_call_started` holds `{}`, which does
                // not read as `DeclaredEffects`: the seed writes the
                // empty declaration, as a call declaring none does.
                event["payload"]["effects"] = json!([]);
                event["payload"]["reversible"] = json!(true);
            }
            if event.get("kind").and_then(|kind| kind.as_str()) == Some("usage_recorded") {
                // The jig's `usage_recorded` holds bench counts, not a
                // call's record, so the fold refuses the log as missing
                // `generation_id`: the seed rewrites the schema-valid
                // record the model host emits in `model_host.rs:emit`.
                let seq = event.get("seq").cloned().unwrap_or(json!(0));
                let payload = event.get_mut("payload").unwrap();
                payload["generation_id"] = serde_json::Value::String(format!("fiber-seed-{seq}"));
                payload["model"] = json!("scripted/s.json");
                payload["tokens"] = json!({
                    "input": 1200,
                    "cache_read": 0,
                    "cache_write": {},
                    "output": 300,
                });
                payload["input_bytes"] = json!(1200);
                payload["cost"] = serde_json::Value::Null;
                for stale in ["input_tokens", "output_tokens"] {
                    payload.as_object_mut().unwrap().remove(stale);
                }
            }
            Some(serde_json::to_string(&event).unwrap())
        })
        .collect::<Vec<_>>();
    // The fold refuses a line whose payload does not read as its kind,
    // one field at a time, so the seed reads every seeded envelope back
    // with the contract's own event type before writing: a drifted jig
    // payload fails here, naming its kind, not on the first frame.
    for line in &log {
        let envelope: contract::Envelope = serde_json::from_str(line).unwrap();
        match contract::events::Event::from_envelope(&envelope) {
            Ok(Some(_)) => {}
            Ok(None) => panic!("the seed holds kind {}", envelope.kind),
            Err(error) => panic!("the seed's {} does not read: {error}", envelope.kind),
        }
    }
    fs::write(dir.join("events.jsonl"), log.join("\n") + "\n").unwrap();
    let key = dir
        .parent()
        .and_then(|sessions| sessions.parent())
        .and_then(|projects| projects.file_name())
        .and_then(|key| key.to_str())
        .unwrap_or_else(|| panic!("the project key in {dir:?}"));
    let row = json!({"session_id": ID, "ts": 1, "project": key,
        "workspace": workspace, "name": "paging", "how": "exited"});
    fs::write(setup.home().join("recent.jsonl"), format!("{row}\n")).unwrap();
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [{"text": ["the scripted reply"]}]}),
    );
    let mut config = json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200}});
    for (key, value) in extra.as_object().unwrap() {
        config[key] = value.clone();
    }
    write_json(&setup.home().join("config.json"), &config);
}

/// Every `op_timing` line in the `tui-<pid>` diagnostic file, in file
/// order; none when the process wrote nothing there, as on the base,
/// where the terminal holds no timer.
fn op_lines(setup: &Setup, pid: u32) -> Vec<Value> {
    let path = setup.home().join("logs").join(format!("tui-{pid}.log"));
    if !path.exists() {
        return Vec::new();
    }
    fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|line: &Value| line["code"] == "op_timing")
        .collect()
}

fn ops(setup: &Setup, pid: u32, op: &str) -> Vec<Value> {
    op_lines(setup, pid)
        .into_iter()
        .filter(|line| line["data"]["op"] == op)
        .collect()
}

/// The wire promise on every terminal timer: the process identity, no
/// session (the terminal's file never changes on attach), exactly the
/// keys `op` and `ms` with an unsigned whole `ms`.
#[track_caller]
fn assert_terminal_wire(lines: &[Value], op: &str) {
    assert_eq!(lines.len(), 1, "one {op} line: {lines:?}");
    let line = &lines[0];
    assert_eq!(line["code"], "op_timing", "{line}");
    assert_eq!(line["level"], "debug", "{line}");
    assert_eq!(line["process"], "tui", "{line}");
    assert!(line.get("session_id").is_none(), "{line}");
    assert_eq!(line["message"], "An operation ended.", "{line}");
    let data = line["data"].as_object().expect("data is an object");
    let mut keys: Vec<&str> = data.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["ms", "op"], "{line}");
    assert!(line["data"]["ms"].as_u64().is_some(), "{line}");
    assert_eq!(line["data"]["op"], op, "{line}");
}

/// Attaches over a pty, waits for the seeded tail on the grid, and quits
/// to a zero exit: the waits key off the title, the seeded rows and the
/// restored primary screen, never a sleep.
fn attached_run(setup: &Setup, env: &[(&str, &str)]) -> (u32, Run) {
    let mut run = Run::spawn(setup, COLS, ROWS, &["resume", ID], env);
    let pid = run.id();
    // The first frame's title proves the input reader runs before the
    // quit goes out, and the seeded rows prove the tail read built the
    // preview they came from.
    run.ready();
    run.wait_screen("the last turn's rows", |grid| grid.contents.contains(LAST));
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    (pid, run)
}

#[test]
fn a_debug_resume_records_first_render_log_read_and_transcript_once_each() {
    let setup = Setup::new();
    seed(&setup, json!({"diagnostics": {"level": "debug"}}));
    let (pid, run) = attached_run(&setup, &[("FIBER_TEST_FAKE_KEY", "sk-test")]);
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    // No `tui.frame` assertion: a loaded machine can push a real frame
    // past 16 ms, so absence would flake; suppression is pinned in the
    // `log` crate and over-budget wiring in the `tui` crate.
    assert_terminal_wire(&ops(&setup, pid, "tui.first_render"), "tui.first_render");
    assert_terminal_wire(&ops(&setup, pid, "tui.log_read"), "tui.log_read");
    assert_terminal_wire(&ops(&setup, pid, "tui.transcript"), "tui.transcript");
    for line in op_lines(&setup, pid) {
        if line["process"] != "tui" {
            continue;
        }
        let data = line["data"].as_object().expect("data is an object");
        for key in data.keys() {
            assert!(
                ["ms", "op", "tool"].contains(&key.as_str()),
                "no field outside op, ms and tool: {line}"
            );
        }
    }
}

#[test]
fn at_default_no_debug_line_is_written() {
    let setup = Setup::new();
    seed(&setup, json!({}));
    let (pid, run) = attached_run(&setup, &[("FIBER_TEST_FAKE_KEY", "sk-test")]);
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    let path = setup.home().join("logs").join(format!("tui-{pid}.log"));
    if !path.exists() {
        return;
    }
    for line in fs::read_to_string(&path).unwrap().lines() {
        let line: Value = serde_json::from_str(line).unwrap();
        assert_ne!(line["level"], "debug", "the default stays silent: {line}");
    }
}
