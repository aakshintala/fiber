//! Tests for the open's overlay: the second fold never changes the
//! screen, the landing swaps exactly at the pass boundary, and every
//! abnormal end keeps the preview (`docs/tui.md`, "History and paging").

use std::path::PathBuf;

use contract::{ActionId, JobId, SessionId};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::super::{App, Effect, Kind, Pass, Phase};
use super::Overlay;
use crate::app::Target;
use crate::input::Draft;
use crate::keys::{Button, Mouse, MouseKind};
use crate::link::Line;
use crate::mouse::TargetId;
use crate::turn::target_id;
use crate::view::scroll_bar::THUMB;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

fn id(session: &str) -> SessionId {
    SessionId(session.to_owned())
}

/// An app attached to [`SESSION`] with the link up, 80x24.
fn app() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(80, 24);
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.attach(id(SESSION));
    app
}

/// An app on home at 80x24, launched to open `open_at`.
fn home_at(open_at: crate::OpenAt) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        open_at,
        ..Default::default()
    });
    app.set_size(80, 24);
    app
}

/// One durable session line numbered `seq`.
fn line(kind: &str, seq: u64, action: Option<&str>, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: id(SESSION),
        ts: seq,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|action| ActionId(action.to_owned())),
        seq: Some(contract::Seq(seq)),
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A turn's lines from `seq`: prompt, step, message, text, complete.
fn turn(seq: &mut u64, message: &str, text: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut push = |kind: &str, action: Option<&str>, payload: Value| {
        lines.push(line(kind, *seq, action, payload));
        *seq += 1;
    };
    push(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "prompt"}]}]}),
    );
    push("step_started", None, json!({}));
    push("assistant_message_started", Some(message), json!({}));
    push("text_completed", Some(message), json!({"text": text}));
    push(
        "assistant_message_completed",
        Some(message),
        json!({"outcome": "completed"}),
    );
    push("turn_completed", None, json!({"outcome": "completed"}));
    lines
}

/// A turn's lines from `seq` with one requested call.
fn call_turn(seq: &mut u64, message: &str, call: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut push = |kind: &str, action: Option<&str>, payload: Value| {
        lines.push(line(kind, *seq, action, payload));
        *seq += 1;
    };
    push(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "prompt"}]}]}),
    );
    push("step_started", None, json!({}));
    push("assistant_message_started", Some(message), json!({}));
    push(
        "tool_call_requested",
        Some(call),
        json!({"name": "read", "arguments": {}}),
    );
    push(
        "assistant_message_completed",
        Some(message),
        json!({"outcome": "completed"}),
    );
    push("turn_completed", None, json!({"outcome": "completed"}));
    lines
}

/// A session `command_accepted` for `id`, without a result.
fn session_accepted(session: &str, command: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: id(session),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [("command_id".to_owned(), Value::String(command.to_owned()))]
            .into_iter()
            .collect(),
    })
}

/// The raise's acceptance for `id`: the pass ends at `last`.
fn raise_accepted(session: &str, command: &str, last: u64) -> Line {
    Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: id(session),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [
            ("command_id".to_owned(), Value::String(command.to_owned())),
            ("result".to_owned(), json!({"last_seq": last})),
        ]
        .into_iter()
        .collect(),
    })
}

/// A hub line for `session` carrying `last_seq` when set.
fn hub(kind: &str, session: &str, last: Option<u64>) -> Line {
    let mut payload = serde_json::Map::new();
    payload.insert("session_id".to_owned(), Value::String(session.to_owned()));
    if let Some(last) = last {
        payload.insert("last_seq".to_owned(), json!(last));
    }
    Line::Hub(contract::HubLine {
        kind: kind.to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload,
    })
}

/// A hub `command_rejected` for `id` with `message`.
fn hub_refused(id: &str, message: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            (
                "code".to_owned(),
                Value::String("invalid_arguments".to_owned()),
            ),
            ("message".to_owned(), Value::String(message.to_owned())),
        ]
        .into_iter()
        .collect(),
    })
}

/// Folds `lines` into a fresh 80-column `Pages`.
fn folded(lines: &[Line]) -> crate::window::Pages {
    let mut pages = crate::window::Pages::new(80);
    for line in lines {
        if let Line::Session(envelope) = line {
            pages.apply(envelope);
        }
    }
    pages
}

