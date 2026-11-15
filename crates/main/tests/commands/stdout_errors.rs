//! Closed stdout handling for attach, session copy and list output.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{self, BufRead, BufReader};
use std::process::Stdio;

use crate::support::{self, Setup};
use fakes::{ProviderServer, Watchdog};

use crate::hub_start;
use serde_json::{Value, json};

fn installed_hub(setup: &Setup) -> hub_start::Served {
    hub_start::serve_installed(
        setup,
        hub_start::ATTEMPTS,
        &mut hub_start::free_port,
        &mut |port| {
            let path = setup.home().join("config.json");
            let mut config: Value = fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or(json!({}));
            let root = config.as_object_mut().unwrap();
            let hub = root.entry("hub").or_insert_with(|| json!({}));
            hub.as_object_mut()
                .unwrap()
                .insert("port".to_owned(), json!(port));
            support::write_json(&path, &config);
            setup.fiber(&["hub", "serve", "--installed"])
        },
    )
}

fn run_with_hub(args: &[&str], stdout: Stdio) -> std::process::Output {
    let setup = Setup::new();
    let served = installed_hub(&setup);
    let mut command = setup.fiber(args);
    command.stdout(stdout);
    let output = support::run_to_exit(setup.deadline, "command with redirected stdout", command);
    drop(served.hub);
    served.process.kill("TERM");
    assert_eq!(served.process.wait().code(), Some(143));
    output
}

fn ask_with_stdout(stdout: Stdio) -> std::process::Output {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&server);
    let mut command = setup.fiber(&["ask", "say hello"]);
    command.stdout(stdout);
    support::run_to_exit(setup.deadline, "ask with redirected stdout", command)
}

fn live_attach_with_stdout(stdout: Stdio) -> std::process::Output {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&server);
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "session": {"idle_exit_ms": 3600000}}),
    );

    let id = "s_0123456789abcdef";
    let workspace = setup.workspace();
    let mut holder = setup.fiber(&[
        "session",
        "--id",
        id,
        "--workspace",
        workspace.to_str().unwrap(),
    ]);
    holder.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = holder.spawn().unwrap();
    let group = child.id();
    let watchdog = Watchdog::group(group);
    let holder_stdout = child.stdout.take().unwrap();
    let ready = support::bounded(
        setup.deadline,
        "the session's first stdout line",
        move || {
            let mut reader = BufReader::new(holder_stdout);
            let mut line = String::new();
            reader.read_line(&mut line).map(|_| line)
        },
    )
    .unwrap();
    assert!(!ready.is_empty(), "the session emitted its startup line");
    drop(support::Socket::connect(
        setup.deadline,
        &setup.session_socket(id),
    ));

    let mut command = setup.fiber(&["ask", "--resume", id, "hello"]);
    command.current_dir(&workspace);
    command.stdout(stdout);
    let output = support::run_to_exit(setup.deadline, "attach to a live session", command);

    fakes::kill_group(group, "TERM").unwrap();
    let status = support::bounded(setup.deadline, "the live session to exit", move || {
        let mut child = child;
        child.wait()
    })
    .unwrap();
    assert_eq!(status.code(), Some(143));
    assert!(!fakes::group_alive(group));
    watchdog.stand_down(setup.deadline.cleanup_phase());
    output
}

fn closed_stdout(args: &[&str]) -> std::process::Output {
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    run_with_hub(args, Stdio::from(writer))
}

#[test]
fn attach_closed_stdout_exits_quietly() {
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    let output = live_attach_with_stdout(Stdio::from(writer));
    assert_eq!(output.status.code(), Some(0));
    assert!(
        output.stderr.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn session_copy_closed_stdout_exits_quietly() {
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    let output = ask_with_stdout(Stdio::from(writer));
    assert_eq!(output.status.code(), Some(0));
    assert!(
        output.stderr.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn attach_full_stdout_is_reported_once() {
    let full = fs::File::options().write(true).open("/dev/full").unwrap();
    let output = live_attach_with_stdout(Stdio::from(full));
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "fiber: stdout could not be written: No space left on device (os error 28)"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn session_copy_full_stdout_is_reported_once() {
    let full = fs::File::options().write(true).open("/dev/full").unwrap();
    let output = ask_with_stdout(Stdio::from(full));
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "fiber: stdout could not be written: No space left on device (os error 28)"
    );
}

#[test]
fn sessions_closed_stdout_exits_quietly() {
    let output = closed_stdout(&["sessions"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        output.stderr.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn hub_token_list_closed_stdout_exits_quietly() {
    let output = closed_stdout(&["hub", "token", "list"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        output.stderr.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn doctor_closed_stdout_exits_quietly() {
    let setup = Setup::new();
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    let mut command = setup.fiber(&["doctor"]);
    command.stdout(Stdio::from(writer));
    let output = support::run_to_exit(setup.deadline, "doctor with closed stdout", command);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        output.stderr.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn full_stdout_remains_an_error() {
    let full = fs::File::options().write(true).open("/dev/full").unwrap();
    let output = run_with_hub(&["sessions"], Stdio::from(full));
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "fiber: standard output: No space left on device (os error 28)"
    );
}
