//! Binary-level tests of the hub's session environment
//! (`docs/invocation.md`, "A session's environment"): the built `fiber`
//! runs `hub serve` with a fake login shell, and every session the hub
//! starts runs with what that shell ended with.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::*;

/// A fake login shell under its own temporary directory, covered by a
/// command-line watchdog: the capture detaches it outside the hub's
/// process group.
struct FakeShell {
    /// Holds the directory: drop kills the group.
    _held: fakes::TempDir,
    /// The fake `$SHELL`.
    shell: PathBuf,
    /// Covers the shell by its command line.
    _watchdog: Watchdog,
}

impl FakeShell {
    /// Writes `body` as the fake shell: `$1` is `-l`, `$2` is `-i`,
    /// `$3` is `-c` and `$4` is the framed script.
    fn new(body: &str) -> Self {
        let held = fakes::TempDir::new("hs");
        let dir = held.path().to_path_buf();
        let shell = fakes::script(&dir, "shell", body);
        let pattern = dir.to_string_lossy().into_owned();
        let watchdog = Watchdog::matching(&pattern);
        Self {
            _held: held,
            shell,
            _watchdog: watchdog,
        }
    }
}

/// The successful capture's body: the provider key, the test's `PATH`, a
/// mark, another home for the overlay to replace, and the check-run nonce
/// when one is there, then the framed script.
fn success_body(other_home: &Path) -> String {
    let nonce = match fakes::check_run() {
        Some((name, value)) => format!("export {name}='{}'\n", value.to_string_lossy()),
        None => String::new(),
    };
    format!(
        "export FIBER_TEST_FAKE_KEY='sk-test'\nexport PATH='{}'\nexport LOGIN_MARK='1'\nexport FIBER_HOME='{}'\n{}exec /bin/sh -c \"$4\"\n",
        std::env::var_os("PATH")
            .unwrap_or_default()
            .to_string_lossy(),
        other_home.display(),
        nonce,
    )
}

/// The check-run nonce `connect_hub` gives the hub when the run has
/// none, so the nonce assertion bites outside `scripts/check` too.
const TEST_NONCE: &str = "hub-environment-test";

/// Connects to the hub, starting `fiber hub serve` with `HUB_ONLY` set
/// and `shell` as `$SHELL` when given. Outside `scripts/check` the hub
/// gets a nonce of its own, so the nonce assertion bites there too. The
/// starter records the hub for the caller to kill and wait.
#[track_caller]
fn connect_hub(setup: &Setup, hub: &Arc<Mutex<Option<HubProc>>>, shell: Option<&Path>) -> Socket {
    let slot = Arc::clone(hub);
    let deadline = setup.deadline;
    let mut serve = setup.fiber(&["hub", "serve"]);
    serve.env("HUB_ONLY", "1");
    if fakes::check_run().is_none() {
        serve.env("FIBER_CHECK_RUN", TEST_NONCE);
    }
    if let Some(shell) = shell {
        serve.env("SHELL", shell);
    }
    let home = setup.home();
    let (done, connected) = mpsc::channel();
    thread::spawn(move || {
        let mut start = move || {
            *slot.lock().unwrap() = Some(HubProc::spawn_command(deadline, &mut serve));
            Ok(())
        };
        done.send(doors::hub::connect(&home, &mut start, &SystemClock))
            .unwrap();
    });
    let connected = match deadline.recv(&connected) {
        Ok(connected) => connected.unwrap(),
        Err(_) => panic!("waited for the hub to start and say hub_hello"),
    };
    let hello = serde_json::to_value(&connected.1).unwrap();
    assert_eq!(hello["kind"], "hub_hello");
    Socket::from(deadline, connected.0)
}

/// The log of session `id`, wherever its project is.
fn session_log(setup: &Setup, id: &str) -> Vec<Value> {
    let projects = setup.home().join("projects");
    let log = fs::read_dir(&projects)
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .path()
                .join("sessions")
                .join(id)
                .join("events.jsonl")
        })
        .find(|log| log.is_file())
        .expect("the session's log exists");
    fs::read_to_string(log)
        .map(|text| complete_lines(&text))
        .unwrap()
}

