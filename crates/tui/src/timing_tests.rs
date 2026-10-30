//! The terminal's timing scopes (`docs/state.md`, "Diagnostic logs"): the
//! attach latches and the first-render, attach and frame scopes.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::time::{Duration, Instant, SystemTime};

use contract::clock::Clock;
use fakes::Deadline;
use fakes::clock::FakeClock;
use ratatui::backend::TestBackend;
use serde_json::{Value, json};

use super::tests::{DEADLINE, hello, launch, new_loop, open};
use crate::pty_watch::{watch, watched};

/// At `info` the terminal records nothing: a refused-connect run writes
/// no file under the home's `logs/`.
#[test]
fn an_info_level_run_writes_no_diagnostic_line() {
    let home = fakes::TempDir::new("tui-timing-info");
    let diag = Arc::new(log::diag::Diag::new(
        home.path(),
        log::diag::Process::Tui,
        log::diag::Level::Info,
        FakeClock::new(),
    ));
    let mut pair = open();
    let slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    // The watcher drains the first frame, so no write blocks.
    let frames = watch(&pair.main, vec![b"refused" as &[u8]]);
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("timing-run".to_owned())
        .spawn(move || {
            let code = super::run_with(
                slave,
                launch(),
                Box::new(|| Err(std::io::Error::other("refused"))),
                Box::new(|_| {}),
                FakeClock::new(),
                super::Reader::spawn,
                |builder, body| builder.spawn(body),
                diag,
            )
            .unwrap_or_else(|error| panic!("run: {error}"));
            done.send(code).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    watched(&frames, "the connect notice", &Deadline::after(DEADLINE));
    pair.main
        .write_all(&[0x03, 0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    pair.main
        .flush()
        .unwrap_or_else(|err| panic!("flush: {err}"));
    let code = Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for run to return: {err}"));
    assert_eq!(code, 0);
    assert!(
        !home.path().join("logs").exists(),
        "at info no diagnostic line is written"
    );
}

/// A clock that steps `step` on every monotonic read, so a scope held
/// across the frame's reads ends over budget without any sleep.
struct StepClock {
    base: Instant,
    step: Duration,
    reads: AtomicU64,
}

impl StepClock {
    fn step() -> Self {
        Self {
            base: FakeClock::new().now(),
            step: Duration::from_millis(20),
            reads: AtomicU64::new(0),
        }
    }
}

impl contract::clock::Clock for StepClock {
    fn now(&self) -> Instant {
        let read = self.reads.fetch_add(1, Ordering::SeqCst);
        self.base + self.step * u32::try_from(read).unwrap_or(u32::MAX)
    }

    fn wall(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }

    fn sleep(&self, _d: Duration) {}

    fn wait_until(&self, _until: Option<Instant>, _wait: &mut dyn FnMut(Option<Duration>)) {}

    fn subscribe(&self, _waker: Weak<dyn contract::clock::Wake>) {}
}

/// The process's diagnostic file under `home`.
fn log_file(home: &std::path::Path) -> std::path::PathBuf {
    home.join("logs")
        .join(format!("tui-{}.log", std::process::id()))
}

/// Every `op_timing` line in the process's diagnostic file.
fn op_lines(home: &std::path::Path) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(log_file(home))
        .unwrap_or_else(|err| panic!("read the diagnostic file: {err}"));
    text.lines()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|err| panic!("a line parses: {err}: {line}"))
        })
        .collect()
}

