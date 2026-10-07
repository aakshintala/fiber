//! Tests for navigate mode through the loop: keys reach the app, and a
//! frame that drops the focused target draws again.

use super::Input;
use super::tests::{feed, new_loop};
use crate::link::Line;
use ratatui::backend::TestBackend;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

fn session(kind: &str, payload: serde_json::Value, action: Option<&str>) -> Input {
    Input::Hub(Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }))
}

fn started() -> Input {
    session(
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    )
}

fn done() -> Input {
    session(
        "turn_completed",
        serde_json::json!({"outcome": "completed"}),
        None,
    )
}

fn reply(action: &str, text: &str) -> Vec<Input> {
    vec![
        session(
            "assistant_message_delta",
            serde_json::json!({"text": text}),
            Some(action),
        ),
        session(
            "text_completed",
            serde_json::json!({"text": text}),
            Some(action),
        ),
    ]
}

/// Twenty replies with the turn left open: no line target, scrolled up
/// with new output on the way.
fn replies_only() -> Vec<Input> {
    let mut out = vec![started()];
    for i in 1..=20 {
        out.extend(reply(&format!("m_{i}"), &format!("reply {i}")));
    }
    out
}

fn keys(bytes: &[u8]) -> Input {
    Input::Bytes(bytes.to_vec())
}

#[test]
fn focus_that_loses_its_target_returns_in_the_same_frame() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.app.attach(contract::SessionId(SESSION.to_owned()));
    let mut inputs = replies_only();
    inputs.push(keys(b"\x1b[5~"));
    inputs.extend(reply("m_99", "fresh"));
    inputs.push(keys(b"\x1b[Z"));
    inputs.push(keys(b"\r"));
    feed(&mut lp, inputs);
    assert_eq!(lp.app.focused(), None);
    assert!(
        lp.screen
            .last
            .as_ref()
            .is_some_and(|(_, cursor)| cursor.is_some()),
        "the redrawn frame shows the cursor"
    );
}

#[test]
fn shift_tab_and_esc_reach_the_app_through_the_loop() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.app.attach(contract::SessionId(SESSION.to_owned()));
    feed(
        &mut lp,
        vec![
            started(),
            session(
                "tool_call_requested",
                serde_json::json!({"name": "read", "arguments": {"path": "src/1.rs"}}),
                Some("a_1"),
            ),
            session(
                "tool_call_completed",
                serde_json::json!({"status": "completed",
                    "content": [{"type": "text", "text": "ok"}]}),
                Some("a_1"),
            ),
            done(),
        ],
    );
    assert_eq!(lp.app.focused(), None);
    feed(&mut lp, vec![keys(b"\x1b[Z")]);
    assert!(lp.app.focused().is_some());
    feed(&mut lp, vec![keys(b"\x1b")]);
    assert_eq!(lp.app.focused(), None);
}
