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
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
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
        kind: contract::events::EventKind::SessionStatus,
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "name": "work", "workspace": "/w", "project": "-w",
            "state": "idle", "started": 0, "calls": 0, "since": since,
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
    // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "No code under test waits on the fake clock (ruling 6179406086, #2334 comment, part B)."
    )]
    clock.advance(Duration::from_secs(60));
    feed(&mut lp, vec![Input::Resize]);
    assert!(shown(&lp).contains("1m"), "{}", shown(&lp));
    assert!(!shown(&lp).contains("16s"), "{}", shown(&lp));
}

#[test]
fn alt_one_switches_in_the_loop() {
    let mut lp = wide();
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    lp.hub.store(ours);
    lp.app.on_line(Line::Hub(hello()));
    lp.app.on_line(live(B, 0));
    lp.app.on_line(live(A, 0));
    // `B` is card 1; `A` is on screen.
    feed(&mut lp, vec![Input::Bytes(b"\x1b1".to_vec())]);
    let (_, subscribe) = command(BufReader::new(theirs), "the subscribe for B");
    assert_eq!(subscribe["command"], "subscribe");
    assert_eq!(subscribe["session_id"], B);
    assert_eq!(subscribe["args"]["level"], "summary");
    assert_eq!(lp.app.session().map(|id| id.0.as_str()), Some(B));
}

/// A hub `session_left` for `session`, ended `how`.
fn left(session: &str, how: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: contract::HubEventKind::SessionLeft,
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({"session_id": session, "how": how})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

#[test]
fn exited_card_keeps_wall_time_elapsed_in_the_loop() {
    let mut lp = wide();
    let clock = fakes::clock::FakeClock::new();
    lp.clock = clock.clone();
    let since = contract::clock::wall_ms(clock.wall()) - 16_000;
    lp.app.on_line(live(A, since));
    lp.app.on_line(live(B, since));
    // The session on screen exits: its card stays in place with its
    // number and its elapsed from the loop's wall time.
    lp.app.on_line(left(A, "exited"));
    feed(&mut lp, vec![Input::Resize]);
    let frame = shown(&lp);
    assert!(frame.contains("1 ○ EXITED"), "{frame}");
    assert!(frame.contains("16s"), "{frame}");
    // The card persists across the next frame too.
    feed(&mut lp, vec![Input::Resize]);
    let frame = shown(&lp);
    assert!(frame.contains("1 ○ EXITED"), "{frame}");
    assert!(frame.contains("16s"), "{frame}");
}

/// A live `session_status` for `session` named `name`, in `state`.
fn named(session: &str, name: &str, state: serde_json::Value) -> Line {
    let mut payload = serde_json::json!({
        "name": name, "workspace": "/w", "project": "-w",
        "state": "idle", "started": 0, "calls": 0, "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    for (key, value) in state.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    Line::Session(contract::Envelope {
        kind: contract::events::EventKind::SessionStatus,
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// The durable archive marker for `session`, as a log replay delivers
/// it: an inert event, never a card.
fn archived(session: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: contract::events::EventKind::Archived,
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::Map::new(),
    })
}

fn waiting() -> serde_json::Value {
    serde_json::json!({"state": "waiting",
        "waiting": {"request_id": "r_1", "kind": "approval", "summary": "x"}})
}

/// The rail's card session ids, in feed order.
fn cards(lp: &super::Loop<TestBackend>) -> Vec<String> {
    lp.app
        .rail_cards()
        .unwrap_or_else(|| panic!("home holds cards"))
        .0
        .iter()
        .map(|row| row.id.0.clone())
        .collect()
}

#[test]
fn the_rail_excludes_archived_sessions_on_fresh_and_replayed_feed() {
    const E: &str = "s_eeeeeeeeeeeeeeee";
    const F: &str = "s_ffffffffffffffff";
    const C: &str = "s_cccccccccccccccc";
    const D: &str = "s_dddddddddddddddd";
    let mut lp = wide();
    // A fresh connection seeds the rail: two live sessions, an
    // ordinary crashed one and an ordinary waiting one. The hub's
    // store also holds a crashed archive (E) and a waiting archive
    // (F); default queries withhold them, and their replayed markers
    // stay inert: no card, no outgoing line.
    let _ = lp.app.on_line(Line::Hub(hello()));
    lp.app.on_line(named(A, "alpha", serde_json::json!({})));
    lp.app.on_line(named(B, "beta", serde_json::json!({})));
    lp.app.on_line(named(C, "gamma", serde_json::json!({})));
    lp.app.on_line(left(C, "crashed"));
    lp.app.on_line(named(D, "delta", waiting()));
    assert!(lp.app.on_line(archived(E)).is_empty());
    assert!(lp.app.on_line(archived(F)).is_empty());
    feed(&mut lp, vec![Input::Resize]);
    let fresh = shown(&lp);
    assert_eq!(cards(&lp), [A, B, C, D]);
    // A reconnect replays the same feed: the frame is unchanged, and
    // the archived pair still draws nothing.
    let _ = lp.app.on_line(Line::Hub(hello()));
    lp.app.on_line(named(A, "alpha", serde_json::json!({})));
    lp.app.on_line(named(B, "beta", serde_json::json!({})));
    lp.app.on_line(named(C, "gamma", serde_json::json!({})));
    lp.app.on_line(left(C, "crashed"));
    lp.app.on_line(named(D, "delta", waiting()));
    assert!(lp.app.on_line(archived(E)).is_empty());
    assert!(lp.app.on_line(archived(F)).is_empty());
    feed(&mut lp, vec![Input::Resize]);
    assert_eq!(shown(&lp), fresh);
    insta::assert_snapshot!("rail_excludes_archived_sessions", fresh);
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
