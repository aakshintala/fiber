//! Binary-level tests of the session-end watcher drain (`docs/testing.md`,
//! "Levels"): the built `fiber` runs in its own process group with its own
//! `FIBER_HOME`, holding an ordinary provider whose base URL is the fake
//! server, and `fiber ask` runs to its exit under the test's deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod extension_harness;
mod support;

use std::fs;
use std::sync::mpsc;
use std::thread;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{Setup, function_call, hello, is_status, run_to_exit, stream, text_reply};

/// Runs `fiber ask` with `args` to its exit: its exit code, parsed stdout
/// lines and stderr text.
fn run_ask(setup: &Setup, args: &[&str]) -> (Option<i32>, Vec<Value>, String) {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    let output = run_to_exit(
        setup.deadline,
        &format!("fiber {} to exit", args.join(" ")),
        command,
    );
    let lines = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|line| !is_status(line))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (
        output.status.code(),
        lines,
        String::from_utf8(output.stderr).unwrap(),
    )
}

/// The session's log file, from the ask's first stdout line.
fn events_file(setup: &Setup, lines: &[Value]) -> std::path::PathBuf {
    let workspace = fs::canonicalize(setup.root.path().join("w")).unwrap();
    let key = workspace.to_string_lossy().replace('/', "-");
    setup
        .home()
        .join("projects")
        .join(key)
        .join("sessions")
        .join(lines[0]["session_id"].as_str().unwrap())
        .join("events.jsonl")
}

/// A failing session-end watcher writes its data-directory file and prints
/// under `fiber_exited`: the drain collects the failure for stderr in every
/// session instead of a notice, and seal-passing writes still land.
#[test]
fn a_failing_session_end_watcher_writes_its_data_directory_and_prints_under_fiber_exited() {
    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let sentinel = setup.workspace().join("sentinel");
    setup.lua(
        "watched",
        &format!(
            "fiber.watch({{\"fiber_exited\"}}, {{ timeout = 2000, run = function(ev) local dir = host.data_dir(\"machine\") host.fs.write(dir .. \"/drained\", \"drained:\" .. ev.kind) host.fs.write({sentinel:?}, dir) error(\"boom\") end }})"
        ),
    );
    let (code, _, stderr) = run_ask(&setup, &["ask", "hi"]);
    assert_eq!(code, Some(0));
    // Under `fiber_exited` the failing watcher is named with its failure.
    assert!(
        stderr.contains("fiber.test/watched")
            && stderr.contains("watcher:0")
            && stderr.contains("boom"),
        "{stderr:?}"
    );
    // Its seal-passing write landed in its machine data directory.
    let dir = fs::read_to_string(&sentinel).unwrap();
    assert_eq!(
        fs::read_to_string(std::path::Path::new(&dir).join("drained")).unwrap(),
        "drained:fiber_exited"
    );
}

/// A parked mid-session offer prints nothing and drains nothing: the drain
/// replays only what the mid-session offer did not already cover, and the
/// cursor the live offer recorded pins exactly that. The model's second
/// reply stays held until the watcher parks, so the session cannot exit
/// before the mid-session run happened.
#[test]
fn a_parked_mid_session_offer_prints_nothing_and_drains_nothing() {
    let setup = Setup::new_with_fm_root();
    let marker = setup.workspace().join("marker");
    // The second call blocks the turn on the marker: the session cannot end
    // until every live run printed, so the drain can replay none.
    let unblock = format!(
        "echo hi; for i in $(seq 1 200); do if [ \"$(wc -l < {:?})\" -ge 2 ]; then break; fi; sleep 0.2; done; echo done",
        marker
    );
    let model = ProviderServer::start([
        stream(&[
            json!({"type": "response.output_item.done", "item": {
                "type": "message", "content": [{"type": "output_text", "text": "one"}]}}),
            function_call("call_1", "shell", &json!({"command": "echo hi"})),
            json!({"type": "response.completed", "response": {
                "id": "resp_1", "status": "completed",
                "usage": {"input_tokens": 10, "output_tokens": 3}}}),
        ]),
        stream(&[
            json!({"type": "response.output_item.done", "item": {
                "type": "message", "content": [{"type": "output_text", "text": "two"}]}}),
            function_call("call_2", "shell", &json!({"command": unblock})),
            json!({"type": "response.completed", "response": {
                "id": "resp_2", "status": "completed",
                "usage": {"input_tokens": 10, "output_tokens": 3}}}),
        ]),
        text_reply("three"),
    ])
    .unwrap();
    model.hold_from(2, fakes::deadline::BUDGET);
    setup.provider(&model);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "echo hi"})
        ),
    )
    .unwrap();
    let held = ProviderServer::start([]).unwrap();
    held.hold();
    let held_url = held.url();
    setup.lua(
        "parked",
        &format!(
            "fiber.watch({{\"text_completed\"}}, {{ timeout = 2000, run = function(ev) local ok, old = pcall(host.fs.read, {marker:?}) if not ok then old = \"\" end host.fs.write({marker:?}, old .. \"run\\n\") host.http({{ url = \"{held_url}\" }}) end }})"
        ),
    );
    let deadline = setup.deadline;
    let mut command = setup.fiber(&["ask", "hi"]);
    command.current_dir(setup.workspace());
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let _ignored = done_tx.send(run_to_exit(deadline, "fiber ask to exit", command));
    });
    // The first run parked on the held server; the mid-session run happened
    // exactly once.
    assert!(
        held.await_requests(1, setup.deadline),
        "waited for the watcher to park mid-session"
    );
    assert_eq!(fs::read_to_string(&marker).unwrap(), "run\n");
    model.release();
    // Freeing the parked run lets the queued second offer execute while the
    // turn waits on it: whichever lands first, both live runs print before
    // the exit, so the drain replays neither.
    drop(held);
    let output = setup
        .deadline
        .recv(&done_rx)
        .expect("waited for `fiber ask` to exit");
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains("failed at session end"),
        "the drain printed nothing: {stderr:?}"
    );
    // Every live run executed; the drain replayed none.
    assert_eq!(fs::read_to_string(&marker).unwrap(), "run\nrun\nrun\n");
}

/// An ephemeral kind the turn never appends replays nothing post-exit: the
/// last line stays `fiber_exited`, and the watcher's run never happens.
#[test]
fn an_unappended_ephemeral_kind_replays_nothing_post_exit() {
    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let marker = setup.workspace().join("marker");
    setup.lua(
        "quiet",
        &format!(
            "fiber.watch({{\"tool_call_delta\"}}, {{ timeout = 2000, run = function(ev) host.fs.write({marker:?}, \"replayed\") end }})"
        ),
    );
    let (code, lines, stderr) = run_ask(&setup, &["ask", "hi"]);
    assert_eq!(code, Some(0), "{stderr:?}");
    assert!(!marker.exists(), "nothing replayed the unappended kind");
    let events = fs::read_to_string(events_file(&setup, &lines)).unwrap();
    let last: Value = serde_json::from_str(events.lines().last().unwrap()).unwrap();
    assert_eq!(last["kind"], "fiber_exited");
}