/// The `variables` of session `id`'s `session_started` line.
fn started_variables(setup: &Setup, id: &str) -> Value {
    session_log(setup, id)
        .into_iter()
        .find(|line| line["kind"] == "session_started")
        .expect("the session wrote session_started")["payload"]["variables"]
        .clone()
}

/// The names of `variables`, sorted as recorded.
fn names_of(variables: &Value) -> Vec<String> {
    variables["names"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap().to_owned())
        .collect()
}

/// Ends the test's session, hub and every session in the guarded
/// workspace: the close is accepted, which proves a relayed command still
/// reaches the session, then everything is reaped.
fn finish(hub: &Arc<Mutex<Option<HubProc>>>, guard: SessionGuard, client: Socket, session: &str) {
    client.send(&format!(
        "{{\"id\":\"c_close\",\"session_id\":\"{session}\",\"command\":\"close\"}}"
    ));
    until(&client, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
    drop(client);
    hub.lock()
        .unwrap()
        .take()
        .expect("the hub ran")
        .kill_and_wait();
    guard.wait_gone();
}

#[test]
fn a_session_runs_with_the_login_shell_capture() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let other_home = setup.root.path().join("other-home");
    let fake = FakeShell::new(&success_body(&other_home));
    let hub = Arc::new(Mutex::new(None));
    let client = connect_hub(&setup, &hub, Some(&fake.shell));
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, PROMPT);
    // The start is accepted, which proves the session bound in the hub's
    // home although the capture set another `FIBER_HOME`.
    assert!(
        setup.session_socket(&session).exists(),
        "the session bound in the hub's home"
    );
    subscribe(&client, &session);
    until(&client, "the first turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    let variables = started_variables(&setup, &session);
    assert_eq!(variables["source"], "login_shell");
    assert_eq!(
        variables["path"].as_str(),
        Some(
            std::env::var_os("PATH")
                .unwrap_or_default()
                .to_string_lossy()
                .as_ref()
        )
    );
    let names = names_of(&variables);
    assert!(names.contains(&"LOGIN_MARK".to_owned()), "{names:?}");
    assert!(names.contains(&"FIBER_HOME".to_owned()), "{names:?}");
    assert!(!names.contains(&"HUB_ONLY".to_owned()), "{names:?}");
    // After the session exits, a relayed command resumes it with the
    // hub's current environment.
    client.send(&format!(
        "{{\"id\":\"c_close\",\"session_id\":\"{session}\",\"command\":\"close\"}}"
    ));
    until(&client, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
    // Until the session process is gone it still holds its connections
    // and answers a command `closing`, so the prompt waits for it.
    fakes::try_matching_exits(&workspace, setup.deadline).unwrap_or_else(|err| {
        panic!("waited until the deadline for the session process to exit: {err}")
    });
    client.send(&format!(
        "{{\"id\":\"c_two\",\"session_id\":\"{session}\",\"command\":\"prompt\",\"args\":{{\"content\":[{{\"type\":\"text\",\"text\":\"two\"}}]}}}}"
    ));
    let accepted = until(&client, "the resumed prompt is accepted", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_two"
    });
    assert_eq!(
        accepted.last().unwrap()["kind"],
        "command_accepted",
        "{accepted:?}"
    );
    // The resumed turn runs to completion: the relay replays the first
    // turn's lines, so the log on disk, not the socket, proves the second
    // turn ran.
    loop {
        let log = session_log(&setup, &session);
        let completed = log
            .iter()
            .filter(|line| line["kind"] == "turn_completed")
            .count();
        if completed >= 2 {
            assert_eq!(
                log.iter()
                    .filter(|line| line["kind"] == "text_completed"
                        && line["payload"]["text"] == "Hello.")
                    .count(),
                2,
                "both turns answer"
            );
            break;
        }
        if setup.deadline.left().is_zero() {
            panic!("waited for the resumed turn to complete");
        }
        thread::yield_now();
    }
    finish(&hub, guard, client, &session);
}

