//! A client's listed websocket and forwarded hubs, through the built binary.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]
#[path = "support/hub_start.rs"]
mod hub_start;
mod support;
use serde_json::{Value, json};
use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::process::Output;
use support::*;

fn run(setup: &Setup, args: &[&str]) -> Output {
    run_to_exit(setup.deadline, "the hub list command", setup.fiber(args))
}

fn succeeds(output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn add(setup: &Setup, name: &str, address: &str, code: &str, default: bool) -> Output {
    let input = setup.root.path().join("pairing-input");
    fs::write(&input, format!("{code}\n")).unwrap();
    let mut args = vec!["hub", "add", name, address];
    if default {
        args.push("--default");
    }
    let mut command = setup.fiber(&args);
    command.stdin(fs::File::open(input).unwrap());
    run_to_exit(
        setup.deadline,
        "hub add with a pairing code on stdin",
        command,
    )
}

fn entry(setup: &Setup, name: &str) -> Option<Value> {
    config::get_global(&setup.home(), &format!("hubs.\"{name}\".address")).unwrap()
}

#[test]
fn listed_hubs_pair_list_authenticate_revoke_and_remove_without_a_local_hub() {
    let a = Setup::new();
    let b = Setup::new();
    // A listed hub must ignore the client's repository scope.
    let mut git = std::process::Command::new("git");
    git.args(["init", "-q"])
        .arg(b.root.path())
        .env_clear()
        .envs(fakes::check_run())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", b.root.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0);
    succeeds(&run_to_exit(b.deadline, "the client's git repository", git));
    let row = json!({"session_id":"s_remote", "ts":1, "project":"remote-project", "workspace":"/remote/workspace", "name":"remote session", "how":"exited"});
    fs::create_dir_all(a.home().join("projects/remote-project/sessions/s_remote")).unwrap();
    fs::write(a.home().join("recent.jsonl"), format!("{row}\n")).unwrap();
    let served = hub_start::serve_installed(
        &a,
        hub_start::ATTEMPTS,
        &mut hub_start::free_port,
        &mut |port| {
            write_json(&a.home().join("config.json"), &json!({"hub":{"port":port}}));
            a.fiber(&["hub", "serve", "--installed"])
        },
    );
    assert_eq!(served.hub.1.kind, "hub_hello");
    let address = format!("ws://127.0.0.1:{}", served.port);
    let code = run(&a, &["hub", "pair", "laptop"]);
    succeeds(&code);
    let code = String::from_utf8(code.stdout).unwrap();
    let added = add(&b, "box", &address, code.trim(), true);
    succeeds(&added);
    assert!(added.stdout.is_empty());
    assert_eq!(entry(&b, "box"), Some(json!(address)));
    assert_eq!(
        config::get_global(&b.home(), "hub.default").unwrap(),
        Some(json!("box"))
    );
    let token_file = b.home().join("credentials/hubs/box");
    assert_eq!(
        fs::metadata(&token_file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let token = config::read_credential(&b.home(), "hubs", "box")
        .unwrap()
        .unwrap();
    assert_eq!(token.expose().len(), 43);
    let listed = run(&b, &["sessions", "--hub", "box", "--json"]);
    succeeds(&listed);
    let rows: Vec<Value> = String::from_utf8(listed.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "s_remote");
    assert!(!b.hub_socket().exists());
    let devices = run(&b, &["hub", "token", "list", "--hub", "box"]);
    succeeds(&devices);
    assert!(String::from_utf8_lossy(&devices.stdout).contains("laptop"));

    let wrong = add(&b, "wrong", &address, "wrong-code", false);
    assert_eq!(wrong.status.code(), Some(1));
    assert!(entry(&b, "wrong").is_none());
    assert!(!b.home().join("credentials/hubs/wrong").exists());
    assert!(!String::from_utf8_lossy(&wrong.stderr).contains("wrong-code"));
    let revoked = run(&a, &["hub", "token", "revoke", "laptop"]);
    succeeds(&revoked);
    let refused = run(&b, &["sessions", "--hub", "box"]);
    assert_eq!(refused.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("box:"));
    assert!(stderr.contains("token"));
    assert!(!stderr.contains(token.expose()));
    assert!(!String::from_utf8_lossy(&refused.stdout).contains(token.expose()));
    let removed = run(&b, &["hub", "remove", "box"]);
    succeeds(&removed);
    assert!(removed.stdout.is_empty());
    assert!(entry(&b, "box").is_none());
    assert!(!token_file.exists());
    assert!(
        config::get_global(&b.home(), "hub.default")
            .unwrap()
            .is_none()
    );

    let fresh_code = run(&a, &["hub", "pair", "laptop"]);
    succeeds(&fresh_code);
    let fresh_code = String::from_utf8(fresh_code.stdout).unwrap();
    succeeds(&add(&b, "box", &address, fresh_code.trim(), true));
    let devices = run(&b, &["hub", "token", "list", "--hub", "box"]);
    succeeds(&devices);
    assert!(String::from_utf8_lossy(&devices.stdout).contains("laptop"));
    succeeds(&run(&b, &["hub", "remove", "box"]));
    assert!(
        config::get_global(&b.home(), "hub.default")
            .unwrap()
            .is_none()
    );

    let forwarded = format!("unix:{}", a.hub_socket().display());
    let fwd = run(&b, &["hub", "add", "--default", "fwd", &forwarded]);
    succeeds(&fwd);
    assert!(fwd.stdout.is_empty());
    assert!(!b.home().join("credentials/hubs/fwd").exists());
    let listed = run(&b, &["sessions", "--json"]);
    succeeds(&listed);
    let rows: Vec<Value> = String::from_utf8(listed.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "s_remote");
    assert!(!b.hub_socket().exists());
    drop(served.hub);
    served.process.kill("TERM");
    assert_eq!(served.process.wait().code(), Some(143));
}

#[test]
fn a_taken_port_is_retried_with_a_fresh_one() {
    let setup = Setup::new();
    let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let taken = held.local_addr().unwrap().port();
    let mut draws = 0;
    let served = hub_start::serve_installed(
        &setup,
        hub_start::ATTEMPTS,
        &mut || {
            draws += 1;
            if draws == 1 {
                taken
            } else {
                hub_start::free_port()
            }
        },
        &mut |port| {
            write_json(
                &setup.home().join("config.json"),
                &json!({"hub":{"port":port}}),
            );
            setup.fiber(&["hub", "serve", "--installed"])
        },
    );
    assert_ne!(served.port, taken, "the hub that said hello is the retry");
    assert!(
        setup.hub_log().contains(&format!("127.0.0.1:{taken}: ")),
        "the first hub failed on the taken port: {}",
        setup.hub_log()
    );
    drop(served.hub);
    served.process.kill("TERM");
    assert_eq!(served.process.wait().code(), Some(143));
    drop(held);
}

#[test]
fn a_hub_that_never_gets_a_port_fails_naming_the_port_and_the_log() {
    let setup = Setup::new();
    let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let taken = held.local_addr().unwrap().port();
    let mut draws = 0;
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        hub_start::serve_installed(
            &setup,
            3,
            &mut || {
                draws += 1;
                taken
            },
            &mut |port| {
                write_json(
                    &setup.home().join("config.json"),
                    &json!({"hub":{"port":port}}),
                );
                setup.fiber(&["hub", "serve", "--installed"])
            },
        )
    }));
    let payload = failed.err().expect("a hub that cannot bind fails");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
        })
        .expect("the panic carries text");
    for needle in [
        format!("127.0.0.1:{taken}"),
        "exited".to_owned(),
        "Address already in use".to_owned(),
        "attempt 3 of 3".to_owned(),
    ] {
        assert!(message.contains(&needle), "{needle:?} missing: {message}");
    }
    assert_eq!(draws, 3, "one port is drawn per attempt");
    drop(held);
}

#[test]
fn unknown_hub_is_usage_before_any_hub_starts() {
    let setup = Setup::new();
    let output = run(&setup, &["sessions", "--hub", "nope"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(output.stderr, b"fiber: No hub named `nope` in this client's list; run `fiber hub add <name> <address>`. Run `fiber --help` for usage.\n");
    assert!(output.stdout.is_empty());
    assert!(!setup.hub_socket().exists());
}
