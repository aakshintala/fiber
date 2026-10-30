//! The main journeys (#3667): screen tests that drive the release binary
//! the backstop built, through the existing screen harness, against the
//! fake provider. Each journey walks one path a person walks, from start
//! to finish, and asserts what the screen shows at every step
//! (`docs/testing.md`, "Journeys").
//!
//! The binary runs after merge (`docs/ci.md`, "The journey suite"), never
//! in the gate: `crates/main/Cargo.toml` declares it with `test = false`,
//! so only an explicit `--test main_journeys` builds, lints or runs it.
//! Every run names its binary in `FIBER_JOURNEY_BIN` and its failures file
//! in `FIBER_JOURNEY_FAILURES`; the caller creates the failures file empty
//! beforehand, and each failed journey appends one line naming itself.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::path::PathBuf;

/// The release binary under test: the path in `FIBER_JOURNEY_BIN`. Fails
/// naming the variable when unset or not executable, and never falls back
/// to a debug build: this is the file's only binary source.
fn journey_bin() -> PathBuf {
    let bin = std::env::var("FIBER_JOURNEY_BIN").unwrap_or_else(|_| {
        panic!("FIBER_JOURNEY_BIN is not set: the journey suite drives the release binary it names")
    });
    let path = PathBuf::from(&bin);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let executable = path.is_file()
            && path
                .metadata()
                .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0);
        assert!(executable, "FIBER_JOURNEY_BIN is not executable: {bin}");
    }
    #[cfg(not(unix))]
    {
        assert!(path.is_file(), "FIBER_JOURNEY_BIN is not executable: {bin}");
    }
    path
}

/// Records one failed journey: prints its name to stderr first, then
/// appends it to the caller-created `FIBER_JOURNEY_FAILURES` file. On an
/// open or write failure panics naming the path, after the stderr print,
/// so the name is never lost silently.
fn record_failure(name: &str) {
    eprintln!("Journey failure: {name}");
    let path = std::env::var("FIBER_JOURNEY_FAILURES").unwrap_or_else(|_| {
        panic!(
            "FIBER_JOURNEY_FAILURES is not set: the caller creates the empty failures file before the run"
        )
    });
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap_or_else(|error| {
            panic!("Journey failure: {name}: cannot append to FIBER_JOURNEY_FAILURES at {path}: {error}")
        });
    std::io::Write::write_all(&mut file, format!("{name}\n").as_bytes()).unwrap_or_else(|error| {
        panic!("Journey failure: {name}: cannot write to FIBER_JOURNEY_FAILURES at {path}: {error}")
    });
}

/// Runs one journey, recording its failure under its own test name: runs
/// `body` under `catch_unwind`, and on failure appends `name` to the
/// failures file, then resumes the unwind so nextest still reports the
/// failure itself. Never truncates the shared file.
fn run_journey(name: &str, body: impl FnOnce() + std::panic::UnwindSafe) {
    if let Err(payload) = std::panic::catch_unwind(body) {
        record_failure(name);
        std::panic::resume_unwind(payload);
    }
}
