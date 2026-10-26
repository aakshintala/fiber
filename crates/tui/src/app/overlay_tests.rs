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
    let sent = app.on_line(session_accepted(SESSION, summary_id));
    assert_eq!(sent.len(), 1);
    let commands: Value = serde_json::from_str(&sent[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(commands["command"], "commands");
    assert_eq!(commands["session_id"], SESSION);
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
        landing: false,
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
fn session_gate_event_during_fold_survives_fresh_snapshot_and_landing() {
    let lines = call_turn(&mut 0, "gate-message", "gate-call");
    let mut app = folding(&lines, &lines, 5);
    let mut fake = crate::ext::tests::SessionRenderer::new("FOLD-GATED");
    fake.gated = true;
    app.use_renderer(std::rc::Rc::new(fake));
    let mut fresh = app.pages().fresh();
    for line in &lines {
        if let Line::Session(line) = line {
            fresh.apply(line);
        }
    }
    if let Overlay::Folding { fold, .. } = &mut app.overlay {
        **fold = fresh;
    }
    let Line::Session(mut event) = line(
        "extensions_loaded",
        6,
        None,
        json!({"extensions":[{"name":"session-test","version":"v1.0.0"}]}),
    ) else {
        unreachable!()
    };
    event.seq = None;
    app.on_line(Line::Session(event));
    let Overlay::Folding { fold, .. } = &app.overlay else {
        panic!("fold")
    };
    assert!(
        fold.page_text_open(0)
            .unwrap()
            .0
            .iter()
            .any(|(line, _)| line.to_string().contains("FOLD-GATED"))
    );
    // A later fresh snapshot must carry the same latest full set.
    let mut later = fold.fresh();
    for line in &lines {
        if let Line::Session(line) = line {
            later.apply(line);
        }
    }
    assert!(
        later
            .page_text_open(0)
            .unwrap()
            .0
            .iter()
            .any(|(line, _)| line.to_string().contains("FOLD-GATED"))
    );
    let Overlay::Folding { fold, .. } = std::mem::take(&mut app.overlay) else {
        panic!("fold")
    };
    app.land(*fold, false);
    assert!(
        app.pages()
            .page_text_open(0)
            .unwrap()
            .0
            .iter()
            .any(|(line, _)| line.to_string().contains("FOLD-GATED"))
    );
}

#[test]
fn renderer_replacement_preserves_failure_memo_and_notice_across_fold_recount() {
    let lines = call_turn(&mut 0, "message", "call");
    let mut app = folding(&lines, &lines, 5);
    app.use_renderer(std::rc::Rc::new(crate::ext::tests::SessionRenderer::new(
        "OLD-ROW",
    )));
    app.screen.pages_mut().toggle_ledgers();
    let mut fresh = app.pages().fresh();
    for line in &lines {
        if let Line::Session(line) = line {
            fresh.apply(line);
        }
    }
    if let Overlay::Folding { fold, .. } = &mut app.overlay {
        **fold = fresh;
    }
    let mut replacement = crate::ext::tests::SessionRenderer::new("replacement");
    replacement.failure = Some(crate::ext::FailureKind::HookFailed);
    let replacement = std::rc::Rc::new(replacement);
    app.use_renderer(replacement.clone());
    // The displayed and fresh-fold copies draw the same page/input. Their
    // shared failure memo suppresses a second call, not delivery to App.
    assert_eq!(replacement.calls.borrow().len(), 1);
    assert!(
        app.drawn(&[]),
        "the replacement failure requests immediate redraw"
    );
    assert_eq!(app.notices().iter().filter(|line| matches!(line, crate::app::NoticeLine::Notice {code,text,..} if code == "hook_failed" && text.contains("session-test"))).count(), 1);
    assert!(
        !app.drawn(&[]),
        "docs/tui.md: one failure notice per extension and slot"
    );
}

#[test]
fn reload_during_fold_recounts_before_land_and_landed_cache_can_reload_again() {
    let lines = call_turn(&mut 0, "message", "call");
    let mut app = folding(&lines, &lines, 5);
    let mut fake = crate::ext::tests::SessionRenderer::new("failed-fold");
    fake.failure = Some(crate::ext::FailureKind::HookFailed);
    app.use_renderer(std::rc::Rc::new(fake));
    // Real folding starts after startup loading and retains the installed handle.
    let mut fresh = app.pages().fresh();
    for line in &lines {
        if let Line::Session(line) = line {
            fresh.apply(line);
        }
    }
    if let Overlay::Folding { fold, .. } = &mut app.overlay {
        **fold = fresh;
    }
    let Overlay::Folding { fold, .. } = &app.overlay else {
        panic!("fold")
    };
    fold.page_text_open(0);
    let before = fold.recounts;
    let loader = crate::ext::tests::TestLoad::new("RELOAD-IN-FOLD");
    app.set_home(crate::home::Launch {
        extensions: Some(loader.clone()),
        ..Default::default()
    });
    crate::app::commands::run_reload(&mut app, "");
    let Overlay::Folding { fold, .. } = &app.overlay else {
        panic!("fold")
    };
    assert!(fold.recounts > before);
    assert!(
        fold.page_text_open(0)
            .unwrap()
            .0
            .iter()
            .any(|(line, _)| line.to_string().contains("RELOAD-IN-FOLD"))
    );
    let Overlay::Folding { fold, .. } = std::mem::take(&mut app.overlay) else {
        panic!("fold")
    };
    app.land(*fold, false);
    app.install_renderer(std::rc::Rc::new({
        let mut fake = crate::ext::tests::SessionRenderer::new("landed-fail");
        fake.failure = Some(crate::ext::FailureKind::HookFailed);
        fake
    }));
    app.pages().page_text_open(0);
    crate::app::commands::run_reload(&mut app, "");
    assert!(
        app.pages()
            .page_text_open(0)
            .unwrap()
            .0
            .iter()
            .any(|(line, _)| line.to_string().contains("RELOAD-IN-FOLD")),
        "landing promotes cache so reload clears shared failure memo"
    );
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

/// A held landing keeps the second copy of a repeated line: the anchor
/// is the top row's `seq`, applied after the unfreeze. The fold carries
/// the same lines with a longer first reply, so the later pages start
/// on later rows: matching on text would point at the first copy, and
/// jumping while frozen would leave the stale row, so both defects fail
/// this test.
#[test]
fn a_held_landing_keeps_the_second_copy_by_seq() {
    // Twenty-three turns, six lines each: three pages, with one reply
    // text shared by turns 18 and 19. The top sits on the second copy,
    // with turns below so the jump never clamps at the bottom.
    let mut seq = 0u64;
    let mut preview = Vec::new();
    for n in 0..23 {
        let text = if n == 18 || n == 19 {
            "same reply".to_owned()
        } else {
            format!("reply {n}")
        };
        preview.extend(turn(&mut seq, &format!("a_m{n}"), &text));
    }
    let mut app = app();
    for line in &preview {
        app.on_line(line.clone());
    }
    assert_eq!(app.attached_screen().pages().wrap_width(), 79);
    // Following keeps a suffix resident: the absolute row adds the
    // rows trimmed above the resident window.
    let resident = app.attached_screen().pages().rows();
    let start = app.attached_screen().pages().index().total() - resident.len();
    let copies: Vec<usize> = resident
        .into_iter()
        .enumerate()
        .filter(|(_, (line, _))| line.to_string().contains("same reply"))
        .map(|(at, _)| start + at)
        .collect();
    assert_eq!(copies.len(), 2, "two copies draw");
    app.jump(copies[1]);
    assert_eq!(app.attached_screen().top(), Some(copies[1]));
    // Only the second copy shows, on the top row: the first sits
    // above the window.
    let buf = rendered(&app);
    let shown = crate::view::text(&buf);
    assert_eq!(shown.matches("same reply").count(), 1, "{shown}");
    let area = app.conversation_area();
    let first: String = (area.left()..area.right())
        .map(|x| buf.cell((x, area.top())).map_or(" ", |cell| cell.symbol()))
        .collect();
    assert!(first.contains("same reply"), "{shown}");
    // The same lines with a longer first reply: the splits stay on the
    // same lines, but every later page starts on a later row.
    let mut then = 0u64;
    let mut fold_lines = turn(&mut then, "a_m0", &"x".repeat(200));
    fold_lines.extend(preview.iter().skip(6).cloned());
    // The fold wraps at the attached width, so its pages split on the
    // same lines with the same rows inside each page.
    let width = app.attached_screen().pages().wrap_width();
    let mut fold = crate::window::Pages::new(width);
    for line in &fold_lines {
        if let Line::Session(envelope) = line {
            fold.apply(envelope);
        }
    }
    app.overlay = Overlay::Folding {
        session: id(SESSION),
        fold: Box::new(folded(&preview)),
        busy: None,
        held: true,
        landing: false,
    };
    app.attached_screen_mut().set_frozen(true);
    app.land(fold, true);
    assert!(
        !app.attached_screen().frozen(),
        "landing unfreezes before the jump"
    );
    let top = app
        .attached_screen()
        .top()
        .unwrap_or_else(|| panic!("a top row"));
    assert_ne!(top, copies[1], "the jump applies after the unfreeze");
    // The first copy scrolled off above: only the second shows, on the
    // top row. A text match would sit on the first copy, and a frozen
    // jump would leave both copies showing.
    let buf = rendered(&app);
    let shown = crate::view::text(&buf);
    assert_eq!(shown.matches("same reply").count(), 1, "{shown}");
    let area = app.conversation_area();
    let first: String = (area.left()..area.right())
        .map(|x| buf.cell((x, area.top())).map_or(" ", |cell| cell.symbol()))
        .collect();
    assert!(first.contains("same reply"), "{shown}");
}

/// A held landing with a resize deferred while frozen keeps the top on
/// the row with the same `seq`: the two long replies above the top on
/// its page wrap to more rows at the new width, so the top's old offset
/// in the page points into them. Anchoring by the page's first `seq`
/// plus that offset lands on a long reply; anchoring by the top row's
/// own `seq` lands on the same reply again.
#[test]
fn a_held_landing_after_a_deferred_resize_keeps_the_top_seq() {
    // Twenty-three turns, six lines each: three pages, the second from
    // turn 11. Turns 11 and 12 reply at length; the top sits on turn
    // 13's reply, with turns below so the jump never clamps.
    let mut seq = 0u64;
    let mut lines = Vec::new();
    for n in 0..23 {
        let text = if n == 11 || n == 12 {
            format!("long {n} {}", "word ".repeat(200))
        } else {
            format!("reply {n}")
        };
        lines.extend(turn(&mut seq, &format!("a_m{n}"), &text));
    }
    let mut app = app();
    for line in &lines {
        app.on_line(line.clone());
    }
    assert_eq!(app.attached_screen().pages().wrap_width(), 79);
    let resident = app.attached_screen().pages().rows();
    let start = app.attached_screen().pages().index().total() - resident.len();
    let top = resident
        .iter()
        .position(|(line, _)| line.to_string().contains("reply 13"))
        .map(|at| start + at)
        .unwrap_or_else(|| panic!("turn 13's reply draws"));
    app.jump(top);
    assert_eq!(app.attached_screen().top(), Some(top));
    let (page, _) = app
        .attached_screen()
        .pages()
        .index()
        .locate(top)
        .unwrap_or_else(|| panic!("a page holds the top"));
    assert_eq!(
        app.attached_screen()
            .pages()
            .index()
            .page_of(&id(SESSION), contract::Seq(66)),
        Some(page),
        "the top shares its page with the long replies"
    );
    // The open holds the preview: the resize waits for the unfreeze.
    app.overlay = Overlay::Folding {
        session: id(SESSION),
        fold: Box::new(folded(&lines)),
        busy: None,
        held: true,
        landing: false,
    };
    app.attached_screen_mut().set_frozen(true);
    app.set_size(60, 24);
    assert_eq!(
        app.attached_screen().pages().wrap_width(),
        79,
        "the resize is deferred"
    );
    let mut fold = crate::window::Pages::new(79);
    for line in &lines {
        if let Line::Session(envelope) = line {
            fold.apply(envelope);
        }
    }
    app.land(fold, true);
    assert!(!app.attached_screen().frozen());
    assert!(
        app.attached_screen().pages().wrap_width() < 79,
        "the unfreeze applies the resize"
    );
    let area = Rect::new(0, 0, 60, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let shown = crate::view::text(&buf);
    let conversation = app.conversation_area();
    let first: String = (conversation.left()..conversation.right())
        .map(|x| {
            buf.cell((x, conversation.top()))
                .map_or(" ", |cell| cell.symbol())
        })
        .collect();
    assert!(first.contains("reply 13"), "{shown}");
}

/// A held landing with a resize deferred while frozen keeps the top on
/// the same `seq` when the fold dropped the top's page: the unfreeze
/// only marks a dropped page stale, so its rows and pieces keep the old
/// width until it loads. The anchor holds through the loads and applies
/// again once they count the pages at the new width.
#[test]
fn a_held_landing_keeps_the_top_seq_when_the_fold_dropped_its_page() {
    // As above: three pages, the second from turn 11, whose long
    // replies sit above the top on turn 13's reply.
    let mut seq = 0u64;
    let mut lines = Vec::new();
    for n in 0..23 {
        let text = if n == 11 || n == 12 {
            format!("long {n} {}", "word ".repeat(200))
        } else {
            format!("reply {n}")
        };
        lines.extend(turn(&mut seq, &format!("a_m{n}"), &text));
    }
    let mut app = app();
    for line in &lines {
        app.on_line(line.clone());
    }
    let resident = app.attached_screen().pages().rows();
    let start = app.attached_screen().pages().index().total() - resident.len();
    let top = resident
        .iter()
        .position(|(line, _)| line.to_string().contains("reply 13"))
        .map(|at| start + at)
        .unwrap_or_else(|| panic!("turn 13's reply draws"));
    app.jump(top);
    assert_eq!(app.attached_screen().top(), Some(top));
    app.overlay = Overlay::Folding {
        session: id(SESSION),
        fold: Box::new(folded(&lines)),
        busy: None,
        held: true,
        landing: false,
    };
    app.attached_screen_mut().set_frozen(true);
    app.set_size(60, 24);
    let mut fold = crate::window::Pages::new(79);
    for line in &lines {
        if let Line::Session(envelope) = line {
            fold.apply(envelope);
        }
    }
    // The fold keeps only its last row's page: the top's page drops.
    let total = fold.index().total();
    fold.trim(total - 1, 1);
    let page = fold
        .index()
        .page_of(&id(SESSION), contract::Seq(66))
        .unwrap_or_else(|| panic!("a page holds turn 11"));
    let range = fold
        .index()
        .pages()
        .get(page)
        .map(|page| page.first_seq..=page.last_seq)
        .unwrap_or_else(|| panic!("the top's page"));
    assert!(
        fold.needs(0, total).contains(&(id(SESSION), range)),
        "the fold dropped the top's page"
    );
    app.land(fold, true);
    assert!(app.attached_screen().pages().wrap_width() < 79);
    // The frame's loads, as the loop pages them in.
    let log = envelopes(&lines);
    for _ in 0..8 {
        let Some((session, range)) = app.needs().into_iter().next() else {
            break;
        };
        let held: Vec<contract::Envelope> = log
            .iter()
            .filter(|envelope| envelope.seq.is_some_and(|seq| range.contains(&seq)))
            .cloned()
            .collect();
        app.load(&session, held);
    }
    assert!(app.needs().is_empty(), "every page loaded");
    let area = Rect::new(0, 0, 60, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let shown = crate::view::text(&buf);
    let conversation = app.conversation_area();
    let first: String = (conversation.left()..conversation.right())
        .map(|x| {
            buf.cell((x, conversation.top()))
                .map_or(" ", |cell| cell.symbol())
        })
        .collect();
    assert!(first.contains("reply 13"), "{shown}");
}

fn held_landing_with_dropped_pages() -> (App, Vec<contract::Envelope>) {
    let mut seq = 0u64;
    let mut lines = Vec::new();
    for n in 0..23 {
        let text = if n == 11 || n == 12 {
            format!("long {n} {}", "word ".repeat(200))
        } else {
            format!("reply {n}")
        };
        lines.extend(turn(&mut seq, &format!("a_m{n}"), &text));
    }
    let mut app = app();
    for line in &lines {
        app.on_line(line.clone());
    }
    let resident = app.attached_screen().pages().rows();
    let start = app.attached_screen().pages().index().total() - resident.len();
    let top = resident
        .iter()
        .position(|(line, _)| line.to_string().contains("reply 13"))
        .map(|at| start + at)
        .unwrap_or_else(|| panic!("turn 13's reply draws"));
    app.jump(top);
    app.overlay = Overlay::Folding {
        session: id(SESSION),
        fold: Box::new(folded(&lines)),
        busy: None,
        held: true,
        landing: false,
    };
    app.attached_screen_mut().set_frozen(true);
    app.set_size(60, 24);
    let mut fold = crate::window::Pages::new(79);
    for line in &lines {
        if let Line::Session(envelope) = line {
            fold.apply(envelope);
        }
    }
    let total = fold.index().total();
    fold.trim(total - 1, 1);
    app.land(fold, true);
    (app, envelopes(&lines))
}

fn landing_top_text(app: &App) -> String {
    let area = Rect::new(0, 0, 60, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    let conversation = app.conversation_area();
    (conversation.left()..conversation.right())
        .map(|x| {
            buf.cell((x, conversation.top()))
                .map_or(" ", |cell| cell.symbol())
        })
        .collect()
}

#[test]
fn a_failed_held_landing_does_not_move_the_top_on_a_later_load() {
    let (mut app, log) = held_landing_with_dropped_pages();
    let (session, range) = app
        .needs()
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a dropped page"));
    app.load_failed(&session, &range, "page failed");
    // Following leaves the failed page for the resident, unrelated tail.
    app.on_key(
        crate::keys::Key::End,
        contract::clock::Clock::now(fakes::clock::FakeClock::new().as_ref()),
    );
    let before = landing_top_text(&app);
    assert_eq!(app.attached_screen().top(), None, "End follows the tail");
    let top = app.attached_screen().top();
    let tail: Vec<_> = log
        .into_iter()
        .filter(|line| line.seq.is_some_and(|seq| seq > *range.end()))
        .collect();
    assert!(!tail.is_empty());
    app.load(&session, tail);
    let after = landing_top_text(&app);
    assert_eq!(
        app.attached_screen().top(),
        top,
        "before: {before:?}; after: {after:?}"
    );
}

#[test]
fn a_scrolled_held_landing_does_not_move_the_top_on_a_later_load() {
    let (mut app, log) = held_landing_with_dropped_pages();
    app.on_key(
        crate::keys::Key::PageDown,
        contract::clock::Clock::now(fakes::clock::FakeClock::new().as_ref()),
    );
    let top = app.attached_screen().top();
    let before = landing_top_text(&app);
    let (session, range) = app
        .needs()
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a dropped page"));
    let page: Vec<_> = log
        .into_iter()
        .filter(|line| line.seq.is_some_and(|seq| range.contains(&seq)))
        .collect();
    assert!(!page.is_empty());
    app.load(&session, page);
    let after = landing_top_text(&app);
    assert_eq!(
        app.attached_screen().top(),
        top,
        "before: {before:?}; after: {after:?}"
    );
}

/// A wheel-up after a held landing releases the anchor: the wheel
/// scrolls past `route_key`, straight through `Screen::scroll_rows`,
/// so a later load must not jump the top back.
#[test]
fn a_wheel_up_after_a_held_landing_does_not_jump_back_on_load() {
    let (mut app, log) = held_landing_with_dropped_pages();
    // What `scroll_conversation` runs for a wheel-up over the
    // conversation: three rows up, past the key router.
    app.attached_screen_mut().scroll_rows(true, 3, 5);
    let top = app.attached_screen().top();
    assert!(top.is_some(), "the wheel scrolled up from the landing");
    let before = landing_top_text(&app);
    let (session, range) = app
        .needs()
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a dropped page"));
    let page: Vec<_> = log
        .into_iter()
        .filter(|line| line.seq.is_some_and(|seq| range.contains(&seq)))
        .collect();
    assert!(!page.is_empty());
    app.load(&session, page);
    let after = landing_top_text(&app);
    assert_eq!(
        app.attached_screen().top(),
        top,
        "before: {before:?}; after: {after:?}"
    );
}

/// Clicking "new below" after a held landing releases the anchor: the
/// click follows past `route_key`, straight through `Screen::follow`,
/// so a later load must not jump the top back.
#[test]
fn a_follow_after_a_held_landing_does_not_jump_back_on_load() {
    let (mut app, log) = held_landing_with_dropped_pages();
    // What `on_click` runs for "new below": follow, past the key router.
    app.attached_screen_mut().follow();
    assert_eq!(app.attached_screen().top(), None, "the click follows");
    let before = landing_top_text(&app);
    let (session, range) = app
        .needs()
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a dropped page"));
    let page: Vec<_> = log
        .into_iter()
        .filter(|line| line.seq.is_some_and(|seq| range.contains(&seq)))
        .collect();
    assert!(!page.is_empty());
    app.load(&session, page);
    let after = landing_top_text(&app);
    assert_eq!(
        app.attached_screen().top(),
        None,
        "before: {before:?}; after: {after:?}"
    );
}

/// A search jump after a held landing releases the anchor: the reveal
/// jumps past `route_key`, straight through `Screen::jump`, so a later
/// load must not jump the top back.
#[test]
fn a_search_jump_after_a_held_landing_does_not_jump_back_on_load() {
    let (mut app, log) = held_landing_with_dropped_pages();
    // What `reveal_current` runs for a match on a resident page: jump,
    // past the key router and past `App::jump`.
    app.attached_screen_mut().jump(0);
    assert_eq!(app.attached_screen().top(), Some(0));
    let before = landing_top_text(&app);
    let (session, range) = app
        .needs()
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a dropped page"));
    let page: Vec<_> = log
        .into_iter()
        .filter(|line| line.seq.is_some_and(|seq| range.contains(&seq)))
        .collect();
    assert!(!page.is_empty());
    app.load(&session, page);
    let after = landing_top_text(&app);
    assert_eq!(
        app.attached_screen().top(),
        Some(0),
        "before: {before:?}; after: {after:?}"
    );
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
    let sent = app.on_line(session_accepted(SESSION, summary_id));
    assert_eq!(sent.len(), 1);
    let commands: Value = serde_json::from_str(&sent[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(commands["command"], "commands");
    assert_eq!(commands["session_id"], SESSION);
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
    assert!(matches!(
        app.overlay,
        Overlay::Folding { landing: false, .. }
    ));
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

/// A `stream_closed` with no preview lands the pass's lines: the tail
/// read failed and installed nothing, so the fold is the only copy. It
/// becomes the screen, unfrozen, and the kept pass still skips nothing.
#[test]
fn stream_closed_with_no_preview_lands_the_pass_lines() {
    let mut seq = 0u64;
    let mut fold_lines = Vec::new();
    for n in 0..2 {
        fold_lines.extend(turn(
            &mut seq,
            &format!("a_f{n}"),
            &format!("fold reply {n}"),
        ));
    }
    let mut app = folding(&[], &fold_lines, 100);
    assert_eq!(total_of(&app), 0, "no preview installed");
    app.on_line(hub("stream_closed", SESSION, None));
    assert!(matches!(app.overlay, Overlay::Kept { last: None }));
    assert!(!app.attached_screen().frozen(), "the screen unfreezes");
    assert!(total_of(&app) > 0, "the pass lines land as the screen");
    let first = app
        .attached_screen()
        .pages()
        .turn_text(0)
        .unwrap_or_default();
    assert!(
        first.contains("fold reply 0"),
        "the first reply shows: {first}"
    );
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
fn session_left_keeps_and_disconnect_ends_the_overlay() {
    let mut seq = 0u64;
    let preview = turn(&mut seq, "a_m", "the preview reply");
    let mut app = folding(&preview, &preview, seq + 10);
    assert!(app.attached_screen().frozen());
    app.on_line(hub("session_left", SESSION, None));
    // A pinned expectation this ticket changes: the pass ended
    // without its boundary, so the preview is kept, unfrozen, with
    // the bar still hidden, as `stream_closed` keeps it.
    let Overlay::Kept { .. } = &app.overlay else {
        panic!("the preview is kept");
    };
    assert!(!app.attached_screen().frozen());
    assert!(!app.counts_exact(), "the bar stays hidden");
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
        landing: false,
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
    app.outbox
        .pending
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
        | Effect::Feedback { .. }
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
        landing: false,
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
        assert_eq!(
            tail.turn_lines().map(|lines| lines.len()),
            Some(many),
            "{count} lines"
        );
    }
}

/// The read stops when the held lines starting at a turn fill the screen:
/// at one row below the total the tail asks again, at the total and
/// above it is filled. Lines cut before the turn's start never count.
#[test]
fn reads_stop_when_the_held_lines_fill_the_screen() {
    use crate::app::overlay::Tail;
    let mut seq = 0u64;
    // Lines cut inside an older turn: no `turn_started` among them.
    let mut cut = turn(&mut seq, "a_cut", "a cut reply");
    cut.drain(..3);
    let mut held = cut;
    for at in 0..3 {
        held.extend(turn(&mut seq, &format!("a_m{at}"), &format!("reply {at}")));
    }
    let all = envelopes(&held);
    // The held window starts past zero, so only the row count fills.
    let all: Vec<contract::Envelope> = all
        .into_iter()
        .map(|mut envelope| {
            envelope.seq = envelope.seq.map(|seq| contract::Seq(seq.0 + 1000));
            envelope
        })
        .collect();
    let mut tail = Tail::new();
    tail.prepend(all)
        .unwrap_or_else(|_| panic!("a contiguous answer"));
    let from = tail
        .turn_lines()
        .unwrap_or_else(|| panic!("a turn start in the window"));
    assert_eq!(
        from.first().map(|line| line.kind.as_str()),
        Some("turn_started"),
        "the cut lines never count"
    );
    let app = app();
    let mut fresh = app.attached_screen().pages().fresh();
    for envelope in from {
        fresh.apply(envelope);
    }
    let total = fresh.index().total();
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
    let from = tail
        .turn_lines()
        .unwrap_or_else(|| panic!("the turn start"));
    assert_eq!(from.len(), 700);
    assert_eq!(
        from.first().map(|line| line.kind.as_str()),
        Some("turn_started")
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
    assert!(tail.turn_lines().is_none());
}

/// One line at seq zero starts at the beginning and ends the read.
#[test]
fn a_log_of_one_line_starts_at_zero() {
    use crate::app::overlay::Tail;
    let mut tail = Tail::new();
    tail.prepend(plain(1))
        .unwrap_or_else(|_| panic!("one line"));
    assert!(tail.start_reached());
    assert_eq!(tail.turn_lines().map(|lines| lines.len()), Some(1));
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

// ---- Task 7: turn-boundary reading ----

/// A turn from `seq` with `texts` reply lines.
fn roomy_turn(seq: &mut u64, message: &str, text: &str, texts: usize) -> Vec<Line> {
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
    for _ in 0..texts {
        push("text_completed", Some(message), json!({"text": text}));
    }
    push(
        "assistant_message_completed",
        Some(message),
        json!({"outcome": "completed"}),
    );
    push("turn_completed", None, json!({"outcome": "completed"}));
    lines
}

/// Opens [`SESSION`] from home through the summary ack, with the tail
/// read running.
fn reading() -> App {
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
    let sent = app.on_line(session_accepted(SESSION, summary_id));
    assert_eq!(sent.len(), 1);
    let commands: Value = serde_json::from_str(&sent[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(commands["command"], "commands");
    assert_eq!(commands["session_id"], SESSION);
    assert!(app.tail_due().is_some(), "the tail read starts");
    assert!(
        matches!(app.overlay, Overlay::Reading(_)),
        "the tail read runs"
    );
    app
}

/// Reads `all` through the open's tail to the chain root, then
/// installs the preview.
fn install_preview(app: &mut App, all: &[contract::Envelope]) {
    while let Some(args) = app.tail_next() {
        app.tail_lines(doors(all, &args));
    }
    app.install_tail();
}

/// The attached screen's conversation rows as text.
fn row_texts(app: &App) -> Vec<String> {
    app.lines().iter().map(ToString::to_string).collect()
}

/// The read stops at the turn start that fills the screen: the newest
/// turn starts inside the first answer, and the tail below the height
/// asks once more while the tail reaching it fills, through the real
/// path. The boundary is `>=`: each tail's own total fills it, and one
/// past asks again.
#[test]
fn reads_stop_at_the_turn_start_that_fills_the_screen() {
    let height = app().conversation_height();
    // The reply length per row total: each text draws two rows.
    let mut totals: Vec<(usize, usize)> = Vec::new();
    for texts in 0..200 {
        let total = turn_start_total(texts);
        if !totals.iter().any(|(seen, _)| *seen == total) {
            totals.push((total, texts));
        }
        if totals.iter().any(|(total, _)| *total < height)
            && totals.iter().any(|(total, _)| *total >= height)
        {
            break;
        }
    }
    let below = totals
        .iter()
        .filter(|(total, _)| *total < height)
        .max_by_key(|(total, _)| *total)
        .unwrap_or_else(|| panic!("a reply short of {height}"));
    let above = totals
        .iter()
        .filter(|(total, _)| *total >= height)
        .min_by_key(|(total, _)| *total)
        .unwrap_or_else(|| panic!("a reply reaching {height}"));
    for (total, texts) in [below, above] {
        let tail = turn_start_tail(*texts);
        // The newest turn starts inside the first answer; lines cut
        // before its start never count.
        let from = tail
            .turn_lines()
            .unwrap_or_else(|| panic!("the newest turn start"));
        assert_eq!(
            from.first().map(|line| line.kind.as_str()),
            Some("turn_started"),
            "the cut lines never count"
        );
        let probe = app();
        let mut fresh = probe.attached_screen().pages().fresh();
        for line in from {
            fresh.apply(line);
        }
        assert_eq!(fresh.index().total(), *total);
        // The boundary itself is `>=`: the total fills, one past asks.
        assert!(tail.filled(&fresh, *total), "the total itself fills");
        assert!(
            !tail.filled(&fresh, total + 1),
            "one past the total asks again"
        );
        // The real path stops the read on its own segment once the
        // display fills: the pass counts the lines above the preview.
        // Below the height it still asks.
        let mut probe = app();
        probe.overlay = Overlay::Reading(tail);
        if *total >= height {
            assert!(
                probe.tail_next().is_none(),
                "a filled display stops the read"
            );
        } else {
            assert!(
                probe.tail_next().is_some(),
                "below the height it still asks"
            );
        }
    }
}

/// A filled display still walks the seam: the own run fills the
/// screen inside one window, yet the next window asks the ancestor
/// under its session, for the joint total.
#[test]
fn a_filled_display_still_walks_the_seam() {
    let mut child = segment_lines(SESSION, 1, Some((ANCESTOR, 9)));
    let mut seq = 1u64;
    child.extend(envelopes(&roomy_turn(&mut seq, "a_m", "a new reply", 90)));
    assert!(child.len() <= 256, "the own run fits one window");
    let ancestor = segment_lines(ANCESTOR, 10, None);
    let mut app = reading();
    let height = app.conversation_height();
    let mut fresh = app.attached_screen().pages().fresh();
    for line in &child {
        fresh.apply(line);
    }
    assert!(
        fresh.index().total() >= height,
        "the own run fills the screen"
    );
    // The first window fills the display, yet the seam still asks:
    // the joint total needs the ancestor.
    app.tail_lines(doors(&child, &json!({})));
    let args = app
        .tail_next()
        .expect("a filled display still walks the seam");
    assert_eq!(
        args,
        json!({"session_id": ANCESTOR, "to_seq": 9}),
        "the ancestor under its session"
    );
    app.tail_lines(doors(&ancestor, &args));
    assert!(app.tail_next().is_none(), "the ancestor root ends the walk");
}

/// A tail holding lines cut inside an older turn plus a newest turn
/// with `texts` reply lines.
fn turn_start_tail(texts: usize) -> crate::app::overlay::Tail {
    use crate::app::overlay::Tail;
    let mut seq = 0u64;
    let mut cut = turn(&mut seq, "a_old", "an old reply");
    cut.drain(..3);
    let mut held = cut;
    held.extend(roomy_turn(&mut seq, "a_new", "a new reply", texts));
    let mut tail = Tail::new();
    tail.prepend(envelopes(&held))
        .unwrap_or_else(|_| panic!("the first answer"));
    tail
}

/// The rows a [`turn_start_tail`] with `texts` reply lines reports.
fn turn_start_total(texts: usize) -> usize {
    let tail = turn_start_tail(texts);
    let from = tail
        .turn_lines()
        .unwrap_or_else(|| panic!("the newest turn start"));
    let probe = app();
    let mut fresh = probe.attached_screen().pages().fresh();
    for line in from {
        fresh.apply(line);
    }
    fresh.index().total()
}

/// A turn longer than one answer reads until its start with no cap: the
/// rows pass the height while the `turn_started` is still unread, so the
/// read keeps going down to it.
#[test]
fn a_turn_longer_than_one_answer_reads_until_its_start() {
    use crate::app::overlay::Tail;
    // Sixty older lines, then one 700-line turn starting past zero.
    let mut all = plain(60);
    let mut seq = 60u64;
    all.extend(envelopes(&roomy_turn(&mut seq, "a_m", "part", 695)));
    assert_eq!(all.len(), 760);
    for envelope in &mut all {
        envelope.seq = envelope.seq.map(|seq| contract::Seq(seq.0 + 1000));
    }
    let probe = app();
    let height = probe.conversation_height();
    let mut tail = Tail::new();
    let mut reads = 0;
    loop {
        let mut fresh = probe.attached_screen().pages().fresh();
        if let Some(from) = tail.turn_lines() {
            for line in from {
                fresh.apply(line);
            }
        }
        if tail.filled(&fresh, height) {
            break;
        }
        let args = tail.next_args();
        let answer = doors(&all, &args);
        tail.prepend(answer)
            .unwrap_or_else(|_| panic!("a contiguous answer"));
        reads += 1;
    }
    assert_eq!(reads, 3, "256, 256 and 248 lines");
    let from = tail
        .turn_lines()
        .unwrap_or_else(|| panic!("the turn start"));
    assert_eq!(from.len(), 700);
    assert_eq!(
        from.first().map(|line| line.kind.as_str()),
        Some("turn_started")
    );
}

/// A failed or abandoned read keeps whole turns or nothing: the preview
/// is the turn-start lines when the first answer holds one, else an
/// empty conversation, and no frame shows a cut turn.
#[test]
fn a_failed_read_keeps_whole_turns_or_nothing() {
    use crate::app::overlay::Tail;
    // A first answer holding a turn start keeps from that start.
    let mut seq = 0u64;
    let mut lines = Vec::new();
    for n in 0..50 {
        lines.extend(turn(&mut seq, &format!("a_m{n}"), &format!("reply {n}")));
    }
    let all = envelopes(&lines);
    let first = doors(&all, &json!({}));
    assert!(first.len() < all.len(), "the first answer is a window");
    let mut app = reading();
    assert!(app.tail_next().is_some());
    app.tail_lines(first.clone());
    app.tail_ended(Some("gone"));
    app.install_tail();
    assert!(
        matches!(app.overlay, Overlay::Shown { .. }),
        "the preview shows"
    );
    let cut = (all.len() - first.len()) / 6;
    let mut tail = Tail::new();
    tail.prepend(first)
        .unwrap_or_else(|_| panic!("the first answer"));
    let from = tail
        .turn_lines()
        .unwrap_or_else(|| panic!("a turn start in the window"));
    let mut fresh = app.attached_screen().pages().fresh();
    for line in from {
        fresh.apply(line);
    }
    let expect: Vec<String> = fresh
        .rows()
        .iter()
        .map(|(line, _)| line.to_string())
        .collect();
    assert_eq!(row_texts(&app), expect, "the preview is the whole turns");
    assert!(
        !row_texts(&app)
            .iter()
            .any(|row| row.contains(&format!("reply {cut}"))),
        "no cut turn shows"
    );
    // A first answer holding no turn start keeps nothing.
    let all = plain(300);
    let first = doors(&all, &json!({}));
    let mut app = reading();
    app.tail_lines(first);
    app.tail_ended(Some("gone"));
    app.install_tail();
    assert!(
        matches!(app.overlay, Overlay::Shown { .. }),
        "the preview shows"
    );
    assert_eq!(
        app.attached_screen().pages().index().total(),
        0,
        "an empty conversation"
    );
}

/// The preview is exact for the last turns: folding the tail's
/// turn-start lines draws the last screenful, the totals line, the open
/// ledger's step numbers and the prompt bubble exactly as the whole log
/// does. A log with an orphaned job completion, an automatic handoff
/// trigger and a ledger call from before the held turns lands with only
/// those allowed kinds differing (B3).
#[test]
fn the_preview_is_exact_for_the_last_turns() {
    // Three long turns, then one short turn with a tool call, so the
    // last screenful holds the short turn's bubble, ledger and totals:
    // more than one window in all.
    let mut seq = 0u64;
    let mut lines = Vec::new();
    for n in 0..3 {
        lines.extend(roomy_turn(
            &mut seq,
            &format!("a_m{n}"),
            &format!("reply {n}"),
            90,
        ));
    }
    let message = "a_m3".to_owned();
    let call = "a_c3".to_owned();
    let mut turn = Vec::new();
    let mut push = |kind: &str, action: Option<&str>, payload: Value| {
        turn.push(line(kind, seq, action, payload));
        seq += 1;
    };
    push(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "prompt"}]}]}),
    );
    push("step_started", None, json!({}));
    push("assistant_message_started", Some(&message), json!({}));
    push(
        "tool_call_requested",
        Some(&call),
        json!({"name": "read", "arguments": {"path": "src/a.rs"}}),
    );
    push(
        "tool_call_completed",
        Some(&call),
        json!({"status": "completed",
            "content": [{"type": "text", "text": "ok"}]}),
    );
    push("text_completed", Some(&message), json!({"text": "reply 3"}));
    push(
        "assistant_message_completed",
        Some(&message),
        json!({"outcome": "completed"}),
    );
    push("turn_completed", None, json!({"outcome": "completed"}));
    lines.extend(turn);
    let all = envelopes(&lines);
    assert!(all.len() > 256, "the log needs more than one window");
    let mut app = reading();
    // The whole log folded at the screen's own wrap width.
    let mut whole = app.attached_screen().pages().fresh();
    for envelope in &all {
        whole.apply(envelope);
    }
    install_preview(&mut app, &all);
    assert!(
        matches!(app.overlay, Overlay::Shown { .. }),
        "the preview shows"
    );
    // The open ledger draws its step numbers in both folds.
    app.toggle_ledgers();
    whole.toggle_ledgers();
    let height = app.conversation_height();
    let preview = row_texts(&app);
    let whole_rows: Vec<String> = whole
        .rows()
        .iter()
        .map(|(line, _)| line.to_string())
        .collect();
    assert!(preview.len() >= height && whole_rows.len() >= height);
    let screen = &preview[preview.len() - height..];
    assert_eq!(
        screen,
        &whole_rows[whole_rows.len() - height..],
        "the last screenful is exact"
    );
    let screenful = screen.join("\n");
    assert!(screenful.contains("▣"), "the totals line shows");
    assert!(
        screenful
            .lines()
            .any(|row| row.trim_start().starts_with(|c: char| c.is_ascii_digit())),
        "the open ledger's step numbers show: {screenful:?}"
    );
    assert!(
        screenful.contains("prompt"),
        "the prompt bubble shows: {screenful:?}"
    );
}

/// The landing adds the lines above a filled preview: the open
/// holds the filled tail, the pass replays the whole log through to
/// its end, and the landed screen keeps the preview's rows with the
/// counted lines above them.
#[test]
fn the_landing_adds_the_lines_above_a_filled_preview() {
    let mut seq = 0u64;
    let mut lines = Vec::new();
    let mut push = |kind: &str, action: Option<&str>, payload: Value| {
        lines.push(line(kind, seq, action, payload));
        seq += 1;
    };
    for _ in 0..10 {
        push("usage_recorded", None, json!({}));
    }
    // Before the held lines: a job starts, a handoff starts, a tool
    // call is requested.
    push(
        "job_started",
        None,
        json!({"job_id": "j_1", "description": "build the docs",
            "output_path": "/tmp/o"}),
    );
    push("handoff_started", None, json!({"trigger": "person"}));
    push(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "prompt"}]}]}),
    );
    push("step_started", None, json!({}));
    push("assistant_message_started", Some("a_early"), json!({}));
    push(
        "tool_call_requested",
        Some("a_c0"),
        json!({"name": "read", "arguments": {"path": "src/a.rs"}}),
    );
    push(
        "assistant_message_completed",
        Some("a_early"),
        json!({"outcome": "completed"}),
    );
    push("turn_completed", None, json!({"outcome": "completed"}));
    // Filler the first window skips over.
    for _ in 0..260 {
        push("usage_recorded", None, json!({}));
    }
    // The held turns: the orphaned completion, the handoff trigger's
    // completion, and the ledger call's result.
    push(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "prompt"}]}]}),
    );
    push("step_started", None, json!({}));
    push("assistant_message_started", Some("a_held"), json!({}));
    push(
        "text_completed",
        Some("a_held"),
        json!({"text": "a held reply"}),
    );
    push(
        "assistant_message_completed",
        Some("a_held"),
        json!({"outcome": "completed"}),
    );
    push("turn_completed", None, json!({"outcome": "completed"}));
    push(
        "job_completed",
        None,
        json!({"job_id": "j_1", "status": "failed",
            "error": {"code": "orphaned", "message": "j_1 orphaned."}}),
    );
    push(
        "handoff_completed",
        None,
        json!({"outcome": "completed", "tokens_before": 300_000,
            "note_text": "From the hook.", "extension": "notes"}),
    );
    push(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "prompt"}]}]}),
    );
    push("step_started", None, json!({}));
    push("assistant_message_started", Some("a_last"), json!({}));
    push(
        "tool_call_completed",
        Some("a_c0"),
        json!({"status": "completed",
            "content": [{"type": "text", "text": "ok"}]}),
    );
    push(
        "assistant_message_completed",
        Some("a_last"),
        json!({"outcome": "completed"}),
    );
    push("turn_completed", None, json!({"outcome": "completed"}));
    let all = envelopes(&lines);
    assert!(all.len() > 256, "the early turns leave the first window");
    let mut app = reading();
    install_preview(&mut app, &all);
    assert!(
        matches!(app.overlay, Overlay::Shown { .. }),
        "the preview shows"
    );
    let before = row_texts(&app);
    // Raise, then replay the whole log through the pass to its end.
    let raise: Value = serde_json::from_str(&app.take_raise().unwrap_or_else(|| panic!("a raise")))
        .unwrap_or_else(|panic| panic!("{panic}"));
    let raise_id = raise["id"].as_str().unwrap_or_else(|| panic!("an id"));
    let last = all.len() as u64 - 1;
    assert!(
        app.on_line(raise_accepted(SESSION, raise_id, last))
            .is_empty()
    );
    assert!(
        matches!(app.overlay, Overlay::Folding { .. }),
        "the pass folds"
    );
    for envelope in &all {
        app.on_line(Line::Session(envelope.clone()));
    }
    assert!(matches!(app.overlay, Overlay::None), "the pass lands");
    let after = row_texts(&app);
    // The preview holds the filled tail while the pass replays the
    // whole log: the landing adds the rows above it.
    assert!(
        after.len() > before.len(),
        "the pass counts the lines above the preview"
    );
    assert!(
        after.join("\n").contains("a held reply"),
        "the held turn lands"
    );
}

