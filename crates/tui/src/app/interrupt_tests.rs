//! Tests for interrupting the turn on screen (`docs/tui.md`,
//! "Interrupts"): ⌥I and the two click targets cancel the turn of the
//! conversation on screen, a delegate's own turn in its view, and Esc in
//! that view only closes it.

use std::sync::Arc;

use contract::SessionId;
use contract::events::EventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use serde_json::{Value, json};

use super::commands::commands_testkit::{now, sent};
use super::items::testkit::{
    DELEGATE_A, SESSION, complete, home, opened, opened_item, session_line, start_delegate,
    start_job,
};
use super::{App, Effect};
use crate::Configure;
use crate::configure::KeyEdit;
use crate::configure_fake::Fake;
use crate::keys::Key;
use crate::link::Line;
use crate::mouse::{Target, TargetId};
use crate::stroke::Stroke;
use crate::theme::Role;

/// One envelope of `session`.
fn line_of(session: &str, kind: &str, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: EventKind::from_name(kind),
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A `turn_started` payload.
fn turn() -> Value {
    json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]})
}

/// The attached session starts a turn.
fn parent_turn(app: &mut App) {
    app.on_line(session_line("turn_started", turn()));
}

/// The delegate starts a turn.
fn delegate_turn(app: &mut App) {
    app.on_line(line_of(DELEGATE_A, "turn_started", turn()));
}

/// An attached app with the delegate's view open, its parent idle.
fn delegate_view() -> App {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app
}

/// Presses the stroke `name` names, returning the lines it sends.
fn press(app: &mut App, name: &str) -> Vec<Value> {
    let stroke = Stroke::parse(name).unwrap_or_else(|err| panic!("{name}: {err}"));
    sent(app.on_press(stroke, now()))
}

