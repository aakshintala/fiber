//! The `paging` jig (`docs/testing.md`, "Jigs"): its `open` mode reopens an
//! events file and prints one JSON line, and its positional mode generates
//! the session as it always ran.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// How long the jig may take. `.config/nextest.toml` kills a slow test
/// after a 30s period times 4 (120s total); with the 5s watchdog
/// stand-down the deadlines sum to 55s, so 120s is at least twice them:
/// `docs/testing.md`, "Waits and timeouts", needs nextest's timeout to be
/// at least twice the test's own deadlines, so a hang reports which wait
/// expired.
const JIG_DEADLINE: Duration = Duration::from_secs(50);

/// The example `cargo test` built beside this test binary
/// (`target/<profile>/examples/paging`), so the test starts no cargo and
/// waits on no build lock.
fn example_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.join("examples").join("paging")
}

/// The built jig through this test's own path, with a directory for its
/// events file.
struct Jig {
    dir: fakes::TempDir,
    /// `<CARGO_TARGET_TMPDIR>/<dir's name>`, holding the `paging` link.
    bin: PathBuf,
}

impl Drop for Jig {
    fn drop(&mut self) {
        match std::fs::remove_dir_all(&self.bin) {
            Ok(()) | Err(_) => {}
        }
    }
}

impl Jig {
    fn new() -> Self {
        let dir = fakes::TempDir::new("fiber-paging-open");
        // A hard link, not a copy: the bytes and inode are the built
        // example's, so nothing new is executed (`docs/testing.md`, "Waits
        // and timeouts"), but the path is this test's own. It lives in
        // Cargo's per-target temporary directory, which is on the target
        // directory's filesystem, in a directory named after the test's
        // directory.
        let name = dir.path().file_name().unwrap();
        let bin = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("paging");
        if let Err(err) = std::fs::hard_link(example_path(), &exe) {
            panic!("hard-linking `paging` to {}: {err}", exe.display());
        }
        Self { dir, bin }
    }

    /// This test's own path to the built jig.
    fn exe(&self) -> PathBuf {
        self.bin.join("paging")
    }

    /// The directory holding this test's events file.
    fn dir(&self) -> &Path {
        self.dir.path()
    }
}

/// Runs the jig with `args` and waits for it under [`JIG_DEADLINE`]. The
/// environment holds `PATH` alone, as the bench harness runs the jig.
fn run(exe: &Path, args: &[&str]) -> Output {
    let child = Command::new(exe)
        .args(args)
        .env_clear()
        .envs(fakes::check_run())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let watchdog = fakes::Watchdog::group(child.id());
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = finished
        .recv_timeout(JIG_DEADLINE)
        .expect("the paging jig finished: the paging example")
        .unwrap();
    watchdog.stand_down(Duration::from_secs(5));
    output
}

/// A small events file: two durable lines the log holds.
fn events() -> String {
    let mut out = String::new();
    let mut seq = 0u64;
    let mut line = |kind: &str, payload: serde_json::Value| {
        out.push_str(
            &serde_json::json!({
                "kind": kind,
                "session_id": "s_paging0000000000",
                "ts": 1_790_604_120_000_u64 + seq,
                "schema_version": contract::SCHEMA_VERSION,
                "payload": payload,
                "seq": seq,
            })
            .to_string(),
        );
        out.push('\n');
        seq += 1;
    };
    line(
        "fiber_started",
        serde_json::json!({"version": "0.0.0", "resumed": false}),
    );
    line("session_started", serde_json::json!({"workspace": "/w"}));
    out
}

fn four_keys(stdout: &str) -> serde_json::Value {
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    assert!(
        !stdout.trim().is_empty(),
        "the report is one line, not none"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|err| panic!("{err}: {stdout}"));
    for key in ["parse_ms", "fold_ms", "frames", "frame_ms"] {
        assert!(parsed.get(key).is_some(), "{stdout}");
    }
    assert!(parsed.get("bogus").is_none(), "{stdout}");
    parsed
}

/// The jig's usage error: exit code 2 with the usage on stderr and no
/// report, so a renamed argument fails here before the release job.
fn assert_usage(out: &Output) {
    assert_eq!(out.status.code(), Some(2), "a usage error exits 2");
    assert_ne!(out.status.code(), Some(0));
    let stderr = String::from_utf8(out.stderr.clone()).unwrap();
    assert!(stderr.contains("usage:"), "{stderr}");
    assert!(out.stdout.is_empty(), "a usage error prints no report");
}

