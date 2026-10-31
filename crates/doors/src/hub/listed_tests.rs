use super::*;
use config::{Config, ProjectKey, Sources};
use contract::{ErrorCode, HubLine};
use fakes::MUST_SUCCEED_WITHIN;
use fakes::{Deadline, clock::FakeClock};
use serde_json::json;
use std::io::{self, BufRead, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn spawn_authenticate(client: UnixStream, idle: Duration) -> mpsc::Receiver<Result<(), Error>> {
    let (done, done_rx) = mpsc::channel();
    let clock = FakeClock::new();
    thread::spawn(move || {
        done.send(authenticate(
            &client,
            "box",
            &Secret::new("device-token".into()),
            clock.as_ref(),
            idle,
        ))
        .unwrap();
    });
    done_rx
}

fn authenticate_after_replies(replies: &[serde_json::Value]) -> Result<(), Error> {
    let (client, mut server) = UnixStream::pair().unwrap();
    let done_rx = spawn_authenticate(client, Duration::from_secs(1));
    for reply in replies {
        writeln!(server, "{reply}").unwrap();
    }
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&done_rx, "the authentication reply sequence")
}

#[test]
fn authentication_answer_preserves_hub_line_kind_and_payload() {
    let line: HubLine = serde_json::from_value(json!({
        "kind": "future_kind",
        "ts": 1,
        "schema_version": 1,
        "payload": {"command_id": "c_authenticate", "marker": "retained"},
    }))
    .unwrap();
    let answer = AuthenticationAnswer::from(line);
    let expected_payload = json!({
        "command_id": "c_authenticate",
        "marker": "retained",
    })
    .as_object()
    .unwrap()
    .clone();

    assert_eq!(answer.payload, expected_payload);
}

#[test]
fn authentication_skips_unknown_and_hub_hello_before_refusal() {
    let unknown = json!({
        "ts": 1,
        "schema_version": 1,
        "kind": "future_kind",
        "payload": {"command_id": "c_authenticate"},
    });
    // #3666 Part 2 R6: matching-ID unknown and known non-answer kinds are skipped.
    let error = authenticate_after_replies(&[
        unknown,
        json!({
            "ts": 1,
            "schema_version": 1,
            "kind": "hub_hello",
            "payload": {"command_id": "c_authenticate"},
        }),
        json!({
            "ts": 1,
            "schema_version": 1,
            "kind": "command_rejected",
            "payload": {
                "command_id": "c_authenticate",
                "code": "unauthenticated",
                "message": "skip probe: device-token was revoked",
            },
        }),
    ])
    .unwrap_err();

    assert_eq!(error.code(), ErrorCode::AuthenticationFailed);
    let Error::Refused { hub, message } = &error else {
        panic!("a matching command_rejected answer must refuse authentication")
    };
    assert_eq!(hub, "box");
    assert_eq!(message, "skip probe: [redacted] was revoked");
    assert!(!error.to_string().contains("device-token"));
}

#[test]
fn authentication_skips_unknown_and_wrong_command_ack_before_acceptance() {
    let result = authenticate_after_replies(&[
        json!({
            "ts": 1,
            "schema_version": 1,
            "kind": "future_kind",
            "payload": {"command_id": "c_authenticate"},
        }),
        json!({
            "ts": 1,
            "schema_version": 1,
            "kind": "command_accepted",
            "payload": {"command_id": "another-command"},
        }),
        json!({
            "ts": 1,
            "schema_version": 1,
            "kind": "command_accepted",
            "payload": {"command_id": "c_authenticate"},
        }),
    ]);
    result.unwrap();
}

#[test]
fn authentication_answer_keeps_hub_line_validation() {
    for line in [
        b"{".as_slice(),
        br#"{"ts":1,"schema_version":1,"payload":{}}"#,
        br#"{"kind":"hub_hello","schema_version":1,"payload":{}}"#,
        br#"{"kind":"hub_hello","ts":1,"payload":{}}"#,
        br#"{"kind":"hub_hello","ts":1,"schema_version":1}"#,
        br#"{"kind":1,"ts":1,"schema_version":1,"payload":{}}"#,
        br#"{"kind":"hub_hello","ts":1,"schema_version":1,"payload":null}"#,
        br#"{"kind":"hub_hello","ts":1,"schema_version":1,"payload":[]}"#,
    ] {
        let (client, mut server) = UnixStream::pair().unwrap();
        let done_rx = spawn_authenticate(client, Duration::from_secs(1));
        server.write_all(line).unwrap();
        server.write_all(b"\n").unwrap();
        let error = Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&done_rx, "the malformed authentication answer")
            .unwrap_err();
        let Error::Hello { source, .. } = error else {
            panic!("malformed answer must be an Error::Hello")
        };
        assert_eq!(source.kind(), io::ErrorKind::InvalidData);
        assert_eq!(source.to_string(), "invalid authentication answer");
    }
}

