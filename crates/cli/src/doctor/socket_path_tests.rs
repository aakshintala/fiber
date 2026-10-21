//! Doctor with a `FIBER_HOME` too long for a socket: the session sentence
//! blocks the report, and the hub line names the cause instead of the
//! operating system's error.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use contract::ErrorCode;
use serde_json::{Value, json};

use super::clean;
use super::startup::startup;
use crate::hub_status::{Diagnostic, diagnose};

/// The session-start socket limit, by platform: `sun_path` less its
/// terminating byte. The padding below is computed from it at runtime so
/// the home fails past the limit on every platform.
const SOCKET_PATH_MAX: usize = if cfg!(target_os = "macos") { 103 } else { 107 };

/// The longest session id the code mints: `s_` plus 16 hex.
const LONGEST_SESSION_ID: &str = "s_aaaaaaaaaaaaaaaa";

/// A home with a workspace under it, ready to start a session.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new(prefix: &str) -> Self {
        let root = fakes::TempDir::new(prefix);
        fs::create_dir_all(root.path().join("workspace")).unwrap();
        Self { root }
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    /// Hand-places extension `fake` with model `m` on the default env key
    /// source, and selects it: a home built by `place` starts sessions.
    fn place_ready(&self, home: &Path) {
        let dir = home.join("extensions").join("fake");
        fs::create_dir_all(dir.join("providers")).unwrap();
        fs::write(
            dir.join("extension.json"),
            json!({"name": "fake", "version": "v1.0.0", "fiber": "0.1.0", "api": 1,
                   "providers": {"fake": ["http://127.0.0.1:1/v1"]}})
            .to_string(),
        )
        .unwrap();
        fs::write(
            dir.join("providers").join("fake.json"),
            json!({"name": "fake", "credential": {"env": "FIBER_TEST_FAKE_KEY"}, "models": [
                {"id": "m", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:1/v1", "context_window": 1000},
            ]})
            .to_string(),
        )
        .unwrap();
        fakes::write_record(&dir);
        fs::write(
            home.join("config.json"),
            json!({"model": "fake/m"}).to_string(),
        )
        .unwrap();
    }

    fn env() -> impl Fn(&str) -> Option<String> {
        |name| (name == "FIBER_TEST_FAKE_KEY").then(|| "k".to_owned())
    }
}

/// The session sentence for a home whose socket is too long.
fn expected_sentence() -> String {
    format!(
        "FIBER_HOME is too long: a session's socket path must fit in \
         {SOCKET_PATH_MAX} bytes. Set FIBER_HOME to a shorter path."
    )
}

/// A home whose `run/hub` is past the limit, so the longer session socket
/// is past it too.
fn overlong_home(root: &Path) -> PathBuf {
    let base = root.to_string_lossy().len();
    let pad = SOCKET_PATH_MAX - base - "/run/hub".len() + 1;
    root.join("p".repeat(pad))
}

/// A home whose longest session socket is exactly the limit.
fn at_limit_home(root: &Path) -> PathBuf {
    let base = root.to_string_lossy().len();
    let pad = SOCKET_PATH_MAX - base - "/run/".len() - LONGEST_SESSION_ID.len();
    root.join("q".repeat(pad))
}

#[test]
fn an_overlong_home_blocks_startup_with_the_session_sentence() {
    assert_eq!(LONGEST_SESSION_ID.len(), 18);
    let setup = Setup::new("fiber-doctor-socket-path");
    let home = overlong_home(setup.root.path());
    fs::create_dir_all(&home).unwrap();
    setup.place_ready(&home);

    let checked = startup(&home, &setup.workspace(), &[], &Setup::env(), &|_| false);
    let expected = expected_sentence();
    // The sentence carries no control character, so cleaning keeps it
    // byte for byte.
    assert_eq!(clean(&expected), expected);
    let Some(blocker) = checked.blocker else {
        panic!("an overlong home blocks every session");
    };
    assert_eq!(blocker.code, ErrorCode::Usage);
    assert!(!blocker.key);
    assert_eq!(blocker.line, expected);
    assert_eq!(checked.lines, vec![expected]);
}

#[test]
fn an_overlong_home_names_fiber_home_in_the_hub_line() {
    let setup = Setup::new("fiber-doctor-socket-hub");
    let home = overlong_home(setup.root.path());
    fs::create_dir_all(&home).unwrap();
    setup.place_ready(&home);

    let clock = fakes::clock::FakeClock::new();
    let Diagnostic::Failed(failure) = diagnose(&home, &*clock, Duration::from_secs(5)) else {
        panic!("an overlong home fails the hub check instead of the connect");
    };
    assert_eq!(failure.code, ErrorCode::Usage);
    assert_eq!(failure.message, expected_sentence());
}

#[test]
fn an_overlong_home_reports_exit_non_zero() {
    let setup = Setup::new("fiber-doctor-socket-exit");
    let home = overlong_home(setup.root.path());
    fs::create_dir_all(&home).unwrap();
    setup.place_ready(&home);
    let workspace = setup.workspace();

    let clock = fakes::clock::FakeClock::new();
    let inputs = super::Inputs {
        home: &home,
        workspace: &workspace,
        version: "0.0.0 (test)",
        now: SystemTime::UNIX_EPOCH,
        env: &Setup::env(),
        has_userinfo: &|_| false,
        hub: &|home, clock| super::probe(home, clock),
        clock: &*clock,
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    match super::run(&inputs, &mut out, &mut err) {
        Ok(can_start) => assert!(!can_start, "an overlong home cannot start a session"),
        Err(error) => panic!("the report itself fails: {error:?}"),
    }
    let report = String::from_utf8(out).unwrap();
    assert!(report.contains(&expected_sentence()), "{report:?}");
    assert!(
        report.contains(&format!("hub: unknown ({})", expected_sentence())),
        "{report:?}"
    );
}

#[test]
fn a_home_at_the_limit_still_starts() {
    let setup = Setup::new("fiber-doctor-socket-edge");
    let home = at_limit_home(setup.root.path());
    fs::create_dir_all(&home).unwrap();
    setup.place_ready(&home);

    let checked = startup(&home, &setup.workspace(), &[], &Setup::env(), &|_| false);
    assert!(
        checked.blocker.is_none(),
        "{:?}",
        checked.blocker.map(|blocker| blocker.line)
    );
    let clock = fakes::clock::FakeClock::new();
    assert!(
        matches!(
            diagnose(&home, &*clock, Duration::from_secs(5)),
            Diagnostic::Absent
        ),
        "a limit-length hub socket probes absent, never failed"
    );
}
