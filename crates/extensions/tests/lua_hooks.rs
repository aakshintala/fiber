//! A session's `after_tool` hooks under their timeouts, on the fake clock
//! (`docs/extensions.md`, "When a hook fails" and "How an extension runs").

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use common::{Setup, install, manifest, write};
use config::{Config, ProjectKey, Sources};
use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::CallStatus;
use contract::hook::{AfterToolAnswer, AfterToolCall, AfterToolOutcome, Hooks};
use extensions::SessionExtensions;
use fakes::clock::FakeClock;
use serde_json::Map;

/// The session's per-path lock, offered to `host.fs`: this test never
/// takes it, so it runs every call straight through.
struct NoLock;

impl contract::files::PathLock for NoLock {
    fn hold(&self, _path: &Path, run: &mut dyn FnMut()) {
        run();
    }

    fn hold_all(&self, _paths: &[PathBuf], run: &mut dyn FnMut()) {
        run();
    }
}

/// How long a test waits for a signal or an answer before failing. A hook
/// that is stopped returns on the fake clock, so this fires only when one
/// is never stopped.
const WAIT: Duration = Duration::from_secs(5);

/// When the hook cannot stop the VM, the caller waits 1 second more
/// (`docs/extensions.md`, "How an extension runs").
const GRACE: Duration = Duration::from_secs(1);

/// A hook that spins on its first call, after reading the signal module
/// `go_spin`, and appends `|<short>` on every later call.
fn spinning(on_failure: &str, timeout: u64, short: &str) -> String {
    format!(
        "local calls = 0\n\
         fiber.hook(\"after_tool\", {{ timeout = {timeout}, on_failure = \"{on_failure}\",\n\
           run = function(call)\n\
             calls = calls + 1\n\
             if calls == 1 then require(\"go_spin\") while true do end end\n\
             return {{ content = call.content .. \"|{short}\" }}\n\
           end }})\n"
    )
}

/// Installs `fiber.test/<short>` with the entry script `init`, and returns
/// its installed directory.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
fn installed(setup: &Setup, short: &str, init: &str) -> PathBuf {
    let name = format!("fiber.test/{short}");
    let src = setup.source(short, &manifest(&name), &[]);
    write(&src.join("init.lua"), init);
    install(&setup.home(), &src, "0.1.0").unwrap();
    setup.home().join("extensions").join(name.replace('/', "-"))
}

/// `require("go_spin")` in `dir` signals that the hook has started: the
/// loader opens the fifo for read, the writer here reports it and closes the
/// fifo, and the module reads empty.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
fn go_module(dir: &Path) -> mpsc::Receiver<()> {
    let path = dir.join("go_spin.lua");
    let made = std::process::Command::new("mkfifo").arg(&path).status();
    assert!(made.unwrap().success(), "mkfifo {path:?}");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let held = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        match tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
        drop(held);
    });
    rx
}

#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
fn load(setup: &Setup, overrides: &[&str], clock: Arc<FakeClock>) -> Arc<SessionExtensions> {
    let config = Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("p").unwrap(),
        overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
    })
    .unwrap();
    // On its own thread under `WAIT`: the fake clock never ends a wait the
    // runtime does not end itself.
    let home = setup.home();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
        let _sent = tx.send(SessionExtensions::load(&home, &config, clock, locks));
    });
    Arc::new(
        rx.recv_timeout(WAIT)
            .expect("waited for the extensions to load"),
    )
}