#[test]
fn open_prints_one_json_line_with_the_four_keys() {
    let jig = Jig::new();
    let file = jig.dir().join("events.jsonl");
    std::fs::write(&file, events()).unwrap();
    let arg = file.display().to_string();
    let out = run(&jig.exe(), &["open", arg.as_str(), "60", "12", "end"]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.is_empty(), "a report writes no stderr: {stderr}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let parsed = four_keys(&stdout);
    // The single final frame draws once, so a miscounted frame fails
    // here before the release job.
    assert_eq!(parsed.get("frames"), Some(&serde_json::json!(1)));
    assert_ne!(parsed.get("frames"), Some(&serde_json::json!(2)));
}

#[test]
fn open_spells_the_single_frame_as_its_number_too() {
    // The bench prints the single frame's count as its number, so the
    // jig spells it both ways.
    let jig = Jig::new();
    let file = jig.dir().join("events.jsonl");
    std::fs::write(&file, events()).unwrap();
    let arg = file.display().to_string();
    let out = run(
        &jig.exe(),
        &["open", arg.as_str(), "60", "12", "18446744073709551615"],
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{stderr}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let parsed = four_keys(&stdout);
    assert_eq!(parsed.get("frames"), Some(&serde_json::json!(1)));
    assert_ne!(parsed.get("frames"), Some(&serde_json::json!(0)));
}

#[test]
fn open_needs_a_file_a_size_and_a_frame_count() {
    let jig = Jig::new();
    let file = jig.dir().join("events.jsonl");
    std::fs::write(&file, events()).unwrap();
    let arg = file.display().to_string();
    // A missing word is a usage error, not a defaulted open run.
    let out = run(&jig.exe(), &["open", arg.as_str(), "60", "12"]);
    assert_usage(&out);
    // A word that is neither a number nor `end` is a usage error.
    let out = run(&jig.exe(), &["open", arg.as_str(), "60", "12", "every"]);
    assert_usage(&out);
    // So is a size that is not a number.
    let out = run(&jig.exe(), &["open", arg.as_str(), "60", "x", "64"]);
    assert_usage(&out);
}

#[test]
fn a_word_that_is_neither_a_number_nor_open_is_a_usage_error() {
    let jig = Jig::new();
    let out = run(&jig.exe(), &["wide"]);
    assert_usage(&out);
}

#[test]
fn a_line_the_log_cannot_hold_is_an_error_not_a_report() {
    let jig = Jig::new();
    let file = jig.dir().join("events.jsonl");
    std::fs::write(&file, "not json\n").unwrap();
    let arg = file.display().to_string();
    let out = run(&jig.exe(), &["open", arg.as_str(), "60", "12", "end"]);
    assert_eq!(out.status.code(), Some(1), "a bad line exits 1");
    assert_ne!(out.status.code(), Some(0));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(!stderr.trim().is_empty(), "a bad line writes stderr");
    assert!(out.stdout.is_empty(), "a bad line prints no report");
}

#[test]
fn open_on_a_missing_file_fails_with_stderr() {
    let jig = Jig::new();
    let missing = jig.dir().join("missing.jsonl");
    let arg = missing.display().to_string();
    let out = run(&jig.exe(), &["open", arg.as_str(), "60", "12", "end"]);
    assert_eq!(out.status.code(), Some(1), "a missing file exits 1");
    assert_ne!(out.status.code(), Some(0));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(!stderr.trim().is_empty(), "a missing file writes stderr");
    assert!(stderr.contains("missing.jsonl"), "{stderr}");
    assert!(out.stdout.is_empty(), "a missing file prints no report");
}

#[test]
fn positional_args_generate_the_session_at_the_given_size() {
    // Explicit words run the positional mode at that scale and size.
    let jig = Jig::new();
    let out = run(&jig.exe(), &["0", "60", "12"]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{stderr}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let first = stdout.lines().next().unwrap_or_default();
    assert!(first.contains("scale 0"), "{first}");
    assert!(first.contains("60x12"), "{first}");
    assert!(!first.contains("160x48"), "{first}");
}

#[test]
fn no_word_runs_the_session_as_it_always_ran() {
    // No word is scale 1 at 160 by 48, as the jig ran before the open
    // mode existed.
    let jig = Jig::new();
    let out = run(&jig.exe(), &[]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{stderr}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let first = stdout.lines().next().unwrap_or_default();
    assert!(first.contains("scale 1"), "{first}");
    assert!(first.contains("160x48"), "{first}");
    assert!(!first.contains("scale 0"), "{first}");
}