/// The `cancel` commands among `out`, as `(session, id)`.
fn cancels(out: &[Value]) -> Vec<String> {
    out.iter()
        .filter(|line| line["command"] == "cancel")
        .map(|line| line["session_id"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// Loads `entries` as the person's `keys`.
fn keyed(app: &mut App, entries: &[(&str, Value)]) {
    let mut user = serde_json::Map::new();
    for (id, value) in entries {
        user.insert((*id).to_owned(), value.clone());
    }
    app.set_keys(crate::KeysSetup { user });
}

#[test]
fn alt_i_interrupts_a_busy_main_turn_and_nothing_when_idle() {
    let mut app = home();
    opened(&mut app);
    // Idle: no turn to interrupt.
    assert!(press(&mut app, "alt+i").is_empty());
    // The positive control: the same stroke on a running turn cancels.
    parent_turn(&mut app);
    let out = press(&mut app, "alt+i");
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(cancels(&out), [SESSION]);
}

#[test]
fn alt_i_sends_nothing_while_disconnected() {
    let mut app = home();
    opened(&mut app);
    parent_turn(&mut app);
    app.disconnected();
    assert!(press(&mut app, "alt+i").is_empty());
}

#[test]
fn alt_i_in_a_delegate_view_cancels_the_delegate_with_an_idle_parent() {
    let mut app = delegate_view();
    delegate_turn(&mut app);
    let out = press(&mut app, "alt+i");
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(cancels(&out), [DELEGATE_A]);
    // The view stays open.
    assert!(app.item_open());
}

#[test]
fn alt_i_in_a_delegate_view_cancels_the_delegate_not_a_busy_parent() {
    let mut app = delegate_view();
    parent_turn(&mut app);
    delegate_turn(&mut app);
    let out = press(&mut app, "alt+i");
    assert_eq!(cancels(&out), [DELEGATE_A], "{out:?}");
    assert_eq!(out.len(), 1);
}

#[test]
fn alt_i_in_an_idle_delegate_view_never_cancels_the_busy_parent() {
    let mut app = delegate_view();
    parent_turn(&mut app);
    assert!(press(&mut app, "alt+i").is_empty());
}

#[test]
fn alt_i_in_a_finished_delegate_view_never_cancels_the_busy_parent() {
    let mut app = delegate_view();
    delegate_turn(&mut app);
    parent_turn(&mut app);
    complete(&mut app, "j_1");
    assert!(app.item_open());
    assert!(press(&mut app, "alt+i").is_empty());
}

#[test]
fn alt_i_after_the_delegate_turn_completes_sends_nothing() {
    let mut app = delegate_view();
    delegate_turn(&mut app);
    assert_eq!(cancels(&press(&mut app, "alt+i")), [DELEGATE_A]);
    app.on_line(line_of(
        DELEGATE_A,
        "turn_completed",
        json!({"outcome": "interrupted"}),
    ));
    assert!(press(&mut app, "alt+i").is_empty());
}

#[test]
fn alt_i_in_a_job_view_interrupts_the_busy_main_turn() {
    let mut app = home();
    opened(&mut app);
    start_job(&mut app, "j_2");
    opened_item(&mut app, "j_2");
    assert!(press(&mut app, "alt+i").is_empty());
    parent_turn(&mut app);
    assert_eq!(cancels(&press(&mut app, "alt+i")), [SESSION]);
}

#[test]
fn clicking_the_working_line_target_in_a_delegate_view_cancels_the_delegate() {
    let mut app = delegate_view();
    parent_turn(&mut app);
    delegate_turn(&mut app);
    let out = sent(app.on_click(TargetId::Interrupt));
    assert_eq!(cancels(&out), [DELEGATE_A], "{out:?}");
    assert_eq!(out.len(), 1);
    // Idle delegate: the target is gone and the click is a no-op.
    let mut idle = delegate_view();
    parent_turn(&mut idle);
    assert_eq!(idle.on_click(TargetId::Interrupt), Effect::None);
}

#[test]
fn esc_in_a_delegate_view_closes_it_and_sends_no_cancel() {
    let mut app = delegate_view();
    parent_turn(&mut app);
    delegate_turn(&mut app);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.item_open());
}

#[test]
fn a_cancel_leaves_the_draft_and_the_steering_queue_alone() {
    let mut app = delegate_view();
    delegate_turn(&mut app);
    app.draft.set("half typed");
    let out = press(&mut app, "alt+i");
    assert_eq!(cancels(&out), [DELEGATE_A]);
    assert_eq!(app.draft(), "half typed");
}

#[test]
fn a_rebound_key_interrupts_and_the_old_key_stops() {
    let mut app = home();
    opened(&mut app);
    parent_turn(&mut app);
    keyed(&mut app, &[("interrupt_turn", json!("f5"))]);
    assert!(press(&mut app, "alt+i").is_empty());
    assert_eq!(cancels(&press(&mut app, "f5")), [SESSION]);
}

#[test]
fn an_unbound_action_interrupts_from_no_key() {
    let mut app = home();
    opened(&mut app);
    parent_turn(&mut app);
    keyed(&mut app, &[("interrupt_turn", json!([]))]);
    assert!(press(&mut app, "alt+i").is_empty());
    // The click target still works.
    assert_eq!(cancels(&sent(app.on_click(TargetId::Interrupt))), [SESSION]);
}

/// `app` drawn whole at 100x30: the frame's text and its click targets.
fn frame(app: &App) -> (String, Vec<Target>, Buffer) {
    let area = Rect::new(0, 0, 100, 30);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    (crate::view::text(&buf), targets, buf)
}

/// The row of `text` holding `needle`, with the column the needle starts.
fn find(text: &str, needle: &str) -> (u16, u16) {
    text.lines()
        .enumerate()
        .find_map(|(y, row)| {
            row.find(needle).map(|at| {
                (
                    u16::try_from(row[..at].chars().count()).unwrap_or(u16::MAX),
                    u16::try_from(y).unwrap_or(u16::MAX),
                )
            })
        })
        .unwrap_or_else(|| panic!("no {needle:?} in\n{text}"))
}

/// The interrupt targets of `targets`, top to bottom.
fn interrupt_targets(targets: &[Target]) -> Vec<Target> {
    let mut found: Vec<Target> = targets
        .iter()
        .filter(|target| target.id == TargetId::Interrupt)
        .cloned()
        .collect();
    found.sort_by_key(|target| target.rect.y);
    found
}

/// A delegate's view sized for [`frame`], its turn running.
fn running_view() -> App {
    let mut app = delegate_view();
    app.set_size(100, 30);
    delegate_turn(&mut app);
    app.settle();
    app
}

#[test]
fn a_running_delegate_view_offers_both_targets_before_stop() {
    let app = running_view();
    let (text, targets, buf) = frame(&app);
    let card = format!("⏸ interrupt · ■ stop {DELEGATE_A}");
    let (card_x, card_y) = find(&text, &card);
    let (hint_x, hint_y) = find(&text, "⌥I or click to interrupt");
    assert!(text.contains("Working"), "{text}");
    assert!(text.contains(" · ⌥I or click to interrupt"), "{text}");
    let found = interrupt_targets(&targets);
    assert_eq!(found.len(), 2, "{found:?}");
    // The card's target covers exactly its label, the line's exactly its hint.
    let label = u16::try_from("⏸ interrupt".chars().count()).unwrap_or(u16::MAX);
    assert_eq!(
        (found[0].rect.x, found[0].rect.y, found[0].rect.width),
        (card_x, card_y, label)
    );
    let hint = u16::try_from("⌥I or click to interrupt".chars().count()).unwrap_or(u16::MAX);
    assert_eq!(
        (found[1].rect.x, found[1].rect.y, found[1].rect.width),
        (hint_x, hint_y, hint)
    );
    // The stop target starts after the label and its separator.
    let stop = targets
        .iter()
        .find(|target| target.id == TargetId::Item(crate::app::items::Spot::Stop))
        .expect("the stop target");
    assert_eq!(stop.rect.x, card_x + label + 3);
    // The label draws in `attention` over the card's `surface` tint.
    for dx in 0..label {
        let cell = &buf[(card_x + dx, card_y)];
        assert_eq!(cell.fg, Role::Attention.color(), "cell {dx}");
        assert_eq!(cell.bg, Role::Surface.color(), "cell {dx}");
    }
    let tail: Color = buf[(card_x + label + 3, card_y)].fg;
    assert_eq!(tail, Role::Error.color(), "the stop label keeps `error`");
    insta::assert_snapshot!("delegate_turn_running", text);
}

#[test]
fn an_idle_delegate_offers_neither_target() {
    let mut app = delegate_view();
    app.set_size(100, 30);
    // Before any turn, and with the parent busy: nothing to interrupt.
    parent_turn(&mut app);
    app.settle();
    let (text, targets, _) = frame(&app);
    assert!(!text.contains("interrupt"), "{text}");
    assert!(!text.contains("Working"), "{text}");
    assert!(interrupt_targets(&targets).is_empty());
    assert!(
        text.contains(&format!("■ stop {DELEGATE_A}")),
        "the stop stays: {text}"
    );
    // The positive control: its turn starting brings both back.
    delegate_turn(&mut app);
    app.settle();
    let (text, targets, _) = frame(&app);
    assert!(text.contains("⏸ interrupt"), "{text}");
    assert_eq!(interrupt_targets(&targets).len(), 2);
    // The turn ending takes them away again.
    app.on_line(line_of(
        DELEGATE_A,
        "turn_completed",
        json!({"outcome": "completed"}),
    ));
    app.settle();
    let (text, targets, _) = frame(&app);
    assert!(!text.contains("⏸ interrupt"), "{text}");
    assert!(interrupt_targets(&targets).is_empty());
}

#[test]
fn a_finished_delegate_offers_neither_target() {
    let mut app = delegate_view();
    app.set_size(100, 30);
    delegate_turn(&mut app);
    complete(&mut app, "j_1");
    app.settle();
    let (text, targets, _) = frame(&app);
    assert!(!text.contains("⏸ interrupt"), "{text}");
    assert!(interrupt_targets(&targets).is_empty());
}

#[test]
fn each_target_and_the_key_cancel_the_delegate_in_a_fresh_view() {
    // Each activation gets its own fixture, so one cannot satisfy another.
    let targets_of = |app: &App| interrupt_targets(&frame(app).1);
    for which in 0..2 {
        let mut app = running_view();
        let found = targets_of(&app);
        let out = sent(app.on_click(found[which].id));
        assert_eq!(cancels(&out), [DELEGATE_A], "target {which}: {out:?}");
        assert_eq!(out.len(), 1);
    }
    let mut app = running_view();
    let out = press(&mut app, "alt+i");
    assert_eq!(cancels(&out), [DELEGATE_A]);
    assert_eq!(out.len(), 1);
}

#[test]
fn an_interrupted_delegate_turn_closes_with_the_interrupted_line() {
    let mut app = running_view();
    assert_eq!(cancels(&press(&mut app, "alt+i")), [DELEGATE_A]);
    app.on_line(line_of(
        DELEGATE_A,
        "turn_completed",
        json!({"outcome": "interrupted"}),
    ));
    app.settle();
    let (text, targets, _) = frame(&app);
    assert!(text.contains("▣"), "{text}");
    assert!(text.contains("interrupted"), "{text}");
    assert!(!text.contains("Interrupted"), "no separate row: {text}");
    assert!(!text.contains("⏸ interrupt"), "{text}");
    assert!(interrupt_targets(&targets).is_empty());
    insta::assert_snapshot!("delegate_turn_interrupted", text);
}

#[test]
fn another_harness_delegate_offers_no_delegate_targets() {
    let mut app = home();
    opened(&mut app);
    app.on_line(session_line(
        "job_started",
        json!({"job_id": "j_9", "description": "x", "output_path": "/tmp/o"}),
    ));
    app.on_line(session_line(
        "delegate_started",
        json!({"job_id": "j_9", "delegate_session_id": DELEGATE_A,
            "harness": "claude", "model": "m", "workspace": "/w"}),
    ));
    opened_item(&mut app, "j_9");
    app.set_size(100, 30);
    // The parent is idle and there is no transcript: nothing to interrupt.
    let (text, targets, _) = frame(&app);
    assert!(!text.contains("⏸ interrupt"), "{text}");
    assert!(interrupt_targets(&targets).is_empty());
    assert!(press(&mut app, "alt+i").is_empty());
}

#[test]
fn closing_and_reopening_the_view_keeps_the_targets_with_the_turn() {
    let mut app = running_view();
    app.on_key(Key::Esc, now());
    assert!(!app.item_open());
    app.settle();
    let (_, targets, _) = frame(&app);
    assert!(
        interrupt_targets(&targets).is_empty(),
        "the conversation's own line needs its own busy turn"
    );
    opened_item(&mut app, "j_1");
    // The replay of the log brings the turn back.
    delegate_turn(&mut app);
    app.settle();
    assert_eq!(interrupt_targets(&frame(&app).1).len(), 2);
}

#[test]
fn the_hint_names_the_first_bound_key_and_leaves_it_out_when_unbound() {
    let mut app = running_view();
    keyed(&mut app, &[("interrupt_turn", json!(["f5", "alt+j"]))]);
    app.settle();
    let (text, _, _) = frame(&app);
    assert!(text.contains(" · F5 or click to interrupt"), "{text}");
    assert!(!text.contains("⌥I"), "{text}");
    keyed(&mut app, &[("interrupt_turn", json!([]))]);
    app.settle();
    let (text, targets, _) = frame(&app);
    assert!(text.contains(" · click to interrupt"), "{text}");
    assert!(!text.contains(" or click"), "{text}");
    assert_eq!(interrupt_targets(&targets).len(), 2);
}

/// Draws whatever the built-in working line draws, so the test sees the
/// hint through an installed renderer's cache.
#[derive(Debug)]
struct Echo;

impl crate::ext::Renderer for Echo {
    fn name(&self) -> &str {
        "echo"
    }

    fn fills(&self, slot: &str) -> bool {
        slot == "working_line"
    }

    fn render(
        &self,
        _ctx: &crate::ext::SessionCtx,
        _slot: &str,
        input: &crate::ext::Input<'_>,
        width: u16,
        builtin: &dyn Fn(&crate::ext::Input<'_>, u16) -> Vec<Vec<crate::ext::RawSpan>>,
    ) -> crate::ext::Decision {
        crate::ext::Decision::Draw {
            extension: self.name().into(),
            lines: builtin(input, width),
        }
    }
}

#[test]
fn a_warm_renderer_shows_the_new_hint_after_a_key_change_and_the_old_after_a_refusal() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = running_view();
    app.set_configure(Arc::clone(&fake) as Arc<dyn Configure>);
    app.install_renderer(std::rc::Rc::new(Echo));
    app.settle();
    // Warm: drawn once with the default key.
    assert!(frame(&app).0.contains(" · ⌥I or click to interrupt"));
    // A refused save keeps the old binding and the old hint.
    if let Ok(mut refuse) = fake.refuse.lock() {
        *refuse = Some("disk full".to_owned());
    }
    rebind_in_screen(&mut app, "alt+j");
    app.settle();
    assert!(frame(&app).0.contains(" · ⌥I or click to interrupt"));
    assert_eq!(
        app.keys().first_label("interrupt_turn").as_deref(),
        Some("⌥I")
    );
    if let Ok(mut refuse) = fake.refuse.lock() {
        *refuse = None;
    }
    // A saved one shows the new key on the next frame.
    rebind_in_screen(&mut app, "alt+j");
    app.settle();
    let text = frame(&app).0;
    assert!(text.contains(" · ⌥J or click to interrupt"), "{text}");
    assert!(!text.contains("⌥I or"), "{text}");
    assert!(press(&mut app, "alt+i").is_empty());
    assert_eq!(cancels(&press(&mut app, "alt+j")), [DELEGATE_A]);
}

/// Opens `/keys`, binds `interrupt_turn` to `key` and closes the screen.
fn rebind_in_screen(app: &mut App, key: &str) {
    for ch in "/keys".chars() {
        app.on_key(Key::Char(ch), now());
    }
    app.on_key(Key::Enter, now());
    assert!(app.keys_screen_open());
    // `interrupt_turn` is the ninth action; a failed save leaves the
    // screen open on its message.
    for _ in 0..8 {
        app.on_press(Stroke::parse("down").expect("down"), now());
    }
    app.on_press(Stroke::parse("enter").expect("enter"), now());
    app.on_press(Stroke::parse(key).expect("key"), now());
    app.on_press(Stroke::parse("esc").expect("esc"), now());
    assert!(!app.keys_screen_open());
}

/// The delegate's `working_line` replacement sees the delegate's turn.
#[derive(Debug, Default)]
struct Started(std::cell::Cell<Option<Option<u64>>>);

impl crate::ext::Renderer for Started {
    fn name(&self) -> &str {
        "started"
    }

    fn fills(&self, slot: &str) -> bool {
        slot == "working_line"
    }

    fn render(
        &self,
        _ctx: &crate::ext::SessionCtx,
        _slot: &str,
        input: &crate::ext::Input<'_>,
        width: u16,
        builtin: &dyn Fn(&crate::ext::Input<'_>, u16) -> Vec<Vec<crate::ext::RawSpan>>,
    ) -> crate::ext::Decision {
        if let crate::ext::Input::WorkingLine(line) = input {
            self.0.set(Some(line.started));
        }
        crate::ext::Decision::Draw {
            extension: self.name().into(),
            lines: builtin(input, width),
        }
    }
}

#[test]
fn a_working_line_replacement_in_a_delegate_view_gets_the_delegates_start() {
    let mut app = delegate_view();
    app.set_size(100, 30);
    let seen = std::rc::Rc::new(Started::default());
    app.install_renderer(seen.clone());
    // The parent started its turn at ts 1_000, the delegate at ts 9_000.
    let at = |session: &str, ts: u64| {
        let Line::Session(mut envelope) = line_of(session, "turn_started", turn()) else {
            panic!("a session line")
        };
        envelope.ts = ts;
        Line::Session(envelope)
    };
    app.on_line(at(SESSION, 1_000));
    app.on_line(at(DELEGATE_A, 9_000));
    app.settle();
    frame(&app);
    assert_eq!(seen.0.get(), Some(Some(9_000)));
}

#[test]
fn a_delegate_view_never_draws_the_parents_working_line() {
    let mut app = delegate_view();
    app.set_size(100, 30);
    parent_turn(&mut app);
    app.settle();
    let (text, _, _) = frame(&app);
    assert!(!text.contains("Working"), "{text}");
    // Closing the view brings the parent's own line back.
    app.on_key(Key::Esc, now());
    app.settle();
    let (text, targets, _) = frame(&app);
    assert!(text.contains("Esc to interrupt"), "{text}");
    assert_eq!(interrupt_targets(&targets).len(), 1);
}

/// Opens `/keys` on `app` through the draft.
fn open_keys(app: &mut App) {
    for ch in "/keys".chars() {
        app.on_key(Key::Char(ch), now());
    }
    app.on_key(Key::Enter, now());
    assert!(app.keys_screen_open());
}

/// A busy main conversation with the configuration seam `fake`.
fn busy_main(fake: &Arc<Fake>) -> App {
    let mut app = home();
    opened(&mut app);
    app.set_configure(Arc::clone(fake) as Arc<dyn Configure>);
    app.set_size(100, 30);
    parent_turn(&mut app);
    app
}

#[test]
fn alt_i_is_a_screen_key_on_slash_keys_and_never_interrupts_in_browse() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = busy_main(&fake);
    // The positive control: with `/keys` closed the stroke cancels.
    assert_eq!(cancels(&press(&mut app, "alt+i")), [SESSION]);
    open_keys(&mut app);
    assert!(press(&mut app, "alt+i").is_empty());
    assert!(app.keys_screen_open());
    assert!(fake.keys_saved().is_empty());
}

#[test]
fn capturing_alt_i_for_another_action_prompts_a_clash_and_never_interrupts() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = busy_main(&fake);
    open_keys(&mut app);
    // `new_session` is the sixth action.
    for _ in 0..5 {
        press(&mut app, "down");
    }
    assert!(press(&mut app, "enter").is_empty());
    assert!(press(&mut app, "alt+i").is_empty());
    app.settle();
    let (text, _, _) = frame(&app);
    assert!(
        text.contains("⌥I is bound to Interrupt the turn on screen"),
        "{text}"
    );
    insta::assert_snapshot!("keys_clash_on_alt_i", text);
    assert!(fake.keys_saved().is_empty());
    // In the swap-or-cancel choice the stroke is still the screen's.
    assert!(press(&mut app, "alt+i").is_empty());
    assert!(fake.keys_saved().is_empty());
    // Confirming swaps: `new_session` takes ⌥I and `interrupt_turn` the
    // key `new_session` held.
    assert!(press(&mut app, "enter").is_empty());
    assert_eq!(
        fake.keys_saved(),
        [vec![
            KeyEdit {
                id: "new_session".to_owned(),
                keys: Some(vec!["alt+i".to_owned()]),
            },
            KeyEdit {
                id: "interrupt_turn".to_owned(),
                keys: Some(vec!["ctrl+n".to_owned()]),
            },
        ]]
    );
    assert_eq!(app.keys().first_label("new_session").as_deref(), Some("⌥I"));
    assert_eq!(
        app.keys().first_label("interrupt_turn").as_deref(),
        Some("Ctrl+N")
    );
}

#[test]
fn capturing_alt_i_with_interrupt_turn_unbound_binds_it_at_once() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = busy_main(&fake);
    keyed(&mut app, &[("interrupt_turn", json!([]))]);
    open_keys(&mut app);
    for _ in 0..5 {
        press(&mut app, "down");
    }
    assert!(press(&mut app, "enter").is_empty());
    assert!(press(&mut app, "alt+i").is_empty());
    assert_eq!(
        fake.keys_saved(),
        [vec![KeyEdit {
            id: "new_session".to_owned(),
            keys: Some(vec!["alt+i".to_owned()]),
        }]]
    );
    assert_eq!(app.keys().first_label("new_session").as_deref(), Some("⌥I"));
}

