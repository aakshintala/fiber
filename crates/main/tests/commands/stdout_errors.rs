//! Closed stdout handling for list commands.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io;
use std::process::Stdio;

use crate::support::{self, Setup};

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

fn closed_stdout(args: &[&str]) -> std::process::Output {
    let setup = Setup::new();
    let served = installed_hub(&setup);
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    let mut command = setup.fiber(args);
    command.stdout(Stdio::from(writer));
    let output = support::run_to_exit(setup.deadline, "list command with closed stdout", command);
    drop(served.hub);
    served.process.kill("TERM");
    assert_eq!(served.process.wait().code(), Some(143));
    output
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

#[cfg(target_os = "linux")]
#[test]
fn full_stdout_remains_an_error() {
    let setup = Setup::new();
    let served = installed_hub(&setup);
    let full = fs::File::options().write(true).open("/dev/full").unwrap();
    let mut command = setup.fiber(&["sessions"]);
    command.stdout(Stdio::from(full));
    let output = support::run_to_exit(setup.deadline, "list command with full stdout", command);
    drop(served.hub);
    served.process.kill("TERM");
    assert_eq!(served.process.wait().code(), Some(143));
    assert_eq!(output.status.code(), Some(1));
    assert!(!output.stderr.is_empty());
}