#[test]
fn authenticate_accepts_only_its_ack_and_keeps_the_wire_token_secret() {
    for (reply, expected) in [
        (
            Some(json!({"kind":"command_accepted", "payload":{"command_id":"c_authenticate"}})),
            None,
        ),
        (
            Some(
                json!({"kind":"command_rejected", "payload":{"command_id":"c_authenticate", "code":"unauthenticated", "message":"device-token was revoked"}}),
            ),
            Some(ErrorCode::AuthenticationFailed),
        ),
        (None, Some(ErrorCode::ConnectionFailed)),
    ] {
        let (client, mut server) = UnixStream::pair().unwrap();
        let done_rx = spawn_authenticate(client, Duration::from_secs(1));
        let (wire, wire_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut line = String::new();
            io::BufReader::new(server.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            wire.send(serde_json::from_str::<serde_json::Value>(&line).unwrap())
                .unwrap();
            if let Some(reply) = reply {
                writeln!(server, "{}", json!({"ts":1,"schema_version":1,"kind":"command_rejected","payload":{"command_id":"another-command","message":"not our answer"}})).unwrap();
                writeln!(server, "{}", json!({"ts":1,"schema_version":1,"kind":reply["kind"],"payload":reply["payload"]})).unwrap();
            }
        });
        let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
        assert_eq!(
            deadline.recv_or_fail(&wire_rx, "the authentication command"),
            json!({"id":"c_authenticate","command":"authenticate","args":{"token":"device-token"}})
        );
        let result = deadline.recv_or_fail(&done_rx, "the authentication answer");
        match expected {
            None => result.unwrap(),
            Some(code) => {
                let error = result.unwrap_err();
                assert_eq!(error.code(), code);
                if code == ErrorCode::AuthenticationFailed {
                    assert!(matches!(error, Error::Refused { .. }));
                    assert!(error.to_string().contains("box"));
                    assert!(error.to_string().contains("was revoked"));
                    assert!(!error.to_string().contains("device-token"));
                } else {
                    assert!(
                        matches!(error, Error::Hello { source, .. } if source.kind() == io::ErrorKind::UnexpectedEof)
                    );
                }
            }
        }
    }
}

#[test]
fn authentication_kernel_timeout_needs_no_fake_time_advance() {
    let (client, _silent_server) = UnixStream::pair().unwrap();
    let done_rx = spawn_authenticate(
        client,
        net::Limits::new(Duration::from_secs(15), Duration::from_millis(100))
            .unwrap()
            .idle(),
    );
    let error = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&done_rx, "authentication times out on a silent peer")
        .unwrap_err();
    assert!(
        matches!(error, Error::Hello { source, .. } if source.kind() == io::ErrorKind::TimedOut)
    );
}

#[test]
fn listed_unix_connect_reads_hello_and_never_starts_a_hub() {
    let dir = fakes::TempDir::new("fiber-listed-connect");
    let path = dir.path().join("socket");
    let listener = UnixListener::bind(&path).unwrap();
    let (served, served_rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        writeln!(
            stream,
            "{}",
            json!({"kind":"hub_hello","ts":1,"schema_version":1,"payload":{}})
        )
        .unwrap();
        served.send(()).unwrap();
    });
    let (done, done_rx) = mpsc::channel();
    let home = dir.path().to_path_buf();
    let socket = path.clone();
    thread::spawn(move || {
        let clock = FakeClock::new();
        let target = Target::Listed {
            name: "fwd".into(),
            address: Address::Unix(socket),
            token: None,
        };
        done.send(connect_target(
            &home,
            &target,
            &mut || panic!("listed hub must not start"),
            clock.as_ref(),
        ))
        .unwrap();
    });
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    let (_, hello) = deadline
        .recv_or_fail(&done_rx, "the forwarded hub hello")
        .unwrap();
    assert_eq!(hello.kind, "hub_hello");
    deadline.recv_or_fail(&served_rx, "the listener answered");
    std::fs::remove_file(&path).unwrap();
    let target = Target::Listed {
        name: "fwd".into(),
        address: Address::Unix(path),
        token: None,
    };
    let error = connect_target(
        dir.path(),
        &target,
        &mut || panic!("listed hub must not start"),
        FakeClock::new().as_ref(),
    )
    .unwrap_err();
    assert!(
        matches!(error, Error::Unix { source, .. } if matches!(source.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused))
    );
}