/// Opens [`SESSION`] from home through the raise, whose acceptance ends
/// the pass at `last`: the overlay folds the second `Pages`.
fn opening(last: u64) -> App {
    let mut app = home_at(crate::OpenAt::Home);
    let lines: Vec<Value> = app
        .open_session(id(SESSION), None, None, false)
        .into_iter()
        .map(|line| serde_json::from_str(&line).unwrap_or_else(|panic| panic!("{panic}")))
        .collect();
    let summary = lines
        .iter()
        .find(|line| line["command"] == "subscribe")
        .unwrap_or_else(|| panic!("a summary subscribe"));
    let summary_id = summary["id"].as_str().unwrap_or_else(|| panic!("an id"));
    assert!(
        app.on_line(session_accepted(SESSION, summary_id))
            .is_empty()
    );
    let raise: Value = serde_json::from_str(&app.take_raise().unwrap_or_else(|| panic!("a raise")))
        .unwrap_or_else(|panic| panic!("{panic}"));
    assert_eq!(raise["command"], "subscribe");
    assert_eq!(raise["args"]["level"], "full");
    let raise_id = raise["id"].as_str().unwrap_or_else(|| panic!("an id"));
    assert!(
        app.on_line(raise_accepted(SESSION, raise_id, last))
            .is_empty()
    );
    assert!(
        matches!(app.overlay, Overlay::Folding { .. }),
        "the raise's ack starts the pass"
    );
    app
}

/// Holds the preview's lines over `fold` with the pass ending at `last`:
/// the screen freezes as the raise's take leaves it.
fn folding(preview: &[Line], fold: &[Line], last: u64) -> App {
    let mut app = app();
    for line in preview {
        app.on_line(line.clone());
    }
    app.overlay = Overlay::Folding {
        session: id(SESSION),
        fold: Box::new(folded(fold)),
        busy: None,
        held: false,
    };
    app.passes
        .push((id(SESSION), Pass::Until(contract::Seq(last))));
    app.attached_screen_mut().set_frozen(true);
    app
}

/// The app drawn at 80x24.
fn rendered(app: &App) -> Buffer {
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    buf
}

/// The thumb's rows within the bar's top-relative column.
fn thumb_rows(app: &App, buf: &Buffer) -> Vec<u16> {
    let conversation = app.conversation_area();
    (conversation.top()..conversation.bottom())
        .filter(|y| buf[(79, *y)].symbol() == THUMB)
        .map(|y| y - conversation.y)
        .collect()
}

#[test]
fn landing_swaps_exactly_at_last_seq() {
    let mut seq = 0u64;
    let mut lines = Vec::new();
    for n in 0..10 {
        let text = if n == 8 {
            "the boundary reply"
        } else {
            "a passing reply"
        };
        lines.extend(turn(&mut seq, &format!("a_m{n}"), text));
    }
    assert_eq!(seq, 60);
    // The boundary reply's text folds at seq 51, one line over a batch
    // boundary of 50: the lines under, at and over the boundary each
    // land exactly.
    for last in [49u64, 50, 51] {
        let mut app = opening(last);
        let at = usize::try_from(last).unwrap_or(0);
        for line in &lines[..at] {
            app.on_line(line.clone());
        }
        let held = total_of(&app);
        assert_eq!(total_of(&app), held);
        assert!(
            !crate::view::text(&rendered(&app)).contains("boundary"),
            "the lines under {last} change nothing"
        );
        app.on_line(lines[at].clone());
        assert!(total_of(&app) > held, "the line at {last} swaps");
        if last == 51 {
            assert!(
                crate::view::text(&rendered(&app)).contains("boundary"),
                "the boundary line swaps with its text"
            );
        }
        let swapped = total_of(&app);
        let mut following = seq;
        for line in turn(&mut following, "a_next", "a following reply") {
            app.on_line(line);
        }
        assert!(total_of(&app) > swapped, "the next line appends below");
        assert!(matches!(app.overlay, Overlay::None));
        assert!(!app.attached_screen().frozen());
    }
}

#[test]
fn a_second_fold_never_changes_the_screen() {
    let mut app = opening(1_000_000);
    let mut seq = 0u64;
    let mut lines = Vec::new();
    for n in 0..1000 {
        lines.extend(turn(&mut seq, "a_m", &format!("reply {n}")));
    }
    assert_eq!(lines.len(), 6_000);
    let drawn = crate::view::text(&rendered(&app));
    assert!(idle(&app));
    for batch in lines.chunks(50) {
        for line in batch {
            app.on_line(line.clone());
        }
        assert_eq!(
            crate::view::text(&rendered(&app)),
            drawn,
            "the screen holds the preview"
        );
        assert!(
            thumb_rows(&app, &rendered(&app)).is_empty(),
            "the bar stays hidden"
        );
        assert!(!app.counts_exact());
        assert!(idle(&app), "busy stays the preview's");
    }
    assert!(matches!(app.overlay, Overlay::Folding { .. }));
}

