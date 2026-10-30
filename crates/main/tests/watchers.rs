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
/// lines and stderr text. The exit wait gets its own bound, minted here,
/// so one slow step does not use up a later step's time.
fn run_ask(setup: &Setup, args: &[&str]) -> (Option<i32>, Vec<Value>, String) {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    let wait = setup.deadline.start_wait();
    let output = run_to_exit(wait, &format!("fiber {} to exit", args.join(" ")), command);
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

/// The project's sessions directory, through the canonical workspace,
/// as the project key names it.
fn sessions_dir(setup: &Setup) -> std::path::PathBuf {
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    let key = workspace.to_string_lossy().replace('/', "-");
    setup.home().join("projects").join(key).join("sessions")
}

/// The marker file's bytes, or its absence: a partially written watcher's
/// write shows here as fewer bytes than a full run wrote.
fn marker_state(marker: &std::path::Path) -> String {
    match fs::read(marker) {
        Ok(bytes) => format!("present {} bytes: {bytes:?}", bytes.len()),
        Err(error) => format!("absent ({error})"),
    }
}

/// The tail of this test's sole session `events.jsonl`
/// (`docs/extensions.md:1651-1657`, "When a session ends"): the isolated
/// `FIBER_HOME` holds one project and one session. A watcher timeout notice
/// lands in this log, which tells a timed-out parked run apart from a queued
/// delivery that never arrived; a missing directory or log is itself the
/// state found.
fn events_tail(setup: &Setup) -> String {
    const TAIL_LINES: usize = 20;
    let dir = sessions_dir(setup);
    let ids: Vec<String> = match fs::read_dir(&dir) {
        Ok(entries) => entries
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(error) => {
            return format!("sessions dir {} unreadable ({error})", dir.display());
        }
    };
    if ids.len() != 1 {
        return format!(
            "sessions dir {} holds {} session(s) [{}]",
            dir.display(),
            ids.len(),
            ids.join(", ")
        );
    }
    let path = dir.join(&ids[0]).join("events.jsonl");
    match fs::read_to_string(&path) {
        Ok(text) => {
            let lines: Vec<&str> = text.lines().collect();
            let start = lines.len().saturating_sub(TAIL_LINES);
            format!(
                "session {}: last {} of {} lines:\n{}",
                ids[0],
                lines.len() - start,
                lines.len(),
                lines[start..].join("\n")
            )
        }
        Err(error) => format!(
            "session {}: {} unreadable ({error})",
            ids[0],
            path.display()
        ),
    }
}

/// Held and model request counts, marker bytes and the session event-log
/// tail: every wait failure names the state it found, so the next sighting
/// tells a timed-out parked run apart from a missing queued delivery. The
/// held count is a snapshot taken before the held server is dropped; the
/// model server stays alive, so its count is read fresh.
fn diagnose(
    setup: &Setup,
    marker: &std::path::Path,
    held_requests: usize,
    model: &ProviderServer,
) -> String {
    format!(
        "held requests: {held_requests}, model requests: {}, marker {}: {}, {}",
        model.requests().len(),
        marker.display(),
        marker_state(marker),
        events_tail(setup)
    )
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
    // The second call answers at once. The turn stays alive on the held
    // third model response while the mid-session run is awaited, so the
    // session cannot end before every live run printed and the drain can
    // replay none.
    let unblock = "echo hi";
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
    // exactly once. This wait's bound is minted here; the held fake's own
    // limit outlasts it, so the fake's 500 never fires first.
    let park_wait = setup.deadline.start_wait();
    if !held.await_requests(1, park_wait) {
        panic!(
            "waited for the watcher to park mid-session; {}",
            diagnose(&setup, &marker, held.requests().len(), &model)
        );
    }
    // `docs/extensions.md:1651-1657` ("When a session ends"): the drain replays
    // each watcher's `(cursor, end]` range, one run per line, so one parked
    // run means one marker line. The read fails with the same diagnostic
    // payload the assertion carries, so a missing marker names the state.
    let parked = match fs::read_to_string(&marker) {
        Ok(text) => text,
        Err(error) => panic!(
            "the parked run's marker line was unreadable ({error}); {}",
            diagnose(&setup, &marker, held.requests().len(), &model)
        ),
    };
    assert_eq!(
        parked,
        "run\n",
        "the watcher parked exactly once; {}",
        diagnose(&setup, &marker, held.requests().len(), &model)
    );
    // Free exactly one held model response while the hold stays on, so the
    // third request stays held. Only the second request can be waiting: the
    // first was answered at once and the third is unsent until stream 2 is
    // consumed and call_2 returns (`docs/loop.md`, "One step").
    model.release_one();
    // The third model request proves stream 2 was consumed and call_2 ran
    // while the ask stays alive. Its bound is minted here.
    let model_wait = setup.deadline.start_wait();
    if !model.await_requests(3, model_wait) {
        panic!(
            "waited for the third model request after the model permit, proving stream 2 was consumed and call_2 ran while the ask stays alive; {}",
            diagnose(&setup, &marker, held.requests().len(), &model)
        );
    }
    // Free every held callback: if the first run is still parked it ends
    // here, and if it already timed out the second run already parked. Both
    // orders reach the held second request with two live marker lines,
    // because the ask cannot exit and the drain cannot run while the third
    // model request stays held.
    held.release();
    // The mid-session run's held request proves it executed live, not as a
    // drain replay. Its bound is minted here.
    let held_wait = setup.deadline.start_wait();
    if !held.await_requests(2, held_wait) {
        panic!(
            "waited for the mid-session run's held request while the third model response stays held; {}",
            diagnose(&setup, &marker, held.requests().len(), &model)
        );
    }
    // Two live runs wrote the marker on the extension thread before their
    // HTTP, so the acknowledged second request means two lines. The read
    // fails with the same diagnostic payload the assertion carries.
    let mid = match fs::read_to_string(&marker) {
        Ok(text) => text,
        Err(error) => panic!(
            "the mid-session marker was unreadable ({error}); {}",
            diagnose(&setup, &marker, held.requests().len(), &model)
        ),
    };
    assert_eq!(
        mid,
        "run\nrun\n",
        "the mid-session run executed live while the ask stayed alive; {}",
        diagnose(&setup, &marker, held.requests().len(), &model)
    );
    // Freeing the last held model response lets the turn end: the queued
    // offer executes once live while the turn waits on it, so both live
    // runs print before the exit and the drain replays neither.
    model.release();
    let held_requests_at_release = held.requests().len();
    drop(held);
    // The exit wait's bound is minted here, after the final release.
    let completion_wait = setup.deadline.start_wait();
    let output = match completion_wait.recv(&done_rx) {
        Ok(output) => output,
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "waited until the deadline for `fiber ask` to exit; {}",
            diagnose(&setup, &marker, held_requests_at_release, &model)
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!(
            "the sender for `fiber ask` to exit hung up before sending; {}",
            diagnose(&setup, &marker, held_requests_at_release, &model)
        ),
    };
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains("failed at session end"),
        "the drain printed nothing: {stderr:?}"
    );
    // Every live run executed; the drain replayed none
    // (`docs/extensions.md:1651-1657`, "When a session ends": one run per
    // watcher per line in `(cursor, end]`, so the two queued offers plus the
    // parked run mean three marker lines and no drain replay).
    let drained = match fs::read_to_string(&marker) {
        Ok(text) => text,
        Err(error) => panic!(
            "the final marker was unreadable ({error}); {}",
            diagnose(&setup, &marker, held_requests_at_release, &model)
        ),
    };
    assert_eq!(
        drained,
        "run\nrun\nrun\n",
        "every live run executed and the drain replayed none; {}",
        diagnose(&setup, &marker, held_requests_at_release, &model)
    );
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
