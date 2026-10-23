//! Binary-level tests of a command sent into a session's exit window
//! (`docs/invocation.md`, "Lifecycle" and "What the hub speaks"): the
//! built `fiber` runs `hub serve`, and a session is held between
//! `fiber_exited` and its shutdown by an `import` whose receipt is a FIFO
//! the test holds (`docs/testing.md`, "Waits and timeouts"). The hub
//! answers every command the session left unanswered.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{HubProc, SessionGuard, Setup, Socket, bounded, connect_hub, run_to_exit, until};

/// A session that has run one turn and exited, and a hub that resumes it.
struct World {
    setup: Setup,
    id: String,
    hub: Arc<Mutex<Option<HubProc>>>,
    guard: Option<SessionGuard>,
    // Held so the provider outlives the test.
    _server: ProviderServer,
}

impl World {
    /// Runs `fiber ask one` to an exit, with the provider answering `turns`
    /// requests.
    fn new(turns: usize) -> Self {
        let setup = Setup::new();
        let server = ProviderServer::start((0..turns).map(|_| support::hello())).unwrap();
        setup.provider(&server);
        let mut ask = setup.fiber(&["ask", "one"]);
        ask.current_dir(setup.workspace());
        let output = run_to_exit(setup.deadline, "`fiber ask` to exit", ask);
        assert!(output.status.success(), "{output:?}");
        let first = String::from_utf8(output.stdout).unwrap();
        let first: Value = serde_json::from_str(first.lines().next().unwrap()).unwrap();
        let id = first["session_id"].as_str().unwrap().to_owned();
        let workspace = setup.workspace().to_string_lossy().into_owned();
        let guard = SessionGuard::arm(setup.deadline, &workspace);
        Self {
            setup,
            id,
            hub: Arc::new(Mutex::new(None)),
            guard: Some(guard),
            _server: server,
        }
    }

    fn client(&self) -> Socket {
        connect_hub(&self.setup, &self.hub).0
    }

    /// The path of the receipt `import` reads first.
    fn receipt(&self) -> PathBuf {
        self.setup.home().join("import-codex.json")
    }

    /// Makes the receipt a FIFO: an `import` holds its reader there.
    fn hold_imports(&self) {
        fakes::fifo(self.receipt());
    }

    /// Waits for an `import` to open the receipt, which holds its reader
    /// inside the command, and returns the write end that releases it.
    fn await_import(&self) -> File {
        let path = self.receipt();
        bounded(
            self.setup.deadline,
            "an import to open its receipt",
            move || OpenOptions::new().write(true).open(path).unwrap(),
        )
    }

    /// Waits until the session removes its socket from `run/`: the signal
    /// that its readers are stopped, so releasing the FIFO lands after
    /// the close removed the socket.
    fn await_socket_absent(&self) {
        let socket = self.setup.home().join("run").join(&self.id);
        while socket.exists() {
            if self.setup.deadline.left().is_zero() {
                panic!("waited until the deadline for the session to remove its socket");
            }
            std::thread::yield_now();
        }
    }

    /// Releases the held `import`: the receipt it reads is complete, so it
    /// answers with the stored report and writes nothing.
    fn release(writer: File) {
        let mut writer = writer;
        writer
            .write_all(
                json!({"source": "codex", "imported_at": 0})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
    }

    /// The pids of the session processes running in the workspace now.
    fn session_pids(&self) -> Vec<u32> {
        let workspace = self.setup.workspace().to_string_lossy().into_owned();
        fakes::matching(&format!("--workspace={workspace}")).unwrap()
    }

    /// Waits for `pids` to exit. Not every process in the workspace: the
    /// run a re-route resumes stays up.
    fn await_exit(&self, pids: &[u32]) {
        assert!(
            fakes::pids_exit(pids, self.setup.deadline),
            "waited until the deadline for the session process {pids:?} to exit"
        );
    }

    /// The session's diagnostic log, one parsed line each; empty when the
    /// session wrote none.
    fn diagnostics(&self) -> Vec<Value> {
        let path = self
            .setup
            .home()
            .join("logs")
            .join(format!("session-{}.log", self.id));
        fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// The durable lines of the session's log.
    fn log(&self) -> Vec<Value> {
        let text =
            fs::read_to_string(support::session_dir(&self.setup, &self.id).join("events.jsonl"))
                .unwrap();
        support::complete_lines(&text)
    }

    /// Closes the session if it runs, as the last step of a test: a session
    /// the hub started outlives the hub, and exits on its own only when
    /// idle for `session.idle_exit_ms`.
    fn end_session(&self) {
        let last = self.client();
        subscribe(&last, "c_sub_end", &self.id);
        last.send(&close(&self.id));
        until(&last, "fiber_exited", |line| line["kind"] == "fiber_exited");
    }

    fn finish(mut self) {
        let hub = self.hub.lock().unwrap().take();
        if let Some(hub) = hub {
            hub.kill_and_wait();
        }
        if let Some(guard) = self.guard.take() {
            guard.wait_gone();
        }
    }
}

impl Drop for World {
    /// A test that ends early, by a failed assertion, leaves no session
    /// behind: the hub is killed first, so it starts no more, and then the
    /// guard (dropped after this, as the next field) kills the sessions in
    /// the workspace. A test that reached `finish` has taken both.
    fn drop(&mut self) {
        let hub = self.hub.lock().ok().and_then(|mut hub| hub.take());
        if let Some(hub) = hub {
            // A panic here, while a test's own panic unwinds, would abort.
            let killed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                hub.kill_and_wait();
            }));
            drop(killed);
        }
    }
}