#[test]
fn landing_clears_the_selection_and_remaps_focus() {
    let mut seq = 0u64;
    let preview = call_turn(&mut seq, "a_m", "a_c");
    let last = seq;
    // A selection set on the preview, and focus on its call.
    let mut app = folding(&preview, &preview, last);
    let (_, targets) = {
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        let targets = crate::view::render(&app, area, &mut buf, None);
        (buf, targets)
    };
    let rows: Vec<String> = {
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        crate::view::render(&app, area, &mut buf, None);
        (area.top()..area.bottom())
            .map(|y| {
                (area.left()..area.right())
                    .map(|x| buf.cell((x, y)).map_or(" ", |cell| cell.symbol()))
                    .collect()
            })
            .collect()
    };
    let col = rows
        .iter()
        .position(|row| row.contains("prompt"))
        .map(|y| {
            let byte = rows[y].find("prompt").unwrap_or(0);
            (
                u16::try_from(byte).unwrap_or(0),
                u16::try_from(y).unwrap_or(0),
            )
        })
        .unwrap_or_else(|| panic!("the prompt shows"));
    assert_eq!(
        app.on_select(
            &Mouse {
                kind: MouseKind::Press(Button::Left),
                col: col.0,
                row: col.1,
            },
            &targets
        ),
        Effect::None
    );
    assert_eq!(
        app.on_select(
            &Mouse {
                kind: MouseKind::Drag(Button::Left),
                col: col.0 + 4,
                row: col.1,
            },
            &targets
        ),
        Effect::None
    );
    assert!(app.select.span().is_some(), "a selection is set");
    let call = Target::Call(target_id("a_c"));
    app.focus = Some(TargetId::Line(call));
    app.on_line(line(
        "turn_completed",
        last,
        None,
        json!({"outcome": "completed"}),
    ));
    assert_eq!(app.select.span(), None, "the swap clears the selection");
    assert_eq!(
        app.focused(),
        Some(TargetId::Line(call)),
        "focus follows the same call"
    );
    // A focused line the fold lacks leaves focus empty.
    let mut app = folding(&preview, &preview, last + 1);
    app.focus = Some(TargetId::Line(Target::Call(target_id("a_ghost"))));
    app.on_line(line(
        "turn_completed",
        last + 1,
        None,
        json!({"outcome": "completed"}),
    ));
    assert_eq!(app.focused(), None);
}

#[test]
fn an_empty_log_opens_no_pass() {
    let mut app = home_at(crate::OpenAt::Home);
    let lines: Vec<Value> = app
        .open_session(id(SESSION), None, None, false)
        .into_iter()
        .map(|line| serde_json::from_str(&line).unwrap_or_else(|panic| panic!("{panic}")))
        .collect();
    let summary = lines
        .iter()
        .find(|line| line["command"] == "subscribe")
        .unwrap_or_else(|| panic!("a summary subscribe"));
    let summary_id = summary["id"].as_str().unwrap_or_else(|| panic!("an id"));
    assert!(
        app.on_line(session_accepted(SESSION, summary_id))
            .is_empty()
    );
    let raise: Value = serde_json::from_str(&app.take_raise().unwrap_or_else(|| panic!("a raise")))
        .unwrap_or_else(|panic| panic!("{panic}"));
    assert!(app.attached_screen().frozen());
    // The raise's ack without a result ends the overlay.
    let raise_id = raise["id"].as_str().unwrap_or_else(|| panic!("an id"));
    assert!(app.on_line(session_accepted(SESSION, raise_id)).is_empty());
    assert!(matches!(app.overlay, Overlay::None));
    assert!(!app.attached_screen().frozen());
    assert!(!app.pass_open());
    assert_eq!(app.session(), Some(&id(SESSION)));
}

#[test]
fn reopening_resets_the_second_pages_and_keeps_the_screen() {
    let mut seq = 0u64;
    let mut preview = Vec::new();
    preview.extend(turn(&mut seq, "a_m", "the preview reply"));
    let mut fold_lines = Vec::new();
    for n in 0..4 {
        fold_lines.extend(turn(
            &mut seq,
            &format!("a_f{n}"),
            &format!("fold reply {n}"),
        ));
    }
    let last = seq + 100;
    let mut app = folding(&preview, &fold_lines, last);
    let held = app.attached_screen().pages().index().total();
    assert!(matches!(app.overlay, Overlay::Folding { .. }));
    app.on_line(hub("stream_reopened", SESSION, Some(last + 50)));
    let Overlay::Folding { fold, .. } = &app.overlay else {
        panic!("the pass runs on");
    };
    assert_eq!(
        fold.index().total(),
        crate::window::Pages::new(80).index().total(),
        "the second Pages starts over"
    );
    assert_eq!(
        app.attached_screen().pages().index().total(),
        held,
        "the preview keeps its lines"
    );
    assert!(!app.landing);
}