#[test]
fn local_target_keeps_the_local_start_failure() {
    let dir = fakes::TempDir::new("fiber-target-local");
    let error = connect_target(
        dir.path(),
        &Target::Local,
        &mut || Err(io::Error::other("start refused")),
        FakeClock::new().as_ref(),
    )
    .unwrap_err();
    assert!(matches!(error, Error::Local(_)));
    assert_eq!(error.code(), ErrorCode::IoFailed);
    assert_eq!(error.to_string(), "start refused");
}

#[test]
fn unpaired_connect_reads_only_the_hello() {
    let dir = fakes::TempDir::new("fiber-unpaired-connect");
    let path = dir.path().join("socket");
    let listener = UnixListener::bind(&path).unwrap();
    let (release, release_rx) = mpsc::channel::<()>();
    let (served, served_rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        writeln!(
            stream,
            "{}",
            json!({"kind":"hub_hello","ts":1,"schema_version":1,"payload":{}})
        )
        .unwrap();
        // The test keeps the socket alive without sending an authentication answer.
        let _released = release_rx.recv();
        served.send(()).unwrap();
    });
    let (done, done_rx) = mpsc::channel();
    thread::spawn(move || {
        done.send(connect_unpaired(
            &Address::Unix(path),
            FakeClock::new().as_ref(),
        ))
        .unwrap();
    });
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    let (_, hello) = deadline
        .recv_or_fail(&done_rx, "the unpaired hello without authentication")
        .unwrap();
    assert_eq!(hello.kind, "hub_hello");
    drop(release);
    deadline.recv_or_fail(&served_rx, "the unpaired listener ends");
}

fn load(home: &Path, overrides: &[&str]) -> Config {
    Config::load(Sources {
        home: home.into(),
        workspace: home.join("workspace"),
        project: ProjectKey::new("test-project").unwrap(),
        overrides: overrides.iter().map(|s| s.to_string()).collect(),
    })
    .unwrap()
}

fn write(file: &Path, value: serde_json::Value) {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, value.to_string()).unwrap();
}

fn listed_name(target: Target) -> String {
    let Target::Listed { name, .. } = target else {
        panic!("expected a listed hub")
    };
    name
}

#[test]
fn load_target_uses_the_workspaces_layered_config_and_propagates_load_errors() {
    let dir = fakes::TempDir::new("fiber-load-listed-target");
    let home = dir.path();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    write(
        &home.join("config.json"),
        json!({"hub":{"default":"global"},"hubs":{"global":{"address":"unix:/tmp/global"},"project":{"address":"unix:/tmp/project"}}}),
    );
    let key = log::project_key(&std::fs::canonicalize(&workspace).unwrap());
    let resolve = |named: Option<&str>| {
        let home = home.to_path_buf();
        let workspace = workspace.clone();
        let named = named.map(str::to_owned);
        fakes::within(
            "loading the workspace's hub target",
            Deadline::after(MUST_SUCCEED_WITHIN),
            move || load_target(&home, &workspace, named.as_deref()),
        )
    };
    let project_file = home.join("projects").join(key).join("config.json");
    write(&project_file, json!({"hub":{"default":"project"}}));
    assert_eq!(listed_name(resolve(None).unwrap()), "project");
    assert_eq!(listed_name(resolve(Some("global")).unwrap()), "global");
    write(&project_file, json!({"hub":{"default":12}}));
    assert_eq!(resolve(None).unwrap_err().code(), ErrorCode::ConfigInvalid);
}