fn command(id: &str, session: &str, command: &str, args: &Value) -> String {
    json!({"id": id, "session_id": session, "command": command, "args": args}).to_string()
}

fn subscribe(client: &Socket, id: &str, session: &str) {
    client.send(&command(
        id,
        session,
        "subscribe",
        &json!({"level": "full"}),
    ));
    let ack = support::recv_answer(client, id, "the subscribe acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
}

fn import(id: &str, session: &str) -> String {
    command(id, session, "import", &json!({"source": "codex"}))
}

fn prompt(id: &str, session: &str, text: &str) -> String {
    command(
        id,
        session,
        "prompt",
        &json!({"content": [{"type": "text", "text": text}]}),
    )
}

fn close(session: &str) -> String {
    json!({"id": "c_close", "session_id": session, "command": "close"}).to_string()
}

fn is_answer(line: &Value, id: &str) -> bool {
    matches!(
        line["kind"].as_str(),
        Some("command_accepted" | "command_rejected")
    ) && line["payload"]["command_id"] == id
}

/// The lines that answer command `id`.
fn answers<'a>(lines: &'a [Value], id: &str) -> Vec<&'a Value> {
    lines.iter().filter(|line| is_answer(line, id)).collect()
}

fn position(lines: &[Value], id: &str) -> usize {
    lines
        .iter()
        .position(|line| is_answer(line, id))
        .unwrap_or_else(|| panic!("no answer for {id}; got {lines:?}"))
}

