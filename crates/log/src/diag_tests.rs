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
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::diag::Purpose;

use super::*;

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
                attached_rx.recv_timeout(LIMIT).unwrap();
                diag.line(Severity::Info, None, "after", "After attach.");
                wrote_tx.send(()).unwrap();
            })
            .unwrap()
    };
    wrote_rx.recv_timeout(LIMIT).unwrap();
    diag.attach(&session());
    attached_tx.send(()).unwrap();
    wrote_rx.recv_timeout(LIMIT).unwrap();
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
