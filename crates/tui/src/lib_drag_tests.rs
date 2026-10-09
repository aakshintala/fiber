//! Loop-level tests for dragging the rail's edge: the release saves the
//! share through the configure seam, and a failed save is a notice
//! (`docs/tui.md`, "Layout").

use super::Input;
use super::tests::{Pair, feed, new_loop, open};
use crate::configure::{Configure, Layer};
use crate::configure_fake::Fake;
use crate::home::Launch;
use crate::link::Line;
use crate::osc;
use crate::pty_watch::{watch, watched};
use ratatui::backend::TestBackend;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};

const A: &str = "s_aaaaaaaaaaaaaaaa";
const B: &str = "s_bbbbbbbbbbbbbbbb";
const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

/// The screen's text.
fn shown(lp: &super::Loop<TestBackend>) -> String {
    crate::view::text(lp.screen.backend().buffer())
}

/// A loop at 200x40 with home state, attached to `A`, saving through
/// `seam`.
fn wide(seam: Option<Arc<Fake>>) -> super::Loop<TestBackend> {
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
    lp.app
        .set_configure(seam.map(|seam| seam as Arc<dyn Configure>));
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

/// A loop at 200x40 on a pty pair, with home state and two live sessions.
/// The watcher reads the pty from the first frame to end of file.
fn pty(markers: Vec<&'static [u8]>) -> (Pair, mpsc::Receiver<Vec<u8>>, super::Loop<TestBackend>) {
    let pair = open();
    let frames = watch(&pair.main, markers);
    let tty = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let (mut lp, _) = new_loop(TestBackend::new(200, 40), Some(tty));
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
    lp.app.on_line(live(A));
    lp.app.on_line(live(B));
    (pair, frames, lp)
}

/// Steps `lp` over one terminal read.
fn step(lp: &mut super::Loop<TestBackend>, bytes: &[u8]) {
    let (_, rx) = mpsc::channel();
    assert_eq!(lp.step(Input::Bytes(bytes.to_vec()), &rx), None);
}

#[test]
fn a_drag_saves_through_the_configure_seam() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut lp = wide(Some(Arc::clone(&fake)));
    lp.app.on_line(live(A));
    lp.app.on_line(live(B));
    feed(&mut lp, drag());
    // Exactly one write for the one drag: the share as a number in the
    // global file.
    assert_eq!(
        fake.writes(),
        vec![(
            PathBuf::from("/w"),
            Layer::Global,
            "tui.rail.width".to_owned(),
            "15.5".to_owned()
        )]
    );
}

#[test]
fn a_failed_save_is_a_notice() {
    let fake = Arc::new(Fake::new(Vec::new()));
    if let Ok(mut refuse) = fake.refuse.lock() {
        *refuse = Some("disk full".to_owned());
    }
    let mut lp = wide(Some(fake));
    lp.app.on_line(live(A));
    lp.app.on_line(live(B));
    feed(&mut lp, drag());
    assert!(
        shown(&lp).contains("Could not save tui.rail.width: disk full"),
        "{}",
        shown(&lp)
    );
}

#[test]
fn without_a_seam_a_drag_saves_nothing_silently() {
    let mut lp = wide(None);
    lp.app.on_line(live(A));
    lp.app.on_line(live(B));
    let before = shown(&lp);
    feed(&mut lp, drag());
    assert_ne!(shown(&lp), before, "the drag moved the rail's edge");
    assert!(!shown(&lp).contains("Could not save"), "{}", shown(&lp));
}

#[test]
fn a_save_warning_is_a_notice() {
    let fake = Arc::new(Fake::new(Vec::new()));
    if let Ok(mut warnings) = fake.warnings.lock() {
        *warnings = vec!["the width moved".to_owned()];
    }
    let mut lp = wide(Some(fake));
    lp.app.on_line(live(A));
    lp.app.on_line(live(B));
    feed(&mut lp, drag());
    assert!(shown(&lp).contains("the width moved"), "{}", shown(&lp));
}