/// Asks the hooks about a completed call whose output is `x`, on its own
/// thread.
fn ask(session: &Arc<SessionExtensions>) -> mpsc::Receiver<AfterToolAnswer> {
    let (tx, rx) = mpsc::channel();
    let session = Arc::clone(session);
    std::thread::spawn(move || {
        let arguments = Map::new();
        let answer = session.after_tool(&AfterToolCall {
            tool: "read",
            arguments: &arguments,
            status: CallStatus::Completed,
            content: "x",
            details: None,
            process: None,
        });
        match tx.send(answer) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    rx
}

/// Asks once with the first hook spinning: waits for the spin to start and
/// the caller to park at `timeout` plus the grace, moves the clock by
/// `timeout`, and returns the answer.
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
fn timed_out(
    session: &Arc<SessionExtensions>,
    clock: &Arc<FakeClock>,
    went: &mpsc::Receiver<()>,
    timeout: Duration,
) -> AfterToolAnswer {
    let asked = clock.now();
    let answer = ask(session);
    went.recv_timeout(WAIT)
        .expect("waited for the hook to start");
    assert!(
        clock.await_parked(asked + timeout + GRACE, WAIT),
        "waited for the caller to park at the hook's grace"
    );
    clock.advance(timeout);
    answer.recv_timeout(WAIT).expect("waited for the answer")
}

fn content(answer: &AfterToolAnswer) -> Option<&str> {
    match &answer.outcome {
        AfterToolOutcome::Changed { content, .. } => content.as_deref(),
        AfterToolOutcome::Unchanged | AfterToolOutcome::Withheld { .. } => None,
    }
}

#[test]
#[allow(clippy::indexing_slicing, reason = "the length is asserted first")]
#[allow(clippy::expect_used, reason = "a failure is the test's")]
fn a_non_blocking_hook_past_its_timeout_is_dropped_and_the_next_extension_still_runs() {
    let setup = Setup::new();
    let dir = installed(&setup, "a", &spinning("non-blocking", 50, "a"));
    installed(
        &setup,
        "b",
        "fiber.hook(\"after_tool\", { timeout = 1000, on_failure = \"blocking\",\n\
           run = function(call) return { content = call.content .. \"|b\" } end })\n",
    );
    let went = go_module(&dir);
    let clock = FakeClock::new();
    let session = load(&setup, &[], clock.clone());
    let answer = timed_out(&session, &clock, &went, Duration::from_millis(50));
    assert_eq!(content(&answer), Some("x|b"));
    assert_eq!(answer.changed_by, ["fiber.test/b"]);
    assert_eq!(answer.notices.len(), 1);
    assert_eq!(answer.notices[0].code, ErrorCode::HookFailed);
    assert_eq!(answer.notices[0].extension.as_deref(), Some("fiber.test/a"));
    assert!(
        answer.notices[0].message.contains("50 ms timeout"),
        "{}",
        answer.notices[0].message
    );
    // The hook stopped the loop and left the VM up: the next call runs it.
    let again = ask(&session)
        .recv_timeout(WAIT)
        .expect("waited for the second answer");
    assert_eq!(content(&again), Some("x|a|b"));
    assert!(again.notices.is_empty(), "{:?}", again.notices);
}

#[test]
#[allow(clippy::expect_used, reason = "a failure is the test's")]
fn a_blocking_hook_past_its_timeout_withholds_the_output() {
    let setup = Setup::new();
    let dir = installed(&setup, "a", &spinning("blocking", 50, "a"));
    let went = go_module(&dir);
    let clock = FakeClock::new();
    let session = load(&setup, &[], clock.clone());
    let answer = timed_out(&session, &clock, &went, Duration::from_millis(50));
    assert_eq!(
        answer.outcome,
        AfterToolOutcome::Withheld {
            extension: "fiber.test/a".into()
        }
    );
    assert_eq!(answer.changed_by, ["fiber.test/a"]);
}

#[test]
#[allow(clippy::expect_used, reason = "a failure is the test's")]
fn hook_timeout_ms_is_the_timeout_the_hook_is_stopped_at() {
    let setup = Setup::new();
    let dir = installed(&setup, "a", &spinning("blocking", 60_000, "a"));
    let went = go_module(&dir);
    let clock = FakeClock::new();
    let session = load(
        &setup,
        &["extensions.\"fiber.test/a\".hook_timeout_ms=20"],
        clock.clone(),
    );
    let answer = timed_out(&session, &clock, &went, Duration::from_millis(20));
    assert_eq!(
        answer.outcome,
        AfterToolOutcome::Withheld {
            extension: "fiber.test/a".into()
        }
    );
}