#[test]
fn target_reads_each_loaded_config_without_caching() {
    let dir = fakes::TempDir::new("fiber-listed-target");
    let home = dir.path();
    assert!(!target(&load(home, &[]), home, None).unwrap().is_listed());
    write(
        &home.join("config.json"),
        json!({"hub":{"default":"a"}, "hubs":{"a":{"address":"unix:/tmp/a"}, "b.x":{"address":"unix:/tmp/b"}}}),
    );
    let cfg = load(home, &[]);
    assert_eq!(listed_name(target(&cfg, home, None).unwrap()), "a");
    assert_eq!(listed_name(target(&cfg, home, Some("b.x")).unwrap()), "b.x");
    assert_eq!(
        listed_name(target(&load(home, &["hub.default=b.x"]), home, None).unwrap()),
        "b.x"
    );
    write(
        &home.join("projects/test-project/config.json"),
        json!({"hub":{"default":"b.x"}}),
    );
    assert_eq!(
        listed_name(target(&load(home, &[]), home, None).unwrap()),
        "b.x"
    );
    std::fs::remove_file(home.join("projects/test-project/config.json")).unwrap();
    write(
        &home.join("workspace/.fiber/config.json"),
        json!({"hub":{"default":"b.x"}}),
    );
    let cfg = load(home, &[]);
    assert!(!cfg.notices().is_empty());
    assert_eq!(listed_name(target(&cfg, home, None).unwrap()), "a");
    write(
        &home.join("config.json"),
        json!({"hub":{"default":"b.x"}, "hubs":{"b.x":{"address":"unix:/tmp/b"}}}),
    );
    assert_eq!(
        listed_name(target(&load(home, &[]), home, None).unwrap()),
        "b.x"
    );
}

#[test]
fn target_errors_do_not_fall_back_to_local() {
    let dir = fakes::TempDir::new("fiber-listed-errors");
    let home = dir.path();
    let error = target(&load(home, &[]), home, Some("nope")).unwrap_err();
    assert!(matches!(error, Error::UnknownHub { .. }));
    assert_eq!(error.code(), ErrorCode::Usage);
    assert_eq!(
        error.to_string(),
        "No hub named `nope` in this client's list; run `fiber hub add <name> <address>`. Run `fiber --help` for usage."
    );
    write(&home.join("config.json"), json!({"hub":{"default":"gone"}}));
    let error = target(&load(home, &[]), home, None).unwrap_err();
    assert!(matches!(error, Error::MissingDefault { .. }));
    assert_eq!(error.code(), ErrorCode::ConfigInvalid);
    assert!(error.to_string().contains("hub.default"));
    write(
        &home.join("config.json"),
        json!({"hubs":{"box":{"address":"http://bad"}}}),
    );
    let error = target(&load(home, &[]), home, Some("box")).unwrap_err();
    assert!(matches!(error, Error::StoredAddress { .. }));
    assert_eq!(error.code(), ErrorCode::ConfigInvalid);
    assert!(error.to_string().contains("hubs.\"box\".address"));
}

#[test]
fn malformed_selected_names_are_missing_entries_not_credential_paths() {
    let dir = fakes::TempDir::new("fiber-listed-bad-name");
    let home = dir.path();
    write(
        &home.join("config.json"),
        json!({"hub":{"default":"bad/name"}}),
    );
    let cfg = load(home, &[]);
    let default = target(&cfg, home, None).unwrap_err();
    assert_eq!(default.code(), ErrorCode::ConfigInvalid);
    assert!(matches!(default, Error::MissingDefault { .. }));
    let named = target(&cfg, home, Some("bad/name")).unwrap_err();
    assert_eq!(named.code(), ErrorCode::Usage);
    assert!(matches!(named, Error::UnknownHub { .. }));
    assert!(!home.join("credentials").exists());
}

#[test]
fn websocket_target_needs_a_token_but_unix_never_reads_one() {
    let dir = fakes::TempDir::new("fiber-listed-token");
    let home = dir.path();
    write(
        &home.join("config.json"),
        json!({"hubs":{"box":{"address":"ws://box"}, "fwd":{"address":"unix:/tmp/fwd"}}}),
    );
    let error = target(&load(home, &[]), home, Some("box")).unwrap_err();
    assert!(matches!(error, Error::MissingToken { .. }));
    assert_eq!(error.code(), ErrorCode::CredentialMissing);
    assert!(error.to_string().contains("fiber hub remove box"));
    assert!(error.to_string().contains("fiber hub add box"));
    config::store_credential(
        home,
        "hubs",
        "box",
        &contract::Secret::new("device-token".into()),
    )
    .unwrap();
    let Target::Listed {
        token: Some(token),
        address: Address::Web(_),
        ..
    } = target(&load(home, &[]), home, Some("box")).unwrap()
    else {
        panic!("expected paired websocket")
    };
    assert_eq!(token.expose(), "device-token");
    config::store_credential(
        home,
        "hubs",
        "fwd",
        &contract::Secret::new("ignored-token".into()),
    )
    .unwrap();
    let Target::Listed {
        token,
        address: Address::Unix(_),
        ..
    } = target(&load(home, &[]), home, Some("fwd")).unwrap()
    else {
        panic!("expected forwarded socket")
    };
    assert!(token.is_none());
}