/// Ending the tail read marks it ended: a fresh read is open, and
/// `end` closes it for every path that ends one (failed, abandoned,
/// or lost).
#[test]
fn ending_the_tail_read_marks_it_ended() {
    use crate::app::overlay::Tail;
    let tail = Tail::new();
    assert!(!tail.ended(), "a fresh read is open");
    let mut tail = tail;
    tail.end();
    assert!(tail.ended(), "ending the read marks it");
}

/// A failed load lets the held landing go only without a pass: with
/// one the landing holds until its pages load.
#[test]
fn a_failed_load_clears_the_held_anchor_only_without_a_pass() {
    use contract::Seq;
    let mut app = app();
    let mut seq = 0u64;
    for line in turn(&mut seq, "a_m", "the held reply") {
        app.on_line(line);
    }
    let range = Seq(0)..=Seq(seq.saturating_sub(1));
    app.screen.pages_mut().hold_anchor(&id(SESSION), Seq(0), 0);
    app.load_failed(&id(SESSION), &range, "boom");
    assert!(
        app.screen.pages_mut().anchored_row().is_none(),
        "with no pass the landing lets go"
    );
    app.screen.pages_mut().hold_anchor(&id(SESSION), Seq(0), 0);
    app.passes.push((id(SESSION), Pass::Until(Seq(1_000))));
    app.load_failed(&id(SESSION), &range, "boom");
    assert!(
        app.screen.pages_mut().anchored_row().is_some(),
        "with a pass the landing holds"
    );
}

