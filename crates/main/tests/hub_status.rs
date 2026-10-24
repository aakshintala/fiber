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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use support::*;

/// Connects to `run/hub` once the hub has bound it, through
/// `doors::hub::connect`'s own retry on the process clock: the retry sleeps
/// in product code, the test only receives it with the one deadline and
/// never sleeps itself.
fn connect_when_bound(setup: &Setup) -> Socket {
    let deadline = setup.deadline;
    let (stream, hello) = loop {
        let home = setup.home();
        match bounded(deadline, "the installed hub to bind run/hub", move || {
            // The installed hub is already spawned; the starter starts
            // nothing and lets the retry wait for its socket.
            let mut start = || -> std::io::Result<()> { Ok(()) };
            doors::hub::connect(&home, &mut start, &SystemClock)
        }) {
            Ok(hub) => break hub,
            Err(error) => {
                assert!(
                    !deadline.left().is_zero(),
                    "waited until the deadline for the installed hub to bind run/hub: {error}"
                );
            }
        }
    };
    assert_eq!(hello.kind, "hub_hello");
    Socket::from(deadline, stream)
}

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

/// A fake hub on the home's `run/hub`: it says `hub_hello`, reads the one
/// command line so the close or answer below cannot race the handshake,
/// then runs `then`.
fn fake_status_hub(
    setup: &Setup,
    then: impl FnOnce(std::os::unix::net::UnixStream) + Send + 'static,
) {
    use std::io::{BufRead, Write};
    let socket = setup.hub_socket();
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let hello = serde_json::json!({
            "kind": "hub_hello", "ts": 1,
            "schema_version": contract::SCHEMA_VERSION, "payload": {},
        });
        writeln!(stream, "{hello}").unwrap();
        let mut line = String::new();
        std::io::BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        then(stream);
    });
}

#[test]
fn status_with_an_invalid_hub_port_is_a_config_error() {
    let setup = Setup::new();
    fs::write(
        setup.home().join("config.json"),
        r#"{"hub": {"port": 70000}}"#,
    )
    .unwrap();
    let output = run_to_exit(
        setup.deadline,
        "fiber hub status",
        setup.fiber(&["hub", "status"]),
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "fiber: hub.port in {} is not a port: 70000\n",
            setup.home().join("config.json").display()
        )
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn status_with_an_unreadable_devices_dir_is_an_io_error() {
    let setup = Setup::new();
    fs::create_dir_all(setup.home().join("hub")).unwrap();
    fs::write(setup.home().join("hub/devices"), b"x").unwrap();
    let output = run_to_exit(
        setup.deadline,
        "fiber hub status",
        setup.fiber(&["hub", "status"]),
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "fiber: reading {}: Not a directory (os error 20)\n",
            setup.home().join("hub/devices").display()
        )
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn status_reports_a_hub_that_closes_before_answering() {
    let setup = Setup::new();
    fake_status_hub(&setup, drop);
    let output = run_to_exit(
        setup.deadline,
        "fiber hub status",
        setup.fiber(&["hub", "status"]),
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "fiber: the hub closed the connection before answering status\n"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn status_reports_a_rejection_with_its_code() {
    let setup = Setup::new();
    fake_status_hub(&setup, |mut stream| {
        use std::io::Write;
        let rejected = serde_json::json!({
            "kind": "command_rejected", "ts": 1,
            "schema_version": contract::SCHEMA_VERSION,
            "payload": {"command_id": "c_status", "code": "invalid_arguments", "message": "nope"},
        });
        writeln!(stream, "{rejected}").unwrap();
    });
    let output = run_to_exit(
        setup.deadline,
        "fiber hub status",
        setup.fiber(&["hub", "status"]),
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "fiber: nope\n");
    assert!(output.stdout.is_empty());
}

#[test]
fn status_reports_an_answer_missing_its_fields() {
    let setup = Setup::new();
    fake_status_hub(&setup, |mut stream| {
        use std::io::Write;
        let bare = serde_json::json!({
            "kind": "command_accepted", "ts": 1,
            "schema_version": contract::SCHEMA_VERSION,
            "payload": {"command_id": "c_status", "result": {"running": true}},
        });
        writeln!(stream, "{bare}").unwrap();
    });
    let output = run_to_exit(
        setup.deadline,
        "fiber hub status",
        setup.fiber(&["hub", "status"]),
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "fiber: the hub's answer to status has no fiber_version or clients\n"
    );
    assert!(output.stdout.is_empty());
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
