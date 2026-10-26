//! The terminal's timing scopes (`docs/state.md`, "Diagnostic logs"): the
//! attach latches and the first-render, attach and frame scopes.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::time::{Duration, Instant, SystemTime};

use contract::clock::Clock;
use fakes::Deadline;
use fakes::clock::FakeClock;
use ratatui::backend::TestBackend;

use super::tests::{DEADLINE, launch, new_loop, open};
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