#[test]
fn stream_closed_keeps_the_screen_unfreezes_and_skips_replayed_lines() {
    let mut seq = 0u64;
    let mut preview = Vec::new();
    for n in 0..5 {
        preview.extend(turn(
            &mut seq,
            &format!("a_m{n}"),
            &format!("held reply {n}"),
        ));
    }
    let last_preview = seq - 1;
    let mut fold_lines = Vec::new();
    for n in 0..2 {
        fold_lines.extend(turn(
            &mut seq,
            &format!("a_f{n}"),
            &format!("fold reply {n}"),
        ));
    }
    let mut app = folding(&preview, &fold_lines, 100);
    // Scrollback set before the freeze, so the unfreeze restores scrolls.
    app.attached_screen_mut().set_frozen(false);
    app.attached_screen_mut().scroll_rows(true, 3, 5);
    let top = app.attached_screen().top();
    assert!(top.is_some());
    app.attached_screen_mut().set_frozen(true);
    app.on_line(hub("stream_closed", SESSION, None));
    let Overlay::Kept { last } = &app.overlay else {
        panic!("the preview is kept");
    };
    assert_eq!(*last, Some(contract::Seq(last_preview)));
    assert!(!app.attached_screen().frozen());
    assert!(!app.counts_exact(), "the bar stays hidden");
    app.attached_screen_mut().scroll_rows(true, 1, 5);
    assert_ne!(app.attached_screen().top(), top, "the screen scrolls");
    let held = total_of(&app);
    // The preview's last turn replayed folds nowhere: every seq sits
    // at or under the preview's last.
    let mut replay = last_preview + 1 - 6;
    for line in turn(&mut replay, "a_m4", "held reply 4") {
        app.on_line(line);
    }
    assert_eq!(total_of(&app), held, "the replayed turn folds nowhere");
    // A turn above it appends, the pass's last line included.
    let mut fresh = last_preview + 1;
    for line in turn(&mut fresh, "a_new", "a fresh reply") {
        app.on_line(line);
    }
    assert!(total_of(&app) > held);
    let grown = total_of(&app);
    let mut boundary = 100u64;
    for line in turn(&mut boundary, "a_last", "the pass's last reply") {
        app.on_line(line);
    }
    assert!(total_of(&app) > grown);
    assert!(
        matches!(app.overlay, Overlay::Kept { .. }),
        "the kept preview never lands"
    );
    // With no line held, nothing is skipped.
    let mut app = folding(&[], &fold_lines, 100);
    let held = total_of(&app);
    app.on_line(hub("stream_closed", SESSION, None));
    assert!(matches!(app.overlay, Overlay::Kept { last: None }));
    let mut first = 0u64;
    for line in turn(&mut first, "a_m", "the first replayed reply") {
        app.on_line(line);
    }
    assert!(total_of(&app) > held, "Kept with no last skips nothing");
}

/// Whether the attached screen sits idle.
fn idle(app: &App) -> bool {
    matches!(app.phase, Phase::Attached { busy: false, .. })
}

/// The attached screen's indexed rows.
fn total_of(app: &App) -> usize {
    app.attached_screen().pages().index().total()
}

#[test]
fn session_left_and_disconnect_end_the_overlay() {
    let mut seq = 0u64;
    let preview = turn(&mut seq, "a_m", "the preview reply");
    let mut app = folding(&preview, &preview, seq + 10);
    assert!(app.attached_screen().frozen());
    app.on_line(hub("session_left", SESSION, None));
    assert!(matches!(app.overlay, Overlay::None));
    assert!(!app.attached_screen().frozen());
    let mut app = folding(&preview, &preview, seq + 10);
    app.disconnected();
    assert!(matches!(app.overlay, Overlay::None));
    assert!(!app.attached_screen().frozen());
}

