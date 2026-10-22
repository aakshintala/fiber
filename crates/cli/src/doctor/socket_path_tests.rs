//! Doctor with a `FIBER_HOME` too long for a socket: the session sentence
//! blocks the report, and the hub line names the cause instead of the
//! operating system's error.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use contract::ErrorCode;
use serde_json::json;

use super::clean;
use super::startup::startup;
use crate::hub_status::{Diagnostic, diagnose};

// The session-start socket limit is `doors::SOCKET_PATH_MAX`: `sun_path`
// less its terminating byte. The padding below is computed from it at
// runtime so the home fails past the limit on every platform.

/// A home with a workspace under it, ready to start a session.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new(prefix: &str) -> Self {
        // Socket paths are length-limited, so the home's root sits
        // under a short parent, never the roomy system temp dir.
        let root = fakes::TempDir::under(Path::new("/tmp"), prefix);
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
         {} bytes. Set FIBER_HOME to a shorter path.",
        doors::SOCKET_PATH_MAX
    )
}

/// A home whose `run/<file_name>` socket path is `socket_len` bytes long.
/// A `"hub"` socket past the limit pushes every longer session socket
/// past it too.
fn home_with(root: &Path, file_name: &str, socket_len: usize) -> PathBuf {
    let base = root.to_string_lossy().len();
    // `root` + `/` + pad + `/run/` + `file_name` is `socket_len` bytes.
    let pad = socket_len - base - 1 - "/run/".len() - file_name.len();
    let home = root.join("p".repeat(pad));
    assert_eq!(
        home.join("run").join(file_name).as_os_str().len(),
        socket_len
    );
    home
}

#[test]
fn an_overlong_home_blocks_startup_with_the_session_sentence() {
    let setup = Setup::new("fiber-doctor-socket-path");
    let home = home_with(setup.root.path(), "hub", doors::SOCKET_PATH_MAX + 1);
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
fn an_overlong_home_reports_exit_non_zero() {
    let setup = Setup::new("fiber-doctor-socket-exit");
    let home = home_with(setup.root.path(), "hub", doors::SOCKET_PATH_MAX + 1);
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
    // Every minted session id is `s_` plus 16 hex, so this longest form
    // stands in for all of them.
    let home = home_with(
        setup.root.path(),
        &doors::mint("s_"),
        doors::SOCKET_PATH_MAX,
    );
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
