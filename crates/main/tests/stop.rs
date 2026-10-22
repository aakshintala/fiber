//! Binary-level tests of top-level `fiber stop`
//! (`docs/invocation.md`, "Commands and flags"): the built `fiber` stops a
//! running hub and its sessions through their own shutdown, and a bare hub
//! `stop` leaves the sessions alone.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use fakes::ProviderServer;
use serde_json::Value;
use support::*;

impl Setup {
    /// Runs `fiber` with `args`, waiting under the test's [`Deadline`],
    /// and parses stdout's event lines into a [`Run`].
    #[track_caller]
    fn ask(&self, args: &[&str]) -> Run {
        // Ask resolves the project from the working directory, so it runs
        // in the workspace like the session it resumes.
        let mut command = self.fiber(args);
        command.current_dir(self.workspace());
        let output = run_to_exit(self.deadline, "fiber ask", command);
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines = stdout
            .lines()
            .filter(|line| !is_status(line))
            .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
            .collect();
        Run {
            code: output.status.code(),
            lines,
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

#[test]
fn stop_stops_the_session_and_the_hub_and_the_session_resumes() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, PROMPT);
    // The first model request lands only after the pre-send fsync, which
    // makes the appended first log line durable: without this wait the
    // stop below can land in the append-to-fsync window and the resume
    // finds no session.
    await_first_request(&server, setup.deadline);
    drop(client);

    let output = run_to_exit(setup.deadline, "fiber stop", setup.fiber(&["stop"]));
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Stopped 1 sessions and the hub.\n"
    );
    assert!(
        UnixStream::connect(setup.hub_socket()).is_err(),
        "the hub socket is gone"
    );
    assert!(
        UnixStream::connect(setup.session_socket(&session)).is_err(),
        "the session socket is gone"
    );
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    assert!(
        !hub.wait().success(),
        "the stopped hub exits as SIGTERM does"
    );

    let prefix = &session[..8];
    let second = setup.ask(&["ask", "--resume", prefix, "two"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert_eq!(second.session_id(), session);
    guard.wait_gone();
}

#[test]
fn a_bare_stop_leaves_the_session_socket_accepting() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, PROMPT);

    client.send(r#"{"id":"c_stop","command":"stop","args":{}}"#);
    let ack = recv_answer(&client, "c_stop", "the stop acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    assert!(
        ack["payload"].get("result").is_none(),
        "a bare stop answers with no result: {ack}"
    );
    drop(client);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    assert!(
        !hub.wait().success(),
        "a bare stop ends the hub as SIGTERM does"
    );

    let direct = Socket::connect(setup.deadline, &setup.session_socket(&session));
    close_session(&direct);
    drop(direct);
    assert!(
        !setup.session_socket(&session).exists(),
        "the closed session unlinked its socket"
    );
    guard.wait_gone();
}

#[test]
fn stop_with_a_dead_hub_starts_one_to_stop_the_orphaned_session() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, PROMPT);
    drop(client);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    let status = hub.kill_and_wait();
    assert!(!status.success());
    assert!(
        UnixStream::connect(setup.hub_socket()).is_err(),
        "the killed hub left no socket behind"
    );
    assert!(
        UnixStream::connect(setup.session_socket(&session)).is_ok(),
        "the orphaned session still accepts"
    );

    let output = run_to_exit(setup.deadline, "fiber stop", setup.fiber(&["stop"]));
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Stopped 1 sessions and the hub.\n"
    );
    assert!(
        UnixStream::connect(setup.session_socket(&session)).is_err(),
        "the session socket is gone"
    );
    guard.wait_gone();
}