/// A fold starts only on the shown session's raise ack: a wrong ack,
/// or another session, leaves the preview where it is.
#[test]
fn a_fold_starts_only_on_the_shown_sessions_raise_ack() {
    let mut app = app();
    app.overlay = Overlay::Shown {
        session: id(SESSION),
        busy: None,
        held: false,
    };
    app.begin_fold(&id(SESSION), "a-wrong-ack");
    assert!(
        matches!(app.overlay, Overlay::Shown { .. }),
        "a wrong ack starts no fold"
    );
    app.begin_fold(&id("s_bbbbbbbbbbbbbbbb"), "whatever");
    assert!(
        matches!(app.overlay, Overlay::Shown { .. }),
        "another session starts no fold"
    );
}

/// A reopening for another session keeps the running fold: only its
/// own session restarts the second `Pages`.
#[test]
fn a_reopening_for_another_session_keeps_the_running_fold() {
    let mut seq = 0u64;
    let preview = turn(&mut seq, "a_m", "the preview reply");
    let fold_lines = turn(&mut seq, "a_f", "the fold reply");
    let mut app = folding(&preview, &fold_lines, 100);
    let Overlay::Folding { fold, .. } = &app.overlay else {
        panic!("a fold runs");
    };
    let held = fold.index().total();
    app.on_line(hub("stream_reopened", "s_bbbbbbbbbbbbbbbb", Some(50)));
    let Overlay::Folding { session, fold, .. } = &app.overlay else {
        panic!("the other session's reopen keeps the fold");
    };
    assert_eq!(session, &id(SESSION));
    assert_eq!(fold.index().total(), held, "the fold keeps its lines");
}

