//! A loop-level test for the panel's wheel scrolling: the wheel over the
//! panel scrolls it (`docs/tui.md`, "The panel").

use super::Input;
use super::tests::{feed, new_loop};
use crate::home::Launch;
use crate::link::Line;
use ratatui::backend::TestBackend;
use std::path::PathBuf;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

#[test]
fn focus_and_stale_keypress_send_quota_through_the_loop() {
    use crate::app::items::testkit::{commands, hello, session_line};
    use serde_json::json;
    use std::io::Read;
    use std::sync::mpsc;
    use std::time::Duration;

    for (bytes, age, sends) in [
        (b"\x1b[I".as_slice(), 0, true),
        (b"x".as_slice(), 300, true),
        (b"x".as_slice(), 0, false),
        (b"\x1b[O".as_slice(), 300, false),
    ] {
        let (mut lp, _) = new_loop(TestBackend::new(80, 24), None);
        lp.app = crate::app::items::testkit::home();
        lp.app.on_line(hello());
        lp.app.attach(contract::SessionId(SESSION.to_owned()));
        lp.app.set_now(
            lp.clock
                .now()
                .checked_sub(Duration::from_secs(age))
                .unwrap(),
            0,
        );
        let out = commands(
            lp.app
                .on_line(session_line("clients", json!({"clients": []}))),
        );
        let id = out.iter().find(|v| v["command"] == "quota").unwrap()["id"]
            .as_str()
            .unwrap();
        lp.app.on_line(session_line(
            "command_accepted",
            json!({"command_id": id,
            "result": {"quota": [], "notice_at": 80.0}}),
        ));
        let (ours, mut theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut text = String::new();
            theirs.read_to_string(&mut text).unwrap();
            tx.send(text).unwrap();
        });
        lp.hub.store(ours);
        let events = lp.parser.feed(bytes);
        assert_eq!(lp.handle_events(events), None);
        lp.hub.hang_up();
        let text = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv_or_fail(&rx, "the loop's quota command");
        reader.join().unwrap();
        let out = commands(text.lines().map(str::to_owned).collect());
        assert_eq!(out.len(), usize::from(sends), "{bytes:?}, age {age}");
        if sends {
            assert_eq!(out[0]["command"], "quota");
            assert_eq!(out[0]["session_id"], SESSION);
        }
    }
}

/// The screen's text.
fn shown(lp: &super::Loop<TestBackend>) -> String {
    crate::view::text(lp.screen.backend().buffer())
}

/// A wheel-down report at 0-based `col`, `row`.
fn wheel(col: u16, row: u16) -> Input {
    Input::Bytes(format!("\x1b[<65;{};{}M", col + 1, row + 1).into_bytes())
}

#[test]
fn the_wheel_over_the_panel_scrolls_it_in_the_loop() {
    let (mut lp, _) = new_loop(TestBackend::new(160, 40), None);
    lp.app.set_size(160, 40);
    lp.screen
        .resize(160, 40)
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
    lp.app.attach(contract::SessionId(SESSION.to_owned()));
    let lines: Vec<String> = (0..60).map(|n| format!("line {n:02}")).collect();
    lp.app.on_line(Line::Session(contract::Envelope {
        kind: "extension_ui".to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: serde_json::json!({"extension": "plan", "widget": "tasks", "lines": lines})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }));
    feed(&mut lp, vec![Input::Resize]);
    assert!(shown(&lp).contains("plan · tasks"), "{}", shown(&lp));
    assert!(shown(&lp).contains("line 00"), "{}", shown(&lp));
    let panel = lp
        .app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel"));
    feed(&mut lp, vec![wheel(panel.x + 5, panel.y + 10)]);
    assert_eq!(lp.app.panel_state().scroll(), 3);
    // Five more wheels past the Session card's thirteen rows and the
    // blank row: the widget's third line sits first.
    for _ in 0..5 {
        feed(&mut lp, vec![wheel(panel.x + 5, panel.y + 10)]);
    }
    assert_eq!(lp.app.panel_state().scroll(), 18);
    assert!(shown(&lp).contains("line 02"), "{}", shown(&lp));
    assert!(!shown(&lp).contains("line 00"), "{}", shown(&lp));
    assert!(!shown(&lp).contains("plan · tasks"), "{}", shown(&lp));
}
