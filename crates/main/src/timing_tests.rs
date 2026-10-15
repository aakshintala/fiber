//! Startup and routing timers under an injected clock
//! (`docs/state.md`, "Diagnostic logs"): `parts_in` through the installed
//! timer, and one entered switch preparation.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use contract::clock::Clock;

const ONE_STEP: &str = r#"{"steps": [{"text": "Hi."}]}"#;

/// A clock whose k-th `now()` (from 1) returns the origin plus the k-th
/// triangular number of milliseconds. It never advances a clock and never
/// reads the process clock: `wall`, `sleep`, `wait_until` and `subscribe`
/// delegate to an inner [`fakes::clock::FakeClock`]. A guard spanning
/// reads k and k+1 reports k+1 ms, so a misplaced guard changes the
/// asserted value.
struct ScriptedClock {
    inner: Arc<fakes::clock::FakeClock>,
    reads: AtomicU64,
}

impl ScriptedClock {
    fn new() -> Self {
        Self {
            inner: fakes::clock::FakeClock::new(),
            reads: AtomicU64::new(0),
        }
    }

    fn reads(&self) -> u64 {
        self.reads.load(Ordering::SeqCst)
    }
}

impl Clock for ScriptedClock {
    fn now(&self) -> Instant {
        let k = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        self.inner.origin() + Duration::from_millis(k * (k + 1) / 2)
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

    fn subscribe(&self, waker: std::sync::Weak<dyn contract::clock::Wake>) {
        self.inner.subscribe(waker);
    }
}

/// `parts_in` over a scripted model with `level`, on its own thread so a
/// hang fails the test instead of hanging it. The fixture is a scripted
/// model, so no credential command runs and the only clock reads are the
/// stamp and the timers.
fn parts(
    level: &str,
    model: &str,
    reads: Arc<crate::switch::Reads>,
) -> (
    fakes::TempDir,
    Arc<ScriptedClock>,
    Result<crate::Parts, contract::shapes::Failure>,
) {
    let root = fakes::TempDir::new("fiber-timing-parts");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        home.join("config.json"),
        serde_json::json!({"model": model, "diagnostics": {"level": level}}).to_string(),
    )
    .unwrap();
    std::fs::write(workspace.join("s.json"), ONE_STEP).unwrap();
    let clock = Arc::new(ScriptedClock::new());
    let started = {
        let (home, workspace) = (home.clone(), workspace.clone());
        let clock: Arc<ScriptedClock> = Arc::clone(&clock);
        fakes::within("the startup", Duration::from_secs(20), move || {
            crate::parts_in(
                home,
                workspace,
                Vec::new(),
                None,
                crate::credential::Labels::default(),
                None,
                clock,
                None,
                None,
                reads,
                log::diag::Process::Session,
            )
        })
    };
    (root, clock, started)
}

/// Every `op_timing` line under the home's `logs/`, in file order.
fn op_lines(home: &Path) -> Vec<serde_json::Value> {
    let logs = home.join("logs");
    let mut files: Vec<_> = std::fs::read_dir(&logs)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    files.sort();
    let mut lines = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for line in text.lines() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            if value["code"] == "op_timing" {
                lines.push(value);
            }
        }
    }
    lines
}

#[test]
fn a_malformed_config_keeps_the_load_error_and_writes_nothing() {
    let root = fakes::TempDir::new("fiber-timing-malformed");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(home.join("config.json"), "{not json").unwrap();
    let clock = Arc::new(ScriptedClock::new());
    let reads = Arc::new(crate::switch::Reads::default());
    let (_, project) = ::cli::project_of(&home, &workspace).unwrap();
    let expected = config::Config::load(config::Sources {
        home: home.clone(),
        workspace: workspace.clone(),
        project,
        overrides: Vec::new(),
    })
    .err()
    .unwrap();
    let failed = {
        let (home, workspace) = (home.clone(), workspace.clone());
        let clock: Arc<ScriptedClock> = Arc::clone(&clock);
        fakes::within("the startup", Duration::from_secs(20), move || {
            crate::parts_in(
                home,
                workspace,
                Vec::new(),
                None,
                crate::credential::Labels::default(),
                None,
                clock,
                None,
                None,
                reads,
                log::diag::Process::Session,
            )
        })
    }
    .err()
    .unwrap();
    assert_eq!(failed.code, expected.code());
    assert_eq!(failed.message, expected.to_string());
    assert_eq!(clock.reads(), 1);
    assert!(op_lines(&home).is_empty());
}

#[test]
fn an_info_start_reads_the_clock_once_and_writes_nothing() {
    let reads = Arc::new(crate::switch::Reads::default());
    let (root, clock, started) = parts("info", "scripted/s.json", reads);
    started.unwrap();
    assert_eq!(clock.reads(), 1);
    assert!(op_lines(&root.path().join("home")).is_empty());
}

#[test]
fn a_debug_start_times_each_startup_operation_in_order() {
    let reads = Arc::new(crate::switch::Reads::default());
    let (root, clock, started) = parts("debug", "scripted/s.json", reads);
    started.unwrap();
    let lines = op_lines(&root.path().join("home"));
    assert_eq!(lines.len(), 3);
    for line in &lines {
        assert_eq!(line["level"], "debug");
        assert_eq!(line["process"], "session");
        assert!(line.get("session_id").is_none(), "{line}");
        assert_eq!(line["code"], "op_timing");
        assert_eq!(line["message"], "An operation ended.", "{line}");
        let data = line["data"].as_object().expect("data is an object");
        assert_eq!(data.keys().collect::<Vec<_>>(), vec!["ms", "op"], "{line}");
    }
    let ops: Vec<&str> = lines
        .iter()
        .map(|line| line["data"]["op"].as_str().unwrap())
        .collect();
    assert_eq!(
        ops,
        ["startup.config", "startup.extensions", "startup.routing"],
        "config ends before extensions starts, extensions before routing"
    );
    let ms: Vec<u64> = lines
        .iter()
        .map(|line| line["data"]["ms"].as_u64().unwrap())
        .collect();
    assert_eq!(ms, [2, 4, 6]);
    assert_eq!(clock.reads(), 6);
}

#[test]
fn an_unknown_model_keeps_its_error_and_still_writes_all_three_lines() {
    let debug_reads = Arc::new(crate::switch::Reads::default());
    let (debug_root, debug_clock, debug_started) = parts("debug", "nope/x", debug_reads);
    let debug_failed = debug_started.err().unwrap();
    let info_reads = Arc::new(crate::switch::Reads::default());
    let (info_root, info_clock, info_started) = parts("info", "nope/x", info_reads);
    let info_failed = info_started.err().unwrap();
    assert_eq!(debug_failed.code, info_failed.code);
    assert_eq!(debug_failed.message, info_failed.message);
    let lines = op_lines(&debug_root.path().join("home"));
    let ops: Vec<&str> = lines
        .iter()
        .map(|line| line["data"]["op"].as_str().unwrap())
        .collect();
    assert_eq!(
        ops,
        ["startup.config", "startup.extensions", "startup.routing"]
    );
    let ms: Vec<u64> = lines
        .iter()
        .map(|line| line["data"]["ms"].as_u64().unwrap())
        .collect();
    assert_eq!(ms, [2, 4, 6]);
    assert_eq!(debug_clock.reads(), 6);
    assert!(op_lines(&info_root.path().join("home")).is_empty());
    assert_eq!(info_clock.reads(), 1);
}