#[test]
fn a_session_runs_with_the_inherited_environment_when_the_capture_fails() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let fake = FakeShell::new("exit 1\n");
    let hub = Arc::new(Mutex::new(None));
    let client = connect_hub(&setup, &hub, Some(&fake.shell));
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, PROMPT);
    subscribe(&client, &session);
    until(&client, "the first turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    let variables = started_variables(&setup, &session);
    assert_eq!(variables["source"], "inherited");
    assert!(names_of(&variables).contains(&"HUB_ONLY".to_owned()));
    assert!(
        setup
            .hub_log()
            .contains("because the shell exited before printing its environment"),
        "the hub records why: {}",
        setup.hub_log()
    );
    finish(&hub, guard, client, &session);
}

#[test]
fn a_session_without_a_shell_runs_with_the_inherited_environment() {
    let setup = Setup::new();
    let printenv = stream(&[support::function_call(
        "call_1",
        "shell",
        // One variable: BSD `printenv` (macOS) prints only its first
        // argument, so naming two can never show the second there.
        &json!({"command": "printenv FIBER_CHECK_RUN"}),
    )]);
    let server = ProviderServer::start([printenv, hello()]).unwrap();
    setup.provider(&server);
    // A standing rule allows the command without asking.
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "printenv"})
        ),
    )
    .unwrap();
    let hub = Arc::new(Mutex::new(None));
    let client = connect_hub(&setup, &hub, None);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, PROMPT);
    subscribe(&client, &session);
    let turn = until(&client, "the shell call's turn", |line| {
        line["kind"] == "turn_completed"
    });
    let completed = turn
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .expect("the shell call completed");
    let output = completed["payload"]["content"][0]["text"]
        .as_str()
        .expect("the call's output text");
    let printed: Vec<&str> = output.lines().collect();
    // The run's nonce when one is there, else the one `connect_hub`
    // gave the hub: the assertion bites either way.
    let nonce = match fakes::check_run() {
        Some((_, value)) => value.to_string_lossy().into_owned(),
        None => TEST_NONCE.to_owned(),
    };
    assert!(
        printed.contains(&nonce.as_str()),
        "the call saw the check-run nonce: {output:?}"
    );
    let variables = started_variables(&setup, &session);
    assert_eq!(variables["source"], "inherited");
    let names = names_of(&variables);
    assert!(names.contains(&"HUB_ONLY".to_owned()), "{names:?}");
    assert!(names.contains(&"FIBER_CHECK_RUN".to_owned()), "{names:?}");
    assert!(
        setup.hub_log().contains("because SHELL is not set"),
        "the hub records why: {}",
        setup.hub_log()
    );
    finish(&hub, guard, client, &session);
}

/// The successful capture's body exporting `mark`, for the refresh tests:
/// a session started after a refresh records the new mark's name.
fn mark_body(other_home: &Path, mark: &str) -> String {
    let nonce = match fakes::check_run() {
        Some((name, value)) => format!("export {name}='{}'\n", value.to_string_lossy()),
        None => String::new(),
    };
    format!(
        "export FIBER_TEST_FAKE_KEY='sk-test'\nexport PATH='{}'\nexport {mark}='1'\nexport FIBER_HOME='{}'\n{}exec /bin/sh -c \"$4\"\n",
        std::env::var_os("PATH")
            .unwrap_or_default()
            .to_string_lossy(),
        other_home.display(),
        nonce,
    )
}

impl FakeShell {
    /// Rewrites the fake shell's body at the same path: the hub's next
    /// capture runs the new body.
    fn rewrite(&self, body: &str) {
        fakes::script(self._held.path(), "shell", body);
    }
}