/// A close for another session keeps the running fold: only its own
/// session keeps the preview or lands the pass's lines.
#[test]
fn a_close_for_another_session_keeps_the_running_fold() {
    let mut seq = 0u64;
    let preview = turn(&mut seq, "a_m", "the preview reply");
    let fold_lines = turn(&mut seq, "a_f", "the fold reply");
    let mut app = folding(&preview, &fold_lines, 100);
    let Overlay::Folding { fold, .. } = &app.overlay else {
        panic!("a fold runs");
    };
    let held = fold.index().total();
    app.on_line(hub("stream_closed", "s_bbbbbbbbbbbbbbbb", None));
    let Overlay::Folding { session, fold, .. } = &app.overlay else {
        panic!("the other session's close keeps the fold");
    };
    assert_eq!(session, &id(SESSION));
    assert_eq!(fold.index().total(), held, "the fold keeps its lines");
}

/// The ancestor's session: the chain's first segment.
const ANCESTOR: &str = "s_bbbbbbbbbbbbbbbb";

/// `lines` rehomed under `session`: the ancestor's preview under its own
/// id.
fn rehome(lines: Vec<Line>, session: &str) -> Vec<Line> {
    lines
        .into_iter()
        .map(|line| match line {
            Line::Session(mut envelope) => {
                envelope.session_id = SessionId(session.to_owned());
                Line::Session(envelope)
            }
            Line::Hub(hub) => Line::Hub(hub),
        })
        .collect()
}