#[test]
fn go_home_and_leave_mid_pass_end_the_overlay() {
    let mut seq = 0u64;
    let preview = turn(&mut seq, "a_m", "the preview reply");
    let last = seq + 10;
    let mut app = home_at(crate::OpenAt::Home);
    app.attach(id(SESSION));
    for line in &preview {
        app.on_line(line.clone());
    }
    app.overlay = Overlay::Folding {
        session: id(SESSION),
        fold: Box::new(folded(&preview)),
        busy: None,
        held: false,
    };
    app.passes
        .push((id(SESSION), Pass::Until(contract::Seq(last))));
    app.attached_screen_mut().set_frozen(true);
    app.go_home();
    assert!(matches!(app.overlay, Overlay::None));
    assert!(!app.screen.frozen());
    // The pass's last line arriving at home swaps nothing.
    app.on_line(line(
        "turn_completed",
        last,
        None,
        json!({"outcome": "completed"}),
    ));
    assert!(matches!(app.overlay, Overlay::None));
    assert!(!app.screen.frozen());
}

#[test]
fn a_resize_during_the_pass_recounts_once_at_landing() {
    // A resize while the raise is out stores without recounting.
    let mut app = opening(200);
    let held = app.attached_screen().pages().wrap_width();
    app.set_size(100, 24);
    assert!(app.attached_screen().frozen());
    assert_eq!(app.attached_screen().pages().wrap_width(), held);
    let mut seq = 0u64;
    let mut lines = Vec::new();
    for n in 0..4 {
        lines.extend(turn(&mut seq, &format!("a_m{n}"), &format!("reply {n}")));
    }
    for line in &lines {
        app.on_line(line.clone());
    }
    assert_eq!(app.attached_screen().pages().wrap_width(), held);
    app.on_line(line(
        "turn_completed",
        200,
        None,
        json!({"outcome": "completed"}),
    ));
    assert!(matches!(app.overlay, Overlay::None));
    assert_ne!(
        app.attached_screen().pages().wrap_width(),
        held,
        "unfreezing applies the stored width"
    );
    // Back to the old width before landing recounts nothing.
    let mut app = opening(200);
    let held = app.attached_screen().pages().wrap_width();
    app.set_size(100, 24);
    app.set_size(80, 24);
    let mut seq = 0u64;
    let mut lines = Vec::new();
    for n in 0..4 {
        lines.extend(turn(&mut seq, &format!("a_m{n}"), &format!("reply {n}")));
    }
    for line in &lines {
        app.on_line(line.clone());
    }
    app.on_line(line(
        "turn_completed",
        200,
        None,
        json!({"outcome": "completed"}),
    ));
    assert_eq!(app.attached_screen().pages().wrap_width(), held);
}

#[test]
fn a_gate_state_line_folds_live_during_the_pass() {
    let mut seq = 0u64;
    let mut preview = Vec::new();
    preview.extend(call_turn(&mut seq, "a_m", "a_c"));
    let last = seq + 50;
    let mut app = folding(&preview, &preview, last);
    let held = total_of(&app);
    app.on_line(line(
        "permission_requested",
        seq,
        Some("a_c"),
        json!({"request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "standing_ask",
            "standing_rule": {"scope": "global", "prefix": "rg"},
            "decision": "allow_once"}),
    ));
    assert!(app.panel().is_some(), "the approval folds live");
    app.on_line(line(
        "notice",
        seq,
        None,
        json!({"code": "extension_failed", "message": "the extension fell over"}),
    ));
    assert_eq!(app.notice(), Some("the extension fell over"));
    assert_eq!(total_of(&app), held, "the screen's Pages sit out the pass");
}

#[test]
fn the_second_pages_holds_only_the_window() {
    let mut app = opening(1_000_000);
    let mut seq = 0u64;
    let mut lines = Vec::new();
    for n in 0..1000 {
        lines.extend(turn(&mut seq, "a_m", &format!("reply {n}")));
    }
    for line in &lines {
        app.on_line(line.clone());
    }
    let Overlay::Folding { fold, .. } = &app.overlay else {
        panic!("the pass runs on");
    };
    assert!(
        fold.index().total() > lines.len(),
        "the fold holds every line's rows"
    );
    assert!(
        fold.resident() <= 6,
        "the fold holds only the bottom window, not {} pages",
        fold.resident()
    );
}

#[test]
fn replayed_shell_lines_fold_into_the_second_pages() {
    let last = 100u64;
    let mut app = folding(&[], &[], last);
    let held = total_of(&app);
    app.on_line(line(
        "shell_command",
        10,
        None,
        json!({"command": "echo hi", "output": "hi"}),
    ));
    let shell_id = "c_shell000000000001";
    let mut draft = Draft::default();
    draft.set("!!echo hi");
    app.pending
        .insert(shell_id.to_owned(), (Kind::Shell, draft));
    app.on_line(Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: id(SESSION),
        ts: 11,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: Some(contract::Seq(11)),
        payload: [
            ("command_id".to_owned(), Value::String(shell_id.to_owned())),
            (
                "result".to_owned(),
                json!({"output": "hi", "process": {"exit_code": 0, "timed_out": false}}),
            ),
        ]
        .into_iter()
        .collect(),
    }));
    assert_eq!(total_of(&app), held, "the screen's Pages sit out the pass");
    app.on_line(line(
        "turn_completed",
        last,
        None,
        json!({"outcome": "completed"}),
    ));
    assert!(matches!(app.overlay, Overlay::None));
    assert!(total_of(&app) > held, "the shell shows after the swap");
}

