//! The diagnostic writer (`docs/state.md`, "Diagnostic logs"): line bytes,
//! the debug level, file naming before and after `attach`, rotation, and a
//! line racing `attach`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, SystemTime};

use contract::clock::Wake;
use contract::diag::{Op, Purpose};

use super::*;
use fakes::Deadline;

type Reader = fn() -> Option<u64>;

/// The wall-clock limit on every channel receive below.
const LIMIT: Duration = Duration::from_secs(30);

struct Home {
    held: fakes::TempDir,
}

impl Home {
    fn new(name: &str) -> Self {
        Self {
            held: fakes::TempDir::new(name),
        }
    }

    fn path(&self) -> &Path {
        self.held.path()
    }

    fn diag(&self, process: Process, level: Level) -> Diag {
        Diag::new(
            self.path(),
            process,
            level,
            fakes::clock::FakeClock::new() as Arc<dyn Clock>,
        )
    }

    fn file(&self, name: &str) -> PathBuf {
        self.path().join("logs").join(name)
    }

    fn text(&self, name: &str) -> String {
        fs::read_to_string(self.file(name)).unwrap()
    }
}

fn pid_file(kind: &str) -> String {
    format!("{kind}-{}.log", std::process::id())
}

fn session() -> SessionId {
    SessionId("s_0123456789abcdef".into())
}

fn request() -> ProviderRequest {
    ProviderRequest {
        provider: "fake".into(),
        model: Some("m".into()),
        purpose: Purpose::ModelCall,
        host: Some("127.0.0.1:50731".into()),
        path: None,
        status: Some(200),
        attempt: 1,
        request_bytes: 2214,
        response_bytes: 913,
        headers_ms: Some(41),
        first_token_ms: None,
        total_ms: 180,
        prefix_hashes: None,
        cache_read: None,
        cache_write: None,
    }
}

#[test]
fn a_hub_info_line_has_today_s_bytes() {
    let home = Home::new("ld-hub-info");
    let diag = home.diag(Process::Hub, Level::Info);
    diag.line(Severity::Info, None, "hub_started", "The hub started.");
    diag.line(
        Severity::Warn,
        Some(&session()),
        "io_failed",
        "Session could not start.",
    );
    diag.line(Severity::Error, None, "config_invalid", "Not JSON.");
    assert_eq!(
        home.text("hub.log"),
        "{\"ts\":1700000000000,\"level\":\"info\",\"process\":\"hub\",\
         \"code\":\"hub_started\",\"message\":\"The hub started.\"}\n\
         {\"ts\":1700000000000,\"level\":\"warn\",\"process\":\"hub\",\
         \"session_id\":\"s_0123456789abcdef\",\"code\":\"io_failed\",\
         \"message\":\"Session could not start.\"}\n\
         {\"ts\":1700000000000,\"level\":\"error\",\"process\":\"hub\",\
         \"code\":\"config_invalid\",\"message\":\"Not JSON.\"}\n"
    );
}

