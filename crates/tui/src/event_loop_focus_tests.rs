//! Tests for navigate mode through the loop: keys reach the app, and a
//! frame that drops the focused target draws again.

use super::Input;
use super::tests::{feed, new_loop};
use crate::ext::{Decision, Input as RendererInput, RawSpan, Renderer, SessionCtx, SessionVersion};
use crate::keys::Key;
use crate::link::Line;
use crate::mouse::{Target, TargetId};
use contract::clock::Clock;
use contract::events::EventKind;
use ratatui::backend::TestBackend;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const OTHER: &str = "s_bbbbbbbbbbbbbbbb";
const THIRD: &str = "s_cccccccccccccccc";

fn session(kind: &str, payload: serde_json::Value, action: Option<&str>) -> Input {
    Input::Hub(Line::Session(contract::Envelope {
        kind: EventKind::from_name(kind),
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
fn a_focus_reset_saves_the_rerendered_cells() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    let badge = Target {
        id: TargetId::Badge,
        rect: ratatui::layout::Rect::new(0, 0, 1, 1),
    };
    lp.app.drawn(&[badge]);
    lp.app
        .on_key(Key::BackTab, fakes::clock::FakeClock::new().now());
    assert_eq!(lp.app.focused(), Some(TargetId::Badge));

    let mut renders = 0;
    lp.screen
        .draw_with(&mut lp.app, None, |_, area, cells, _| {
            renders += 1;
            let text = if renders == 1 { "old" } else { "new" };
            cells.set_string(area.x, area.y, text, ratatui::style::Style::default());
            Vec::new()
        })
        .unwrap_or_else(|err| panic!("draw: {err}"));

    assert_eq!(renders, 2, "a stale focus causes a rerender");
    assert_eq!(
        lp.screen
            .last()
            .and_then(|(cells, _)| cells.cell((0, 0)))
            .map(|cell| cell.symbol()),
        Some("n"),
        "the saved frame contains the rerendered cells"
    );
}

#[test]
fn focus_that_loses_its_target_returns_in_the_same_frame() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.app.attach(contract::SessionId(SESSION.to_owned()));
    let mut inputs = replies_only();
    inputs.push(keys(b"\x1b[5~"));
    inputs.extend(reply("m_99", "fresh"));
    inputs.push(keys(b"\x1b[Z"));
    inputs.push(keys(b"\x1b[B"));
    inputs.push(keys(b"\r"));
    feed(&mut lp, inputs);
    assert_eq!(lp.app.focused(), None);
    assert!(
        lp.screen.last().is_some_and(|(_, cursor)| cursor.is_some()),
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

/// A renderer counting `on_focus` runs with the session each saw.
#[derive(Debug, Default)]
struct Counting {
    seen: RefCell<Vec<Option<String>>>,
}

impl Renderer for Counting {
    fn name(&self) -> &str {
        "counting"
    }

    fn fills(&self, _slot: &str) -> bool {
        false
    }

    fn on_focus(&self, ctx: &SessionCtx) {
        self.seen
            .borrow_mut()
            .push(ctx.session_id.as_ref().map(|id| id.0.clone()));
    }

    fn take_pending(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    fn render(
        &self,
        _ctx: &SessionCtx,
        _slot: &str,
        _input: &RendererInput<'_>,
        _width: u16,
        _builtin: &dyn Fn(&RendererInput<'_>, u16) -> Vec<Vec<RawSpan>>,
    ) -> Decision {
        Decision::NotForSession {
            extension: String::new(),
            tui_version: String::new(),
            half: SessionVersion::Unseen,
        }
    }
}

/// A loop on home with the counting renderer installed.
fn focused_loop() -> (super::Loop<TestBackend>, Rc<Counting>) {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    let counting = Rc::new(Counting::default());
    lp.app.install_renderer(counting.clone());
    (lp, counting)
}

fn session_id(id: &str) -> contract::SessionId {
    contract::SessionId(id.to_owned())
}

#[test]
fn focus_attach_home_and_reattach_fire_once_each() {
    let (mut lp, reported) = new_loop(TestBackend::new(60, 12), None);
    let counting = Rc::new(Counting::default());
    lp.app.install_renderer(counting.clone());
    lp.app.attach(session_id(SESSION));
    let _code = lp.after_input(None);
    // An attach input reports `on_attach` and fires focus exactly once each.
    assert_eq!(*reported.lock().unwrap(), vec![SESSION.to_owned()]);
    assert_eq!(*counting.seen.borrow(), vec![Some(SESSION.to_owned())]);
    // Home-leave through a real key input fires once with no session.
    let mut user = serde_json::Map::new();
    user.insert("new_session".to_owned(), serde_json::json!("ctrl+t"));
    lp.app.set_keys(crate::KeysSetup { user });
    let (_tx, rx) = mpsc::channel();
    let _code = lp.step(Input::Bytes(vec![0x14]), &rx);
    assert!(lp.app.session().is_none());
    assert_eq!(
        *counting.seen.borrow(),
        vec![Some(SESSION.to_owned()), None]
    );
    // Re-attach fires once with the observed identity.
    lp.app.attach(session_id(SESSION));
    let _code = lp.after_input(None);
    assert_eq!(
        *counting.seen.borrow(),
        vec![Some(SESSION.to_owned()), None, Some(SESSION.to_owned())]
    );
}

#[test]
fn focus_repeated_same_session_input_fires_nothing() {
    let (mut lp, counting) = focused_loop();
    lp.app.attach(session_id(SESSION));
    let _code = lp.after_input(None);
    // A repeated same-session input fires nothing.
    let (_tx, rx) = mpsc::channel();
    let _code = lp.step(Input::Tick, &rx);
    let _code = lp.after_input(Some(session_id(SESSION)));
    assert_eq!(*counting.seen.borrow(), vec![Some(SESSION.to_owned())]);
}

#[test]
fn focus_batch_holding_a_to_b_to_a_fires_nothing() {
    let (mut lp, counting) = focused_loop();
    lp.app.attach(session_id(SESSION));
    let _code = lp.after_input(None);
    // One batch holding A→B→A fires nothing: the session on screen
    // after it matches the one before it.
    lp.app.attach(session_id(OTHER));
    lp.app.attach(session_id(SESSION));
    let _code = lp.after_input(Some(session_id(SESSION)));
    assert_eq!(*counting.seen.borrow(), vec![Some(SESSION.to_owned())]);
}

#[test]
fn focus_batch_holding_a_to_b_to_c_fires_once_with_c() {
    let (mut lp, counting) = focused_loop();
    lp.app.attach(session_id(SESSION));
    let _code = lp.after_input(None);
    // One batch holding A→B→C fires once, with the session on screen now.
    lp.app.attach(session_id(OTHER));
    lp.app.attach(session_id(THIRD));
    let _code = lp.after_input(Some(session_id(SESSION)));
    assert_eq!(
        *counting.seen.borrow(),
        vec![Some(SESSION.to_owned()), Some(THIRD.to_owned())]
    );
}