#[test]
fn a_command_sent_into_the_exited_window_is_accepted_by_the_resumed_run() {
    let world = World::new(2);
    let id = world.id.clone();
    let first = world.client();
    subscribe(&first, "c_sub", &id);
    world.hold_imports();
    first.send(&import("c_q", &id));
    let writer = world.await_import();
    let held = world.session_pids();
    first.send(&prompt("c_p", &id, "two"));

    let second = world.client();
    subscribe(&second, "c_sub2", &id);
    second.send(&close(&id));
    until(&second, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
    world.await_socket_absent();
    World::release(writer);
    world.await_exit(&held);

    // The resumed run answers `c_p` when its loop takes the prompt, which a
    // later `tools` answer on the same connection does not wait for: the
    // test waits for `c_p`'s own answer, then for a barrier after it.
    let mut lines = until(&first, "c_p's answer", |line| is_answer(line, "c_p"));
    first.send(&command("c_after", &id, "tools", &json!({})));
    lines.extend(until(&first, "c_after's acknowledgement", |line| {
        is_answer(line, "c_after")
    }));
    assert_eq!(answers(&lines, "c_q").len(), 1, "{lines:?}");
    assert_eq!(answers(&lines, "c_q")[0]["kind"], "command_accepted");
    let prompted = answers(&lines, "c_p");
    assert_eq!(prompted.len(), 1, "one answer for the prompt: {lines:?}");
    assert_eq!(prompted[0]["kind"], "command_accepted", "{lines:?}");
    assert!(
        position(&lines, "c_q") < position(&lines, "c_p"),
        "answers keep the order the commands were sent in: {lines:?}"
    );
    let log = world.log();
    let started: Vec<_> = log
        .iter()
        .filter(|line| line["kind"] == "fiber_started")
        .collect();
    assert_eq!(
        started.len(),
        3,
        "the ask, the held run and the resumed run"
    );
    assert_eq!(started[2]["payload"]["resumed"], true);
    world.end_session();
    drop((first, second));
    world.finish();
}

#[test]
fn a_crash_with_a_sent_unanswered_command_rejects_it_indeterminate_and_never_runs_it() {
    let world = World::new(2);
    let id = world.id.clone();
    let first = world.client();
    subscribe(&first, "c_sub", &id);
    world.hold_imports();
    first.send(&import("c_q", &id));
    let writer = world.await_import();
    first.send(&prompt("c_p", &id, "two"));

    // Crash the session mid-dispatch: the log ends in `turn_completed`,
    // neither `fiber_exited` nor `rewound`.
    let workspace = world.setup.workspace().to_string_lossy().into_owned();
    let pids = fakes::matching(&format!("--workspace={workspace}")).unwrap();
    assert_eq!(pids.len(), 1, "one session process, not {pids:?}");
    assert!(support::kill_pid(world.setup.deadline, pids[0], "KILL").unwrap());
    drop(writer);

    first.send(&command("c_after", &id, "tools", &json!({})));
    let lines = until(&first, "c_after's acknowledgement", |line| {
        is_answer(line, "c_after")
    });
    for held in ["c_q", "c_p"] {
        let held_answers = answers(&lines, held);
        assert_eq!(held_answers.len(), 1, "one answer for {held}: {lines:?}");
        assert_eq!(held_answers[0]["kind"], "command_rejected", "{lines:?}");
        assert_eq!(
            held_answers[0]["payload"]["code"], "indeterminate",
            "{lines:?}"
        );
        assert!(
            position(&lines, held) < position(&lines, "c_after"),
            "the rejection comes before c_after's acknowledgement: {lines:?}"
        );
    }
    assert!(
        answers(&lines, "c_p")
            .iter()
            .all(|line| line["kind"] != "command_accepted"),
        "the crashed prompt never runs: {lines:?}"
    );
    let log = world.log();
    let turns = log
        .iter()
        .filter(|line| line["kind"] == "turn_started")
        .count();
    assert_eq!(turns, 1, "the crashed prompt runs no turn: {log:?}");
    world.end_session();
    drop(first);
    world.finish();
}

/// The log's `fiber_started` lines: one for each process that ran.
fn started(log: &[Value]) -> usize {
    log.iter()
        .filter(|line| line["kind"] == "fiber_started")
        .count()
}

fn turns(log: &[Value]) -> usize {
    log.iter()
        .filter(|line| line["kind"] == "turn_started")
        .count()
}

#[test]
fn a_command_rerouted_once_is_rejected_closing_when_the_resumed_run_also_ends() {
    let world = World::new(3);
    let id = world.id.clone();
    let first = world.client();
    subscribe(&first, "c_sub", &id);
    world.hold_imports();
    first.send(&import("c_q", &id));
    let writer = world.await_import();
    let held = world.session_pids();
    // Neither is read: the session's reader is inside `c_q`.
    first.send(&import("c_q2", &id));
    first.send(&prompt("c_p", &id, "two"));

    let second = world.client();
    subscribe(&second, "c_sub2", &id);
    second.send(&close(&id));
    until(&second, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
    world.await_socket_absent();
    World::release(writer);
    world.await_exit(&held);

    // The re-route resumes a second run, whose reader blocks in `c_q2`.
    let writer = world.await_import();
    let resumed = world.session_pids();
    assert_eq!(resumed.len(), 1, "one resumed run: {resumed:?}");
    let third = world.client();
    subscribe(&third, "c_sub3", &id);
    third.send(&close(&id));
    world.await_socket_absent();
    World::release(writer);
    world.await_exit(&resumed);

    // Nothing after the last answer: `c_after` resumes a third run.
    first.send(&command("c_after", &id, "tools", &json!({})));
    let lines = until(&first, "c_after's acknowledgement", |line| {
        is_answer(line, "c_after")
    });
    assert_eq!(answers(&lines, "c_q").len(), 1, "{lines:?}");
    assert_eq!(answers(&lines, "c_q")[0]["kind"], "command_accepted");
    assert_eq!(answers(&lines, "c_q2").len(), 1, "{lines:?}");
    assert_eq!(answers(&lines, "c_q2")[0]["kind"], "command_accepted");
    let prompted = answers(&lines, "c_p");
    assert_eq!(
        prompted.len(),
        1,
        "exactly one answer for the prompt: {lines:?}"
    );
    assert_eq!(prompted[0]["kind"], "command_rejected", "{lines:?}");
    assert_eq!(prompted[0]["payload"]["code"], "closing", "{lines:?}");
    assert!(
        position(&lines, "c_q") < position(&lines, "c_q2")
            && position(&lines, "c_q2") < position(&lines, "c_p"),
        "answers keep the order the commands were sent in: {lines:?}"
    );
    let log = world.log();
    assert_eq!(turns(&log), 1, "the prompt never ran: {log:?}");
    assert_eq!(
        started(&log),
        4,
        "the ask, the held run, the re-routed run, and c_after's run"
    );
    world.end_session();
    drop((first, second, third));
    world.finish();
}

#[test]
fn a_client_that_disconnects_in_the_window_has_nothing_rerouted() {
    let world = World::new(2);
    let id = world.id.clone();
    let first = world.client();
    subscribe(&first, "c_sub", &id);
    world.hold_imports();
    first.send(&import("c_q", &id));
    let writer = world.await_import();
    let held = world.session_pids();
    first.send(&prompt("c_p", &id, "two"));
    drop(first);

    let second = world.client();
    subscribe(&second, "c_sub2", &id);
    second.send(&close(&id));
    until(&second, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
    world.await_socket_absent();
    World::release(writer);
    world.await_exit(&held);

    // A later subscribe resumes the session: it finds no turn waiting.
    let third = world.client();
    subscribe(&third, "c_sub3", &id);
    third.send(&close(&id));
    until(&third, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
    let log = world.log();
    assert_eq!(turns(&log), 1, "the dropped prompt ran no turn: {log:?}");
    assert_eq!(
        started(&log),
        3,
        "the ask, the held run and the run the later subscribe resumed"
    );
    drop((second, third));
    world.finish();
}

#[test]
fn a_command_held_past_the_close_grace_is_rejected_indeterminate_once_and_not_rerouted() {
    let world = World::new(2);
    let id = world.id.clone();
    let first = world.client();
    subscribe(&first, "c_sub", &id);
    world.hold_imports();
    first.send(&import("c_q", &id));
    let writer = world.await_import();
    let held = world.session_pids();

    let second = world.client();
    subscribe(&second, "c_sub2", &id);
    second.send(&close(&id));
    // Never released: the grace passes with the command still running.
    let lines = until(&first, "the cut answer for c_q", |line| {
        is_answer(line, "c_q")
    });
    let cut = answers(&lines, "c_q");
    assert_eq!(cut.len(), 1, "{lines:?}");
    assert_eq!(cut[0]["kind"], "command_rejected", "{lines:?}");
    assert_eq!(cut[0]["payload"]["code"], "indeterminate", "{lines:?}");
    // The reader is still inside the command, and the process ends anyway.
    world.await_exit(&held);
    drop(writer);

    first.send(&command("c_after", &id, "tools", &json!({})));
    let after = until(&first, "c_after's acknowledgement", |line| {
        is_answer(line, "c_after")
    });
    assert!(
        answers(&after, "c_q").is_empty(),
        "no second answer for the command before c_after's: {after:?}"
    );
    let written = world.diagnostics();
    assert_eq!(written.len(), 1, "{written:?}");
    assert_eq!(written[0]["level"], "warn", "{written:?}");
    assert_eq!(written[0]["code"], "timeout", "{written:?}");
    let log = world.log();
    assert_eq!(
        started(&log),
        3,
        "the ask, the held run and the run c_after resumed: {log:?}"
    );
    world.end_session();
    drop((first, second));
    world.finish();
}

#[test]
fn a_test_that_ends_early_leaves_no_session_running() {
    let world = World::new(2);
    let id = world.id.clone();
    let first = world.client();
    subscribe(&first, "c_sub", &id);
    world.hold_imports();
    first.send(&import("c_q", &id));
    let writer = world.await_import();
    let running = world.session_pids();
    assert!(!running.is_empty(), "the held run is a session process");
    let deadline = world.setup.deadline;
    // Dropped as a failed assertion would drop it, with the import held.
    drop(world);
    assert!(
        fakes::pids_exit(&running, deadline),
        "waited until the deadline for the session process {running:?} to exit after the drop"
    );
    drop((first, writer));
}
