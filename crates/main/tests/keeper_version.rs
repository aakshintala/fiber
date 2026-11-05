//! A resume against a keeper at an unreadable `schema_version` declines
//! with `session_held` naming the keeper's version, and succeeds once the
//! keeper exits (see #3892). A shell job only; no delegate tooling.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "binary test assertions"
)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader};
use std::process::Child;
use std::sync::mpsc;
use std::thread;

use fakes::Watchdog;
use serde_json::{Value, json};
use support::{
    Deadline, KillGroup, Setup, complete_lines, run_to_exit, session_dir, spawn_watched, write_json,
};

fn script(setup: &Setup, name: &str, steps: Value) {
    write_json(&setup.workspace().join(name), &json!({"steps":steps}));
}
fn configured() -> Setup {
    let setup = Setup::new();
    script(&setup, "s.json", json!([{"text":"Ready."}]));
    write_json(
        &setup.home().join("config.json"),
        &json!({"model":"scripted/s.json", "sandbox":{"enabled":false}}),
    );
    setup
}
fn run(setup: &Setup, args: &[&str]) -> std::process::Output {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    run_to_exit(
        setup.deadline.start_wait(),
        "fiber keeper version acceptance invocation",
        command,
    )
}
fn stdout(child: &mut Child) -> mpsc::Receiver<String> {
    let out = child.stdout.take().unwrap();
    let (send, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(out).lines() {
            if send.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    lines
}
fn event(lines: &mpsc::Receiver<String>, kind: &str, deadline: Deadline) -> Value {
    let wait = deadline.start_wait();
    loop {
        let text = wait
            .recv(lines)
            .unwrap_or_else(|error| panic!("waiting for live stdout {kind}: {error}"));
        let line: Value = serde_json::from_str(&text).unwrap();
        if line["kind"] == kind {
            return line;
        }
    }
}
fn reap(mut child: Child, deadline: Deadline) -> std::process::ExitStatus {
    let (send, result) = mpsc::channel();
    thread::spawn(move || match send.send(child.wait()) {
        Ok(()) | Err(_) => {}
    });
    deadline.start_wait().recv(&result).unwrap().unwrap()
}

/// Rewrites the latest complete `fiber_started` line's `schema_version`
/// to `contract::SCHEMA_VERSION + 1` with one equal-length `replacen`,
/// so the log is itself a mixed-version history `[v1, v2]` with the
/// keeper's version latest. Returns the keeper's version.
fn forge_latest_fiber_started(dir: &std::path::Path) -> u64 {
    let current = format!("\"schema_version\":{}", contract::SCHEMA_VERSION);
    let version = u64::from(contract::SCHEMA_VERSION) + 1;
    let forged = format!("\"schema_version\":{version}");
    assert_eq!(
        current.len(),
        forged.len(),
        "the forge keeps the file length"
    );
    let path = dir.join("events.jsonl");
    let before = fs::read_to_string(&path).unwrap();
    assert!(before.ends_with('\n'), "the log ends on a complete line");
    let mut lines: Vec<String> = before.lines().map(str::to_owned).collect();
    let at = lines
        .iter()
        .rposition(|line| {
            serde_json::from_str::<Value>(line).is_ok_and(|line| line["kind"] == "fiber_started")
        })
        .unwrap();
    let replaced = lines[at].replacen(&current, &forged, 1);
    assert_ne!(lines[at], replaced, "the forged line carries {current}");
    lines[at] = replaced;
    let after = lines.join("\n") + "\n";
    assert_eq!(after.len(), before.len(), "the forge changes no length");
    fs::write(&path, after).unwrap();
    version
}

#[test]
fn a_resume_against_a_newer_keeper_declines_naming_its_version_then_recovers() {
    let setup = configured();
    // Seed the id before the command so its keeper watchdog is armed first.
    let seed = run(&setup, &["ask", "seed"]);
    assert!(
        seed.status.success(),
        "{}",
        String::from_utf8_lossy(&seed.stderr)
    );
    let seed = complete_lines(&String::from_utf8(seed.stdout).unwrap());
    let id = seed[0]["session_id"].as_str().unwrap();
    let keeper_pattern = format!("keeper --id {id}");
    let keeper_watchdog = Watchdog::matching(&keeper_pattern);
    let marker = format!(
        "keeper-version-{}",
        setup.root.path().file_name().unwrap().to_str().unwrap()
    );
    let job_watchdog = Watchdog::matching(&marker);
    let ready = fakes::children::Ready::new(&setup.workspace());
    let gate = setup.workspace().join("release.fifo");
    let hold = setup.workspace().join("hold-session.fifo");
    fakes::fifo(&gate);
    fakes::fifo(&hold);
    let command = format!(
        "sh -c 'echo $$ > {}; cat {}; echo done-{marker}' {marker}",
        ready.path().display(),
        gate.display()
    );
    // The second foreground call holds the session open without a timing
    // sleep. Its start also fences the first call's completed adoption.
    let foreground = format!("sh -c 'cat {}' {marker}-tool", hold.display());
    script(
        &setup,
        "job.json",
        json!([
            {"tool_calls":[{"name":"shell","arguments":{"command":command,"run_in_background":true}}]},
            {"tool_calls":[{"name":"shell","arguments":{"command":foreground}}]}
        ]),
    );
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n{}\n",
            json!({"decision":"allow","tool":"shell","prefix":command}),
            json!({"decision":"allow","tool":"shell","prefix":foreground})
        ),
    )
    .unwrap();
    let mut ask = setup.fiber(&[
        "ask",
        "--resume",
        id,
        "--model",
        "scripted/job.json",
        "start the job",
    ]);
    ask.current_dir(setup.workspace());
    let (mut child, session_watchdog) = spawn_watched(&mut ask);
    let group = child.id();
    let guard = KillGroup(group);
    let lines = stdout(&mut child);
    let started = event(&lines, "job_started", setup.deadline);
    let job = started["payload"]["job_id"].as_str().unwrap().to_owned();
    event(&lines, "tool_call_started", setup.deadline);
    assert!(!ready.wait(setup.deadline.start_wait()).is_empty());
    fakes::kill_group(group, "KILL").unwrap();
    assert!(!reap(child, setup.deadline).success());
    assert!(fakes::group_empties(group, setup.deadline.start_wait()));
    std::mem::forget(guard);
    session_watchdog.stand_down(setup.deadline.cleanup_phase());

    // Observe the bind after the killed session has released its socket.
    // Do not subscribe: the keeper accepts and closes without protocol.
    let socket = setup.session_socket(id);
    fakes::wait_until(
        "keeper refuse-only socket to accept",
        setup.deadline.start_wait(),
        move || std::os::unix::net::UnixStream::connect(&socket).is_ok(),
    );
    let dir = session_dir(&setup, id);
    // The keeper was spawned from this build's binary, so its `fiber_started`
    // is the latest; forging that line stands in for a newer keeper.
    let keeper_version = forge_latest_fiber_started(&dir);
    let snapshot = fs::read(dir.join("events.jsonl")).unwrap();
    let refused = run(&setup, &["ask", "--resume", id, "go"]);
    assert_eq!(refused.status.code(), Some(1));
    let refusal = complete_lines(&String::from_utf8(refused.stdout).unwrap());
    assert_eq!(refusal[0]["payload"]["error"]["code"], "session_held");
    let message = refusal[0]["payload"]["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("runs schema version {keeper_version}")),
        "the refusal names the keeper's version: {message}"
    );
    assert_eq!(
        fs::read(dir.join("events.jsonl")).unwrap(),
        snapshot,
        "a refused resume writes nothing"
    );
    assert!(
        !fakes::matching(&keeper_pattern).unwrap().is_empty(),
        "the keeper is still alive after the refusal"
    );
    let release = gate.clone();
    support::bounded(
        setup.deadline.start_wait(),
        "releasing the keeper job",
        move || fs::write(release, b"").unwrap(),
    );
    assert!(fakes::matching_exits(
        &keeper_pattern,
        setup.deadline.start_wait()
    ));
    script(&setup, "done.json", json!([{"text":"Recovered."}]));
    let resumed = run(
        &setup,
        &["ask", "--resume", id, "--model", "scripted/done.json", "go"],
    );
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let lines = complete_lines(&fs::read_to_string(dir.join("events.jsonl")).unwrap());
    let ends: Vec<_> = lines
        .iter()
        .filter(|line| line["kind"] == "job_completed")
        .collect();
    assert_eq!(
        ends.len(),
        1,
        "docs/tools.md, \"The keeper\": the keeper reports each exit — one job, one end"
    );
    let end = &ends[0]["payload"];
    assert_eq!(end["job_id"], job);
    assert_eq!(end["status"], "completed");
    assert_eq!(end["process"]["exit_code"], 0);
    for field in ["output_tail", "notice", "changed_by"] {
        assert!(
            end.get(field).is_none(),
            "keeper must not set {field}: {end}"
        );
    }
    assert!(
        !lines.iter().any(|line| line["kind"] == "job_completed"
            && line["payload"]["error"]["code"] == "orphaned")
    );
    let ended_at = lines
        .iter()
        .position(|line| line["kind"] == "job_completed")
        .unwrap();
    let resumed_at = lines
        .iter()
        .rposition(|line| line["kind"] == "fiber_started")
        .unwrap();
    assert!(
        ended_at < resumed_at,
        "docs/tools.md, \"The keeper\": keeper appends the job's end while the session is gone"
    );
    let output = started["payload"]["output_path"].as_str().unwrap();
    assert!(
        fs::read_to_string(dir.join(output))
            .unwrap()
            .contains(&format!("done-{marker}"))
    );
    let turn = lines
        .iter()
        .skip(resumed_at)
        .find(|line| line["kind"] == "turn_started")
        .unwrap();
    let notices: Vec<_> = turn["payload"]["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["source"] == "fiber")
        .collect();
    assert_eq!(
        notices.len(),
        1,
        "docs/tools.md, \"The keeper\": one turn_started message item holds the notices, {turn}"
    );
    let text = notices[0].to_string();
    assert!(text.contains(&job) && text.contains("completed"));
    assert!(fakes::matching_exits(&marker, setup.deadline.start_wait()));
    keeper_watchdog.stand_down(setup.deadline.cleanup_phase());
    job_watchdog.stand_down(setup.deadline.cleanup_phase());
}
