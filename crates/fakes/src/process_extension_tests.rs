//! The shared process fixture through its public seam: startup, JSON-line
//! capture, acknowledged hold/release and cleanup.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use crate::deadline::Deadline;
use crate::{ProcessFixture, warm_up};

/// One deadline for the whole exchange: every wait takes what remains of
/// it, so a stall fails naming its own wait instead of hanging the test.
fn within() -> Deadline {
    Deadline::start()
}

#[test]
fn startup_captures_each_json_line_exactly() {
    let mut fixture = ProcessFixture::spawn("echo");
    let deadline = within();
    for line in [
        r#"{"id":"c_1","command":"ready"}"#,
        r#"{"id":"c_2","command":"call","args":{"call":"host.sha256","args":["YWJj"],"nothing":null,"both":{},"either":[]}}"#,
    ] {
        fixture.send(line);
        assert_eq!(
            fixture.wait_for(deadline, "the echoed line", |got| got == line),
            line,
        );
    }
    fixture.finish(deadline);
}

#[test]
fn a_hold_is_acknowledged_then_released_before_the_join() {
    let mut fixture = ProcessFixture::spawn("hold");
    let deadline = within();
    fixture.send(r#"{"id":"c_1","command":"ready"}"#);
    // The acknowledgment proves the hold is engaged: only then is the
    // release safe to send.
    assert_eq!(
        fixture.wait_for(deadline, "the hold acknowledgment", |got| got
            == r#"{"held":true}"#),
        r#"{"held":true}"#,
    );
    // Closing stdin releases the hold; the fixture answers before it exits,
    // so the join below cannot hang on an engaged hold.
    fixture.release();
    assert_eq!(
        fixture.wait_for(deadline, "the release answer", |got| got
            == r#"{"released":true}"#),
        r#"{"released":true}"#,
    );
    // Finishing joins and proves the group is empty: no child remains.
    fixture.finish(deadline);
}

#[test]
fn a_protocol_peer_acknowledges_each_hold_before_release() {
    let script = serde_json::json!([
        {"read": {"expect": {"kind": "hello"}}},
        {"send": {
            "kind": "ready",
            "request_id": {"$ref": "last.request_id"}
        }},
        {"read": {"expect": {"kind": "ack"}}},
        {"hold": {
            "ack": {"held": true},
            "release_on": "line",
            "release_line": {"kind": "release"}
        }},
        {"send": {"released": true}}
    ]);
    let mut fixture = ProcessFixture::spawn_protocol(&script);
    let deadline = within();
    fixture.send(r#"{"kind":"hello","request_id":"caller-17"}"#);
    assert_eq!(
        fixture.wait_for(deadline, "the peer ready line", |line| {
            line == r#"{"kind":"ready","request_id":"caller-17"}"#
        }),
        r#"{"kind":"ready","request_id":"caller-17"}"#,
    );
    fixture.send(r#"{"kind":"ack"}"#);
    assert_eq!(
        fixture.wait_for(deadline, "the hold acknowledgment", |line| {
            line == r#"{"held":true}"#
        }),
        r#"{"held":true}"#,
    );
    fixture.send(r#"{"kind":"release"}"#);
    assert_eq!(
        fixture.wait_for(deadline, "the release answer", |line| {
            line == r#"{"released":true}"#
        }),
        r#"{"released":true}"#,
    );
    fixture.finish(deadline);
}

#[test]
fn the_fixture_usage_error_names_its_modes() {
    warm_up(&super::example_path(), &["bogus-mode"], 2);
}

#[test]
#[should_panic(expected = "the fixture exits 0")]
fn finish_fails_when_the_fixture_exits_unsuccessfully() {
    let fixture = ProcessFixture::spawn("bogus-mode");
    let deadline = within();
    fixture.finish(deadline);
}
