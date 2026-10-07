//! Tests for the hub's start (`docs/state.md`, "Diagnostic logs"): the
//! lock first, then the log, then configuration and the bind, each
//! failure after the lock written to `hub.log` once; and for the hub's
//! signal arm: a raised signal reaches the flag the idle wait reads, then
//! wakes it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use contract::ErrorCode;
use serde_json::Value;

use super::*;
use crate::fake::{FakeStarter, failure};

const WITHIN: Duration = Duration::from_secs(10);

/// A waker that reports each wake on a channel.
struct Sent(Mutex<mpsc::Sender<()>>);

impl Wake for Sent {
    fn wake(&self) {
        let sender = self.0.lock().unwrap();
        sender.send(()).unwrap_or(());
    }
}

#[test]
fn arm_records_a_signal_raised_at_the_process_and_wakes() {
    let got = Arc::new(AtomicI32::new(0));
    let (woke_tx, woke_rx) = mpsc::channel();
    arm(&got, Arc::new(Sent(Mutex::new(woke_tx))));
    signal_hook::low_level::raise(signal_hook::consts::SIGTERM).unwrap();
    // The handler thread records the signal, then wakes the idle wait.
    woke_rx
        .recv_timeout(WITHIN)
        .expect("the arm wakes the idle wait before its deadline");
    assert_eq!(got.load(Ordering::SeqCst), signal_hook::consts::SIGTERM);
}

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("hs");
        let dir = held.path().join("h");
        fs::create_dir_all(&dir).unwrap();
        Self { dir, held }
    }
}

/// The parsed lines of `home`'s `logs/hub.log`.
fn log_lines(home: &Path) -> Vec<Value> {
    fs::read_to_string(home.join("logs").join("hub.log"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Runs [`serve`] in `home` on a thread, so a start that never returns
/// fails the test under [`WITHIN`] rather than hanging it.
fn serve_in(
    home: &Path,
    configure: impl FnOnce() -> Result<Duration, Failure> + Send + 'static,
) -> Result<i32, StartError> {
    let home = home.to_path_buf();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let starter = Arc::new(FakeStarter::hang(&home));
        let result = serve(
            &home,
            configure,
            "0.0.0",
            starter,
            fakes::clock::FakeClock::new(),
        );
        done_tx.send(result).unwrap_or(());
    });
    done_rx
        .recv_timeout(WITHIN)
        .expect("a hub that cannot start returns before its deadline")
}

/// A home whose `run/hub` is one byte past the platform's socket limit.
fn too_long(temp: &Temp) -> PathBuf {
    let pad = listen::SOCKET_PATH_MAX - temp.dir.to_string_lossy().len() - 1 - 8 + 1;
    let home = temp.dir.join("p".repeat(pad));
    fs::create_dir_all(&home).unwrap();
    home
}

#[test]
fn a_too_long_home_is_usage_written_to_the_hub_log() {
    let temp = Temp::new();
    let home = too_long(&temp);
    let Err(error) = serve_in(&home, || Ok(Duration::from_secs(1))) else {
        panic!("a too-long home cannot start");
    };
    assert_eq!(error.code(), ErrorCode::Usage);
    assert!(error.to_string().contains("FIBER_HOME"), "{error}");
    let lines = log_lines(&home);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["level"], "error");
    assert_eq!(lines[0]["process"], "hub");
    assert_eq!(lines[0]["code"], "usage");
    assert_eq!(lines[0]["message"], error.to_string());
    assert!(!home.join("run").join("hub").exists());
}

#[test]
fn a_configure_failure_is_written_with_its_code_while_the_lock_is_held() {
    let temp = Temp::new();
    let home = temp.dir.clone();
    let called = Arc::new(AtomicBool::new(false));
    let saw = Arc::clone(&called);
    let result = serve_in(&temp.dir, move || {
        // Configuration is read after the lock and the log's directory.
        assert!(home.join("logs").is_dir(), "logs/ exists before configure");
        assert!(
            listen::lock(&home).unwrap().is_none(),
            "the run/ lock is held while configure runs"
        );
        saw.store(true, Ordering::SeqCst);
        Err(failure(
            ErrorCode::ConfigInvalid,
            "config.json is not valid JSON.",
        ))
    });
    assert!(called.load(Ordering::SeqCst), "configure ran");
    let Err(error) = result else {
        panic!("a configure failure cannot start");
    };
    assert_eq!(error.code(), ErrorCode::ConfigInvalid);
    assert_eq!(error.to_string(), "config.json is not valid JSON.");
    let lines = log_lines(&temp.dir);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["level"], "error");
    assert_eq!(lines[0]["code"], "config_invalid");
    assert_eq!(lines[0]["message"], "config.json is not valid JSON.");
    assert!(!temp.dir.join("run").join("hub").exists());
    // The failing start released the lock on its way out.
    assert!(listen::lock(&temp.dir).unwrap().is_some());
}

#[test]
fn a_hub_that_loses_the_lock_writes_nothing_and_reads_no_configuration() {
    let temp = Temp::new();
    let other = listen::lock(&temp.dir)
        .unwrap()
        .expect("the other hub's lock");
    let called = Arc::new(AtomicBool::new(false));
    let saw = Arc::clone(&called);
    let result = serve_in(&temp.dir, move || {
        saw.store(true, Ordering::SeqCst);
        Ok(Duration::from_secs(1))
    });
    assert_eq!(result.unwrap(), 0);
    assert!(!called.load(Ordering::SeqCst), "configure never ran");
    assert!(!temp.dir.join("logs").exists(), "no logs/ was created");
    drop(other);
}

#[test]
fn a_run_that_cannot_be_locked_is_io_failed_and_not_logged() {
    let temp = Temp::new();
    fs::write(temp.dir.join("run"), b"not a directory").unwrap();
    let Err(error) = serve_in(&temp.dir, || Ok(Duration::from_secs(1))) else {
        panic!("a file at run/ cannot start");
    };
    assert_eq!(error.code(), ErrorCode::IoFailed);
    assert!(error.to_string().contains("run"), "{error}");
    assert!(!temp.dir.join("logs").exists(), "no logs/ was created");
}

#[test]
fn a_live_hub_on_the_socket_means_exit_0_with_no_error_line() {
    let temp = Temp::new();
    fs::create_dir_all(temp.dir.join("run")).unwrap();
    let socket = temp.dir.join("run").join("hub");
    let live = UnixListener::bind(&socket).unwrap();
    let result = serve_in(&temp.dir, || Ok(Duration::from_secs(1)));
    assert_eq!(result.unwrap(), 0);
    assert!(log_lines(&temp.dir).is_empty());
    assert!(
        UnixStream::connect(&socket).is_ok(),
        "the live socket stays"
    );
    drop(live);
}
