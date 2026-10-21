use super::*;
use crate::login::Plain;
use fakes::Deadline;
use serde_json::json;
use std::io::{BufRead, Cursor};
use std::os::unix::{fs::PermissionsExt, net::UnixStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn entry(home: &Path, name: &str) -> Option<Value> {
    config::get_global(home, &format!("hubs.\"{name}\".address")).unwrap()
}

fn pair(home: &Path, accepted: bool, break_config: bool) -> (Result<(), Failure>, Vec<u8>) {
    let (client, mut server) = UnixStream::pair().unwrap();
    let (done, done_rx) = mpsc::channel();
    let (served, served_rx) = mpsc::channel();
    let deadline = Deadline::after(Duration::from_secs(5));
    thread::scope(|scope| {
        let client_home = home.to_owned();
        scope.spawn(move || {
            let mut input = Cursor::new(b"  ABCDE12345\n");

            let mut err = Vec::new();
            let mut stream = Some(client);
            let result = add_run(
                &client_home,
                "box",
                "ws://box",
                true,
                &mut Plain,
                &mut input,
                &mut err,
                &mut |_| Ok((stream.take().unwrap(), hello())),
            );
            done.send((result, err)).unwrap();
        });
        scope.spawn(move || {
            server.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut text = String::new();
            io::BufReader::new(server.try_clone().unwrap()).read_line(&mut text).unwrap();
            assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), json!({"id":"c_pair", "command":"pair", "args":{"code":"ABCDE12345"}}));
            if break_config { std::fs::create_dir(home.join("config.json")).unwrap(); }
            let payload = if accepted { json!({"command_id":"c_pair","result":{"device":"laptop","token":"device-token"}}) } else { json!({"command_id":"c_pair", "code":"pairing_failed", "message":"ABCDE12345 is not valid"}) };
            writeln!(server, "{}", json!({"kind":if accepted {"command_accepted"} else {"command_rejected"}, "ts":1,"schema_version":1,"payload":payload})).unwrap();
            served.send(()).unwrap();
        });
        let result = deadline.recv_or_fail(&done_rx, "hub add completes");
        deadline.recv_or_fail(&served_rx, "the pairing peer completes");
        result
    })
}

fn hello() -> contract::HubLine {
    serde_json::from_value(json!({"kind":"hub_hello","ts":1,"schema_version":1,"payload":{}}))
        .unwrap()
}