#[test]
fn esc_cancels_a_capture_then_closes_the_screen_with_no_interrupt() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = busy_main(&fake);
    open_keys(&mut app);
    press(&mut app, "enter");
    assert!(press(&mut app, "esc").is_empty());
    assert!(app.keys_screen_open());
    assert!(press(&mut app, "esc").is_empty());
    assert!(!app.keys_screen_open());
    assert!(fake.keys_saved().is_empty());
}

#[test]
fn a_notice_over_slash_keys_still_hides_alt_i_and_ctrl_c_still_clears() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let mut app = busy_main(&fake);
    open_keys(&mut app);
    app.notices
        .push(contract::ErrorCode::ExtensionFailed, "first".to_owned());
    app.open_more_notices();
    assert!(app.notice_overlay().is_some());
    assert!(press(&mut app, "alt+i").is_empty());
    assert!(app.notice_overlay().is_some());
    // Ctrl+C reaches the bindings: with an empty box it asks to quit.
    press(&mut app, "ctrl+c");
    assert!(app.hint());
}

#[test]
fn alt_i_interrupts_the_main_turn_and_leaves_an_overlay_open() {
    // The key map over the conversation.
    let mut app = home();
    opened(&mut app);
    parent_turn(&mut app);
    app.on_key(Key::F1, now());
    assert!(app.keymap_top().is_some());
    assert_eq!(cancels(&press(&mut app, "alt+i")), [SESSION]);
    assert!(app.keymap_top().is_some(), "the overlay stays open");
    // The model picker over it.
    let mut app = home();
    opened(&mut app);
    parent_turn(&mut app);
    app.on_key(Key::CtrlL, now());
    assert!(app.model_picker_open());
    assert_eq!(cancels(&press(&mut app, "alt+i")), [SESSION]);
    assert!(app.model_picker_open());
    // A notice overlay.
    let mut app = home();
    opened(&mut app);
    parent_turn(&mut app);
    app.notices
        .push(contract::ErrorCode::ExtensionFailed, "first".to_owned());
    app.open_more_notices();
    assert!(app.notice_overlay().is_some());
    assert_eq!(cancels(&press(&mut app, "alt+i")), [SESSION]);
    assert!(app.notice_overlay().is_some());
}

