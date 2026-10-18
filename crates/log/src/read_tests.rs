use super::*;
use crate::open_log;
use contract::SessionId;

fn line() -> Envelope {
    Envelope {
        kind: "x".into(),
        session_id: SessionId("s".into()),
        ts: 0,
        schema_version: 1,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: Default::default(),
    }
}

#[test]
fn open_log_on_a_missing_log_is_not_found_naming_the_session_directory() {
    let home = fakes::TempDir::new("log-open-log-missing");
    let dir = home.path().join("s_aaaaaaaaaaaaaaaa");
    std::fs::create_dir_all(&dir).unwrap();
    let Err(Error::NotFound(path)) = open_log(&dir) else {
        panic!("a missing log opens");
    };
    assert_eq!(path, dir);
}

#[test]
fn open_log_under_a_regular_file_is_io_naming_the_log() {
    // The session path is a file, so the log under it cannot open for a
    // reason other than a missing log (`ENOTDIR`).
    let home = fakes::TempDir::new("log-open-log-file");
    let dir = home.path().join("s_aaaaaaaaaaaaaaaa");
    std::fs::write(&dir, b"not a directory").unwrap();
    let Err(Error::Io { path, .. }) = open_log(&dir) else {
        panic!("a log under a file opens");
    };
    assert_eq!(path, dir.join("events.jsonl"));
}

#[test]
fn open_log_on_an_existing_log_opens_it_for_reading() {
    use std::io::Read as _;
    let home = fakes::TempDir::new("log-open-log-present");
    let dir = home.path().join("s_aaaaaaaaaaaaaaaa");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("events.jsonl"), b"{\"kind\":\"x\"}\n").unwrap();
    let mut file = open_log(&dir).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"{\"kind\":\"x\"}\n");
}

#[test]
fn pending_is_false_for_an_empty_open_queue() {
    let state = State::default();
    assert!(!pending(&state));
}

#[test]
fn pending_is_true_with_a_queued_line() {
    let state = State {
        queue: VecDeque::from([line()]),
        ..Default::default()
    };
    assert!(pending(&state));
}

#[test]
fn pending_is_true_when_lagged() {
    let state = State {
        lagged: true,
        ..Default::default()
    };
    assert!(pending(&state));
}

#[test]
fn pending_is_true_when_closed() {
    let state = State {
        end: End::Closed,
        ..Default::default()
    };
    assert!(pending(&state));
}

#[test]
fn pending_is_true_when_failed() {
    let state = State {
        end: End::Failed {
            session: "s".into(),
            cause: "c".into(),
        },
        ..Default::default()
    };
    assert!(pending(&state));
}