/// Waits under the test's [`Deadline`] until `socket` is gone, naming
/// `what`: the hub holds the `run/` flock until `serve` returns, after
/// unlinking the socket, so a returned flock means the hub is gone.
#[track_caller]
fn until_absent(deadline: Deadline, socket: &Path, what: &str) {
    let run = socket.parent().map(Path::to_owned);
    let socket = socket.to_owned();
    let what = what.to_owned();
    bounded(deadline, &what.clone(), move || {
        if let Some(run) = run
            && let Ok(file) = File::open(&run)
        {
            file.lock().unwrap();
        }
        assert!(
            !socket.exists(),
            "{what}: the hub exited but left its socket"
        );
    });
}

#[test]
fn hub_refresh_rebuilds_the_environment_for_new_sessions() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let other_home = setup.root.path().join("other-home");
    let fake = FakeShell::new(&mark_body(&other_home, "REFRESH_MARK_ONE"));
    let hub = Arc::new(Mutex::new(None));
    let client = connect_hub(&setup, &hub, Some(&fake.shell));
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    // The first session runs with the first capture.
    let first = start_session(&client, &workspace, PROMPT);
    subscribe(&client, &first);
    until(&client, "the first turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    let names = names_of(&started_variables(&setup, &first));
    assert!(names.contains(&"REFRESH_MARK_ONE".to_owned()), "{names:?}");
    // Its close is accepted, so nothing holds the workspace for the
    // sessions after the refresh.
    client.send(&format!(
        "{{\"id\":\"c_close\",\"session_id\":\"{first}\",\"command\":\"close\"}}"
    ));
    until(&client, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
    // A rewritten shell and `fiber hub refresh`: exit 0 with the login
    // shell's line.
    fake.rewrite(&mark_body(&other_home, "REFRESH_MARK_TWO"));
    let output = run_to_exit(
        setup.deadline,
        "fiber hub refresh",
        setup.fiber(&["hub", "refresh"]),
    );
    assert!(
        output.status.success(),
        "refresh exits 0: {:?}",
        output.status
    );
    let out = String::from_utf8(output.stdout).unwrap();
    assert!(
        out.contains("sessions now start with the login shell's environment"),
        "{out:?}"
    );
    // A session started after it records the new mark, not the old one.
    let second = start_session(&client, &workspace, PROMPT);
    subscribe(&client, &second);
    until(&client, "the second turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    let variables = started_variables(&setup, &second);
    assert_eq!(variables["source"], "login_shell");
    let names = names_of(&variables);
    assert!(names.contains(&"REFRESH_MARK_TWO".to_owned()), "{names:?}");
    assert!(!names.contains(&"REFRESH_MARK_ONE".to_owned()), "{names:?}");
    finish(&hub, guard, client, &second);
}

#[test]
fn hub_refresh_starts_a_hub_when_none_runs() {
    let setup = Setup::new();
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"hub": {"idle_exit_ms": 200}}),
    );
    let other_home = setup.root.path().join("other-home");
    let fake = FakeShell::new(&mark_body(&other_home, "REFRESH_MARK_ONE"));
    // No hub runs: `fiber hub refresh` starts one through the connect
    // every client uses, with the fake shell for its first capture.
    let mut command = setup.fiber(&["hub", "refresh"]);
    command.env("SHELL", &fake.shell);
    let output = run_to_exit(setup.deadline, "fiber hub refresh", command);
    assert!(
        output.status.success(),
        "refresh exits 0: {:?}",
        output.status
    );
    let out = String::from_utf8(output.stdout).unwrap();
    assert!(
        out.contains("sessions now start with the login shell's environment"),
        "{out:?}"
    );
    // The hub it started idles out soon after the refresh's connection
    // closes, so none lingers past the test.
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::support::complete_lines;

    #[test]
    fn complete_lines_drops_a_trailing_partial_line() {
        assert!(complete_lines("").is_empty());
        assert!(complete_lines("{\"a\":1}").is_empty());
        assert_eq!(complete_lines("{\"a\":1}\n"), [json!({"a": 1})]);
        assert_eq!(complete_lines("{\"a\":1}\n{\"b\":"), [json!({"a": 1})]);
        assert_eq!(
            complete_lines("{\"a\":1}\n{\"b\":2}\n"),
            [json!({"a": 1}), json!({"b": 2})]
        );
    }
}