#[test]
fn a_session_info_line_has_today_s_bytes_and_creates_logs_mode_0700() {
    let home = Home::new("ld-session-info");
    let diag = home.diag(Process::Session, Level::Info);
    diag.attach(&SessionId("s-1".into()));
    assert!(!home.path().join("logs").exists());
    diag.line(
        Severity::Info,
        None,
        "extension_log",
        "fiber.test/notes: a\nb\"c",
    );
    let mode = fs::metadata(home.path().join("logs"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700);
    assert_eq!(
        home.text("session-s-1.log"),
        "{\"ts\":1700000000000,\"level\":\"info\",\"process\":\"session\",\
         \"session_id\":\"s-1\",\"code\":\"extension_log\",\
         \"message\":\"fiber.test/notes: a\\nb\\\"c\"}\n"
    );
}

#[test]
fn at_info_debug_lines_write_nothing_and_create_nothing() {
    let home = Home::new("ld-info-quiet");
    for process in [Process::Hub, Process::Session, Process::Ask, Process::Tui] {
        let diag = home.diag(process, Level::Info).with_peak(|| Some(7));
        assert!(!diag.debugging());
        diag.provider_request(&request());
        diag.peak_memory();
    }
    assert!(!home.path().join("logs").exists());
}

#[test]
fn at_debug_a_provider_request_is_one_line_with_data_last() {
    let home = Home::new("ld-debug-request");
    let diag = home.diag(Process::Ask, Level::Debug);
    assert!(diag.debugging());
    diag.attach(&SessionId("s_7f3a".into()));
    diag.provider_request(&request());
    assert_eq!(
        home.text("ask-s_7f3a.log"),
        "{\"ts\":1700000000000,\"level\":\"debug\",\"process\":\"ask\",\
         \"session_id\":\"s_7f3a\",\"code\":\"provider_request\",\
         \"message\":\"A provider request ended.\",\"data\":{\"provider\":\"fake\",\
         \"model\":\"m\",\"purpose\":\"model_call\",\"host\":\"127.0.0.1:50731\",\
         \"status\":200,\"attempt\":1,\"request_bytes\":2214,\"response_bytes\":913,\
         \"headers_ms\":41,\"total_ms\":180}}\n"
    );
}

#[test]
fn at_debug_peak_memory_writes_the_reader_s_value_as_given() {
    let readers: [(Reader, &str); 2] = [(|| Some(0), "0"), (|| Some(11_840), "11840")];
    for (peak, want) in readers {
        let home = Home::new("ld-debug-peak");
        let diag = home
            .diag(Process::Hub, Level::Info)
            .with_level(Level::Debug)
            .with_peak(peak);
        diag.peak_memory();
        assert_eq!(
            home.text("hub.log"),
            format!(
                "{{\"ts\":1700000000000,\"level\":\"debug\",\"process\":\"hub\",\
                 \"code\":\"peak_memory\",\"message\":\"The process's peak memory so far.\",\
                 \"data\":{{\"peak_kib\":{want}}}}}\n"
            ),
            "{want}"
        );
    }
}

#[test]
fn a_failed_memory_read_writes_nothing() {
    let home = Home::new("ld-peak-none");
    let diag = home.diag(Process::Session, Level::Debug).with_peak(|| None);
    diag.peak_memory();
    assert!(!home.path().join("logs").exists());
}

#[test]
fn each_process_names_its_file_before_and_after_attach() {
    for (process, kind, attaches) in [
        (Process::Session, "session", true),
        (Process::Ask, "ask", true),
        (Process::Tui, "tui", false),
    ] {
        let home = Home::new("ld-names");
        let diag = home.diag(process, Level::Info);
        diag.line(Severity::Info, None, "first", "First.");
        diag.attach(&session());
        diag.line(Severity::Info, None, "second", "Second.");
        let before = home.text(&pid_file(kind));
        assert!(before.contains("\"code\":\"first\""), "{kind}: {before}");
        assert!(!before.contains("session_id"), "{kind}: {before}");
        let attached = format!("{kind}-s_0123456789abcdef.log");
        if attaches {
            let after = home.text(&attached);
            assert!(after.contains("\"code\":\"second\""), "{kind}: {after}");
            assert!(
                after.contains("\"session_id\":\"s_0123456789abcdef\""),
                "{kind}: {after}"
            );
            assert!(!before.contains("second"), "{kind}");
        } else {
            assert!(before.contains("\"code\":\"second\""), "{kind}: {before}");
            assert!(!before.contains("session_id"), "{kind}");
            assert!(!home.file(&attached).exists(), "{kind}");
        }
    }
    let home = Home::new("ld-names-hub");
    let diag = home.diag(Process::Hub, Level::Info);
    diag.attach(&session());
    diag.line(Severity::Info, None, "hub_started", "The hub started.");
    assert!(!home.text("hub.log").contains("session_id"));
    assert_eq!(fs::read_dir(home.path().join("logs")).unwrap().count(), 1);
}

#[test]
fn a_named_session_wins_over_the_attached_one() {
    let home = Home::new("ld-named");
    let diag = home.diag(Process::Session, Level::Info);
    diag.attach(&SessionId("s-1".into()));
    diag.line(
        Severity::Warn,
        Some(&SessionId("s-2".into())),
        "w",
        "Warned.",
    );
    assert!(
        home.text("session-s-1.log")
            .contains("\"session_id\":\"s-2\"")
    );
}

#[test]
fn without_rotating_a_large_file_is_never_renamed() {
    let home = Home::new("ld-no-rotate");
    fs::create_dir_all(home.path().join("logs")).unwrap();
    fs::write(home.file("hub.log"), vec![b'x'; 10 * 1024 * 1024 + 1]).unwrap();
    home.diag(Process::Hub, Level::Info).line(
        Severity::Info,
        None,
        "hub_started",
        "The hub started.",
    );
    assert!(!home.file("hub.log.1").exists());
}

#[test]
fn rotating_renames_only_a_file_over_the_limit() {
    let home = Home::new("ld-rotate");
    fs::create_dir_all(home.path().join("logs")).unwrap();
    fs::write(home.file("hub.log"), vec![b'x'; 100]).unwrap();
    let diag = home.diag(Process::Hub, Level::Info).rotating(100);
    diag.line(Severity::Info, None, "a", "A.");
    assert!(!home.file("hub.log.1").exists(), "exactly the limit stays");
    diag.line(Severity::Info, None, "b", "B.");
    let previous = fs::read(home.file("hub.log.1")).unwrap();
    assert!(previous.len() > 100);
    assert!(home.text("hub.log").contains("\"code\":\"b\""));
    assert!(!home.text("hub.log").contains("\"code\":\"a\""));
}

#[test]
fn a_failed_write_is_silent() {
    let home = Home::new("ld-unwritable");
    fs::write(home.path().join("logs"), b"not a directory").unwrap();
    let diag = home
        .diag(Process::Session, Level::Debug)
        .with_peak(|| Some(1));
    diag.line(Severity::Info, None, "extension_log", "x: y");
    diag.provider_request(&request());
    diag.peak_memory();
    assert_eq!(
        fs::read(home.path().join("logs")).unwrap(),
        b"not a directory"
    );
}

/// A line from another thread and `attach`, in both orders, each forced by
/// a channel handshake.
#[test]
fn a_removed_logs_directory_is_made_again_on_the_next_line() {
    let home = Home::new("ld-remade");
    let diag = home.diag(Process::Hub, Level::Info);
    diag.line(Severity::Info, None, "first", "First.");
    fs::remove_dir_all(home.path().join("logs")).unwrap();
    diag.line(Severity::Info, None, "second", "Second.");
    let text = home.text("hub.log");
    assert!(text.contains("\"code\":\"second\""), "{text}");
    assert!(!text.contains("\"code\":\"first\""), "{text}");
    let mode = fs::metadata(home.path().join("logs"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700);
}

/// A line from another thread and `attach`, in both orders, each forced by
/// a channel handshake.
#[test]
fn a_line_racing_attach_lands_whole_in_the_file_its_order_implies() {
    let home = Home::new("ld-race");
    let diag = Arc::new(home.diag(Process::Session, Level::Info));
    let (wrote_tx, wrote_rx) = mpsc::channel::<()>();
    let (attached_tx, attached_rx) = mpsc::channel::<()>();
    let writer = {
        let diag = Arc::clone(&diag);
        thread::Builder::new()
            .name("log-test-diag".to_owned())
            .spawn(move || {
                diag.line(Severity::Info, None, "before", "Before attach.");
                wrote_tx.send(()).unwrap();
                Deadline::after(LIMIT).recv(&attached_rx).unwrap();
                diag.line(Severity::Info, None, "after", "After attach.");
                wrote_tx.send(()).unwrap();
            })
            .unwrap()
    };
    Deadline::after(LIMIT).recv(&wrote_rx).unwrap();
    diag.attach(&session());
    attached_tx.send(()).unwrap();
    Deadline::after(LIMIT).recv(&wrote_rx).unwrap();
    writer.join().unwrap();
    let before: serde_json::Value =
        serde_json::from_str(home.text(&pid_file("session")).trim_end()).unwrap();
    assert_eq!(before["code"], "before");
    assert!(before.get("session_id").is_none());
    let after: serde_json::Value =
        serde_json::from_str(home.text("session-s_0123456789abcdef.log").trim_end()).unwrap();
    assert_eq!(after["code"], "after");
    assert_eq!(after["session_id"], "s_0123456789abcdef");
}

/// `for_session` writes to the session's file with its id and leaves the
/// original writing the pid-named file with none: the complete contents
/// of both files, byte for byte.
#[test]
fn for_session_writes_the_session_file_and_leaves_the_original_unattached() {
    let home = Home::new("ld-for-session");
    let diag = home.diag(Process::Session, Level::Info);
    let bound = diag.for_session(&session());
    bound.line(
        Severity::Error,
        None,
        "timeout",
        "The shutdown reached its bound.",
    );
    diag.line(Severity::Info, None, "first", "First.");
    assert_eq!(
        home.text("session-s_0123456789abcdef.log"),
        "{\"ts\":1700000000000,\"level\":\"error\",\"process\":\"session\",\
         \"session_id\":\"s_0123456789abcdef\",\"code\":\"timeout\",\
         \"message\":\"The shutdown reached its bound.\"}\n"
    );
    assert_eq!(
        home.text(&pid_file("session")),
        "{\"ts\":1700000000000,\"level\":\"info\",\"process\":\"session\",\
         \"code\":\"first\",\"message\":\"First.\"}\n"
    );
}

/// A second writer waits while a pair holds the files lock: the pair's
/// seam holds the lock until the test releases it, the competing writer
/// itself observes `WouldBlock` on the files lock and signals what it
/// saw before writing, and its completion stays unreceived while the
/// pair is still parked holding the lock. After the pair returns the
/// line lands after both lines of the pair: the whole ordered file.
#[test]
fn a_second_writer_waits_for_the_pair() {
    let home = Home::new("ld-pair-waits");
    let (seam_tx, seam_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let between: Arc<dyn Fn(bool) + Send + Sync> = Arc::new(move |_held| {
        seam_tx.send(()).unwrap();
        let release_rx = release_rx.lock().unwrap();
        Deadline::after(LIMIT).recv_or_fail(&release_rx, "the test releases the pair callback");
    });
    let pair = home
        .diag(Process::Hub, Level::Debug)
        .with_peak(|| Some(7))
        .with_between(between);
    let worker = thread::Builder::new()
        .name("log-test-pair".to_owned())
        .spawn(move || {
            pair.peak_memory_then_info("hub_stopped", "The hub stopped.");
        })
        .unwrap();
    Deadline::after(LIMIT).recv_or_fail(&seam_rx, "the pair pauses between its two appends");
    let second = home.diag(Process::Hub, Level::Debug);
    let (observed_tx, observed_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let writer = thread::Builder::new()
        .name("log-test-second".to_owned())
        .spawn(move || {
            // The test waits for the pair's pause before spawning this
            // thread, so the pair holds the files lock here: the
            // observation below, not this thread's arrival, proves the
            // competing write ran while the pair was parked.
            assert!(
                matches!(FILES.try_lock(), Err(std::sync::TryLockError::WouldBlock)),
                "the pair holds the files lock while the second writer waits"
            );
            observed_tx.send(()).unwrap();
            second.line(Severity::Info, None, "second", "Second.");
            done_tx.send(()).unwrap_or(());
        })
        .unwrap();
    Deadline::after(LIMIT).recv_or_fail(
        &observed_rx,
        "the second writer observes the files lock held",
    );
    // The pair is still parked in its seam with the files lock held, so
    // the competing write cannot have landed yet.
    assert!(
        FILES.try_lock().is_err(),
        "the pair still holds the files lock while the second writer waits"
    );
    assert!(
        done_rx.try_recv().is_err(),
        "a second writer waits while the pair holds the files lock"
    );
    release_tx.send(()).unwrap();
    Deadline::after(LIMIT).recv_or_fail(&done_rx, "the second writer finishes after the release");
    writer.join().unwrap();
    worker.join().unwrap();
    // docs/state.md:288-292 promises `peak_memory` just before each
    // `hub_stopped` at debug; the controlled pause holds the files lock
    // until the release above, so the competing `second` line lands after
    // that documented pair.
    assert_eq!(
        home.text("hub.log"),
        "{\"ts\":1700000000000,\"level\":\"debug\",\"process\":\"hub\",\
         \"code\":\"peak_memory\",\"message\":\"The process's peak memory so far.\",\
         \"data\":{\"peak_kib\":7}}\n\
         {\"ts\":1700000000000,\"level\":\"info\",\"process\":\"hub\",\
         \"code\":\"hub_stopped\",\"message\":\"The hub stopped.\"}\n\
         {\"ts\":1700000000000,\"level\":\"info\",\"process\":\"hub\",\
         \"code\":\"second\",\"message\":\"Second.\"}\n"
    );
}

/// `peak_memory_then_info` holds one lock across both lines: the seam
/// runs between the pair's two appends and reports whether the state
/// lock is still held, which only the single-lock implementation can
/// satisfy. No threads, no waits: the property is read directly off
/// the lock.
#[test]
fn peak_memory_then_info_writes_its_pair_with_nothing_between() {
    let home = Home::new("ld-pair-forced");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let between: Arc<dyn Fn(bool) + Send + Sync> = Arc::new({
        let calls = Arc::clone(&calls);
        move |held| calls.lock().unwrap().push(held)
    });
    home.diag(Process::Hub, Level::Debug)
        .with_peak(|| Some(7))
        .with_between(between)
        .peak_memory_then_info("hub_stopped", "The hub stopped: signal.");
    assert_eq!(*calls.lock().unwrap(), [true]);
    let lines: Vec<serde_json::Value> = fs::read_to_string(home.file("hub.log"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        lines
            .iter()
            .map(|line| line["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["peak_memory", "hub_stopped"],
    );
    assert_eq!(lines[0]["data"]["peak_kib"], 7);
}

/// A clock that counts its monotonic reads, proving `timed` reads none
/// at `info`.
struct SpyClock {
    inner: Arc<fakes::clock::FakeClock>,
    reads: AtomicUsize,
}

impl SpyClock {
    fn new() -> (Arc<fakes::clock::FakeClock>, Arc<Self>) {
        let inner = fakes::clock::FakeClock::new();
        let spy = Arc::new(Self {
            inner: Arc::clone(&inner),
            reads: AtomicUsize::new(0),
        });
        (inner, spy)
    }

    fn reads(spy: &Arc<Self>) -> usize {
        spy.reads.load(Ordering::SeqCst)
    }
}

impl Clock for SpyClock {
    fn now(&self) -> Instant {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.now()
    }

    fn wall(&self) -> SystemTime {
        self.inner.wall()
    }

    fn sleep(&self, d: Duration) {
        self.inner.sleep(d);
    }

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        self.inner.wait_until(until, wait);
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        self.inner.subscribe(waker);
    }
}

fn debug_diag(home: &Home, spy: &Arc<SpyClock>) -> Diag {
    Diag::new(
        home.path(),
        Process::Hub,
        Level::Debug,
        Arc::clone(spy) as Arc<dyn Clock>,
    )
}

fn op_line(home: &Home, name: &str) -> serde_json::Value {
    let text = home.text(name);
    let mut lines = text.lines();
    let line: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert!(lines.next().is_none(), "one line: {text}");
    line
}

#[test]
fn at_info_timed_reads_no_clock_and_writes_nothing() {
    let home = Home::new("ld-timed-info");
    let (_inner, spy) = SpyClock::new();
    let diag = Diag::new(
        home.path(),
        Process::Session,
        Level::Info,
        Arc::clone(&spy) as Arc<dyn Clock>,
    );
    assert!(timed(&diag, Op::TurnRequest, None, None).is_none());
    assert!(diag.timings()(Op::TurnRequest, None, None).is_none());
    assert_eq!(SpyClock::reads(&spy), 0);
    assert!(!home.path().join("logs").exists());
}

#[test]
fn at_debug_a_dropped_guard_writes_one_line_and_no_file_before() {
    let home = Home::new("ld-timed-drop");
    let (inner, spy) = SpyClock::new();
    let diag = debug_diag(&home, &spy);
    let start = inner.now();
    // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    inner.advance(Duration::from_millis(7));
    let timer = timed(&diag, Op::ToolRun, Some("read"), Some(start));
    assert!(timer.is_some(), "a debug guard is active");
    assert!(!home.path().join("logs").exists(), "no file before drop");
    drop(timer);
    assert_eq!(
        home.text("hub.log"),
        "{\"ts\":1700000000007,\"level\":\"debug\",\"process\":\"hub\",\
         \"code\":\"op_timing\",\"message\":\"An operation ended.\",\
         \"data\":{\"op\":\"tool.run\",\"ms\":7,\"tool\":\"read\"}}\n"
    );
}

fn early_return(diag: &Diag, stop: bool) -> u32 {
    let _timer = timed(diag, Op::TurnRequest, None, None);
    if stop {
        return 7;
    }
    0
}

fn propagated_error(diag: &Diag, fail: bool) -> Result<u32, &'static str> {
    let _timer = timed(diag, Op::TurnRequest, None, None);
    if fail {
        return Err("boom");
    }
    Ok(0)
}

#[test]
fn an_early_return_and_a_propagated_error_each_emit_once() {
    let home = Home::new("ld-timed-scope");
    let diag = home.diag(Process::Hub, Level::Debug);
    assert_eq!(early_return(&diag, true), 7);
    assert_eq!(
        propagated_error(&diag, true).unwrap_err(),
        "boom",
        "the error still propagates"
    );
    let text = fs::read_to_string(home.file("hub.log")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "one line per scope");
    for line in lines {
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(parsed["code"], "op_timing");
        assert_eq!(parsed["data"]["op"], "turn.request");
    }
}

#[test]
fn stop_stamps_completion_and_drop_emits_it() {
    let home = Home::new("ld-timed-stop");
    let (inner, spy) = SpyClock::new();
    let diag = debug_diag(&home, &spy);
    let start = inner.now();
    // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    inner.advance(Duration::from_millis(5));
    let mut timer = timed(&diag, Op::TurnModelCall, None, Some(start)).expect("debug guard");
    // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    inner.advance(Duration::from_millis(100));
    timer.stop();
    // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    inner.advance(Duration::from_secs(1));
    drop(timer);
    let line = op_line(&home, "hub.log");
    assert_eq!(line["data"]["op"], "turn.model_call");
    assert_eq!(line["data"]["ms"], 105);
}

#[test]
fn a_discarded_timer_writes_no_line() {
    let home = Home::new("ld-timed-discard");
    let (inner, spy) = SpyClock::new();
    let diag = debug_diag(&home, &spy);
    // Stopped, then discarded: no line, though an end was stamped.
    let start = inner.now();
    // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    inner.advance(Duration::from_millis(5));
    let mut timer = timed(&diag, Op::TurnRequest, None, Some(start)).expect("debug guard");
    // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    inner.advance(Duration::from_millis(100));
    timer.stop();
    timer.discard();
    // Discarded without a stop: no line either.
    let timer = timed(&diag, Op::TurnRequest, None, None).expect("debug guard");
    timer.discard();
    assert!(!home.path().join("logs").exists(), "no line yet");
    // Stopped, then dropped: one line with the stamped `ms`.
    let start = inner.now();
    // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    inner.advance(Duration::from_millis(5));
    let mut timer = timed(&diag, Op::TurnRequest, None, Some(start)).expect("debug guard");
    // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    inner.advance(Duration::from_millis(100));
    timer.stop();
    drop(timer);
    let line = op_line(&home, "hub.log");
    assert_eq!(line["data"]["op"], "turn.request");
    assert_eq!(line["data"]["ms"], 105);
}

#[test]
fn a_failed_timing_write_is_silent() {
    let home = Home::new("ld-timed-unwritable");
    fs::write(home.path().join("logs"), b"not a directory").unwrap();
    let diag = home.diag(Process::Hub, Level::Debug);
    let timer = timed(&diag, Op::HubSessionStart, None, None);
    assert!(timer.is_some(), "a debug guard is active");
    drop(timer);
    assert_eq!(
        fs::read(home.path().join("logs")).unwrap(),
        b"not a directory"
    );
}

#[test]
fn every_process_shape_writes_through_the_same_writer() {
    for (process, kind) in [
        (Process::Hub, "hub"),
        (Process::Session, "session"),
        (Process::Ask, "ask"),
        (Process::Tui, "tui"),
    ] {
        let home = Home::new("ld-timed-shapes");
        let diag = home.diag(process, Level::Debug);
        drop(timed(&diag, Op::NetConnect, None, None).expect("debug guard"));
        let name = if kind == "hub" {
            "hub.log".to_owned()
        } else {
            pid_file(kind)
        };
        let line = op_line(&home, &name);
        assert_eq!(line["level"], "debug", "{kind}");
        assert_eq!(line["process"], kind, "{kind}");
        assert_eq!(line["code"], "op_timing", "{kind}");
        assert_eq!(line["message"], "An operation ended.", "{kind}");
        assert_eq!(line["data"]["op"], "net.connect", "{kind}");
        assert!(line["data"]["ms"].is_u64(), "{kind}: {line}");
    }
}

#[test]
fn the_callable_times_without_a_second_check_and_emits_once() {
    let home = Home::new("ld-timed-callable");
    let (inner, spy) = SpyClock::new();
    let diag = debug_diag(&home, &spy);
    let time = diag.timings();
    let start = inner.now();
    // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    inner.advance(Duration::from_millis(3));
    let guard = time(Op::StartupConfig, None, Some(start)).expect("debug guard");
    // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    inner.advance(Duration::from_millis(100));
    drop(guard);
    let line = op_line(&home, "hub.log");
    assert_eq!(line["data"]["op"], "startup.config");
    assert_eq!(line["data"]["ms"], 103);
}

#[test]
fn whole_milliseconds_truncate_and_saturate() {
    for (elapsed, want) in [
        (Duration::ZERO, 0),
        (Duration::from_nanos(999_999), 0),
        (Duration::from_nanos(1_000_000), 1),
        (Duration::from_nanos(1_999_999), 1),
        (Duration::from_millis(u64::MAX - 1), u64::MAX - 1),
        (Duration::from_millis(u64::MAX), u64::MAX),
    ] {
        assert_eq!(duration_ms(elapsed), want, "{elapsed:?}");
    }
    let past_max = Duration::from_millis(u64::MAX)
        .checked_add(Duration::from_millis(1))
        .expect("one millisecond past the maximum");
    assert_eq!(duration_ms(past_max), u64::MAX);
}

/// Runs one guard for `elapsed` on the fake clock and returns the file's
/// text: empty when nothing was written.
fn timed_text(op: Op, elapsed: Duration) -> String {
    let home = Home::new("ld-timed-ms");
    let inner = fakes::clock::FakeClock::new();
    let clock: Arc<dyn Clock> = inner.clone();
    let diag = Diag::new(home.path(), Process::Hub, Level::Debug, clock);
    let start = inner.now();
    // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    inner.advance(elapsed);
    drop(timed(&diag, op, None, Some(start)).expect("debug guard"));
    fs::read_to_string(home.file("hub.log")).unwrap_or_default()
}

#[test]
fn a_backwards_clock_contributes_zero() {
    let home = Home::new("ld-timed-backwards");
    let inner = fakes::clock::FakeClock::new();
    let clock: Arc<dyn Clock> = inner.clone();
    let diag = Diag::new(home.path(), Process::Hub, Level::Debug, clock);
    let start = inner
        .now()
        .checked_add(Duration::from_secs(1))
        .expect("one second ahead");
    drop(timed(&diag, Op::TurnHandoff, None, Some(start)).expect("debug guard"));
    let line = op_line(&home, "hub.log");
    assert_eq!(line["data"]["op"], "turn.handoff");
    assert_eq!(line["data"]["ms"], 0);
}

#[test]
fn a_frame_within_budget_writes_nothing_and_one_past_writes_sixteen() {
    let frame = Duration::from_millis(16);
    let within = timed_text(
        Op::TuiFrame,
        frame
            .checked_sub(Duration::from_nanos(1))
            .expect("one less"),
    );
    assert!(
        within.is_empty(),
        "16 ms minus 1 ns writes nothing: {within}"
    );
    let at = timed_text(Op::TuiFrame, frame);
    assert!(at.is_empty(), "16 ms writes nothing: {at}");
    let past = timed_text(Op::TuiFrame, frame + Duration::from_nanos(1));
    let line: serde_json::Value = serde_json::from_str(past.trim_end()).unwrap();
    assert_eq!(line["data"]["op"], "tui.frame");
    assert_eq!(line["data"]["ms"], 16);
    let other = timed_text(Op::TurnRequest, frame);
    let line: serde_json::Value = serde_json::from_str(other.trim_end()).unwrap();
    assert_eq!(line["data"]["ms"], 16, "the budget binds frames only");
}

/// Every operation with and without a tool: the op and the tool the line
/// carries.
fn every_timed_case() -> Vec<(Op, Option<&'static str>, &'static str)> {
    use Op::*;
    let bare = [
        (StartupConfig, "startup.config"),
        (StartupExtensions, "startup.extensions"),
        (StartupPreamble, "startup.preamble"),
        (StartupSocket, "startup.socket"),
        (StartupRouting, "startup.routing"),
        (RoutingSwitch, "routing.switch"),
        (TuiLogRead, "tui.log_read"),
        (TuiTranscript, "tui.transcript"),
        (TuiFirstRender, "tui.first_render"),
        // `TuiFrame` at rest writes no line; the frame test above times it.
        (LogAppend, "log.append"),
        (LogFsync, "log.fsync"),
        (ToolPermission, "tool.permission"),
        (ToolReviewer, "tool.reviewer"),
        (ToolHooks, "tool.hooks"),
        (ToolRun, "tool.run"),
        (TurnRequest, "turn.request"),
        (TurnModelCall, "turn.model_call"),
        (TurnHandoff, "turn.handoff"),
        (McpConnect, "mcp.connect"),
        (JobsStart, "jobs.start"),
        (WorktreeCreate, "worktree.create"),
        (WorktreeRemove, "worktree.remove"),
        (PictureRun, "picture.run"),
        (HubSessionStart, "hub.session_start"),
        (HubResume, "hub.resume"),
        (HubSubscribeReplay, "hub.subscribe_replay"),
        (NetConnect, "net.connect"),
    ];
    let mut cases = Vec::new();
    for (op, want) in bare {
        cases.push((op, None, want));
        cases.push((op, Some("shell"), want));
    }
    use contract::diag::HookChain::*;
    for (chain, want) in [
        (SessionStart, "hook.session_start"),
        (TurnStart, "hook.turn_start"),
        (BeforeMessage, "hook.before_message"),
        (BeforeModelCall, "hook.before_model_call"),
        (TurnEnd, "hook.turn_end"),
    ] {
        cases.push((Hook(chain), None, want));
        cases.push((Hook(chain), Some("shell"), want));
    }
    cases
}

/// Asserts the closed `data` key set on one parsed `op_timing` line: `op`
/// and `ms`, plus `tool` exactly when the guard carried one. Every new
/// timing test passes its lines through here.
fn assert_timing_data(line: &serde_json::Value, op: &str, tool: Option<&str>) {
    assert_eq!(line["code"], "op_timing");
    let data = line["data"].as_object().expect("data is an object");
    let mut keys: Vec<&str> = data.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut want = vec!["ms", "op"];
    if tool.is_some() {
        want.push("tool");
    }
    assert_eq!(keys, want, "closed data keys: {line}");
    assert_eq!(data["op"], op);
    assert!(data["ms"].is_u64(), "{line}");
    match tool {
        Some(tool) => assert_eq!(data["tool"], tool),
        None => assert!(data.get("tool").is_none(), "{line}"),
    }
}

#[test]
fn op_timing_data_holds_only_op_ms_and_tool() {
    let home = Home::new("ld-timed-data");
    let inner = fakes::clock::FakeClock::new();
    let clock: Arc<dyn Clock> = inner.clone();
    let diag = Diag::new(home.path(), Process::Hub, Level::Debug, clock);
    let cases = every_timed_case();
    for (op, tool, _) in &cases {
        let start = inner.now();
        drop(timed(&diag, *op, *tool, Some(start)).expect("debug guard"));
    }
    let text = home.text("hub.log");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), cases.len(), "one line per guard");
    for (line, (_, tool, want)) in lines.iter().zip(&cases) {
        // The raw bytes pin the key order: `op`, `ms`, then `tool`.
        let data = match tool {
            Some(tool) => format!("\"data\":{{\"op\":\"{want}\",\"ms\":0,\"tool\":\"{tool}\"}}"),
            None => format!("\"data\":{{\"op\":\"{want}\",\"ms\":0}}"),
        };
        assert!(line.contains(&data), "{line}");
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_timing_data(&parsed, want, *tool);
    }
}

#[test]
fn every_process_shape_writes_the_closed_data_keys() {
    for (process, kind) in [
        (Process::Hub, "hub"),
        (Process::Session, "session"),
        (Process::Ask, "ask"),
        (Process::Tui, "tui"),
    ] {
        let home = Home::new("ld-timed-keys");
        let inner = fakes::clock::FakeClock::new();
        let clock: Arc<dyn Clock> = inner.clone();
        let diag = Diag::new(home.path(), process, Level::Debug, clock);
        for (op, tool, want) in [
            (Op::ToolRun, Some("shell"), "tool.run"),
            (Op::TurnRequest, None, "turn.request"),
        ] {
            let start = inner.now();
            drop(timed(&diag, op, tool, Some(start)).expect("debug guard"));
            let name = if kind == "hub" {
                "hub.log".to_owned()
            } else {
                pid_file(kind)
            };
            let text = home.text(&name);
            let parsed: serde_json::Value =
                serde_json::from_str(text.lines().last().unwrap()).unwrap();
            assert_timing_data(&parsed, want, tool);
        }
    }
}

#[test]
fn info_warn_and_error_lines_carry_no_data() {
    let home = Home::new("ld-timed-nodata");
    let diag = home.diag(Process::Hub, Level::Debug);
    diag.line(Severity::Info, None, "hub_started", "The hub started.");
    diag.line(Severity::Warn, None, "slow", "Slow.");
    diag.line(Severity::Error, None, "failed", "Failed.");
    for line in home.text("hub.log").lines() {
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(parsed.get("data").is_none(), "{line}");
    }
}