#[test]
fn alt_i_over_a_key_map_in_a_delegate_view_cancels_the_delegate_and_keeps_both_open() {
    let mut app = delegate_view();
    parent_turn(&mut app);
    delegate_turn(&mut app);
    app.on_key(Key::F1, now());
    assert!(app.keymap_top().is_some());
    let out = press(&mut app, "alt+i");
    assert_eq!(cancels(&out), [DELEGATE_A], "{out:?}");
    assert!(app.keymap_top().is_some());
    assert!(app.item_open());
}

/// A `steering_queue` line of `session` holding `texts`.
fn queue(session: &str, texts: &[&str]) -> Line {
    let messages: Vec<Value> = texts
        .iter()
        .map(|text| json!({"content": [{"type": "text", "text": text}], "source": "driver"}))
        .collect();
    line_of(session, "steering_queue", json!({"messages": messages}))
}

#[test]
fn an_interrupt_leaves_queued_steering_and_the_next_turn_reads_it() {
    let mut app = delegate_view();
    app.set_size(100, 30);
    delegate_turn(&mut app);
    app.on_line(queue(DELEGATE_A, &["use the parser", "then the tests"]));
    app.settle();
    let before = frame(&app).0;
    assert!(before.contains("↳ use the parser"), "{before}");
    let out = press(&mut app, "alt+i");
    assert_eq!(cancels(&out), [DELEGATE_A]);
    assert_eq!(out.len(), 1, "only the cancel goes out: {out:?}");
    app.settle();
    // The queue stands until the runtime writes the turn's end.
    let held = frame(&app).0;
    assert!(held.contains("↳ use the parser"), "{held}");
    assert!(held.contains("↳ then the tests"), "{held}");
    // The runtime closes the turn, then starts the next one on the queue.
    app.on_line(line_of(
        DELEGATE_A,
        "turn_completed",
        json!({"outcome": "interrupted"}),
    ));
    app.settle();
    let closed = frame(&app).0;
    assert!(closed.contains("▣"), "{closed}");
    assert!(!closed.contains("⏸ interrupt"), "{closed}");
    insta::assert_snapshot!("delegate_interrupted_with_steering_queued", closed);
    app.on_line(queue(DELEGATE_A, &[]));
    app.on_line(line_of(
        DELEGATE_A,
        "turn_started",
        json!({"input": [
            {"type": "message", "source": "driver",
                "content": [{"type": "text", "text": "use the parser"}]},
            {"type": "message", "source": "driver",
                "content": [{"type": "text", "text": "then the tests"}]},
        ]}),
    ));
    app.settle();
    let next = frame(&app).0;
    let parser = next.find("use the parser").expect("first message");
    let tests = next.find("then the tests").expect("second message");
    assert!(parser < tests, "queue order: {next}");
    assert!(next.contains("⏸ interrupt"), "{next}");
    insta::assert_snapshot!("delegate_next_turn_reads_the_queue", next);
}

#[test]
fn an_interrupt_leaves_both_drafts_and_the_parents_queue_for_the_close() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    // The parent's draft and queue are parked when the view opens.
    app.draft.set("parent draft");
    app.on_line(queue(SESSION, &["parent steer"]));
    opened_item(&mut app, "j_1");
    delegate_turn(&mut app);
    app.draft.set("delegate draft");
    // A parent queue update folds while the delegate's view is open.
    app.on_line(queue(SESSION, &["parent steer", "parent two"]));
    let out = press(&mut app, "alt+i");
    assert_eq!(cancels(&out), [DELEGATE_A]);
    assert_eq!(app.draft(), "delegate draft");
    app.on_key(Key::Esc, now());
    assert!(!app.item_open());
    assert_eq!(app.draft(), "parent draft");
    app.settle();
    let (text, _, _) = frame(&app);
    assert!(text.contains("parent steer"), "{text}");
    assert!(text.contains("parent two"), "{text}");
}
