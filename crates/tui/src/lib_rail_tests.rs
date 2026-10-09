//! Loop-level tests for the rail: the wall time a card's elapsed time
//! reads comes from the loop's clock, ⌥N switches and the wheel scrolls
//! (`docs/tui.md`, "The rail").

use super::Input;
use super::tests::{command, feed, hello, new_loop};
use crate::home::Launch;
use crate::link::Line;
use contract::clock::Clock;
use ratatui::backend::TestBackend;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

const A: &str = "s_aaaaaaaaaaaaaaaa";
const B: &str = "s_bbbbbbbbbbbbbbbb";
const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

/// The screen's text.
fn shown(lp: &super::Loop<TestBackend>) -> String {
    crate::view::text(lp.screen.backend().buffer())
}

/// A loop at 200x40 with home state, attached to `A`.
fn wide() -> super::Loop<TestBackend> {
    let (mut lp, _) = new_loop(TestBackend::new(200, 40), None);
    lp.app.set_size(200, 40);
    lp.screen
        .resize(200, 40)
        .unwrap_or_else(|err| panic!("resize: {err}"));
    lp.app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: CARDS.map(str::to_owned).to_vec(),
        ..Default::default()
    });
    lp.app.attach(contract::SessionId(A.to_owned()));
    lp
}

/// A live idle `session_status` for `session` in that state since
/// `since`.
fn live(session: &str, since: u64) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "name": "work", "workspace": "/w", "project": "-w",
            "state": "idle", "since": since,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

#[test]
fn the_loop_gives_the_app_the_wall_time() {
    let mut lp = wide();
    let clock = fakes::clock::FakeClock::new();
    lp.clock = clock.clone();
    let since = contract::clock::wall_ms(clock.wall()) - 16_000;
    lp.app.on_line(live(A, since));
    lp.app.on_line(live(B, since));
    feed(&mut lp, vec![Input::Resize]);
    assert!(shown(&lp).contains("16s"), "{}", shown(&lp));
    clock.advance(Duration::from_secs(60));
    feed(&mut lp, vec![Input::Resize]);
    assert!(shown(&lp).contains("1m"), "{}", shown(&lp));
    assert!(!shown(&lp).contains("16s"), "{}", shown(&lp));
}

#[test]
fn alt_one_switches_in_the_loop() {
    let mut lp = wide();
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    lp.hub = Some(ours);
    lp.app.on_line(Line::Hub(hello()));
    lp.app.on_line(live(B, 0));
    lp.app.on_line(live(A, 0));
    // `B` is card 1; `A` is on screen.
    feed(&mut lp, vec![Input::Bytes(b"\x1b1".to_vec())]);
    let (_, subscribe) = command(BufReader::new(theirs), "the subscribe for B");
    assert_eq!(subscribe["command"], "subscribe");
    assert_eq!(subscribe["session_id"], B);
    assert_eq!(subscribe["args"]["level"], "full");
    assert_eq!(lp.app.session().map(|id| id.0.as_str()), Some(B));
}

#[test]
fn the_wheel_over_the_rail_scrolls_it_in_the_loop() {
    let mut lp = wide();
    lp.app.on_line(live(A, 0));
    for n in 1..10 {
        lp.app.on_line(live(&format!("s_{n:016x}"), 0));
    }
    feed(&mut lp, vec![Input::Resize]);
    assert!(shown(&lp).contains("  w "), "{}", shown(&lp));
    // A wheel-down report over the rail, 1-based.
    feed(&mut lp, vec![Input::Bytes(b"\x1b[<65;6;11M".to_vec())]);
    assert_eq!(lp.app.rail_state().scroll(), 1);
    assert!(!shown(&lp).contains("  w "), "{}", shown(&lp));
}