#[test]
fn add_pairs_stores_private_token_entry_and_default_and_prints_nothing() {
    let dir = fakes::TempDir::new("fiber-hub-add");
    let (result, err) = pair(dir.path(), true, false);
    result.unwrap();

    assert_eq!(err, b"pairing code: ");
    assert_eq!(entry(dir.path(), "box"), Some(json!("ws://box")));
    assert_eq!(
        config::get_global(dir.path(), "hub.default").unwrap(),
        Some(json!("box"))
    );
    assert_eq!(
        config::read_credential(dir.path(), "hubs", "box")
            .unwrap()
            .unwrap()
            .expose(),
        "device-token"
    );
    assert_eq!(
        std::fs::metadata(dir.path().join("credentials/hubs/box"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn refused_pairing_or_failed_entry_write_leaves_no_token() {
    for (accepted, broken, code) in [
        (false, false, ErrorCode::PairingFailed),
        (true, true, ErrorCode::IoFailed),
    ] {
        let dir = fakes::TempDir::new("fiber-hub-add-failed");
        let (result, err) = pair(dir.path(), accepted, broken);
        let error = result.unwrap_err();
        assert_eq!(error.code, code);
        assert!(!error.message.contains("ABCDE12345"));

        assert_eq!(err, b"pairing code: ");
        assert!(
            config::read_credential(dir.path(), "hubs", "box")
                .unwrap()
                .is_none()
        );
        if !broken {
            assert!(entry(dir.path(), "box").is_none());
        }
    }
}

#[test]
fn unix_add_never_reads_or_connects_and_default_is_optional() {
    let dir = fakes::TempDir::new("fiber-hub-add-unix");

    add_run(
        dir.path(),
        "fwd",
        "unix:/tmp/hub",
        false,
        &mut NeverKeys,
        &mut Cursor::new([]),
        &mut Vec::new(),
        &mut |_| panic!("unix add must not connect"),
    )
    .unwrap();

    assert_eq!(entry(dir.path(), "fwd"), Some(json!("unix:/tmp/hub")));
    assert!(
        config::get_global(dir.path(), "hub.default")
            .unwrap()
            .is_none()
    );
    assert!(
        config::read_credential(dir.path(), "hubs", "fwd")
            .unwrap()
            .is_none()
    );
    let error = add_run(
        dir.path(),
        "fwd",
        "ws://box",
        false,
        &mut NeverKeys,
        &mut Cursor::new([]),
        &mut Vec::new(),
        &mut |_| panic!("existing name must not connect"),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Usage);
    assert!(error.message.contains("fiber hub remove fwd"));
}

#[test]
fn empty_code_and_unreachable_hub_store_nothing() {
    let dir = fakes::TempDir::new("fiber-hub-add-no-code");
    let error = add_run(
        dir.path(),
        "box",
        "ws://box",
        false,
        &mut Plain,
        &mut Cursor::new(b" \n"),
        &mut Vec::new(),
        &mut |_| panic!("empty code must not connect"),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Usage);
    let error = add_run(
        dir.path(),
        "box",
        "ws://box",
        false,
        &mut Plain,
        &mut Cursor::new(b"ABCDE12345\n"),
        &mut Vec::new(),
        &mut |_| {
            Err(doors::Error::Hello {
                hub: "box".into(),
                source: io::Error::other("unreachable"),
            })
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ConnectionFailed);
    assert!(entry(dir.path(), "box").is_none());
    assert!(
        config::read_credential(dir.path(), "hubs", "box")
            .unwrap()
            .is_none()
    );
}

#[test]
fn add_and_remove_refuse_a_held_name_lock_without_touching_its_token() {
    let dir = fakes::TempDir::new("fiber-hub-add-held");
    let file = CredentialFile::new(dir.path(), "hubs", "box").unwrap();
    let _held = file.try_lock().unwrap().unwrap();
    config::store_credential(
        dir.path(),
        "hubs",
        "box",
        &Secret::new("holder-token".into()),
    )
    .unwrap();
    let add = add_run(
        dir.path(),
        "box",
        "ws://box",
        false,
        &mut NeverKeys,
        &mut Cursor::new([]),
        &mut Vec::new(),
        &mut |_| panic!("held lock must not connect"),
    )
    .unwrap_err();
    let remove = remove_run(dir.path(), "box").unwrap_err();
    for error in [add, remove] {
        assert_eq!(error.code, ErrorCode::IoFailed);
        assert!(
            error
                .message
                .contains("another `fiber hub add` or `fiber hub remove` of `box` is running")
        );
    }
    assert_eq!(
        config::read_credential(dir.path(), "hubs", "box")
            .unwrap()
            .unwrap()
            .expose(),
        "holder-token"
    );
}

#[test]
fn settle_keeps_a_renamed_entrys_token_but_rolls_back_other_errors() {
    let dir = fakes::TempDir::new("fiber-hub-settle");
    let file = CredentialFile::new(dir.path(), "hubs", "box").unwrap();
    let held = file.try_lock().unwrap().unwrap();
    config::store_credential(
        dir.path(),
        "hubs",
        "box",
        &Secret::new("device-token".into()),
    )
    .unwrap();
    let unsynced = ConfigError::Unsynced {
        file: dir.path().join("config.json"),
        source: io::Error::other("sync failed"),
    };
    settle_token(dir.path(), "box", &unsynced, &held, &mut Vec::new());
    assert!(
        config::read_credential(dir.path(), "hubs", "box")
            .unwrap()
            .is_some()
    );
    settle_token(
        dir.path(),
        "box",
        &ConfigError::Io {
            file: dir.path().join("config.json"),
            source: io::Error::other("write failed"),
        },
        &held,
        &mut Vec::new(),
    );
    assert!(
        config::read_credential(dir.path(), "hubs", "box")
            .unwrap()
            .is_none()
    );
}

#[test]
fn remove_deletes_the_entry_token_and_only_a_matching_default() {
    for default in ["box", "other"] {
        let dir = fakes::TempDir::new("fiber-hub-remove");
        add_run(
            dir.path(),
            "box",
            "unix:/tmp/hub",
            false,
            &mut NeverKeys,
            &mut Cursor::new([]),
            &mut Vec::new(),
            &mut |_| panic!("unix add must not connect"),
        )
        .unwrap();
        config::store_credential(
            dir.path(),
            "hubs",
            "box",
            &Secret::new("device-token".into()),
        )
        .unwrap();
        config::replace_global(dir.path(), "hub.default", Some(json!(default))).unwrap();

        remove_run(dir.path(), "box").unwrap();

        assert!(entry(dir.path(), "box").is_none());
        assert!(
            config::read_credential(dir.path(), "hubs", "box")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            config::get_global(dir.path(), "hub.default").unwrap(),
            if default == "box" {
                None
            } else {
                Some(json!("other"))
            }
        );
        assert_eq!(
            remove_run(dir.path(), "box").unwrap_err().code,
            ErrorCode::Usage
        );
    }
}

#[test]
fn entry_failure_names_a_kept_token_only_after_a_paired_entrys_rename() {
    for paired in [false, true] {
        for renamed in [false, true] {
            let source = io::Error::other("write failed");
            let file = "/tmp/config.json".into();
            let error = if renamed {
                ConfigError::Unsynced { file, source }
            } else {
                ConfigError::Io { file, source }
            };
            let message = error.to_string();
            let failure = entry_failure(error, paired);
            assert_eq!(failure.code, ErrorCode::IoFailed);
            assert_eq!(
                failure.message,
                if paired && renamed {
                    format!("{message} The device token the entry refers to was kept.")
                } else {
                    message
                }
            );
        }
    }
}

const PUBLIC_CHILD: &str = "FIBER_HUB_LIST_CHILD";

#[test]
fn public_add_writes_the_entry_and_reports_usage_on_a_duplicate() {
    if std::env::var_os(PUBLIC_CHILD).is_some() {
        let default = std::env::var_os("FIBER_HUB_LIST_DEFAULT").is_some();
        std::process::exit(hub_add(
            "fwd",
            "unix:/tmp/hub",
            default,
            fakes::clock::FakeClock::new().as_ref(),
        ));
    }
    for default in [false, true] {
        let dir = fakes::TempDir::new("fiber-hub-public-add");
        let args = if default {
            vec![(PUBLIC_CHILD, "1"), ("FIBER_HUB_LIST_DEFAULT", "1")]
        } else {
            vec![(PUBLIC_CHILD, "1")]
        };
        let name = module_path!().split_once("::").unwrap().1.to_owned()
            + "::public_add_writes_the_entry_and_reports_usage_on_a_duplicate";
        let run = || {
            fakes::rerun_prepared(
                &name,
                &args,
                Deadline::after(Duration::from_secs(10)),
                |command| {
                    command
                        .env("FIBER_HOME", dir.path())
                        .current_dir(dir.path());
                },
            )
        };
        let output = run();
        assert_eq!(output.status.code(), Some(0));
        assert_eq!(entry(dir.path(), "fwd"), Some(json!("unix:/tmp/hub")));
        assert_eq!(
            config::get_global(dir.path(), "hub.default").unwrap(),
            if default { Some(json!("fwd")) } else { None }
        );
        let duplicate = run();
        assert_eq!(duplicate.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&duplicate.stderr).contains("fiber hub remove fwd"));
    }
}

#[test]
fn public_remove_deletes_state_and_reports_usage_when_absent() {
    if std::env::var_os(PUBLIC_CHILD).is_some() {
        std::process::exit(hub_remove("fwd"));
    }
    let dir = fakes::TempDir::new("fiber-hub-public-remove");
    add_run(
        dir.path(),
        "fwd",
        "unix:/tmp/hub",
        true,
        &mut NeverKeys,
        &mut Cursor::new([]),
        &mut Vec::new(),
        &mut |_| panic!("unix does not connect"),
    )
    .unwrap();
    let name = module_path!().split_once("::").unwrap().1.to_owned()
        + "::public_remove_deletes_state_and_reports_usage_when_absent";
    let run = || {
        fakes::rerun_prepared(
            &name,
            &[(PUBLIC_CHILD, "1")],
            Deadline::after(Duration::from_secs(10)),
            |command| {
                command
                    .env("FIBER_HOME", dir.path())
                    .current_dir(dir.path());
            },
        )
    };
    let output = run();
    assert_eq!(output.status.code(), Some(0));
    assert!(entry(dir.path(), "fwd").is_none());
    assert!(
        config::get_global(dir.path(), "hub.default")
            .unwrap()
            .is_none()
    );
    assert_eq!(run().status.code(), Some(2));
}

#[test]
fn public_add_reads_plain_input_when_only_stderr_is_a_terminal() {
    if std::env::var_os(PUBLIC_CHILD).is_some() {
        std::process::exit(hub_add(
            "box",
            "ws://box",
            false,
            fakes::clock::FakeClock::new().as_ref(),
        ));
    }
    let dir = fakes::TempDir::new("fiber-hub-public-pipe");
    let (_master, slave_path) = fakes::pty::open().unwrap();
    let name = module_path!().split_once("::").unwrap().1.to_owned()
        + "::public_add_reads_plain_input_when_only_stderr_is_a_terminal";
    let output = fakes::rerun_prepared(
        &name,
        &[(PUBLIC_CHILD, "1")],
        Deadline::after(Duration::from_secs(10)),
        |command| {
            command
                .env("FIBER_HOME", dir.path())
                .current_dir(dir.path())
                .stdin(std::process::Stdio::null())
                .stderr(
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(&slave_path)
                        .unwrap(),
                );
        },
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "empty piped input is usage, not a terminal IO failure"
    );
    assert!(entry(dir.path(), "box").is_none());
}

struct NeverKeys;
impl KeyReader for NeverKeys {
    fn read_key(
        &mut self,
        _: &str,
        _: &mut dyn io::BufRead,
        _: &mut dyn Write,
    ) -> io::Result<Secret> {
        panic!("must not read a code")
    }
    fn read_key_exact(
        &mut self,
        _: &str,
        _: &mut dyn io::BufRead,
        _: &mut dyn Write,
    ) -> io::Result<Secret> {
        panic!("must not read a code")
    }
}