#[test]
fn land_carries_closed_ancestor_pages_across_the_swap() {
    let mut seq = 0u64;
    let mut ancestor = Vec::new();
    for n in 0..12 {
        ancestor.extend(turn(
            &mut seq,
            &format!("a_m{n}"),
            &format!("ancestor reply {n}"),
        ));
    }
    let ancestor = rehome(ancestor, ANCESTOR);
    let mut dseq = 0u64;
    let mut descendant = Vec::new();
    for n in 0..14 {
        descendant.extend(turn(
            &mut dseq,
            &format!("b_m{n}"),
            &format!("descendant reply {n}"),
        ));
    }
    let mut app = app();
    // The ancestor runs page in from history, straight into the
    // preview's pages; the own run streams live.
    for line in &ancestor {
        if let Line::Session(envelope) = line {
            app.attached_screen_mut().pages_mut().apply(envelope);
        }
    }
    for line in &descendant {
        app.on_line(line.clone());
    }
    let before = app.attached_screen().pages().index().total();
    // The ancestor run, then the own run: each page carries its
    // segment's session, so the runs read through the pages alone.
    let seam = app
        .attached_screen()
        .pages()
        .index()
        .pages()
        .iter()
        .position(|page| page.session == id(SESSION))
        .expect("the own run");
    assert!(seam > 0, "the ancestor runs first");
    // The fold replays the own log only.
    let fold = folded(&descendant);
    app.land(fold, false);
    let pages = app.attached_screen().pages();
    assert_eq!(pages.index().total(), before, "the landing keeps every row");
    let carried = pages
        .index()
        .pages()
        .iter()
        .position(|page| page.session == id(SESSION))
        .expect("the own run");
    assert!(carried > 0, "the chain survives the swap");
    assert!(
        pages.index().pages()[..carried]
            .iter()
            .all(|page| page.session == id(ANCESTOR)),
        "every carried page is the ancestor's"
    );
    // Every ancestor seq routes to an ancestor page.
    for line in &ancestor {
        let Line::Session(envelope) = line else {
            continue;
        };
        let seq = envelope.seq.expect("a durable seq");
        let at = pages
            .index()
            .page_of(&id(ANCESTOR), seq)
            .expect("an ancestor page");
        assert_eq!(pages.index().pages()[at].session, id(ANCESTOR));
    }
    // Away from the seam and back: each dropped side reloads under its
    // own session, and the joint count holds.
    let seam = pages.index().start(carried);
    let combined: Vec<Line> = ancestor.iter().chain(&descendant).cloned().collect();
    app.jump(0);
    app.jump(seam);
    let needs = app.needs();
    assert!(!needs.is_empty(), "the seam pages dropped");
    assert!(
        needs.iter().any(|(session, _)| *session == id(ANCESTOR)),
        "the ancestor side is asked under its session"
    );
    assert!(
        needs.iter().any(|(session, _)| *session == id(SESSION)),
        "the own side is asked under its session"
    );
    for (session, range) in needs {
        let lines: Vec<contract::Envelope> = combined
            .iter()
            .filter_map(|line| match line {
                Line::Session(envelope)
                    if envelope.session_id == session
                        && envelope.seq.is_some_and(|seq| range.contains(&seq)) =>
                {
                    Some(envelope.clone())
                }
                Line::Session(_) | Line::Hub(_) => None,
            })
            .collect();
        assert!(!lines.is_empty(), "no lines for {session:?} {range:?}");
        app.load(&session, lines);
    }
    assert!(app.needs().is_empty(), "every side reloaded");
    assert_eq!(
        app.attached_screen().pages().index().total(),
        before,
        "the joint count survives the reload"
    );
}

/// `count` durable lines under `session`, numbered from zero: a
/// `session_started` carrying `parent` as its `forked_from`, then
/// `usage_recorded` filler.
fn segment_lines(
    session: &str,
    count: usize,
    parent: Option<(&str, u64)>,
) -> Vec<contract::Envelope> {
    let mut lines = Vec::with_capacity(count);
    let mut started = json!({});
    if let Some((parent_session, point)) = parent {
        started = json!({"forked_from": {"session_id": parent_session, "seq": point}});
    }
    lines.push(contract::Envelope {
        kind: "session_started".to_owned(),
        session_id: id(session),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: Some(contract::Seq(0)),
        payload: started.as_object().cloned().unwrap_or_default(),
    });
    for seq in 1..count {
        let seq = u64::try_from(seq).expect("a session line");
        if let Line::Session(envelope) = line("usage_recorded", seq, None, json!({})) {
            let mut envelope = envelope;
            envelope.session_id = id(session);
            lines.push(envelope);
        }
    }
    lines
}

/// Reads `child` then its ancestor `root` through a fresh tail,
/// routing each answer by the args' `session_id`, and returns each
/// `next_args` in order.
fn read_chain(
    child: &[contract::Envelope],
    root: &[contract::Envelope],
) -> (Vec<Value>, crate::app::overlay::Tail) {
    use crate::app::overlay::Tail;
    let mut tail = Tail::new();
    let mut asked = Vec::new();
    loop {
        let args = tail.next_args();
        asked.push(args.clone());
        let pool = if args.get("session_id").is_some() {
            root
        } else {
            child
        };
        let answer = doors(pool, &args);
        let empty = answer.is_empty();
        tail.prepend(answer)
            .unwrap_or_else(|_| panic!("a contiguous answer"));
        if tail.chain_done() || empty {
            break;
        }
    }
    (asked, tail)
}

/// The tail walks the chain past the seam: the own windows, then the
/// ancestor's under its session, down to the root's first line.
#[test]
fn the_tail_walks_the_chain_past_the_seam() {
    let root = segment_lines(ANCESTOR, 300, None);
    let child = segment_lines(SESSION, 300, Some((ANCESTOR, 299)));
    let (asked, tail) = read_chain(&child, &root);
    assert_eq!(
        asked,
        vec![
            json!({}),
            json!({"to_seq": 43}),
            json!({"session_id": ANCESTOR, "to_seq": 299}),
            json!({"session_id": ANCESTOR, "to_seq": 43}),
        ],
        "the own windows, then the ancestor's under its session"
    );
    assert!(tail.chain_done(), "the root's first line ends the walk");
    let held: Vec<(String, u64)> = tail
        .turn_lines()
        .unwrap_or_else(|| panic!("the held run"))
        .iter()
        .map(|line| {
            (
                line.session_id.0.clone(),
                line.seq.map(|seq| seq.0).unwrap_or(u64::MAX),
            )
        })
        .collect();
    assert_eq!(held.len(), 600, "the joint run is held");
    assert!(
        held.iter()
            .take(300)
            .all(|(session, _)| session == ANCESTOR),
        "ancestors first"
    );
    assert!(
        held.iter().skip(300).all(|(session, _)| session == SESSION),
        "then the own run"
    );
}

/// An ancestor longer than one window walks every window: 255 and 256
/// lines read one window, 257 and 258 read two, every window past the
/// seam naming its segment.
#[test]
fn an_ancestor_longer_than_one_window_walks_every_window() {
    for (count, expected) in [
        (
            255usize,
            vec![json!({"session_id": ANCESTOR, "to_seq": 254})],
        ),
        (256, vec![json!({"session_id": ANCESTOR, "to_seq": 255})]),
        (
            257,
            vec![
                json!({"session_id": ANCESTOR, "to_seq": 256}),
                json!({"session_id": ANCESTOR, "to_seq": 0}),
            ],
        ),
        (
            258,
            vec![
                json!({"session_id": ANCESTOR, "to_seq": 257}),
                json!({"session_id": ANCESTOR, "to_seq": 1}),
            ],
        ),
    ] {
        let root = segment_lines(ANCESTOR, count, None);
        let point = u64::try_from(count).expect("an ancestor line") - 1;
        let child = segment_lines(SESSION, 3, Some((ANCESTOR, point)));
        let (asked, tail) = read_chain(&child, &root);
        let mut wants = vec![json!({})];
        wants.extend(expected);
        assert_eq!(asked, wants, "{count} ancestor lines");
        assert!(tail.chain_done(), "{count} ancestor lines");
        assert_eq!(
            tail.turn_lines().map(|lines| lines.len()),
            Some(count + 3),
            "{count} ancestor lines"
        );
    }
}

