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
        connect_hub(&self.setup, &self.hub).0.pumped()
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
        // The session's command line carries the physical path; on macOS the
        // temp directory is reached through the `/var` symlink.
        let workspace = self
            .setup
            .workspace()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
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

    /// `finish`, keeping a `wait_gone` failure's message and appending what
    /// the logs say about the session's exit: a lingering process means the
    /// session never reached its exit, so the next sighting needs the
    /// session log's started/exited lines and the hub log's tail. Only the
    /// workspace-exit wait runs inside the catch: the hub kill runs first,
    /// outside it, so its own failure stays its own.
    fn finish_with_exit_diagnostics(mut self) {
        let diagnostics = exit_wait_diagnostics(&self);
        let hub = self.hub.lock().unwrap().take();
        if let Some(hub) = hub {
            hub.kill_and_wait();
        }
        if let Some(guard) = self.guard.take() {
            let waited =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| guard.wait_gone()));
            if let Err(payload) = waited {
                panic!("{}", with_exit_diagnostics(payload, &diagnostics));
            }
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

/// One short line per socket line for failure output: the kind plus
/// `payload.command_id`, `payload.code`, and `seq` where present. Large
/// env/prompt payloads are what truncated the original `{lines:?}` dump, so
/// the summary carries only what the next sighting's triage needs.
fn summarize_lines(lines: &[Value]) -> String {
    lines
        .iter()
        .map(|line| {
            let mut summary = line["kind"].as_str().unwrap_or("?").to_owned();
            if let Some(command_id) = line["payload"]["command_id"].as_str() {
                summary.push_str(&format!(" command_id={command_id}"));
            }
            if let Some(code) = line["payload"]["code"].as_str() {
                summary.push_str(&format!(" code={code}"));
            }
            if let Some(seq) = line["seq"].as_u64() {
                summary.push_str(&format!(" seq={seq}"));
            }
            summary
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn line_summary_keeps_what_the_next_sighting_needs() {
    let lines = vec![
        json!({"kind": "command_rejected", "payload": {"command_id": "c_p", "code": "closing"}}),
        json!({"kind": "command_rejected", "payload": {"command_id": "c_p", "code": "indeterminate"}}),
        json!({"kind": "fiber_exited"}),
        json!({"kind": "command_accepted", "payload": {"command_id": "c_after"}, "seq": 7}),
        json!({"payload": {"command_id": "c_x"}}),
    ];
    assert_eq!(
        summarize_lines(&lines),
        "command_rejected command_id=c_p code=closing\n\
         command_rejected command_id=c_p code=indeterminate\n\
         fiber_exited\n\
         command_accepted command_id=c_after seq=7\n\
         ? command_id=c_x"
    );
    assert_eq!(
        summarize_lines(&lines).lines().count(),
        lines.len(),
        "one summary line per socket line"
    );
}

/// The session log's `fiber_started` and `fiber_exited` lines, one summary
/// line each: the started lines say which runs existed, the exited lines
/// say which reached their exit, and nothing else shapes that question.
fn summarize_exit_lines(lines: &[Value]) -> String {
    summarize_lines(
        &lines
            .iter()
            .filter(|line| line["kind"] == "fiber_started" || line["kind"] == "fiber_exited")
            .cloned()
            .collect::<Vec<_>>(),
    )
}

/// The context a workspace-exit wait failure carries: the session log's
/// started/exited lines plus the hub log's tail. A missing or corrupt log
/// contributes nothing, so building the context never hides the wait
/// failure it annotates.
fn exit_wait_diagnostics(world: &World) -> String {
    let path = support::session_dir(&world.setup, &world.id).join("events.jsonl");
    let text = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    format!(
        "session log started/exited lines:\n{}\nhub.log tail (last {} lines):\n{}",
        summarize_exit_lines(&lines),
        support::HUB_LOG_TAIL,
        support::hub_log_tail(&world.setup.hub_log())
    )
}

/// A caught `wait_gone` failure's message with the exit diagnostics
/// appended: the hook prints a panic once, at its site, so a resumed payload
/// would show the original message only, and the combined message must be
/// raised as a new panic. A panic without a message still carries the
/// diagnostics, so the wait failure is never silent about what the logs say.
fn with_exit_diagnostics(payload: Box<dyn std::any::Any + Send>, diagnostics: &str) -> String {
    match payload.downcast::<String>() {
        Ok(failure) => format!("{failure}\n{diagnostics}"),
        Err(_) => format!("<no message with the workspace-exit wait failure>\n{diagnostics}"),
    }
}

#[test]
fn exit_summary_keeps_only_started_and_exited_lines() {
    let lines = vec![
        json!({"kind": "fiber_started", "seq": 1}),
        json!({"kind": "turn_started", "seq": 2}),
        json!({"kind": "fiber_exited", "seq": 3}),
        json!({"kind": "rewound", "seq": 4}),
    ];
    let summary = summarize_exit_lines(&lines);
    for kept in ["fiber_started", "fiber_exited"] {
        assert!(
            summary.lines().any(|line| line.starts_with(kept)),
            "the summary keeps {kept}: {summary}"
        );
    }
    for dropped in ["turn_started", "rewound"] {
        assert!(
            !summary.contains(dropped),
            "the summary drops {dropped}: {summary}"
        );
    }
    assert_eq!(summarize_exit_lines(&[]), "");
}

#[test]
fn exit_diagnostics_keep_the_failure_message_and_append_the_context() {
    let payload =
        std::panic::catch_unwind(|| panic!("{}", "waited for the lingering process")).unwrap_err();
    let message = with_exit_diagnostics(payload, "session log started/exited lines:\n...");
    assert!(
        message.contains("waited for the lingering process"),
        "the failure message survives: {message}"
    );
    assert!(
        message.contains("session log started/exited lines:"),
        "the diagnostics follow it: {message}"
    );
}

#[test]
fn exit_diagnostics_carry_the_context_without_a_failure_message() {
    let payload = std::panic::catch_unwind(|| std::panic::panic_any(42_u32)).unwrap_err();
    let message = with_exit_diagnostics(payload, "session log started/exited lines:\n...");
    assert!(
        message.contains("session log started/exited lines:"),
        "the diagnostics survive: {message}"
    );
}

#[test]
fn finish_with_exit_diagnostics_without_a_hub_or_a_guard_is_silent() {
    let setup = Setup::new();
    let server = ProviderServer::start((0..0).map(|_| support::hello())).unwrap();
    let world = World {
        setup,
        id: "no such session".to_owned(),
        hub: Arc::new(Mutex::new(None)),
        guard: None,
        _server: server,
    };
    world.finish_with_exit_diagnostics();
}

#[test]
fn a_command_released_inside_the_close_grace_is_accepted_before_fiber_exited() {
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
    // The prompt runs its turn on the free connection before the close, so
    // no command straddles the quiesce reader stop: a line the reader reads
    // after the close is answered `closing` at once, on a path with no
    // ordering against the relayed answers, which no deterministic test can
    // park on (`docs/invocation.md`, "Lifecycle").
    second.send(&prompt("c_p", &id, "two"));
    let prompted = support::recv_answer(&second, "c_p", "the prompt's answer");
    assert_eq!(prompted["kind"], "command_accepted", "{prompted}");
    second.send(&close(&id));
    // The loop accepts the close before it quiesces: releasing now lets
    // the held import finish inside the quiesce grace, so the held run
    // answers it before `fiber_exited`.
    let closing = support::recv_answer(&second, "c_close", "the closing command");
    World::release(writer);
    // Every answer reaches its own connection exactly once
    // (`docs/invocation.md`, "What the hub speaks", "Commands keep their
    // order": every line a session reads gets exactly one answer on its
    // connection before the session shuts it).
    let mut second_lines = vec![prompted, closing];
    second_lines.extend(until(&second, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    }));
    // One answer for each command the session read (`docs/invocation.md`,
    // "What the hub speaks", "Commands keep their order").
    assert_eq!(answers(&second_lines, "c_p").len(), 1, "{second_lines:?}");
    assert_eq!(
        answers(&second_lines, "c_close").len(),
        1,
        "{second_lines:?}"
    );
    world.await_socket_absent();
    world.await_exit(&held);

    // `c_after` resumes the session, which is why three runs started.
    // The released answer streams before the exit line: both reach the
    // command's own connection in the order the session wrote them.
    let mut lines = until(&first, "c_q's answer", |line| is_answer(line, "c_q"));
    lines.extend(until(&first, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    }));
    first.send(&command("c_after", &id, "tools", &json!({})));
    lines.extend(until(&first, "c_after's acknowledgement", |line| {
        is_answer(line, "c_after")
    }));
    // One answer for the released command (`docs/invocation.md`, "What the
    // hub speaks", "Commands keep their order": every line a session reads
    // gets exactly one answer on its connection).
    assert_eq!(answers(&lines, "c_q").len(), 1, "{lines:?}");
    assert_eq!(answers(&lines, "c_q")[0]["kind"], "command_accepted");
    // The replay carries the setup run's own exit before the answer, so
    // the exit the assertion pins is the first one after it.
    let answered_at = position(&lines, "c_q");
    let exited_at = lines
        .iter()
        .enumerate()
        .find(|(at, line)| *at > answered_at && line["kind"] == "fiber_exited")
        .map(|(at, _)| at)
        .expect("fiber_exited streams after the released answer");
    // The session's answers come back in the order it sent them: the
    // released command is answered before `fiber_exited`
    // (`docs/invocation.md`, "What the hub speaks", "Commands keep their
    // order": a command the session answers when it ends is acknowledged
    // then, so the order holds for acknowledgements as the session sends
    // them).
    assert!(
        answered_at < exited_at,
        "the released command is answered before fiber_exited: {lines:?}"
    );
    let log = world.log();
    let started: Vec<_> = log
        .iter()
        .filter(|line| line["kind"] == "fiber_started")
        .collect();
    assert_eq!(
        started.len(),
        3,
        "the ask, the held run and the run c_after resumed"
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
    let pids = world.session_pids();
    assert_eq!(pids.len(), 1, "one session process, not {pids:?}");
    assert!(fakes::kill_pid(pids[0], "KILL").unwrap());
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
    world.finish_with_exit_diagnostics();
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
    // The loop accepts the close before it quiesces: releasing now lets
    // the held import finish inside the quiesce grace, so the resumed run
    // answers it before `fiber_exited`.
    support::recv_answer(&third, "c_close", "the closing command");
    World::release(writer);
    world.await_socket_absent();
    world.await_exit(&resumed);

    // Nothing after the last answer: `c_after` resumes a third run.
    first.send(&command("c_after", &id, "tools", &json!({})));
    let lines = until(&first, "c_after's acknowledgement", |line| {
        is_answer(line, "c_after")
    });
    assert_eq!(answers(&lines, "c_q").len(), 1, "{lines:?}");
    let prompted = answers(&lines, "c_p");
    assert_eq!(
        prompted.len(),
        1,
        "exactly one answer for the prompt: {lines:?}"
    );
    assert_eq!(
        prompted[0]["kind"],
        "command_rejected",
        "prompted[0] = {}\nlines:\n{}",
        prompted[0],
        summarize_lines(&lines)
    );
    assert_eq!(
        prompted[0]["payload"]["code"],
        "closing",
        "prompted[0] = {}\nlines:\n{}",
        prompted[0],
        summarize_lines(&lines)
    );
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
