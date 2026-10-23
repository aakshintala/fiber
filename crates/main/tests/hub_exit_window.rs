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

    /// Waits for every process holding the workspace path to exit.
    fn await_session_exit(&self) {
        let workspace = self.setup.workspace().to_string_lossy().into_owned();
        fakes::try_matching_exits(&workspace, self.setup.deadline).unwrap_or_else(|err| {
            panic!("waited until the deadline for the session process to exit: {err}")
        });
    }

    /// The durable lines of the session's log.
    fn log(&self) -> Vec<Value> {
        let text =
            fs::read_to_string(support::session_dir(&self.setup, &self.id).join("events.jsonl"))
                .unwrap();
        support::complete_lines(&text)
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
    first.send(&prompt("c_p", &id, "two"));

    let second = world.client();
    subscribe(&second, "c_sub2", &id);
    second.send(&close(&id));
    until(&second, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
    world.await_socket_absent();
    World::release(writer);
    world.await_session_exit();

    first.send(&command("c_after", &id, "tools", &json!({})));
    let lines = until(&first, "c_after's acknowledgement", |line| {
        is_answer(line, "c_after")
    });
    assert_eq!(answers(&lines, "c_q").len(), 1, "{lines:?}");
    assert_eq!(answers(&lines, "c_q")[0]["kind"], "command_accepted");
    let prompted = answers(&lines, "c_p");
    assert_eq!(prompted.len(), 1, "one answer for the prompt: {lines:?}");
    assert_eq!(prompted[0]["kind"], "command_accepted", "{lines:?}");
    assert!(
        position(&lines, "c_q") < position(&lines, "c_p")
            && position(&lines, "c_p") < position(&lines, "c_after"),
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
    drop(first);
    world.finish();
}
