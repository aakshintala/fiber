use super::*;
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