#[test]
fn a_delegate_transcript_open_mid_pass_leaves_the_pass_on_the_attached_screen() {
    let mut seq = 0u64;
    let mut preview = Vec::new();
    for n in 0..3 {
        preview.extend(turn(
            &mut seq,
            &format!("a_m{n}"),
            &format!("parent reply {n}"),
        ));
    }
    let mut fold_lines = preview.clone();
    for n in 3..5 {
        fold_lines.extend(turn(
            &mut seq,
            &format!("a_m{n}"),
            &format!("parent reply {n}"),
        ));
    }
    let last = seq;
    let mut app = folding(&preview, &fold_lines, last);
    // The item view swaps in over the frozen parent.
    let stashed = std::mem::replace(
        app.attached_screen_mut(),
        super::super::screen::Screen::new(),
    );
    let mut item_seq = 0u64;
    for line in turn(&mut item_seq, "a_d", "the delegate reply") {
        if let Line::Session(envelope) = line {
            app.screen.pages_mut().apply(&envelope);
        }
    }
    app.screen.scroll_rows(true, 2, 5);
    let item_top = app.screen.top();
    assert!(item_top.is_some());
    app.items.open = Some(crate::app::items::Open {
        job_id: JobId("j_1".to_owned()),
        stashed: Some(stashed),
        message: None,
    });
    // The stashed parent stays frozen while the item scrolls with its bar.
    assert!(app.attached_screen().frozen());
    assert!(app.counts_exact());
    app.screen.scroll_rows(true, 1, 5);
    assert_ne!(app.screen.top(), item_top);
    let item_total = app.screen.pages().index().total();
    // The boundary line lands the swap in the stashed screen.
    app.on_line(line(
        "turn_completed",
        last,
        None,
        json!({"outcome": "completed"}),
    ));
    assert!(matches!(app.overlay, Overlay::None));
    let stashed_total = app
        .items
        .open
        .as_ref()
        .and_then(|open| open.stashed.as_ref())
        .map(|stashed| stashed.pages().index().total())
        .unwrap_or(0);
    assert!(stashed_total > item_total);
    assert_eq!(app.screen.pages().index().total(), item_total);
    assert!(
        !app.attached_screen().frozen(),
        "the swap unfreezes the parent"
    );
}

#[test]
fn no_thumb_while_the_screen_is_frozen() {
    let mut app = app();
    let mut seq = 0u64;
    for n in 0..14 {
        for line in turn(&mut seq, &format!("a_m{n}"), &format!("reply {n}")) {
            app.on_line(line);
        }
    }
    app.attached_screen_mut().jump(0);
    let following = thumb_rows(&app, &rendered(&app));
    assert!(!following.is_empty(), "a thumb shows unfrozen");
    app.attached_screen_mut().set_frozen(true);
    assert!(!app.counts_exact());
    assert!(
        thumb_rows(&app, &rendered(&app)).is_empty(),
        "no thumb while frozen"
    );
    app.attached_screen_mut().set_frozen(false);
    assert!(app.counts_exact());
    assert_eq!(thumb_rows(&app, &rendered(&app)), following);
}

/// A turn whose prompt carried an image at `path`.
fn started_with_image(app: &mut App, path: &str) {
    app.on_line(Line::Session(contract::Envelope {
        kind: "turn_started".to_owned(),
        session_id: id(SESSION),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"input": [{
            "type": "message", "source": "driver",
            "content": [
                {"type": "image", "path": path,
                    "mime_type": "image/png",
                    "width": 1280, "height": 800},
            ],
        }]})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }));
}