/// A new segment the held run does not name, or one reaching above the
/// named point, ends the tail as failed with the out-of-order notice.
#[test]
fn an_unnamed_segment_across_the_seam_ends_the_tail_as_failed() {
    let child = segment_lines(SESSION, 10, Some((ANCESTOR, 299)));
    let mut app = reading();
    app.tail_lines(child.clone());
    let mut wrong = segment_lines(ANCESTOR, 5, None);
    for line in &mut wrong {
        line.session_id = id("s_cccccccccccccccc");
    }
    app.tail_lines(wrong);
    assert_eq!(
        app.notice(),
        Some("Could not load history: the answer was out of order"),
        "another session across the seam fails"
    );
    let mut above = segment_lines(ANCESTOR, 5, None);
    for line in &mut above {
        line.seq = line.seq.map(|seq| contract::Seq(seq.0 + 300));
    }
    app.tail_lines(above);
    assert_eq!(
        app.notice(),
        Some("Could not load history: the answer was out of order"),
        "above the point fails"
    );
}
/// A segment's durable lines under `session`: a `session_started`
/// carrying `parent` as its `forked_from`, then `turns` turns replying
/// `marker N`, numbered from zero.
fn chain_segment(
    session: &str,
    parent: Option<(&str, u64)>,
    marker: &str,
    turns: usize,
) -> Vec<contract::Envelope> {
    let mut lines = Vec::new();
    let mut started = json!({});
    if let Some((parent_session, point)) = parent {
        started = json!({"forked_from": {"session_id": parent_session, "seq": point}});
    }
    lines.push(contract::Envelope {
        kind: "session_started".to_owned(),
        session_id: id(session),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: Some(contract::Seq(0)),
        payload: started.as_object().cloned().unwrap_or_default(),
    });
    let mut seq = 1u64;
    for turn_at in 0..turns {
        for line in turn(
            &mut seq,
            &format!("a_m{turn_at}"),
            &format!("{marker} {turn_at}"),
        ) {
            if let Line::Session(mut envelope) = line {
                envelope.session_id = id(session);
                // Distinct actions per segment: the seqs repeat across
                // the seam.
                if let Some(action) = envelope.action_id.take() {
                    envelope.action_id = Some(contract::ActionId(format!("{session}{action:?}")));
                }
                lines.push(envelope);
            }
        }
    }
    lines
}

/// Reads `child` then `ancestor` through `app`'s tail, routing each
/// answer by the args' `session_id`, and installs the preview.
fn install_chain(app: &mut App, ancestor: &[contract::Envelope], child: &[contract::Envelope]) {
    while let Some(args) = app.tail_next() {
        let pool = if args.get("session_id").is_some() {
            ancestor
        } else {
            child
        };
        app.tail_lines(doors(pool, &args));
    }
    app.install_tail();
}

/// The acceptance screen across one continuation point: the preview
/// the chain walk installs and the screen the pass lands show the
/// same paged rows, root and child turns both.
#[test]
fn the_landed_screen_matches_the_preview_across_the_seam() {
    let ancestor = chain_segment(ANCESTOR, None, "root reply", 3);
    let point = ancestor.len() as u64 - 1;
    let child = chain_segment(SESSION, Some((ANCESTOR, point)), "child reply", 2);
    let mut app = reading();
    install_chain(&mut app, &ancestor, &child);
    assert!(
        matches!(app.overlay, Overlay::Shown { .. }),
        "the preview shows"
    );
    let before = row_texts(&app);
    assert!(
        before.iter().any(|row| row.contains("root reply 0")),
        "the preview draws the root"
    );
    assert!(
        before.iter().any(|row| row.contains("child reply 1")),
        "the preview draws the child"
    );
    // Raise, then replay the own run through the pass to its end.
    let raise: Value = serde_json::from_str(&app.take_raise().unwrap_or_else(|| panic!("a raise")))
        .unwrap_or_else(|panic| panic!("{panic}"));
    let raise_id = raise["id"].as_str().unwrap_or_else(|| panic!("an id"));
    let last = child.len() as u64 - 1;
    assert!(
        app.on_line(raise_accepted(SESSION, raise_id, last))
            .is_empty()
    );
    assert!(
        matches!(app.overlay, Overlay::Folding { .. }),
        "the pass folds"
    );
    for envelope in &child {
        app.on_line(Line::Session(envelope.clone()));
    }
    assert!(matches!(app.overlay, Overlay::None), "the pass lands");
    let after = row_texts(&app);
    assert_eq!(
        after, before,
        "the landing shows the same paged rows across the seam"
    );
}

/// An ended read is done even before its start: `chain_done` is the
/// ended flag or the root, never both. Kills the `||` to `&&` mutant,
/// which would wait for the start after the end.
#[test]
fn an_ended_read_is_done_even_before_its_start() {
    use crate::app::overlay::Tail;
    let mut tail = Tail::new();
    tail.end();
    assert!(!tail.start_reached(), "nothing was read");
    assert!(tail.chain_done(), "an ended read is done without its start");
}

/// Seq zero with no parent pends no seam: `seam_pending` needs the
/// first line and the named parent together. Kills the `&&` to `||`
/// mutant, which would cross past the chain root.
#[test]
fn seq_zero_with_no_parent_pends_no_seam() {
    use crate::app::overlay::Tail;
    let mut tail = Tail::new();
    tail.prepend(segment_lines(SESSION, 1, None))
        .unwrap_or_else(|_| panic!("the root's first line"));
    assert!(tail.start_reached(), "seq zero ends the segment");
    assert!(
        !tail.seam_pending(),
        "the root's first line names no parent"
    );
}

/// A `session_started` past seq zero names no parent: `forked_from`
/// needs the kind and seq zero together. Kills the `||` to `&&`
/// mutant, which would read the parent off a later `session_started`.
#[test]
fn a_session_started_past_seq_zero_names_no_parent() {
    use crate::app::overlay::Tail;
    let mut held = segment_lines(SESSION, 1, Some((ANCESTOR, 9)));
    held[0].seq = Some(contract::Seq(5));
    let mut tail = Tail::new();
    tail.prepend(held)
        .unwrap_or_else(|_| panic!("the held run"));
    assert!(
        tail.forked_from().is_none(),
        "seq 5 is not the segment's first line"
    );
    // The parent lookup misses, so a chunk across the seam is
    // refused even under the named point: the mutant would accept it.
    let ancestor = segment_lines(ANCESTOR, 5, None);
    assert!(
        tail.prepend(ancestor).is_err(),
        "no parent crosses the seam"
    );
}

/// A non-`session_started` first line at seq zero names no parent:
/// `forked_from` needs the kind and seq zero together. Kills the `||`
/// to `&&` mutant, which would read the parent off another kind.
#[test]
fn a_non_session_started_first_line_at_seq_zero_names_no_parent() {
    use crate::app::overlay::Tail;
    let Line::Session(mut first) = line(
        "usage_recorded",
        0,
        None,
        json!({"forked_from": {"session_id": ANCESTOR, "seq": 9}}),
    ) else {
        panic!("a session line");
    };
    first.session_id = id(SESSION);
    let mut tail = Tail::new();
    tail.prepend(vec![first])
        .unwrap_or_else(|_| panic!("the held run"));
    assert!(tail.start_reached(), "seq zero ends the segment");
    assert!(tail.forked_from().is_none(), "another kind names no parent");
    assert!(
        !tail.seam_pending(),
        "another kind at seq zero pends no seam"
    );
    assert!(tail.chain_done(), "another kind at seq zero ends the walk");
}

/// An older chunk above the named point is refused: `prepend` rejects
/// a newest seq strictly greater than the held run's `forked.seq`.
/// Kills the `>` to `<` mutant, which would accept the chunk above.
#[test]
fn an_older_chunk_above_the_named_point_is_refused() {
    use crate::app::overlay::Tail;
    let mut tail = Tail::new();
    tail.prepend(segment_lines(SESSION, 10, Some((ANCESTOR, 9))))
        .unwrap_or_else(|_| panic!("the held run"));
    let above = segment_lines(ANCESTOR, 11, None);
    assert_eq!(
        above.last().and_then(|line| line.seq),
        Some(contract::Seq(10)),
        "the chunk's newest sits above the point"
    );
    assert!(
        tail.prepend(above).is_err(),
        "a newest seq above the point is refused"
    );
}

/// An older chunk ending at the named point is accepted: `prepend`
/// only refuses above it, so `>` does not fire at equality. Pins the
/// contiguity edge beside the refusal above.
#[test]
fn an_older_chunk_ending_at_the_named_point_is_accepted() {
    use crate::app::overlay::Tail;
    let mut tail = Tail::new();
    tail.prepend(segment_lines(SESSION, 10, Some((ANCESTOR, 9))))
        .unwrap_or_else(|_| panic!("the held run"));
    let at_point = segment_lines(ANCESTOR, 10, None);
    assert_eq!(
        at_point.last().and_then(|line| line.seq),
        Some(contract::Seq(9)),
        "the chunk's newest sits on the point"
    );
    assert!(
        tail.prepend(at_point).is_ok(),
        "a newest seq on the point crosses"
    );
    assert!(
        !tail.seam_pending(),
        "the crossing lands on the ancestor's first line"
    );
}

/// The surfaces on top: two open in either order give the same keys,
/// Esc, wheel and drawn top frame (`docs/tui.md`, "Keys", "Overlays").
mod stacked {
    use super::{id, rendered, turn};
    use crate::app::top::tests::{
        ON_HOME, ON_SESSION, SESSION, home_app, open, session_app, stacked,
    };
    use crate::app::{App, Effect, Surface};
    use crate::keys::{Key, Mouse, MouseKind};
    use crate::stroke::Stroke;
    use contract::clock::Clock;

    /// A session app with a conversation long enough to scroll.
    fn long_session() -> App {
        let mut app = session_app();
        let mut seq = 0;
        for at in 0..40 {
            for next in turn(&mut seq, &format!("a_m{at}"), "a one-line reply") {
                app.on_line(next);
            }
        }
        assert_eq!(app.top(), None, "following the bottom");
        assert_eq!(app.session(), Some(&id(SESSION)));
        app
    }

    fn now() -> std::time::Instant {
        fakes::clock::FakeClock::new().now()
    }

    fn wheel(col: u16, row: u16, down: bool) -> Mouse {
        Mouse {
            kind: if down {
                MouseKind::WheelDown
            } else {
                MouseKind::WheelUp
            },
            col,
            row,
        }
    }

    /// Presses the stroke `name` names, the way the terminal's keys arrive.
    fn press(app: &mut App, name: &str) -> Effect {
        let stroke = Stroke::parse(name).unwrap_or_else(|err| panic!("{name}: {err}"));
        app.on_press(stroke, now())
    }

    #[test]
    fn a_character_reaches_the_draft_only_through_the_notice_overlay_and_completions() {
        for (set, make) in [
            (&ON_SESSION[..], session_app as fn() -> App),
            (&ON_HOME[..], home_app as fn() -> App),
        ] {
            for (first, second, mut app) in stacked(set, make) {
                let head = app.top_surface();
                let before = app.draft();
                press(&mut app, "x");
                // A completion panel leaves the character to a tty job below it.
                let types = matches!(head, Surface::Notice | Surface::Completion)
                    && !app.top_surfaces().contains(&Surface::Job);
                assert_eq!(app.draft() != before, types, "{first:?} then {second:?}");
            }
        }
    }

    #[test]
    fn no_other_key_closes_the_notice_overlay() {
        for (set, make) in [
            (&ON_SESSION[..], session_app as fn() -> App),
            (&ON_HOME[..], home_app as fn() -> App),
        ] {
            for (first, second, mut app) in stacked(set, make) {
                if first != Surface::Notice && second != Surface::Notice {
                    continue;
                }
                for key in ["up", "down", "pageup", "enter", "tab"] {
                    press(&mut app, key);
                    assert!(
                        app.top_surfaces().contains(&Surface::Notice),
                        "{first:?} then {second:?} after {key}"
                    );
                }
            }
        }
    }

