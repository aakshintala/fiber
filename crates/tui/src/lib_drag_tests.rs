//! Loop-level tests for dragging the rail's edge: the release saves the
//! share through the launch callback, and a failed save is a notice
//! (`docs/tui.md`, "Layout").

use super::Input;
use super::tests::{feed, new_loop};
use crate::home::Launch;
use crate::link::Line;
use ratatui::backend::TestBackend;
use std::path::PathBuf;
use std::sync::mpsc;

const A: &str = "s_aaaaaaaaaaaaaaaa";
const B: &str = "s_bbbbbbbbbbbbbbbb";
const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

/// The screen's text.
fn shown(lp: &super::Loop<TestBackend>) -> String {
    crate::view::text(lp.screen.backend().buffer())
}

/// A loop at 200x40 with home state, attached to `A`, saving through
/// `save`.
fn wide(save: Option<crate::Save>) -> super::Loop<TestBackend> {
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
    lp.save = save;
    lp
}

/// A live idle `session_status` for `session`.
fn live(session: &str) -> Line {
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
            "state": "idle", "since": 0,
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

/// A drag of the rail's edge to column 30, 1-based SGR reports: the press
/// on column 29, the drag to column 30, then the release.
fn drag() -> Vec<Input> {
    vec![
        Input::Bytes(b"\x1b[<0;30;21M".to_vec()),
        Input::Bytes(b"\x1b[<32;31;21M".to_vec()),
        Input::Bytes(b"\x1b[<0;31;21m".to_vec()),
    ]
}

#[test]
fn a_drag_saves_through_the_launch_callback() {
    let (out, saved) = mpsc::channel();
    let save: crate::Save = Box::new(move |key, share| {
        drop(out.send((key.to_owned(), share)));
        Ok(())
    });
    let mut lp = wide(Some(save));
    lp.app.on_line(live(A));
    lp.app.on_line(live(B));
    feed(&mut lp, drag());
    assert_eq!(
        saved.try_recv().unwrap_or_else(|err| panic!("save: {err}")),
        ("tui.rail.width".to_owned(), 15.5)
    );
    // Exactly one save for the one drag.
    assert!(saved.try_recv().is_err());
}

#[test]
fn a_failed_save_is_a_notice() {
    let save: crate::Save = Box::new(|_, _| Err("disk full".to_owned()));
    let mut lp = wide(Some(save));
    lp.app.on_line(live(A));
    lp.app.on_line(live(B));
    feed(&mut lp, drag());
    assert!(
        shown(&lp).contains("Could not save tui.rail.width: disk full"),
        "{}",
        shown(&lp)
    );
}