#[test]
fn a_refusal_recorded_at_one_id_holds_after_the_swap() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/one.png");
    started_with_image(&mut app, "artifacts/x.png");
    // X draws second.
    let lines = match app.view_image(2) {
        Effect::Send(lines) => lines,
        Effect::None
        | Effect::Quit
        | Effect::Exit(_)
        | Effect::ListFiles
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Copy(_)
        | Effect::FindPause { .. }
        | Effect::OpenLink(_)
        | Effect::ReadImage(_)
        | Effect::OpenFile(_) => panic!("a read for X"),
    };
    assert_eq!(lines.len(), 1);
    let read: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|panic| panic!("{panic}"));
    assert_eq!(read["args"]["path"], "artifacts/x.png");
    let fetch = read["id"]
        .as_str()
        .unwrap_or_else(|| panic!("an id"))
        .to_owned();
    app.on_line(hub_refused(&fetch, "gone"));
    assert_eq!(app.view_image(2), Effect::None, "refused at id 2");
    // A new fold draws X fifth; the landing swaps it in.
    let mut fold = crate::window::Pages::new(80);
    for path in [
        "artifacts/a.png",
        "artifacts/b.png",
        "artifacts/c.png",
        "artifacts/d.png",
        "artifacts/x.png",
    ] {
        fold_image(&mut fold, path);
    }
    let held = total_of(&app);
    app.overlay = Overlay::Folding {
        session: id(SESSION),
        fold: Box::new(fold),
        busy: None,
        held: false,
    };
    app.land_pass();
    assert!(matches!(app.overlay, Overlay::None));
    assert!(total_of(&app) > held);
    assert_eq!(
        app.view_image(5),
        Effect::None,
        "X's refusal survives its new id"
    );
    assert!(
        matches!(app.view_image(2), Effect::Send(_)),
        "another image at id 2 sends"
    );
}

/// Folds a prompt carrying an image at `path` into `pages`.
fn fold_image(pages: &mut crate::window::Pages, path: &str) {
    pages.apply(&contract::Envelope {
        kind: "turn_started".to_owned(),
        session_id: id(SESSION),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"input": [{
            "type": "message", "source": "driver",
            "content": [
                {"type": "image", "path": path,
                    "mime_type": "image/png",
                    "width": 1280, "height": 800},
            ],
        }]})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    });
}

/// The tail read's window answers, as the doors answer history: the last
/// 256 lines at or below the asked `to_seq`, oldest first, or the last
/// 256 when the args hold none.
fn doors(lines: &[contract::Envelope], args: &Value) -> Vec<contract::Envelope> {
    let end = args
        .get("to_seq")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    let mut out: Vec<contract::Envelope> = lines
        .iter()
        .filter(|line| line.seq.is_some_and(|seq| seq.0 <= end))
        .cloned()
        .collect();
    out.drain(..out.len().saturating_sub(256));
    out
}

/// The session lines as durable envelopes.
fn envelopes(lines: &[Line]) -> Vec<contract::Envelope> {
    lines
        .iter()
        .filter_map(|line| {
            if let Line::Session(envelope) = line {
                Some(envelope.clone())
            } else {
                None
            }
        })
        .collect()
}

/// `count` plain lines numbered from zero.
fn plain(count: u64) -> Vec<contract::Envelope> {
    (0..count)
        .map(|seq| {
            if let Line::Session(envelope) = line("usage_recorded", seq, None, json!({})) {
                envelope
            } else {
                unreachable!("a session line");
            }
        })
        .collect()
}

/// Reads `all` through a fresh tail, following the answers' windows, and
/// returns each `next_args` in order.
fn read_all(all: Vec<contract::Envelope>) -> (Vec<Value>, crate::app::overlay::Tail) {
    use crate::app::overlay::Tail;
    let mut tail = Tail::new();
    let mut asked = Vec::new();
    loop {
        let args = tail.next_args();
        asked.push(args.clone());
        let answer = doors(&all, &args);
        let empty = answer.is_empty();
        tail.prepend(answer)
            .unwrap_or_else(|_| panic!("a contiguous answer"));
        if tail.start_reached() || empty {
            break;
        }
    }
    (asked, tail)
}

/// A read ends after one window when the log fits it, and walks
/// `to_seq` down to zero when it does not: the exact args sequence at
/// 255, 256, 257 and 258 lines.
#[test]
fn reads_follow_the_answer_window_at_the_edges() {
    for (count, expected) in [
        (255u64, vec![json!({})]),
        (256, vec![json!({})]),
        (257, vec![json!({}), json!({"to_seq": 0})]),
        (258, vec![json!({}), json!({"to_seq": 1})]),
    ] {
        let (asked, tail) = read_all(plain(count));
        let many = usize::try_from(count).unwrap_or(usize::MAX);
        assert_eq!(asked, expected, "{count} lines");
        assert!(tail.start_reached(), "{count} lines");
        assert_eq!(tail.lines().len(), many, "{count} lines");
    }
}