    #[test]
    fn esc_closes_one_surface_in_either_order_on_a_session_and_on_home() {
        for (set, make) in [
            (&ON_SESSION[..], session_app as fn() -> App),
            (&ON_HOME[..], home_app as fn() -> App),
        ] {
            for (first, second, mut app) in stacked(set, make) {
                // The head, except that the notice overlay closes ahead of
                // the heads that yield their Esc to it.
                let head = first.min(second);
                let yields = matches!(
                    head,
                    Surface::KeysScreen
                        | Surface::Offer
                        | Surface::Results
                        | Surface::Find
                        | Surface::Focus
                );
                // A tty job's view sends every key but Esc to the program, so
                // Esc falls through it to what lies below.
                let closes = if head == Surface::Job {
                    first.max(second)
                } else if first.max(second) == Surface::Notice && yields {
                    Surface::Notice
                } else {
                    head
                };
                assert_eq!(press(&mut app, "esc"), Effect::None);
                let listed = app.top_surfaces();
                assert!(
                    !listed.contains(&closes),
                    "{first:?} then {second:?} leaves {closes:?} open: {listed:?}"
                );
                let kept = if closes == first { second } else { first };
                assert!(
                    listed.contains(&kept),
                    "{first:?} then {second:?} closes {kept:?} too: {listed:?}"
                );
            }
        }
    }

    #[test]
    fn the_wheel_scrolls_the_report_whatever_else_is_open() {
        for (set, make) in [
            (&ON_SESSION[..], session_app as fn() -> App),
            (&ON_HOME[..], home_app as fn() -> App),
        ] {
            for (first, second, mut app) in stacked(set, make) {
                if first != Surface::Report && second != Surface::Report {
                    continue;
                }
                let top = |app: &App| app.import_report().map(|report| report.top());
                assert_eq!(top(&app), Some(0));
                app.on_wheel(&wheel(40, 12, true));
                assert_eq!(top(&app), Some(3), "{first:?} then {second:?}");
                app.on_wheel(&wheel(0, 0, true));
                assert_eq!(top(&app), Some(6), "{first:?} then {second:?}");
            }
        }
    }

