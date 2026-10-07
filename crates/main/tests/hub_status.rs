//! Binary-level tests of `fiber hub status`, the installed hub
//! (`fiber hub serve --installed`) and `fiber hub install`'s parse errors
//! (`docs/invocation.md`, "The hub"). None of them reaches a service
//! manager: `HOME` is the test's temporary root.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use contract::clock::Clock;
use serde_json::Value;
use support::*;

/// How long the test waits between attempts to reach a hub still binding.
const RETRY: Duration = Duration::from_millis(20);

/// Every path under `root`, relative to it, sorted.
fn tree(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path.clone());
            }
            found.push(path.strip_prefix(root).unwrap().to_path_buf());
        }
    }
    found.sort();
    found
}

/// Connects to `run/hub` once the hub has bound it, retrying until the
/// test's deadline, and reads its `hub_hello`.
fn connect_when_bound(setup: &Setup) -> Socket {
    let socket = setup.hub_socket();
    loop {
        match UnixStream::connect(&socket) {
            Ok(stream) => {
                let client = Socket::from(setup.deadline, stream);
                let hello = recv(&client, "the installed hub's hub_hello");
                assert_eq!(hello["kind"], "hub_hello", "{hello}");
                return client;
            }
            Err(error) => {
                assert!(
                    !setup.deadline.left().is_zero(),
                    "waited until the deadline for the installed hub to bind run/hub: {error}"
                );
                SystemClock.sleep(RETRY);
            }
        }
    }
}

fn status_json(setup: &Setup) -> Value {
    let output = run_to_exit(
        setup.deadline,
        "fiber hub status --json",
        setup.fiber(&["hub", "status", "--json"]),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    serde_json::from_str(&stdout).unwrap()
}

#[test]
fn status_with_no_hub_prints_not_running_and_starts_none() {
    let setup = Setup::new();
    let output = run_to_exit(
        setup.deadline,
        "fiber hub status",
        setup.fiber(&["hub", "status"]),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "running: no\nversion: none\nport: none\nclients: 0\ndevices: none\ninstalled: no\n"
    );
    assert!(output.stderr.is_empty());
    assert!(
        !setup.hub_socket().exists(),
        "status started no hub: {:?}",
        tree(setup.root.path())
    );
}

#[test]
fn status_json_reports_a_running_hub_and_the_other_clients() {
    let setup = Setup::new();
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let status = status_json(&setup);
    assert_eq!(
        status,
        serde_json::json!({
            "running": true,
            "version": env!("CARGO_PKG_VERSION"),
            "port": null,
            "clients": 1,
            "devices": [],
            "installed": false,
        })
    );
    drop(client);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.kill("TERM");
    assert_eq!(hub.wait().code(), Some(143));
}

#[test]
fn the_installed_hub_binds_answers_status_and_exits_143_on_sigterm() {
    let setup = Setup::new();
    let hub = HubProc::spawn_command(
        setup.deadline,
        &mut setup.fiber(&["hub", "serve", "--installed"]),
    );
    let client = connect_when_bound(&setup);
    client.send(r#"{"id":"c_status","command":"status"}"#);
    let answer = recv_reply(&client, "the status answer");
    assert_eq!(answer["kind"], "command_accepted", "{answer}");
    assert_eq!(answer["payload"]["command_id"], "c_status", "{answer}");
    assert_eq!(answer["payload"]["result"]["running"], true, "{answer}");
    assert_eq!(
        answer["payload"]["result"]["fiber_version"],
        env!("CARGO_PKG_VERSION"),
        "{answer}"
    );
    drop(client);
    hub.kill("TERM");
    assert_eq!(hub.wait().code(), Some(143));
}

#[test]
fn install_with_port_0_is_a_usage_error_that_writes_nothing() {
    let setup = Setup::new();
    let before = tree(setup.root.path());
    let output = run_to_exit(
        setup.deadline,
        "fiber hub install --port 0",
        setup.fiber(&["hub", "install", "--port", "0"]),
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.starts_with("fiber: "), "{stderr}");
    assert!(output.stdout.is_empty());
    assert_eq!(tree(setup.root.path()), before);
}