/// The read stops when the held lines fill the screen: at one row below
/// the total the tail asks again, at the total and above it is filled.
#[test]
fn reads_stop_when_the_held_lines_fill_the_screen() {
    use crate::app::overlay::Tail;
    let mut seq = 0u64;
    let mut all = Vec::new();
    for at in 0..3 {
        all.extend(turn(&mut seq, &format!("a_m{at}"), &format!("reply {at}")));
    }
    let all = envelopes(&all);
    // The held window starts past zero, so only the row count fills.
    let all: Vec<contract::Envelope> = all
        .into_iter()
        .map(|mut envelope| {
            envelope.seq = envelope.seq.map(|seq| contract::Seq(seq.0 + 1000));
            envelope
        })
        .collect();
    let app = app();
    let mut fresh = app.attached_screen().pages().fresh();
    for envelope in &all {
        fresh.apply(envelope);
    }
    let total = fresh.index().total();
    let mut tail = Tail::new();
    tail.prepend(all)
        .unwrap_or_else(|_| panic!("a contiguous answer"));
    assert!(!tail.filled(&fresh, total + 1), "one row short asks again");
    assert!(tail.filled(&fresh, total), "at the total it is filled");
    assert!(
        tail.filled(&fresh, total - 1),
        "past the total it is filled"
    );
}

/// A 700-line turn reads to the start with no cap: three windows, the
/// first two partial and never filled at conversation height.
#[test]
fn a_long_turn_reads_until_the_screen_is_full_with_no_cap() {
    use crate::app::overlay::Tail;
    let message = "a_m";
    let mut all = vec![if let Line::Session(envelope) = line(
        "turn_started",
        0,
        None,
        json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": "prompt"}]}]}),
    ) {
        envelope
    } else {
        unreachable!("a session line");
    }];
    for seq in 1..=697 {
        let kind = if seq == 1 {
            "step_started"
        } else if seq == 2 {
            "assistant_message_started"
        } else {
            "text_completed"
        };
        let action = if kind == "step_started" || kind == "turn_started" {
            None
        } else {
            Some(message)
        };
        if let Line::Session(envelope) = line(kind, seq, action, json!({"text": "part"})) {
            all.push(envelope);
        }
    }
    for (seq, kind) in [
        (698, "assistant_message_completed"),
        (699, "turn_completed"),
    ] {
        if let Line::Session(envelope) =
            line(kind, seq, Some(message), json!({"outcome": "completed"}))
        {
            all.push(envelope);
        }
    }
    let mut tail = Tail::new();
    let mut reads = 0;
    loop {
        let args = tail.next_args();
        let answer = doors(&all, &args);
        let empty = answer.is_empty();
        tail.prepend(answer)
            .unwrap_or_else(|_| panic!("a contiguous answer"));
        reads += 1;
        if tail.start_reached() || empty {
            break;
        }
    }
    assert_eq!(reads, 3, "256, 256 and 188 lines");
    assert_eq!(tail.lines().len(), 700);
    assert_eq!(
        tail.lines().first().and_then(|line| line.seq),
        Some(contract::Seq(0))
    );
}

/// An empty answer ends the read with nothing held.
#[test]
fn an_empty_log_ends_with_nothing_held() {
    use crate::app::overlay::Tail;
    let mut tail = Tail::new();
    tail.prepend(Vec::new())
        .unwrap_or_else(|_| panic!("an empty answer"));
    assert!(tail.start_reached());
    assert!(tail.lines().is_empty());
}

/// One line at seq zero starts at the beginning and ends the read.
#[test]
fn a_log_of_one_line_starts_at_zero() {
    use crate::app::overlay::Tail;
    let mut tail = Tail::new();
    tail.prepend(plain(1))
        .unwrap_or_else(|_| panic!("one line"));
    assert!(tail.start_reached());
    assert_eq!(tail.lines().len(), 1);
}

/// An answer that leaves a gap, or a line carrying no seq, is refused:
/// the tail ends as failed with the out-of-order notice.
#[test]
fn an_answer_that_leaves_a_gap_is_refused() {
    use crate::app::overlay::{Overlay, Tail};
    let mut tail = Tail::new();
    let window: Vec<contract::Envelope> = plain(200).drain(100..).collect();
    tail.prepend(window.clone())
        .unwrap_or_else(|_| panic!("the first answer"));
    assert!(tail.prepend(plain(51)).is_err(), "a gap is refused");
    let mut noseq = window.clone();
    noseq[0].seq = None;
    assert!(tail.prepend(noseq).is_err(), "a missing seq is refused");
    let mut app = app();
    app.overlay = Overlay::Reading(Tail::new());
    app.tail_lines(window);
    assert_eq!(app.notice(), None);
    app.tail_lines(plain(51));
    assert_eq!(
        app.notice(),
        Some("Could not load history: the answer was out of order")
    );
}