/// The wire promise on every terminal timer: the process identity, exactly
/// the keys `op` and `ms` with an unsigned whole `ms`, and no session.
#[track_caller]
fn assert_terminal_wire(lines: &[serde_json::Value], op: &str) {
    assert_eq!(lines.len(), 1, "one line: {lines:?}");
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

/// `clock` behind the trait object the writer takes.
fn dyn_clock(clock: &Arc<FakeClock>) -> Arc<dyn contract::clock::Clock> {
    let held: Arc<FakeClock> = Arc::clone(clock);
    held
}

/// A debug writer in `home` on `clock`.
fn debug_diag(
    home: &std::path::Path,
    clock: Arc<dyn contract::clock::Clock>,
) -> Arc<log::diag::Diag> {
    Arc::new(log::diag::Diag::new(
        home,
        log::diag::Process::Tui,
        log::diag::Level::Debug,
        clock,
    ))
}

/// The first render writes one `tui.first_render` line in the wire shape.
#[test]
fn first_render_writes_one_line_in_the_wire_shape() {
    let home = fakes::TempDir::new("tui-timing-first");
    let clock = FakeClock::new();
    let timing = crate::timing::Timing::new(debug_diag(home.path(), dyn_clock(&clock)));
    drop(timing.first_render());
    let lines = op_lines(home.path());
    assert_terminal_wire(&lines, "tui.first_render");
    assert_eq!(lines[0]["data"]["ms"], 0, "the frozen clock reads zero");
}

/// A discarded first render writes nothing: setup work never lands as a
/// frame that never drew.
#[test]
fn discarded_first_render_writes_nothing() {
    let home = fakes::TempDir::new("tui-timing-discard");
    let clock = FakeClock::new();
    let timing = crate::timing::Timing::new(debug_diag(home.path(), dyn_clock(&clock)));
    crate::timing::discard(timing.first_render());
    crate::timing::discard(None);
    assert!(
        !log_file(home.path()).exists(),
        "a discarded guard writes no line"
    );
}

/// The first attach's log read writes once: a second attach writes
/// nothing.
#[test]
fn log_read_writes_once_per_process() {
    let home = fakes::TempDir::new("tui-timing-log-read");
    let clock = FakeClock::new();
    let mut timing = crate::timing::Timing::new(debug_diag(home.path(), dyn_clock(&clock)));
    drop(timing.log_read());
    drop(timing.log_read());
    let lines = op_lines(home.path());
    assert_terminal_wire(&lines, "tui.log_read");
}

/// The first attach's transcript build writes once: a second attach
/// writes nothing.
#[test]
fn transcript_writes_once_per_process() {
    let home = fakes::TempDir::new("tui-timing-transcript");
    let clock = FakeClock::new();
    let mut timing = crate::timing::Timing::new(debug_diag(home.path(), dyn_clock(&clock)));
    drop(timing.transcript());
    drop(timing.transcript());
    let lines = op_lines(home.path());
    assert_terminal_wire(&lines, "tui.transcript");
}

/// The extension load writes one `tui.extensions` line in the wire shape.
#[test]
fn extensions_writes_one_line_in_the_wire_shape() {
    let home = fakes::TempDir::new("tui-timing-extensions");
    let clock = FakeClock::new();
    let timing = crate::timing::Timing::new(debug_diag(home.path(), dyn_clock(&clock)));
    drop(timing.extensions());
    let lines = op_lines(home.path());
    assert_terminal_wire(&lines, "tui.extensions");
}

/// The opening summary subscribe writes once: a second attach writes
/// nothing.
#[test]
fn attach_summary_writes_once_per_process() {
    let home = fakes::TempDir::new("tui-timing-attach-summary");
    let clock = FakeClock::new();
    let mut timing = crate::timing::Timing::new(debug_diag(home.path(), dyn_clock(&clock)));
    drop(timing.attach_summary());
    assert!(
        timing.attach_summary().is_none(),
        "a second attach takes no summary guard"
    );
    let lines = op_lines(home.path());
    assert_terminal_wire(&lines, "tui.attach_summary");
}

/// The pre-frame tail read writes once: the post-frame count takes no
/// guard, so a second tail phase writes nothing.
#[test]
fn tail_wait_writes_once_per_process() {
    let home = fakes::TempDir::new("tui-timing-tail-wait");
    let clock = FakeClock::new();
    let mut timing = crate::timing::Timing::new(debug_diag(home.path(), dyn_clock(&clock)));
    drop(timing.tail_wait());
    assert!(
        timing.tail_wait().is_none(),
        "a second tail phase takes no guard"
    );
    let lines = op_lines(home.path());
    assert_terminal_wire(&lines, "tui.tail_wait");
}

/// Each preview fold writes its own line: two answers fold twice.
#[test]
fn preview_fold_writes_one_line_per_fold() {
    let home = fakes::TempDir::new("tui-timing-preview-fold");
    let clock = FakeClock::new();
    let timing = crate::timing::Timing::new(debug_diag(home.path(), dyn_clock(&clock)));
    drop(timing.preview_fold());
    drop(timing.preview_fold());
    let lines = op_lines(home.path());
    assert_eq!(lines.len(), 2, "one line per fold: {lines:?}");
    for line in &lines {
        assert_terminal_wire(std::slice::from_ref(line), "tui.preview_fold");
    }
}

/// The pre-frame install writes once: a second install takes no guard,
/// so a second open writes nothing.
#[test]
fn tail_install_writes_once_per_process() {
    let home = fakes::TempDir::new("tui-timing-tail-install");
    let clock = FakeClock::new();
    let mut timing = crate::timing::Timing::new(debug_diag(home.path(), dyn_clock(&clock)));
    drop(timing.tail_install());
    assert!(
        timing.tail_install().is_none(),
        "a second install takes no guard"
    );
    let lines = op_lines(home.path());
    assert_terminal_wire(&lines, "tui.tail_install");
}

/// At `info` the attach spans record nothing: the extension load, the
/// summary, the tail wait, a fold and the install write no file.
#[test]
fn attach_spans_write_nothing_at_info() {
    let home = fakes::TempDir::new("tui-timing-attach-spans-info");
    let mut timing = crate::timing::Timing::info();
    crate::timing::discard(timing.extensions());
    crate::timing::discard(timing.attach_summary());
    crate::timing::discard(timing.tail_wait());
    crate::timing::discard(timing.preview_fold());
    crate::timing::discard(timing.tail_install());
    crate::timing::discard(None);
    assert!(
        !home.path().join("logs").exists(),
        "at info no attach span writes a line"
    );
}

/// A frame the scripted clock pushes over budget writes `tui.frame`.
#[test]
fn frame_over_budget_writes_through_the_loop() {
    let home = fakes::TempDir::new("tui-timing-frame");
    let clock: Arc<dyn contract::clock::Clock> = Arc::new(StepClock::step());
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.clock = Arc::clone(&clock);
    lp.timing = crate::timing::Timing::new(debug_diag(home.path(), Arc::clone(&clock)));
    let (_tx, rx) = mpsc::channel();
    assert_eq!(lp.frame(&rx), None);
    let lines = op_lines(home.path());
    assert_terminal_wire(&lines, "tui.frame");
    assert!(
        lines[0]["data"]["ms"].as_u64().is_some_and(|ms| ms >= 20),
        "the stepped clock ends over budget: {}",
        lines[0]
    );
}

/// A frame on a frozen clock writes nothing: the log stays small while
/// scrolling.
#[test]
fn frame_within_budget_writes_nothing() {
    let home = fakes::TempDir::new("tui-timing-frame-quiet");
    let clock = FakeClock::new();
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.clock = dyn_clock(&clock);
    lp.timing = crate::timing::Timing::new(debug_diag(home.path(), dyn_clock(&clock)));
    let (_tx, rx) = mpsc::channel();
    assert_eq!(lp.frame(&rx), None);
    assert!(
        !log_file(home.path()).exists(),
        "a frame within budget writes no line"
    );
}

/// The attached session: forty turns of short replies, tall enough to
/// page.
fn session_log(turns: u64) -> Vec<Value> {
    let mut lines = Vec::new();
    let mut seq = 0u64;
    let mut push = |kind: &str, payload: Value| {
        lines.push(json!({
            "kind": kind,
            "session_id": "s_aaaaaaaaaaaaaaaa",
            "ts": 0,
            "schema_version": contract::SCHEMA_VERSION,
            "turn_id": null,
            "action_id": null,
            "seq": seq,
            "payload": payload,
        }));
        seq += 1;
    };
    for turn in 0..turns {
        push(
            "turn_started",
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": format!("turn {turn}")}]}]}),
        );
        push("step_started", json!({}));
        push("assistant_message_started", json!({}));
        let text = if turn + 1 == turns {
            "the last reply holds quokkas".to_owned()
        } else {
            format!("reply {turn}")
        };
        push("text_completed", json!({"text": text}));
        push(
            "assistant_message_completed",
            json!({"outcome": "completed"}),
        );
        push("turn_completed", json!({"outcome": "completed"}));
        push("usage_recorded", json!({}));
    }
    lines
}

/// The log's window from `from_seq` to `to_seq`, as a `history` answer
/// holds it.
fn window(log: &[Value], from: u64, to: u64) -> Vec<Value> {
    log.iter()
        .filter(|line| {
            let seq = line["seq"].as_u64().unwrap_or(u64::MAX);
            seq >= from && seq <= to
        })
        .cloned()
        .collect()
}

/// The first fetched page times `tui.log_read` to completion, and its
/// successful load times `tui.transcript`, each once per process
/// (`docs/state.md`, "Diagnostic logs").
#[test]
fn first_page_fetch_times_log_read_and_successful_load_times_transcript() {
    let home = fakes::TempDir::new("tui-timing-page-in");
    let clock = FakeClock::new();
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.clock = dyn_clock(&clock);
    lp.timing = crate::timing::Timing::new(debug_diag(home.path(), dyn_clock(&clock)));
    lp.app.on_line(crate::link::Line::Hub(hello()));
    lp.app
        .attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let log = Arc::new(session_log(40));
    for value in log.iter() {
        lp.app
            .on_line(crate::link::Line::Session(contract::Envelope {
                kind: value["kind"].as_str().unwrap_or("").to_owned(),
                session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
                ts: 0,
                schema_version: contract::SCHEMA_VERSION,
                turn_id: None,
                action_id: None,
                seq: value["seq"].as_u64().map(contract::Seq),
                payload: value["payload"].as_object().cloned().unwrap_or_default(),
            }));
    }
    lp.app.jump(0);
    assert!(
        !lp.app.needs().is_empty(),
        "a page is missing before the first fetch"
    );
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    lp.hub.store(ours);
    let (tx, rx) = mpsc::channel();
    let worker_log = Arc::clone(&log);
    let (seen_tx, seen) = mpsc::channel();
    std::thread::Builder::new()
        .name("timing-page-in".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(theirs);
            let mut text = String::new();
            while reader.read_line(&mut text).is_ok_and(|read| read > 0) {
                let command: Value = serde_json::from_str(&text).unwrap_or_default();
                text.clear();
                if command["command"] != "history" {
                    continue;
                }
                let from = command["args"]["from_seq"].as_u64().unwrap_or(0);
                let to = command["args"]["to_seq"].as_u64().unwrap_or(u64::MAX);
                let id = command["id"].clone();
                let lines: Vec<contract::Envelope> = window(&worker_log, from, to)
                    .into_iter()
                    .map(|v| contract::Envelope {
                        kind: v["kind"].as_str().unwrap_or("").to_owned(),
                        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
                        ts: 0,
                        schema_version: contract::SCHEMA_VERSION,
                        turn_id: None,
                        action_id: None,
                        seq: v["seq"].as_u64().map(contract::Seq),
                        payload: v["payload"].as_object().cloned().unwrap_or_default(),
                    })
                    .collect();
                let payload = json!({"command_id": id, "result": {"lines": lines}});
                let envelope = contract::Envelope {
                    kind: "command_accepted".to_owned(),
                    session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
                    ts: 0,
                    schema_version: contract::SCHEMA_VERSION,
                    turn_id: None,
                    action_id: None,
                    seq: None,
                    payload: payload.as_object().cloned().unwrap_or_default(),
                };
                if tx
                    .send(crate::Input::Hub(crate::link::Line::Session(envelope)))
                    .is_err()
                {
                    break;
                }
            }
            seen_tx.send(()).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // `page_in` blocks in `recv`: it runs on a thread of its own and its
    // result arrives through one deadline, so a wait that never ends
    // fails here instead of hanging the suite (`docs/testing.md`,
    // "Waits and timeouts").
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("timing-page-in-fetch".to_owned())
        .spawn(move || {
            let mut lp = lp;
            lp.page_in(&rx);
            done.send(lp).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let lp = Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the fetch: {err}"));
    drop(lp);
    Deadline::after(DEADLINE)
        .recv(&seen)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the hub to see the hang-up: {err}"));
    let lines = op_lines(home.path());
    let log_read: Vec<Value> = lines
        .iter()
        .filter(|line| line["data"]["op"] == "tui.log_read")
        .cloned()
        .collect();
    assert_terminal_wire(&log_read, "tui.log_read");
    let transcript: Vec<Value> = lines
        .iter()
        .filter(|line| line["data"]["op"] == "tui.transcript")
        .cloned()
        .collect();
    assert_terminal_wire(&transcript, "tui.transcript");
}

/// A failed first fetch still times `tui.log_read`, but leaves the
/// `tui.transcript` latch unset for the next successful load: the test
/// asserts no transcript line after the failure, then a later successful
/// `app.load` of the failed page writes exactly one. Flipping the
/// success-only guard to take the latch on failure writes a transcript
/// line before that load and fails (`docs/state.md`, "Diagnostic logs").
#[test]
fn failed_first_fetch_times_log_read_but_leaves_transcript_unset() {
    let home = fakes::TempDir::new("tui-timing-page-in-failed");
    let clock = FakeClock::new();
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.clock = dyn_clock(&clock);
    lp.timing = crate::timing::Timing::new(debug_diag(home.path(), dyn_clock(&clock)));
    lp.app.on_line(crate::link::Line::Hub(hello()));
    lp.app
        .attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let failed_log = session_log(40);
    for value in failed_log.iter() {
        lp.app
            .on_line(crate::link::Line::Session(contract::Envelope {
                kind: value["kind"].as_str().unwrap_or("").to_owned(),
                session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
                ts: 0,
                schema_version: contract::SCHEMA_VERSION,
                turn_id: None,
                action_id: None,
                seq: value["seq"].as_u64().map(contract::Seq),
                payload: value["payload"].as_object().cloned().unwrap_or_default(),
            }));
    }
    lp.app.jump(0);
    assert!(
        !lp.app.needs().is_empty(),
        "a page is missing before the first fetch"
    );
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let failed_need = lp
        .app
        .needs()
        .into_iter()
        .next()
        .expect("a page is missing before the first fetch");
    lp.hub.store(ours);
    let (tx, rx) = mpsc::channel();
    let (seen_tx, seen) = mpsc::channel();
    std::thread::Builder::new()
        .name("timing-page-in-rejected".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(theirs);
            let mut text = String::new();
            while reader.read_line(&mut text).is_ok_and(|read| read > 0) {
                let command: Value = serde_json::from_str(&text).unwrap_or_default();
                text.clear();
                if command["command"] != "history" {
                    continue;
                }
                let id = command["id"].clone();
                let payload = json!({"command_id": id, "message": "injected failure"});
                let envelope = contract::Envelope {
                    kind: "command_rejected".to_owned(),
                    session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
                    ts: 0,
                    schema_version: contract::SCHEMA_VERSION,
                    turn_id: None,
                    action_id: None,
                    seq: None,
                    payload: payload.as_object().cloned().unwrap_or_default(),
                };
                if tx
                    .send(crate::Input::Hub(crate::link::Line::Session(envelope)))
                    .is_err()
                {
                    break;
                }
            }
            seen_tx.send(()).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // `page_in` blocks in `recv`: it runs on a thread of its own and its
    // result arrives through one deadline, so a wait that never ends
    // fails here instead of hanging the suite (`docs/testing.md`,
    // "Waits and timeouts").
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("timing-page-in-rejected-fetch".to_owned())
        .spawn(move || {
            let mut lp = lp;
            lp.page_in(&rx);
            done.send(lp).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let mut lp = Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the fetch: {err}"));
    // The failed fetch wrote its `tui.log_read` line to completion but
    // left `tui.transcript` unset.
    let lines = op_lines(home.path());
    let log_read: Vec<Value> = lines
        .iter()
        .filter(|line| line["data"]["op"] == "tui.log_read")
        .cloned()
        .collect();
    assert_terminal_wire(&log_read, "tui.log_read");
    let transcript: Vec<Value> = lines
        .iter()
        .filter(|line| line["data"]["op"] == "tui.transcript")
        .cloned()
        .collect();
    assert!(
        transcript.is_empty(),
        "a failed fetch leaves tui.transcript unset: {transcript:?}"
    );
    // A later successful load of the failed page takes the waiting latch
    // exactly as `page_in`'s success branch does, and writes one line.
    let (failed_session, failed_range) = failed_need;
    let folded: Vec<contract::Envelope> =
        window(&failed_log, failed_range.start().0, failed_range.end().0)
            .into_iter()
            .map(|value| contract::Envelope {
                kind: value["kind"].as_str().unwrap_or("").to_owned(),
                session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
                ts: 0,
                schema_version: contract::SCHEMA_VERSION,
                turn_id: None,
                action_id: None,
                seq: value["seq"].as_u64().map(contract::Seq),
                payload: value["payload"].as_object().cloned().unwrap_or_default(),
            })
            .collect();
    assert!(
        !folded.is_empty(),
        "the failed page holds lines for the later load"
    );
    let transcript_guard = lp.timing.transcript();
    lp.app.load(&failed_session, folded);
    drop(transcript_guard);
    drop(lp);
    Deadline::after(DEADLINE)
        .recv(&seen)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the hub to see the hang-up: {err}"));
    let lines = op_lines(home.path());
    let log_read: Vec<Value> = lines
        .iter()
        .filter(|line| line["data"]["op"] == "tui.log_read")
        .cloned()
        .collect();
    assert_terminal_wire(&log_read, "tui.log_read");
    let transcript: Vec<Value> = lines
        .iter()
        .filter(|line| line["data"]["op"] == "tui.transcript")
        .cloned()
        .collect();
    assert_terminal_wire(&transcript, "tui.transcript");
}

/// A debug-diag loop whose launch open sent its summary subscribe: the
/// hello went in. Returns the home (for its log), the loop, and the
/// subscribe's id, for the caller to accept.
fn subscribing_lp(name: &str) -> (fakes::TempDir, super::Loop<TestBackend>, String) {
    let home = fakes::TempDir::new(name);
    let clock = FakeClock::new();
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.clock = dyn_clock(&clock);
    lp.timing = crate::timing::Timing::new(debug_diag(home.path(), dyn_clock(&clock)));
    lp.app = crate::app::testkit::home_with(
        80,
        24,
        crate::home::Launch {
            location: crate::LaunchLocation::Local {
                workspace: std::path::PathBuf::from("/w"),
                project: "-w".to_owned(),
                git: false,
            },
            hover: true,
            version: "0.0.1".to_owned(),
            model: None,
            thinking: None,
            logo_glyph: "\u{237b}".to_owned(),
            keys: crate::KeysSetup::default(),
            rail_share: 15.0,
            panel_share: 21.0,
            panel_cards: Vec::new(),
            open_at: crate::OpenAt::Session(
                contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
                None,
            ),
            ..Default::default()
        },
    );
    // The link comes up: the launch open sends the summary subscribe.
    let lines = lp.app.on_line(crate::link::Line::Hub(hello()));
    let summary = lines
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|line| line["command"] == "subscribe")
        .unwrap_or_else(|| panic!("a summary subscribe goes out: {lines:?}"));
    let id = summary["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{summary}"))
        .to_owned();
    (home, lp, id)
}

/// Every line of `lines` is a terminal `op_timing` wire line for `op`.
fn assert_terminal_wires(lines: &[serde_json::Value], op: &str) {
    assert!(!lines.is_empty(), "at least one line for {op}");
    for line in lines {
        assert_terminal_wire(std::slice::from_ref(line), op);
    }
}

/// Accepts the launch open's summary subscribe, so the open reaches the
/// commands-due stage.
fn accept_summary(lp: &mut super::Loop<TestBackend>, id: &str) {
    let _ = lp
        .app
        .on_line(crate::link::Line::Session(contract::Envelope {
            kind: "command_accepted".to_owned(),
            session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: None,
            seq: None,
            payload: [("command_id".to_owned(), json!(id))].into_iter().collect(),
        }));
}

/// The opening summary subscribe is timed through its acceptance: the
/// poll starts the guard while the summary is in flight and drops it on
/// the frame after the acceptance folds, writing one
/// `tui.attach_summary` line. A repeated poll while the summary is in
/// flight starts no second guard.
#[test]
fn summary_span_covers_the_send_through_the_acceptance() {
    let (home, mut lp, id) = subscribing_lp("tui-timing-summary-span");
    assert!(lp.app.summary_in_flight(), "the summary is in flight");
    lp.poll_summary_span();
    assert!(
        lp.summary_span.is_some(),
        "the poll starts the guard while the summary is in flight"
    );
    // A repeated poll while the summary is in flight starts no second
    // guard: flipping the `is_none` check would drop the first guard
    // here and lose the send-to-acceptance span.
    lp.poll_summary_span();
    assert!(
        lp.summary_span.is_some(),
        "a repeated poll keeps the one guard"
    );
    // The acceptance folds: the commands line goes out and the open
    // leaves the summary stage.
    accept_summary(&mut lp, &id);
    assert!(
        !lp.app.summary_in_flight(),
        "the acceptance leaves the summary stage"
    );
    lp.poll_summary_span();
    assert!(
        lp.summary_span.is_none(),
        "the poll drops the guard after the acceptance"
    );
    let lines = op_lines(home.path());
    assert_terminal_wire(&lines, "tui.attach_summary");
}

/// A latched summary timer starts no second guard: the poll leaves the
/// span unset while a second summary is in flight, so a second open
/// writes no second line.
#[test]
fn summary_poll_starts_nothing_once_the_latch_is_set() {
    let (home, mut lp, _) = subscribing_lp("tui-timing-summary-latch");
    // The latch is set, as a first open left it: dropping the guard
    // writes that first open's line.
    crate::timing::discard(lp.timing.attach_summary());
    assert!(lp.app.summary_in_flight(), "the summary is in flight");
    lp.poll_summary_span();
    assert!(
        lp.summary_span.is_none(),
        "a latched timer starts no guard for a second open"
    );
    assert!(
        !log_file(home.path()).exists(),
        "a second open writes no second line"
    );
}

/// The open's pre-frame tail read is timed end to end: one
/// `tui.tail_wait` line spans the whole fetch, two `tui.preview_fold`
/// lines cover the initial seed fold and the answer's fold separately,
/// and one `tui.tail_install` line covers the install, which installs
/// the newest candidate without another fold (`docs/tui.md`, "History
/// and paging"). A `Phase::Count` read takes no `tail_wait` guard, so
/// swapping the arms leaves none here.
#[test]
fn open_tail_times_the_wait_and_every_fold() {
    let (home, mut lp, id) = subscribing_lp("tui-timing-open-tail");
    accept_summary(&mut lp, &id);
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    lp.hub.store(ours);
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("timing-open-tail-hub".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(theirs);
            let mut text = String::new();
            while reader.read_line(&mut text).is_ok_and(|read| read > 0) {
                let command: Value = serde_json::from_str(&text).unwrap_or_default();
                text.clear();
                if command["command"] != "history" {
                    continue;
                }
                let id = command["id"].clone();
                let payload = json!({"command_id": id, "result": {"lines": []}});
                let envelope = contract::Envelope {
                    kind: "command_accepted".to_owned(),
                    session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
                    ts: 0,
                    schema_version: contract::SCHEMA_VERSION,
                    turn_id: None,
                    action_id: None,
                    seq: None,
                    payload: payload.as_object().cloned().unwrap_or_default(),
                };
                if tx
                    .send(crate::Input::Hub(crate::link::Line::Session(envelope)))
                    .is_err()
                {
                    break;
                }
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // `open_tail` blocks in `recv`: it runs on a thread of its own and
    // the loop arrives through one deadline, so a wait that never ends
    // fails here instead of hanging the suite (`docs/testing.md`,
    // "Waits and timeouts").
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("timing-open-tail".to_owned())
        .spawn(move || {
            let mut lp = lp;
            lp.open_tail(&rx);
            done.send(lp).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let lp = Deadline::after(Duration::from_secs(10))
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the tail read: {err}"));
    drop(lp);
    let lines = op_lines(home.path());
    let waits: Vec<Value> = lines
        .iter()
        .filter(|line| line["data"]["op"] == "tui.tail_wait")
        .cloned()
        .collect();
    assert_terminal_wire(&waits, "tui.tail_wait");
    let folds: Vec<Value> = lines
        .iter()
        .filter(|line| line["data"]["op"] == "tui.preview_fold")
        .cloned()
        .collect();
    assert_eq!(
        folds.len(),
        2,
        "the initial seed fold and the answer's fold: {folds:?}"
    );
    assert_terminal_wires(&folds, "tui.preview_fold");
    let installs: Vec<Value> = lines
        .iter()
        .filter(|line| line["data"]["op"] == "tui.tail_install")
        .cloned()
        .collect();
    assert_eq!(
        installs.len(),
        1,
        "the install without another fold: {installs:?}"
    );
    assert_terminal_wires(&installs, "tui.tail_install");
}

/// A refused connect still draws its first frame, and only it.
#[test]
fn refused_connect_writes_only_first_render() {
    let home = fakes::TempDir::new("tui-timing-refused");
    let diag = debug_diag(home.path(), {
        let clock: Arc<dyn Clock> = FakeClock::new();
        clock
    });
    let mut pair = open();
    let slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let frames = watch(&pair.main, vec![b"\x1b]2;fiber\x07" as &[u8]]);
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("timing-refused".to_owned())
        .spawn(move || {
            let code = super::run_with(
                slave,
                launch(),
                Box::new(|| Err(std::io::Error::other("refused"))),
                Box::new(|_| {}),
                FakeClock::new(),
                super::Reader::spawn,
                |builder, body| builder.spawn(body),
                diag,
            )
            .unwrap_or_else(|error| panic!("run: {error}"));
            done.send(code).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    watched(&frames, "the connect notice", &Deadline::after(DEADLINE));
    pair.main
        .write_all(&[0x03, 0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    pair.main
        .flush()
        .unwrap_or_else(|err| panic!("flush: {err}"));
    let code = Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for run to return: {err}"));
    assert_eq!(code, 0);
    let lines = op_lines(home.path());
    assert_terminal_wire(&lines, "tui.first_render");
}
