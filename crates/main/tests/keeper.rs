//! Keeper parentage and crash recovery through the built binary.
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
    Deadline, KillGroup, Setup, Socket, complete_lines, recv_answer, run_to_exit, session_dir,
    spawn_watched, subscribe_direct, until_after, write_json,
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
        "fiber keeper acceptance invocation",
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
    thread::spawn(move || {
        let _ = send.send(child.wait());
    });
    deadline.start_wait().recv(&result).unwrap().unwrap()
}

#[test]
fn a_crash_leaves_the_job_to_the_keeper_and_the_resume_delivers_it() {
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
        "keeper-job-{}",
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
    let before = fs::read(dir.join("events.jsonl")).unwrap();
    let refused = run(&setup, &["ask", "--resume", id, "go"]);
    assert_eq!(refused.status.code(), Some(1));
    let refusal = complete_lines(&String::from_utf8(refused.stdout).unwrap());
    assert_eq!(refusal[0]["payload"]["error"]["code"], "session_held");
    assert!(
        refusal[0]["payload"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("keeper")
    );
    assert_eq!(fs::read(dir.join("events.jsonl")).unwrap(), before);
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
    assert_eq!(ends.len(), 1);
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
        "docs/tools.md: keeper appends the job's end while the session is gone"
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
    assert_eq!(notices.len(), 1, "{turn}");
    let text = notices[0].to_string();
    assert!(text.contains(&job) && text.contains("completed"));
    assert!(fakes::matching_exits(&marker, setup.deadline.start_wait()));
    keeper_watchdog.stand_down(setup.deadline.cleanup_phase());
    job_watchdog.stand_down(setup.deadline.cleanup_phase());
}

#[test]
fn a_session_with_no_command_starts_no_keeper() {
    let setup = configured();
    let id = "s_0123456789abcdef";
    let keeper_pattern = format!("keeper --id {id}");
    let keeper_watchdog = Watchdog::matching(&keeper_pattern);
    let workspace = setup.workspace();
    let mut command = setup.fiber(&[
        "session",
        "--id",
        id,
        "--workspace",
        workspace.to_str().unwrap(),
    ]);
    command.current_dir(&workspace);
    let (mut child, session_watchdog) = spawn_watched(&mut command);
    let group = child.id();
    let guard = KillGroup(group);
    let lines = stdout(&mut child);
    event(&lines, "fiber_started", setup.deadline);
    let client = Socket::connect(setup.deadline, &setup.session_socket(id));
    let sub = subscribe_direct(&client);
    client.send(r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hello"}]}}"#);
    assert_eq!(
        recv_answer(&client, "c_prompt", "prompt accepted")["kind"],
        "command_accepted"
    );
    let live = until_after(&client, &sub, "text-only turn completion", |line| {
        line["kind"] == "turn_completed"
    });
    assert!(live.iter().any(|line| line["kind"] == "turn_completed"));
    assert!(fakes::matching(&keeper_pattern).unwrap().is_empty());
    drop(Socket::connect(setup.deadline, &setup.session_socket(id)));
    client.send(r#"{"id":"c_close","command":"close"}"#);
    assert_eq!(
        recv_answer(&client, "c_close", "close accepted")["kind"],
        "command_accepted"
    );
    assert!(reap(child, setup.deadline).success());
    assert!(fakes::group_empties(group, setup.deadline.start_wait()));
    std::mem::forget(guard);
    session_watchdog.stand_down(setup.deadline.cleanup_phase());
    keeper_watchdog.stand_down(setup.deadline.cleanup_phase());
}
