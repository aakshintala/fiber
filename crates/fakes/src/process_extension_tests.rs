//! The shared process fixture through its public seam: startup, JSON-line
//! capture, acknowledged hold/release and cleanup.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::time::Duration;

use crate::ProcessFixture;
use crate::clock::FakeClock;
use crate::deadline::Deadline;

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
    let _warmed = super::example_path();
}

/// A `wait_for` started after the guard is partly consumed still drains
/// through its started bound: the line sent before the wait arrives.
#[test]
fn wait_for_started_late_still_drains_through_its_started_bound() {
    let clock: &'static std::sync::Arc<FakeClock> = Box::leak(Box::new(FakeClock::new()));
    let guard = Deadline::on(&**clock);
    // Part of the guard is gone before the wait starts. No code under test
    // parks on this clock (receives wait on the wall clock), so time moves
    // here by hand.
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(Duration::from_secs(60));
    let mut fixture = ProcessFixture::spawn("echo");
    let line = r#"{"id":"c_1","command":"ready"}"#;
    fixture.send(line);
    assert_eq!(
        fixture.wait_for(guard, "the echoed line", |got| got == line),
        line,
    );
    fixture.finish(guard);
}

#[test]
#[should_panic(expected = "the fixture exits 0")]
fn finish_fails_when_the_fixture_exits_unsuccessfully() {
    let fixture = ProcessFixture::spawn("bogus-mode");
    let deadline = within();
    fixture.finish(deadline);
}