#[test]
fn hover_over_an_edge_writes_col_resize_once_and_leaving_writes_default() {
    let (pair, frames, mut lp) = pty(vec![b"ENDMARK" as &[u8]]);
    // Motion onto the rail's edge twice, then off it.
    step(&mut lp, b"\x1b[<35;30;21M");
    step(&mut lp, b"\x1b[<35;30;21M");
    step(&mut lp, b"\x1b[<35;100;21M");
    (&pair.slave)
        .write_all(b"ENDMARK")
        .unwrap_or_else(|err| panic!("write: {err}"));
    let written = watched(&frames, "the mark");
    let expected = [
        osc::title("✓ work · fiber").as_slice(),
        osc::pointer(true),
        osc::pointer(false),
        b"ENDMARK".as_slice(),
    ]
    .concat();
    assert_eq!(written, expected);
}

#[test]
fn with_hover_off_no_osc_22_is_written() {
    let (pair, frames, mut lp) = pty(vec![b"ENDMARK" as &[u8]]);
    lp.hover = false;
    for input in drag() {
        let Input::Bytes(bytes) = input else {
            panic!("a drag is terminal bytes");
        };
        step(&mut lp, &bytes);
    }
    (&pair.slave)
        .write_all(b"ENDMARK")
        .unwrap_or_else(|err| panic!("write: {err}"));
    let written = watched(&frames, "the mark");
    assert!(
        !written
            .windows(osc::pointer(true).len())
            .any(|window| window == osc::pointer(true) || window == osc::pointer(false)),
        "{written:?}"
    );
}

#[test]
fn a_drag_off_the_edge_keeps_the_resize_arrow_until_release() {
    let (pair, frames, mut lp) = pty(vec![b"ENDMARK" as &[u8]]);
    // The press writes the title and the arrow; the drag off the edge
    // writes nothing; the release writes the default back.
    step(&mut lp, b"\x1b[<0;30;21M");
    step(&mut lp, b"\x1b[<32;100;21M");
    step(&mut lp, b"\x1b[<0;100;21m");
    (&pair.slave)
        .write_all(b"ENDMARK")
        .unwrap_or_else(|err| panic!("write: {err}"));
    let written = watched(&frames, "the mark");
    let expected = [
        osc::title("✓ work · fiber").as_slice(),
        osc::pointer(true),
        osc::pointer(false),
        b"ENDMARK".as_slice(),
    ]
    .concat();
    assert_eq!(written, expected);
}

#[test]
fn after_the_editor_returns_the_pointer_shape_is_written_again() {
    let (pair, frames, mut lp) = pty(vec![b"\x1b[c" as &[u8], b"ENDMARK" as &[u8]]);
    crate::term::setup(&pair.slave, true).unwrap_or_else(|err| panic!("setup: {err}"));
    watched(&frames, "the setup queries");
    // The pointer sits on the rail's edge before the editor opens.
    step(&mut lp, b"\x1b[<35;30;21M");
    // The size does not change across the hand-over.
    rustix::termios::tcsetwinsize(
        &pair.slave,
        rustix::termios::Winsize {
            ws_col: 200,
            ws_row: 40,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap_or_else(|err| panic!("winsize: {err}"));
    assert_eq!(lp.hand_over(|| {}), None);
    lp.write_shape();
    (&pair.slave)
        .write_all(b"ENDMARK")
        .unwrap_or_else(|err| panic!("write: {err}"));
    let tail = watched(&frames, "the mark");
    // After the resume bytes the arrow is written again.
    let resumed: &[u8] =
        b"\x1b[?1049h\x1b[22;2t\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h";
    let at = tail
        .windows(resumed.len())
        .position(|window| window == resumed)
        .expect("the resume bytes");
    let after = tail
        .get(at.saturating_add(resumed.len())..)
        .unwrap_or_default();
    assert!(
        after
            .windows(osc::pointer(true).len())
            .any(|window| window == osc::pointer(true)),
        "{tail:?}"
    );
    crate::term::restore();
}
