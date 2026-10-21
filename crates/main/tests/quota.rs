//! A client reads quota for every credential label, including providers the
//! session's scripted model does not use (`docs/tools.md`, "Fetching").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc;
use std::thread;
use support::*;

struct Reap(u32);
impl Drop for Reap {
    fn drop(&mut self) {
        kill_group_detached(self.0, "KILL");
    }
}

fn quota(client: &Socket, id: &str) -> Value {
    client.send(&json!({"id": id, "command": "quota"}).to_string());
    let answer = recv_answer(client, id, "the quota answer");
    assert_eq!(answer["kind"], "command_accepted");
    answer["payload"]["result"].clone()
}

#[test]
fn a_scripted_session_answers_every_codex_label_without_a_model_switch() {
    let setup = Setup::new();
    // This test covers labels and wire values, not reset timing. Pin the
    // recording's reset timestamps to the epoch, always already reset.
    let mut body: Value = serde_json::from_slice(&support::package::codex_quota()).unwrap();
    for window in ["primary_window", "secondary_window"] {
        body["rate_limit"][window]["reset_at"] = json!(0);
    }
    let body = body.to_string();
    let server = fakes::ProviderServer::start_responding(move |request| {
        assert_eq!(request.path, "/backend-api/wham/usage");
        fakes::Response::status(200, body.clone())
    })
    .unwrap();
    let package = setup.root.path().join("codex");
    support::package::copy_package("codex", &package, "https://chatgpt.com", &server.url());
    let installed = run_to_exit(
        setup.deadline,
        "installing codex",
        setup.fiber(&["extension", "install", package.to_str().unwrap()]),
    );
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let dir = setup.home().join("credentials/codex");
    fs::create_dir_all(&dir).unwrap();
    for label in ["work", "home"] {
        let file = dir.join(label);
        write_json(
            &file,
            &json!({"token": format!("token-{label}"), "expires_at": 4_102_444_800u64, "refresh_token":"rt", "account_id":format!("account-{label}")}),
        );
        fs::set_permissions(file, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::write(dir.join("broken"), "not a token").unwrap();
    fs::set_permissions(dir.join("broken"), fs::Permissions::from_mode(0o600)).unwrap();
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps":[{"text":"done"}]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"model":"scripted/s.json"}),
    );
    let mut command = setup.fiber(&[
        "session",
        "--id",
        "s_0123456789abcdef",
        "--workspace",
        setup.workspace().to_str().unwrap(),
    ]);
    command.current_dir(setup.workspace());
    let mut child = command.spawn().unwrap();
    let group = child.id();
    let guard = Reap(group);
    let watchdog = fakes::Watchdog::group(group);
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (out, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if out.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let (err, errors) = mpsc::channel();
    thread::spawn(move || {
        let mut text = String::new();
        BufReader::new(stderr).read_to_string(&mut text).unwrap();
        let _sent = err.send(text);
    });
    let mut loaded = false;
    loop {
        let text = setup.deadline.recv(&lines).unwrap_or_else(|error| {
            panic!(
                "the session ready line: {error}; stderr: {}",
                setup.deadline.recv(&errors).unwrap_or_default()
            )
        });
        let line: Value = serde_json::from_str(&text).unwrap();
        if line["kind"] == "extensions_loaded" {
            loaded = true;
        }
        if loaded && line["kind"] == "session_status" {
            break;
        }
    }
    let client = Socket::connect(setup.deadline, &setup.session_socket("s_0123456789abcdef"));
    client.send(r#"{"id":"sub","command":"subscribe","args":{"level":"full"}}"#);
    assert_eq!(
        recv_answer(&client, "sub", "the subscribe answer")["kind"],
        "command_accepted"
    );
    let first = quota(&client, "first");
    assert_eq!(first["notice_at"], 80.0);
    assert!(
        server.await_requests(2, setup.deadline),
        "both labels requested usage"
    );
    let mut next = 0;
    let answer = loop {
        assert!(
            !setup.deadline.left().is_zero(),
            "quota values never reached the cache"
        );
        next += 1;
        let answer = quota(&client, &format!("q_{next}"));
        if answer["quota"].as_array().unwrap().len() == 3 {
            break answer;
        }
    };
    let windows = json!([{"name":"primary", "reset":true}, {"name":"secondary", "reset":true}]);
    assert_eq!(
        answer,
        json!({"quota":[
        {"provider":"codex", "credential":"broken", "unreachable":true},
        {"provider":"codex", "credential":"home", "windows":windows},
        {"provider":"codex", "credential":"work", "windows":windows}
    ], "notice_at":80.0})
    );
    assert_eq!(
        server
            .requests()
            .iter()
            .map(|r| r.path.as_str())
            .collect::<Vec<_>>(),
        ["/backend-api/wham/usage", "/backend-api/wham/usage"]
    );
    client.send(r#"{"id":"close","command":"close"}"#);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = setup
        .deadline
        .recv(&finished)
        .expect("the session exit")
        .unwrap();
    let stderr = setup.deadline.recv(&errors).expect("the stderr drain");
    assert!(status.success(), "{stderr}");
    assert!(!group_alive(setup.deadline, group));
    watchdog.stand_down(setup.deadline.cleanup_phase());
    std::mem::forget(guard);
}
