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

use super::tests::{DEADLINE, hello, launch, new_loop, once, open};
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

/// Attaching through a real `run_with` writes one `tui.log_read` and one
/// `tui.transcript`; a second page fetched after scrolling writes neither
/// again.
#[ignore = "no page is fetched on attach: the tail preview lands resident, so page_in finds nothing to fetch and the latches never fire; design question on #1212"]
#[test]
fn attach_writes_one_log_read_and_transcript_and_scroll_writes_none() {
    let home = fakes::TempDir::new("tui-timing-attach");
    let diag = debug_diag(home.path(), {
        let clock: Arc<dyn Clock> = FakeClock::new();
        clock
    });
    let log = Arc::new(session_log(40));
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let (seen_tx, seen) = mpsc::channel();
    let (hub_done_tx, hub_done) = mpsc::channel();
    let worker_log = Arc::clone(&log);
    let worker = std::thread::Builder::new()
        .name("timing-hub".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(theirs);
            let mut writer = reader
                .get_mut()
                .try_clone()
                .unwrap_or_else(|err| panic!("clone: {err}"));
            let mut write = |value: Value| {
                let mut bytes =
                    serde_json::to_string(&value).unwrap_or_else(|err| panic!("encode: {err}"));
                bytes.push('\n');
                writer
                    .write_all(bytes.as_bytes())
                    .unwrap_or_else(|err| panic!("hub write: {err}"));
            };
            let hub_ack = |id: &Value, result: Value| {
                json!({"kind": "command_accepted", "ts": 0,
                    "schema_version": contract::SCHEMA_VERSION,
                    "payload": {"command_id": id, "result": result}})
            };
            let session_ack = |id: &Value, result: Value| {
                json!({"kind": "command_accepted",
                    "session_id": "s_aaaaaaaaaaaaaaaa", "ts": 0,
                    "schema_version": contract::SCHEMA_VERSION,
                    "turn_id": null, "action_id": null, "seq": null,
                    "payload": {"command_id": id, "result": result}})
            };
            let mut text = String::new();
            while reader.read_line(&mut text).is_ok_and(|read| read > 0) {
                let command: Value = serde_json::from_str(&text).unwrap_or_default();
                text.clear();
                let id = command["id"].clone();
                match command["command"].as_str() {
                    // The full raise carries no `last_seq`, as for a
                    // session with no durable lines: no pass opens, so
                    // the attach pages the preview in through `page_in`.
                    Some("subscribe") if command["args"]["level"] == "full" => {
                        write(session_ack(&id, json!({})));
                    }
                    Some("feed") => write(hub_ack(&id, json!({}))),
                    Some("recent") => write(hub_ack(&id, json!({"sessions": []}))),
                    Some("subscribe") => write(session_ack(&id, json!({}))),
                    Some("commands") => write(session_ack(&id, json!({"commands": []}))),
                    Some("history") => {
                        let from = command["args"]["from_seq"].as_u64().unwrap_or(0);
                        let to = command["args"]["to_seq"].as_u64().unwrap_or(u64::MAX);
                        write(session_ack(
                            &id,
                            json!({"lines": window(&worker_log, from, to)}),
                        ));
                    }
                    _ => write(hub_ack(&id, json!({}))),
                }
                seen_tx.send(command).unwrap_or(());
            }
            hub_done_tx.send(()).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let mut pair = open();
    let slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    // The watcher drains the terminal past the first frame's title, so no
    // write blocks. The preview's last reply precedes the title: both
    // markers prove the first frame drew the attached session.
    let frames = watch(&pair.main, vec![b"\x1b]2;fiber\x07" as &[u8]]);
    let (done, finished) = mpsc::channel();
    let mut launch = launch();
    launch.open_at =
        crate::OpenAt::Session(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()), None);
    std::thread::Builder::new()
        .name("timing-attach".to_owned())
        .spawn(move || {
            let code = super::run_with(
                slave,
                launch,
                once(ours, hello()),
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
    watched(&frames, "the first frame title", &Deadline::after(DEADLINE));
    // The first attach fetch starts after the full subscribe: the pass is
    // closed, so a scroll now lands on the loaded pages, never before the
    // attach.
    let wait = Deadline::after(DEADLINE);
    let mut full = false;
    let mut histories = 0u32;
    while histories < 1 {
        let command = wait.recv_or_fail(&seen, "the full subscribe and first history");
        if command["command"] == "subscribe" && command["args"]["level"] == "full" {
            full = true;
        }
        if full && command["command"] == "history" {
            histories += 1;
        }
    }
    // Ten screens up leaves the loaded last page: the next frame fetches
    // an older one. The key waits behind the fetch's answer or is stashed
    // inside it, so the scroll always lands after the first load.
    pair.main
        .write_all(b"\x1b[5~".repeat(10).as_slice())
        .unwrap_or_else(|err| panic!("write: {err}"));
    pair.main
        .flush()
        .unwrap_or_else(|err| panic!("flush: {err}"));
    let wait = Deadline::after(DEADLINE);
    while histories < 2 {
        let command = wait.recv_or_fail(&seen, "the second page's history");
        if command["command"] == "history" {
            histories += 1;
        }
    }
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
    Deadline::after(DEADLINE)
        .recv(&hub_done)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the hub thread: {err}"));
    worker.join().unwrap_or_else(|_| panic!("the hub thread"));
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