    #[test]
    fn the_quit_frame_draws_last_over_every_other_surface() {
        for (set, make) in [
            (&ON_SESSION[..], session_app as fn() -> App),
            (&ON_HOME[..], home_app as fn() -> App),
        ] {
            let mut alone = make();
            open(&mut alone, Surface::Quit);
            let slab = crate::view::frame_of(&alone, Surface::Quit);
            assert!(!slab.is_empty());
            let reference = rendered(&alone);
            for (first, second, app) in stacked(set, make) {
                if first != Surface::Quit && second != Surface::Quit {
                    continue;
                }
                let buf = rendered(&app);
                // The frame's text is whole; a cell may keep a colour from
                // the text beneath it.
                for y in slab.top()..slab.bottom() {
                    for x in slab.left()..slab.right() {
                        assert_eq!(
                            buf[(x, y)].symbol(),
                            reference[(x, y)].symbol(),
                            "{first:?} then {second:?} at {x},{y}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_report_frame_draws_above_every_surface_below_the_quit_question() {
        for (set, make) in [
            (&ON_SESSION[..], session_app as fn() -> App),
            (&ON_HOME[..], home_app as fn() -> App),
        ] {
            for (first, second, app) in stacked(set, make) {
                if first != Surface::Report && second != Surface::Report {
                    continue;
                }
                // The hooks install offer owns the keys above the
                // report, so neither branch below reads it: the offer's
                // own topology tests cover that pair, including the
                // report drawing above it.
                if app.top_surface() == Surface::HooksOffer {
                    continue;
                }
                let text = crate::view::text(&rendered(&app));
                let title = app.import_report().map(|report| report.title().to_owned());
                let title = title.unwrap_or_default();
                if app.top_surface() == Surface::Report {
                    assert!(text.contains(&title), "{first:?} then {second:?}\n{text}");
                    assert!(!text.contains("leave them running"));
                } else {
                    assert!(text.contains("leave them running"), "{text}");
                }
            }
        }
    }

    #[test]
    fn the_report_frame_draws_above_the_hooks_offer() {
        for (set, make) in [
            (&ON_SESSION[..], session_app as fn() -> App),
            (&ON_HOME[..], home_app as fn() -> App),
        ] {
            for (first, second, app) in stacked(set, make) {
                let offer_pair = (first == Surface::HooksOffer && second == Surface::Report)
                    || (first == Surface::Report && second == Surface::HooksOffer);
                if !offer_pair {
                    continue;
                }
                // Both frames draw whole: the report's above the offer's.
                let text = crate::view::text(&rendered(&app));
                let title = app.import_report().map(|report| report.title().to_owned());
                let title = title.unwrap_or_default();
                assert!(text.contains(&title), "{first:?} then {second:?}\n{text}");
                assert!(
                    text.contains("Install hooks?"),
                    "{first:?} then {second:?}\n{text}"
                );
            }
        }
    }

    #[test]
    fn the_report_draws_above_a_swapped_view_on_home() {
        let mut app = home_app();
        for ch in "/keys".chars() {
            app.on_key(Key::Char(ch), now());
        }
        app.on_key(Key::Enter, now());
        assert!(app.keys_screen_open());
        open(&mut app, Surface::Report);
        assert_eq!(app.top_surface(), Surface::Report);
        let title = app
            .import_report()
            .map(|report| report.title().to_owned())
            .unwrap_or_default();
        assert!(!title.is_empty());
        let text = crate::view::text(&rendered(&app));
        assert!(text.contains(&title), "{text}");
        insta::assert_snapshot!("report_over_keys_screen_on_home", text);
        open(&mut app, Surface::Quit);
        let text = crate::view::text(&rendered(&app));
        assert!(text.contains("leave them running"), "{text}");
        insta::assert_snapshot!("quit_over_report_over_keys_screen_on_home", text);
    }

    #[test]
    fn the_wheel_over_the_notice_frame_leaves_the_conversation_and_beside_it_scrolls() {
        let mut app = long_session();
        open(&mut app, Surface::Notice);
        let slab = crate::view::frame_of(&app, Surface::Notice);
        assert!(!slab.is_empty());
        // Over the frame the wheel moves nothing, beneath rows included.
        app.on_wheel(&wheel(slab.x + 1, slab.y + 1, false));
        assert_eq!(app.top(), None);
        app.on_wheel(&wheel(
            slab.right().saturating_sub(1),
            slab.bottom() - 1,
            false,
        ));
        assert_eq!(app.top(), None);
        // One cell past each edge is the conversation's again.
        let beside = slab.right();
        assert!(beside < 80, "room beside the frame: {slab:?}");
        app.on_wheel(&wheel(beside, slab.y + 1, false));
        assert!(app.top().is_some(), "{slab:?}");
    }

    #[test]
    fn the_wheel_over_the_quit_frame_scrolls_nothing() {
        let mut app = long_session();
        open(&mut app, Surface::Quit);
        let slab = crate::view::frame_of(&app, Surface::Quit);
        assert!(!slab.is_empty());
        app.on_wheel(&wheel(slab.x + 1, slab.y + 1, false));
        assert_eq!(app.top(), None);
        let above = slab.y.checked_sub(1).expect("rows above the frame");
        app.on_wheel(&wheel(slab.x + 1, above, false));
        assert!(app.top().is_some(), "{slab:?}");
    }

    #[test]
    fn the_cross_closes_only_the_head() {
        use crate::mouse::TargetId;
        let mut app = long_session();
        open(&mut app, Surface::Notice);
        open(&mut app, Surface::Keymap);
        open(&mut app, Surface::Report);
        app.on_click(TargetId::CloseOverlay);
        let listed = app.top_surfaces();
        assert!(!listed.contains(&Surface::Report), "{listed:?}");
        assert!(listed.contains(&Surface::Keymap) && listed.contains(&Surface::Notice));
        app.on_click(TargetId::CloseOverlay);
        let listed = app.top_surfaces();
        assert!(!listed.contains(&Surface::Keymap), "{listed:?}");
        assert!(listed.contains(&Surface::Notice));
        app.on_click(TargetId::CloseOverlay);
        assert_eq!(app.top_surfaces(), vec![Surface::Draft]);
    }

    #[test]
    fn the_cross_closes_the_report_beneath_the_quit_question_and_keeps_the_question() {
        use crate::mouse::TargetId;
        let mut app = long_session();
        open(&mut app, Surface::Report);
        open(&mut app, Surface::Quit);
        app.on_click(TargetId::CloseOverlay);
        let listed = app.top_surfaces();
        assert!(!listed.contains(&Surface::Report), "{listed:?}");
        assert!(listed.contains(&Surface::Quit), "{listed:?}");
    }

    #[test]
    fn the_cross_closes_the_notice_overlay_before_the_results_and_the_search_panel_before_both() {
        use crate::mouse::TargetId;
        let mut app = long_session();
        open(&mut app, Surface::Results);
        open(&mut app, Surface::Notice);
        app.on_click(TargetId::CloseOverlay);
        let listed = app.top_surfaces();
        assert!(!listed.contains(&Surface::Notice), "{listed:?}");
        assert!(listed.contains(&Surface::Results));
        open(&mut app, Surface::Notice);
        open(&mut app, Surface::Search);
        app.on_click(TargetId::CloseOverlay);
        let listed = app.top_surfaces();
        assert!(!listed.contains(&Surface::Search), "{listed:?}");
        assert!(listed.contains(&Surface::Notice) && listed.contains(&Surface::Results));
    }

    #[test]
    fn a_surface_covering_the_conversation_takes_the_wheel_over_its_rows() {
        // Nothing open: the wheel over the rows scrolls the conversation.
        let mut app = long_session();
        app.on_wheel(&wheel(10, 5, false));
        assert!(app.top().is_some());
        for covering in [Surface::Keymap, Surface::Offer, Surface::Results] {
            let mut app = long_session();
            open(&mut app, covering);
            app.on_wheel(&wheel(10, 5, false));
            assert_eq!(app.top(), None, "{covering:?}");
        }
    }

    #[test]
    fn a_frame_not_open_has_no_slab() {
        let mut app = long_session();
        assert!(crate::view::frame_of(&app, Surface::Quit).is_empty());
        assert!(crate::view::frame_of(&app, Surface::Notice).is_empty());
        // Open, each frame has a slab, so the empty ones above are not a
        // frame that never gets found.
        open(&mut app, Surface::Quit);
        open(&mut app, Surface::Notice);
        assert!(!crate::view::frame_of(&app, Surface::Quit).is_empty());
        assert!(!crate::view::frame_of(&app, Surface::Notice).is_empty());
    }

    /// A home app on a short screen, so its list has rows past its window.
    fn short_home() -> App {
        let mut app = home_app();
        app.set_size(60, 12);
        app
    }

    #[test]
    fn a_home_question_or_picker_takes_the_wheel_over_the_list() {
        let mut app = short_home();
        app.on_wheel(&wheel(2, 5, true));
        assert!(
            app.home_list_scroll() > 0,
            "the list scrolls with nothing open"
        );
        for covering in [Surface::DeleteQuestion, Surface::WorkspacePicker] {
            let mut app = short_home();
            open(&mut app, covering);
            app.on_wheel(&wheel(2, 5, true));
            assert_eq!(app.home_list_scroll(), 0, "{covering:?}");
        }
    }

    #[test]
    fn the_wheel_off_the_quit_frame_on_home_still_scrolls_the_list() {
        let mut app = short_home();
        open(&mut app, Surface::Quit);
        let slab = crate::view::frame_of(&app, Surface::Quit);
        assert!(
            !slab.contains(ratatui::layout::Position::new(2, 5)),
            "{slab:?}"
        );
        app.on_wheel(&wheel(2, 5, true));
        assert!(app.home_list_scroll() > 0, "{slab:?}");
    }

    #[test]
    fn the_wheel_over_the_quit_or_notice_frame_on_home_moves_nothing() {
        for frame in [Surface::Quit, Surface::Notice] {
            let mut app = short_home();
            open(&mut app, frame);
            let slab = crate::view::frame_of(&app, frame);
            assert!(!slab.is_empty(), "{frame:?}");
            app.on_wheel(&wheel(slab.x + 1, slab.y + 1, true));
            assert_eq!(app.home_list_scroll(), 0, "{frame:?}");
        }
    }

    #[test]
    fn a_notice_overlay_on_home_leaves_the_list_rows_below_it_to_the_wheel() {
        // Thirteen rows put list rows under the notice rows' slab and
        // inside what a whole-screen slab would cover.
        let mut app = home_app();
        app.set_size(60, 13);
        open(&mut app, Surface::Notice);
        let slab = crate::view::frame_of(&app, Surface::Notice);
        assert!(
            slab.bottom() <= 6,
            "the slab sits in the notice rows: {slab:?}"
        );
        app.on_wheel(&wheel(30, 6, true));
        assert!(app.home_list_scroll() > 0);
    }

    #[test]
    fn a_swapped_view_on_home_gives_the_notice_frame_the_whole_screen() {
        for kind in ["keys", "settings", "picker"] {
            let mut app = short_home();
            app.set_configure(Some(
                std::sync::Arc::new(crate::configure_fake::Fake::new(vec![]))
                    as std::sync::Arc<dyn crate::Configure>,
            ));
            match kind {
                "picker" => {
                    app.on_key(Key::CtrlL, now());
                }
                _ => {
                    for ch in format!("/{kind}").chars() {
                        app.on_key(Key::Char(ch), now());
                    }
                    app.on_key(Key::Enter, now());
                }
            }
            assert!(
                app.keys_screen_open() || app.config_view_open() || app.model_picker_open(),
                "{kind}"
            );
            open(&mut app, Surface::Notice);
            let slab = crate::view::frame_of(&app, Surface::Notice);
            // Centred on the 12 rows, not squeezed into the notice rows.
            assert!(!slab.is_empty(), "{kind}");
            assert_eq!(slab.y, (12 - slab.height) / 2, "{kind}: {slab:?}");
        }
    }

    #[test]
    fn the_report_takes_the_wheel_over_the_quit_frame() {
        let mut app = session_app();
        open(&mut app, Surface::Report);
        open(&mut app, Surface::Quit);
        let slab = crate::view::frame_of(&app, Surface::Quit);
        app.on_wheel(&wheel(slab.x + 1, slab.y + 1, true));
        assert_eq!(app.import_report().map(|report| report.top()), Some(3));
    }
}

mod outside_click {
    use crate::Configure;
    use crate::app::config_views::ConfigView;
    use crate::app::top::tests::session_app;
    use crate::app::{App, Surface};
    use crate::configure::{LoginKind, LoginTarget, LogoutRow};
    use crate::configure_fake::Fake;
    use crate::keys::{Button, Key, Mouse, MouseKind};
    use crate::mouse::Pointer;
    use contract::clock::Clock;
    use ratatui::buffer::Buffer;
    use ratatui::layout::{Position, Rect};
    use std::sync::Arc;

    fn now() -> std::time::Instant {
        fakes::clock::FakeClock::new().now()
    }

    /// A session app with a conversation long enough to show rows, the
    /// `long_session` shape of `mod stacked`.
    fn selected_session() -> App {
        let mut app = session_app();
        let mut seq = 0;
        for at in 0..40 {
            for next in super::turn(&mut seq, &format!("a_m{at}"), "a one-line reply") {
                app.on_line(next);
            }
        }
        assert_eq!(app.top(), None, "following the bottom");
        app
    }

    /// The app drawn at its size: the buffer and the last frame's targets.
    fn drawn(app: &App) -> (Buffer, Vec<crate::mouse::Target>) {
        let (width, height) = app.screen_size();
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        let targets = crate::view::render(app, area, &mut buf, None);
        (buf, targets)
    }

    #[test]
    fn a_shown_selection_does_not_keep_the_completion_panel_open() {
        let mut app = selected_session();
        let (_, targets) = drawn(&app);
        app.on_select(
            &Mouse {
                kind: MouseKind::Press(Button::Left),
                col: 6,
                row: 3,
            },
            &targets,
        );
        app.on_select(
            &Mouse {
                kind: MouseKind::Drag(Button::Left),
                col: 14,
                row: 4,
            },
            &targets,
        );
        assert!(app.select.span().is_some(), "a selection shows");
        app.on_key(Key::Char('/'), now());
        assert!(app.select.span().is_some(), "typing keeps the selection");
        assert_eq!(app.top_surface(), Surface::Completion);
        let (width, height) = app.screen_size();
        let (_, targets) = drawn(&app);
        let slab = app
            .frames()
            .slab_of(Surface::Completion)
            .expect("the completion panel drew");
        let (col, row) = [
            (0, 0),
            (width - 1, 0),
            (0, height - 1),
            (width - 1, height - 1),
        ]
        .into_iter()
        .find(|&(col, row)| !slab.contains(Position::new(col, row)))
        .expect("a corner outside the panel");
        let mut pointer = Pointer::default();
        app.on_mouse(
            &Mouse {
                kind: MouseKind::Press(Button::Left),
                col,
                row,
            },
            &targets,
            &mut pointer,
            true,
            now(),
        );
        assert!(app.completions().is_none(), "the panel closed");
        assert!(app.select.span().is_none(), "the selection cleared");
    }

    /// A left press at (`col`, `row`) through the loop's mouse path,
    /// with a fresh pointer.
    fn press_at(
        app: &mut App,
        targets: &[crate::mouse::Target],
        col: u16,
        row: u16,
    ) -> crate::app::Effect {
        let mut pointer = Pointer::default();
        app.on_mouse(
            &Mouse {
                kind: MouseKind::Press(Button::Left),
                col,
                row,
            },
            targets,
            &mut pointer,
            true,
            now(),
        )
    }

    /// The first screen corner outside `slab`.
    #[track_caller]
    fn outside_corner(label: &str, slab: Rect, width: u16, height: u16) -> (u16, u16) {
        [
            (0, 0),
            (width - 1, 0),
            (0, height - 1),
            (width - 1, height - 1),
        ]
        .into_iter()
        .find(|&(col, row)| !slab.contains(Position::new(col, row)))
        .unwrap_or_else(|| panic!("{label}: no screen corner outside {slab:?}"))
    }

    /// Opens `/login` on its key panel: Enter on the key row leaves the
    /// rows for the panel step, which draws its own overlay.
    fn open_login_step(app: &mut App) {
        let fake = Fake::new(Vec::new());
        if let Ok(mut targets) = fake.targets.lock() {
            *targets = Ok(vec![LoginTarget {
                name: "acme".to_owned(),
                kind: LoginKind::Key,
            }]);
        }
        app.set_configure(Some(Arc::new(fake) as Arc<dyn Configure>));
        let _ = app.open_config_view(ConfigView::Login);
        assert!(
            app.top_surfaces().contains(&Surface::ConfigView),
            "/login opened"
        );
        app.on_key(Key::Enter, now());
    }

    /// Opens `/logout` on its delete confirm: Enter on the deletable
    /// row leaves the rows for the confirm, which draws its own overlay.
    fn open_logout_confirm(app: &mut App) {
        let fake = Fake::new(Vec::new());
        if let Ok(mut logout) = fake.logout.lock() {
            *logout = Ok(vec![LogoutRow::Provider {
                name: "acme".to_owned(),
                label: "default".to_owned(),
                default: true,
                deletable: true,
                origin: None,
                siblings: Vec::new(),
            }]);
        }
        app.set_configure(Some(Arc::new(fake) as Arc<dyn Configure>));
        let _ = app.open_config_view(ConfigView::Logout);
        assert!(
            app.top_surfaces().contains(&Surface::ConfigView),
            "/logout opened"
        );
        app.on_key(Key::Enter, now());
    }

    #[test]
    fn a_press_outside_the_login_step_steps_back_to_the_rows() {
        let mut app = session_app();
        open_login_step(&mut app);
        let (width, height) = app.screen_size();
        let (_, targets) = drawn(&app);
        let slab = app
            .frames()
            .slab_of(Surface::ConfigView)
            .expect("the login step drew");
        let (col, row) = outside_corner("login step", slab, width, height);
        // Esc on a twin steps the step back to the rows, leaving the
        // view open: still open proves the opener reached the step, and
        // the redrawn rows slab is what the press must reproduce.
        let mut twin = session_app();
        open_login_step(&mut twin);
        let esc = twin.on_key(Key::Esc, now());
        assert!(
            twin.top_surfaces().contains(&Surface::ConfigView),
            "esc steps the step back to the rows"
        );
        let (_, _) = drawn(&twin);
        let rows = twin
            .frames()
            .slab_of(Surface::ConfigView)
            .expect("the rows drew");
        let effect = press_at(&mut app, &targets, col, row);
        assert_eq!(effect, esc, "the press closes as Esc does");
        assert!(
            app.top_surfaces().contains(&Surface::ConfigView),
            "the view stays on the rows"
        );
        let (_, _) = drawn(&app);
        assert_eq!(
            app.frames().slab_of(Surface::ConfigView),
            Some(rows),
            "the press redrew the rows"
        );
        // One more Esc closes both, from the same rows.
        app.on_key(Key::Esc, now());
        twin.on_key(Key::Esc, now());
        assert!(
            !app.top_surfaces().contains(&Surface::ConfigView),
            "the view closed"
        );
        assert!(
            !twin.top_surfaces().contains(&Surface::ConfigView),
            "the twin closed"
        );
    }

    #[test]
    fn a_press_outside_the_logout_confirm_steps_back_to_the_rows() {
        let mut app = session_app();
        open_logout_confirm(&mut app);
        let (width, height) = app.screen_size();
        let (_, targets) = drawn(&app);
        let slab = app
            .frames()
            .slab_of(Surface::ConfigView)
            .expect("the logout confirm drew");
        let (col, row) = outside_corner("logout confirm", slab, width, height);
        // Esc on a twin steps the confirm back to the rows, leaving
        // the view open: still open proves the opener reached the
        // confirm, and the redrawn rows slab is what the press must
        // reproduce.
        let mut twin = session_app();
        open_logout_confirm(&mut twin);
        let esc = twin.on_key(Key::Esc, now());
        assert!(
            twin.top_surfaces().contains(&Surface::ConfigView),
            "esc steps the confirm back to the rows"
        );
        let (_, _) = drawn(&twin);
        let rows = twin
            .frames()
            .slab_of(Surface::ConfigView)
            .expect("the rows drew");
        let effect = press_at(&mut app, &targets, col, row);
        assert_eq!(effect, esc, "the press closes as Esc does");
        assert!(
            app.top_surfaces().contains(&Surface::ConfigView),
            "the view stays on the rows"
        );
        let (_, _) = drawn(&app);
        assert_eq!(
            app.frames().slab_of(Surface::ConfigView),
            Some(rows),
            "the press redrew the rows"
        );
        // One more Esc closes both, from the same rows.
        app.on_key(Key::Esc, now());
        twin.on_key(Key::Esc, now());
        assert!(
            !app.top_surfaces().contains(&Surface::ConfigView),
            "the view closed"
        );
        assert!(
            !twin.top_surfaces().contains(&Surface::ConfigView),
            "the twin closed"
        );
    }
}
