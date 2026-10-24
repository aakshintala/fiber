//! Tests for opening an image in the system viewer: the `read_file`
//! wire, the queued view, the notices, and forgetting across sessions.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::super::{App, Effect};
use crate::app::Target;
use crate::keys::Key;
use crate::link::Line;
use crate::mouse::{self, TargetId};

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const OTHER: &str = "s_bbbbbbbbbbbbbbbb";

/// An app attached to [`SESSION`] with the link up, 60 columns wide.
fn app() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 24);
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// Folds a turn whose prompt carried an image at `path`.
fn started_with_image(app: &mut App, path: &str) {
    app.on_line(Line::Session(contract::Envelope {
        kind: "turn_started".to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"input": [{
            "type": "message", "source": "driver",
            "content": [
                {"type": "text", "text": "look"},
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

/// An app with inline images on: home carries the flag and graphics
/// were detected, so the images size to the placeholder grid.
fn inline_app() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 24);
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        inline_images: true,
        ..Default::default()
    });
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.set_graphics();
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// The drawn targets naming image `id`: what `drawn` queues from.
fn drawn(id: u32) -> Vec<mouse::Target> {
    vec![mouse::Target {
        id: TargetId::Line(Target::Image(id)),
        rect: ratatui::layout::Rect::new(0, 0, 60, 12),
    }]
}

/// The command lines `effect` sends, panicking for any other effect.
fn sent(effect: Effect) -> Vec<String> {
    match effect {
        Effect::Send(lines) => lines,
        Effect::None
        | Effect::Quit
        | Effect::Exit(_)
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::Feedback { .. }
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::OpenFile(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_) => panic!("expected a send"),
    }
}

/// Clicks the first image and returns its `read_file` line's id.
fn click(app: &mut App) -> String {
    let lines = sent(app.view_image(1));
    assert_eq!(lines.len(), 1);
    lines[0].clone()
}

/// Answers `read_file` `id` with `result`.
fn accept(app: &mut App, id: &str, result: Value) -> Vec<String> {
    app.on_line(Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"command_id": id, "result": result})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }))
}

/// Rejects `read_file` `id` with `message`.
fn reject(app: &mut App, id: &str, message: &str) -> Vec<String> {
    app.on_line(Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"command_id": id, "code": "not_found", "message": message})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }))
}

#[test]
fn a_click_on_an_unknown_image_sends_nothing() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    assert_eq!(app.view_image(7), Effect::None);
}

#[test]
fn a_click_sends_read_file_with_the_logged_path() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let line = click(&mut app);
    let value: Value = serde_json::from_str(&line).unwrap_or_else(|err| panic!("a line: {err}"));
    assert_eq!(value.get("command"), Some(&json!("read_file")));
    // The path stays as logged, `artifacts/` prefix and all.
    assert_eq!(
        value.get("args"),
        Some(&json!({"session": SESSION, "path": "artifacts/shot.png"}))
    );
    assert!(value.get("session_id").is_none());
}

#[test]
fn a_second_click_while_one_is_out_does_nothing() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let _ = click(&mut app);
    assert_eq!(app.view_image(1), Effect::None);
}

#[test]
fn the_answer_queues_a_view_with_the_bytes() {
    use base64::Engine as _;
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let line = click(&mut app);
    let id = serde_json::from_str::<Value>(&line)
        .ok()
        .and_then(|line| line.get("id").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    let bytes = base64::engine::general_purpose::STANDARD.encode(b"png");
    assert!(
        accept(
            &mut app,
            &id,
            json!({"data": bytes, "mime_type": "image/png"})
        )
        .is_empty()
    );
    let mut out = app.take_image_out();
    assert_eq!(out.view.len(), 1);
    let view = out.view.pop().unwrap_or_else(|| panic!("a view"));
    assert_eq!(view.id, 1);
    assert_eq!(view.name, "shot.png");
    assert_eq!(view.session, SESSION);
    assert_eq!(view.data, bytes);
    assert_eq!(view.generation, app.images.generation);
}

#[test]
fn a_rejection_is_a_notice_and_nothing_retries() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let line = click(&mut app);
    let id = serde_json::from_str::<Value>(&line)
        .ok()
        .and_then(|line| line.get("id").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    assert!(reject(&mut app, &id, "no such file.").is_empty());
    assert_eq!(app.notice(), Some("Could not open shot.png: no such file."));
    // The refusal stands: a new click asks nothing again.
    assert_eq!(app.view_image(1), Effect::None);
}

#[test]
fn an_answer_without_data_is_a_notice() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let line = click(&mut app);
    let id = serde_json::from_str::<Value>(&line)
        .ok()
        .and_then(|line| line.get("id").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    assert!(accept(&mut app, &id, json!({"mime_type": "image/png"})).is_empty());
    assert_eq!(
        app.notice(),
        Some("Could not open shot.png: the answer carried no file.")
    );
    assert!(app.take_image_out().view.is_empty());
}

#[test]
fn a_click_with_the_link_down_sends_nothing() {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 24);
    app.attach(contract::SessionId(SESSION.to_owned()));
    started_with_image(&mut app, "artifacts/shot.png");
    assert_eq!(app.view_image(1), Effect::None);
}

#[test]
fn enter_on_the_focused_image_opens_it() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    app.focus = Some(TargetId::Line(Target::Image(1)));
    match app.focus_key(&Key::Enter) {
        Some(Effect::Send(lines)) => assert_eq!(lines.len(), 1),
        Some(
            Effect::None
            | Effect::Quit
            | Effect::Exit(_)
            | Effect::ListFiles
            | Effect::ReadImage(_)
            | Effect::Feedback { .. }
            | Effect::FindPause { .. }
            | Effect::Search { .. }
            | Effect::Editor { .. }
            | Effect::OpenFile(_)
            | Effect::Copy(_)
            | Effect::OpenLink(_),
        )
        | None => panic!("expected a send"),
    }
}

#[test]
fn y_on_an_image_copies_its_line() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    app.focus = Some(TargetId::Line(Target::Image(1)));
    assert_eq!(
        app.focus_key(&Key::Char('y')),
        Some(Effect::Copy("▣ shot.png · 1280×800".to_owned()))
    );
}

#[test]
fn a_session_change_forgets_the_viewer_fetch() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let line = click(&mut app);
    let id = serde_json::from_str::<Value>(&line)
        .ok()
        .and_then(|line| line.get("id").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    // Home, then another session: the answer arrives too late.
    app.go_home();
    app.attach(contract::SessionId(OTHER.to_owned()));
    assert!(
        accept(
            &mut app,
            &id,
            json!({"data": "eA==", "mime_type": "image/png"})
        )
        .is_empty()
    );
    assert!(app.take_image_out().view.is_empty());
    assert_eq!(app.notice(), None);
}

#[test]
fn a_stale_viewer_completion_is_dropped() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let generation = app.images.generation;
    app.attach(contract::SessionId(OTHER.to_owned()));
    app.image_viewed(1, "shot.png", generation, Err("boom".to_owned()));
    assert_eq!(app.notice(), None);
}

#[test]
fn a_failed_viewer_completion_refuses_the_image() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let generation = app.images.generation;
    app.image_viewed(1, "shot.png", generation, Err("boom".to_owned()));
    assert_eq!(app.notice(), Some("Could not open shot.png: boom."));
    assert_eq!(app.view_image(1), Effect::None);
}

#[test]
fn a_stale_viewer_failure_does_not_refuse() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let generation = app.images.generation;
    app.image_viewed(1, "shot.png", generation + 1, Err("boom".to_owned()));
    assert_eq!(app.notice(), None);
    assert!(matches!(app.view_image(1), Effect::Send(_)));
}

#[test]
fn disconnect_drops_the_fetch() {
    let mut app = app();
    started_with_image(&mut app, "artifacts/shot.png");
    let line = click(&mut app);
    let id = serde_json::from_str::<Value>(&line)
        .ok()
        .and_then(|line| line.get("id").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    app.disconnected();
    assert!(
        accept(
            &mut app,
            &id,
            json!({"data": "eA==", "mime_type": "image/png"})
        )
        .is_empty()
    );
    assert!(app.take_image_out().view.is_empty());
    // Only the dropped connection shows: the late answer changed nothing.
    assert_eq!(app.notice(), Some("Connection lost."));
}

/// A fetch blocks only its own `(session, path)`: the same path in
/// another segment, and another path in the same session, still ask.
/// Kills the `&&` to `||` mutant on the in-flight check, which would
/// refuse either half-match.
#[test]
fn a_fetch_blocks_only_its_own_session_and_path() {
    let mut app = app();
    ancestor_image(&mut app, "artifacts/a.png");
    started_with_image(&mut app, "artifacts/a.png");
    started_with_image(&mut app, "artifacts/b.png");
    // The own fetch for `a.png` is out: ids are per segment path, so
    // the ancestor's `a.png` is 1, the own run's `a.png` is 2 and its
    // `b.png` is 3.
    let _ = sent(app.view_image(2));
    // The ancestor's same path is another segment's fetch: it asks
    // under the ancestor's session.
    let lines = sent(app.view_image(1));
    let value: Value =
        serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("a line: {err}"));
    assert_eq!(
        value.get("args"),
        Some(&json!({"session": OTHER, "path": "artifacts/a.png"}))
    );
    // The own run's other path is another fetch: it asks too.
    let lines = sent(app.view_image(3));
    let value: Value =
        serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("a line: {err}"));
    assert_eq!(
        value.get("args"),
        Some(&json!({"session": SESSION, "path": "artifacts/b.png"}))
    );
    // While both are out, the same session and path asks nothing.
    assert_eq!(app.view_image(1), Effect::None);
    assert_eq!(app.view_image(2), Effect::None);
}

/// Folds an ancestor turn whose prompt carried an image at `path`.
/// The ancestor never streams live, so it applies straight to the
/// pages, as paged history would.
fn ancestor_image(app: &mut App, path: &str) {
    let envelope = contract::Envelope {
        kind: "turn_started".to_owned(),
        session_id: contract::SessionId(OTHER.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"input": [{
            "type": "message", "source": "driver",
            "content": [
                {"type": "text", "text": "look back"},
                {"type": "image", "path": path,
                    "mime_type": "image/png",
                    "width": 1280, "height": 800},
            ],
        }]})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    };
    app.attached_screen_mut().pages_mut().apply(&envelope);
}

#[test]
fn an_ancestor_image_read_runs_under_its_session() {
    let mut app = app();
    ancestor_image(&mut app, "artifacts/shot.png");
    started_with_image(&mut app, "artifacts/shot.png");
    // The ancestor's click reads under the ancestor's session, not the
    // attached one: the same path notes twice, the ancestor first.
    let lines = sent(app.view_image(1));
    assert_eq!(lines.len(), 1);
    let value: Value =
        serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("a line: {err}"));
    assert_eq!(value.get("command"), Some(&json!("read_file")));
    assert_eq!(
        value.get("args"),
        Some(&json!({"session": OTHER, "path": "artifacts/shot.png"}))
    );
}

#[test]
fn a_refusal_in_one_segment_still_asks_in_the_other() {
    let mut app = app();
    ancestor_image(&mut app, "artifacts/shot.png");
    started_with_image(&mut app, "artifacts/shot.png");
    let line = sent(app.view_image(1));
    let id = serde_json::from_str::<Value>(&line[0])
        .ok()
        .and_then(|line| line.get("id").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    assert!(reject(&mut app, &id, "no such file.").is_empty());
    assert_eq!(app.notice(), Some("Could not open shot.png: no such file."));
    // The ancestor's refusal stands, but the own segment's same path
    // still asks: the refusal lands per (session, path).
    assert_eq!(app.view_image(1), Effect::None);
    let lines = sent(app.view_image(2));
    let value: Value =
        serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("a line: {err}"));
    assert_eq!(
        value.get("args"),
        Some(&json!({"session": SESSION, "path": "artifacts/shot.png"}))
    );
}

#[test]
fn graphics_and_the_flag_size_images_inline() {
    let mut app = inline_app();
    started_with_image(&mut app, "artifacts/shot.png");
    assert_eq!(
        app.screen.pages().image_sizing(),
        crate::image::Sizing::Inline { cell: (10, 20) }
    );
}

#[test]
fn without_graphics_images_stay_their_one_line() {
    // The flag is on, but the probe never answered.
    let mut app = app();
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        inline_images: true,
        ..Default::default()
    });
    started_with_image(&mut app, "artifacts/shot.png");
    assert_eq!(
        app.screen.pages().image_sizing(),
        crate::image::Sizing::Text
    );
}

#[test]
fn without_the_flag_images_stay_their_one_line() {
    // Graphics were detected, but the flag is off: the default launch
    // carries it false.
    let mut app = app();
    app.set_graphics();
    started_with_image(&mut app, "artifacts/shot.png");
    assert_eq!(
        app.screen.pages().image_sizing(),
        crate::image::Sizing::Text
    );
}

#[test]
fn flat_stays_text() {
    let mut app = inline_app();
    started_with_image(&mut app, "artifacts/shot.png");
    app.attached_screen_mut().pages_mut().set_flat(true);
    app.refresh_image_sizing();
    assert_eq!(
        app.screen.pages().image_sizing(),
        crate::image::Sizing::Text
    );
}

#[test]
fn a_repeated_draw_asks_once() {
    let mut app = inline_app();
    started_with_image(&mut app, "artifacts/shot.png");
    let targets = drawn(1);
    // `Screen::draw_with` may draw twice: the hook still asks once.
    app.images_drawn(&targets);
    app.images_drawn(&targets);
    let out = app.take_image_out();
    assert_eq!(out.display.len(), 1);
    let value: Value =
        serde_json::from_str(&out.display[0]).unwrap_or_else(|err| panic!("a line: {err}"));
    assert_eq!(value.get("command"), Some(&json!("read_file")));
    assert_eq!(
        value.get("args"),
        Some(&json!({"session": SESSION, "path": "artifacts/shot.png"}))
    );
    // An unknown id draws nothing to ask for.
    app.images_drawn(&drawn(7));
    assert!(app.take_image_out().display.is_empty());
}

#[test]
fn a_draw_with_the_link_down_asks_nothing() {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 24);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        inline_images: true,
        ..Default::default()
    });
    app.set_graphics();
    started_with_image(&mut app, "artifacts/shot.png");
    // Inline sizing holds, but the link is down: the loop would drop
    // the command, so the draw queues nothing.
    assert_eq!(
        app.screen.pages().image_sizing(),
        crate::image::Sizing::Inline { cell: (10, 20) }
    );
    app.images_drawn(&drawn(1));
    assert!(app.take_image_out().display.is_empty());
}

#[test]
fn a_rejected_display_answer_leaves_the_line_with_no_notice() {
    let mut app = inline_app();
    started_with_image(&mut app, "artifacts/shot.png");
    let targets = drawn(1);
    app.images_drawn(&targets);
    let out = app.take_image_out();
    assert_eq!(out.display.len(), 1);
    let id = serde_json::from_str::<Value>(&out.display[0])
        .ok()
        .and_then(|line| line.get("id").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    // The rejection lands on the display fetch: no notice, and the one
    // line stays.
    assert!(reject(&mut app, &id, "no such file.").is_empty());
    assert_eq!(app.notice(), None);
    assert!(app.take_image_out().display.is_empty());
    // A failed image is not fetched again in this generation.
    app.images_drawn(&targets);
    assert!(app.take_image_out().display.is_empty());
}

/// Queues the display fetch for image `id` and returns its command id.
fn asked(app: &mut App, id: u32) -> String {
    app.images_drawn(&drawn(id));
    let out = app.take_image_out();
    assert_eq!(out.display.len(), 1);
    serde_json::from_str::<Value>(&out.display[0])
        .ok()
        .and_then(|line| line.get("id").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default()
}

/// The hub's accepted answer carrying `data` as `mime`.
fn accepted(id: &str, data: &str, mime: &str) -> Value {
    json!({"data": data, "mime_type": mime, "command_id": id})
}

#[test]
fn an_accepted_png_answer_transmits_and_places() {
    let mut app = inline_app();
    started_with_image(&mut app, "artifacts/shot.png");
    let id = asked(&mut app, 1);
    assert!(app.image_answered(&id, Some(&accepted(&id, "QUJD", "image/png"))));
    let out = app.take_image_out();
    let kitty = String::from_utf8(out.kitty).expect("escapes are text");
    // The transmit and the place post contiguously, over the grid the
    // answer sizes.
    let transmit = "\x1b_Ga=t,U=1,f=100,i=1,q=2,m=0;QUJD\x1b\\";
    assert!(kitty.starts_with(transmit), "{kitty:?}");
    // The place follows over the grid the answer sizes: a 56-column
    // conversation holds 22 columns, and the height follows at 6 rows.
    let place = "\x1b_Ga=p,U=1,i=1,c=22,r=6\x1b\\";
    let at = kitty.find(place).expect("the place follows the transmit");
    assert_eq!(&kitty[..at], transmit);
    assert_eq!(&kitty[at..], place);
    assert_eq!(app.notice(), None);
    // Shown: a redraw asks nothing more.
    app.images_drawn(&drawn(1));
    let out = app.take_image_out();
    assert!(out.display.is_empty());
    assert!(out.kitty.is_empty());
}

#[test]
fn a_second_answer_for_a_shown_image_posts_nothing() {
    let mut app = inline_app();
    started_with_image(&mut app, "artifacts/shot.png");
    let id = asked(&mut app, 1);
    let answer = accepted(&id, "QUJD", "image/png");
    assert!(app.image_answered(&id, Some(&answer)));
    assert!(!app.take_image_out().kitty.is_empty());
    // The duplicate answer draws nowhere.
    assert!(app.image_answered(&id, Some(&answer)));
    assert!(app.take_image_out().kitty.is_empty());
    assert_eq!(app.notice(), None);
    // Shown stands: a redraw asks nothing more.
    app.images_drawn(&drawn(1));
    let out = app.take_image_out();
    assert!(out.display.is_empty());
    assert!(out.kitty.is_empty());
}

#[test]
fn a_display_answer_without_png_bytes_fails_quietly() {
    for (name, result) in [
        ("missing", None),
        ("empty", Some(json!({"data": "", "mime_type": "image/png"}))),
        (
            "non-png",
            Some(json!({"data": "QUJD", "mime_type": "image/jpeg"})),
        ),
    ] {
        let mut app = inline_app();
        started_with_image(&mut app, "artifacts/shot.png");
        let id = asked(&mut app, 1);
        let result = result.clone();
        assert!(
            app.image_answered(&id, result.as_ref()),
            "{name}: the answer is ours"
        );
        assert_eq!(app.notice(), None, "{name}: no notice");
        let out = app.take_image_out();
        assert!(out.kitty.is_empty(), "{name}: nothing posts");
        // A failed image is not asked for again in this generation.
        app.images_drawn(&drawn(1));
        assert!(
            app.take_image_out().display.is_empty(),
            "{name}: no refetch"
        );
    }
}

#[test]
fn a_stale_generation_answer_posts_nothing() {
    let mut app = inline_app();
    started_with_image(&mut app, "artifacts/shot.png");
    let id = asked(&mut app, 1);
    let answer = accepted(&id, "QUJD", "image/png");
    app.images.generation = app.images.generation.saturating_add(1);
    assert!(app.image_answered(&id, Some(&answer)));
    assert!(app.take_image_out().kitty.is_empty());
    assert_eq!(app.notice(), None);
}

/// The display command asking for `path`.
fn command_for(lines: &[String], path: &str) -> String {
    lines
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|line| {
            line.get("args")
                .and_then(|args| args.get("path"))
                .and_then(Value::as_str)
                == Some(path)
        })
        .and_then(|line| line.get("id").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default()
}

#[test]
fn a_kitty_error_fails_only_its_image() {
    let mut app = inline_app();
    ancestor_image(&mut app, "artifacts/old.png");
    started_with_image(&mut app, "artifacts/shot.png");
    app.images_drawn(&[drawn(1), drawn(2)].concat());
    let out = app.take_image_out();
    assert_eq!(out.display.len(), 2);
    // The ancestor's image errors: its id is the one noting old.png.
    let failed = [1, 2]
        .into_iter()
        .find(|id| {
            app.screen
                .pages()
                .image(*id)
                .is_some_and(|part| part.path == "artifacts/old.png")
        })
        .expect("the ancestor's id");
    let kept = if failed == 1 { 2 } else { 1 };
    app.fail_display_image(failed);
    // Both answers land: the failed image's drops, the other's shows.
    let old = command_for(&out.display, "artifacts/old.png");
    let shot = command_for(&out.display, "artifacts/shot.png");
    assert!(app.image_answered(&old, Some(&accepted(&old, "QUJD", "image/png"))));
    assert!(app.image_answered(&shot, Some(&accepted(&shot, "QUJD", "image/png"))));
    let out = app.take_image_out();
    let kitty = String::from_utf8(out.kitty).expect("escapes are text");
    assert_eq!(
        kitty.matches("i=1,").count() + kitty.matches("i=2,").count(),
        2
    );
    assert!(
        kitty.contains(&format!("i={kept},")),
        "only the kept image posts: {kitty:?}"
    );
    assert_eq!(app.notice(), None);
}

#[test]
fn forgetting_a_shown_image_deletes_it() {
    let mut app = inline_app();
    started_with_image(&mut app, "artifacts/shot.png");
    let id = asked(&mut app, 1);
    assert!(app.image_answered(&id, Some(&accepted(&id, "QUJD", "image/png"))));
    assert!(!app.take_image_out().kitty.is_empty());
    // Attaching a new session deletes the shown image before its id
    // returns, and drops the old fetch.
    app.attach(contract::SessionId("s_bbbbbbbbbbbbbbbbbbbb".to_owned()));
    let out = app.take_image_out();
    assert_eq!(out.kitty, crate::graphics::delete(1));
}

#[test]
fn forgetting_without_a_shown_image_deletes_nothing() {
    let mut app = inline_app();
    started_with_image(&mut app, "artifacts/shot.png");
    asked(&mut app, 1);
    // Asked but never shown: nothing reached the terminal.
    app.attach(contract::SessionId("s_bbbbbbbbbbbbbbbbbbbb".to_owned()));
    assert!(app.take_image_out().kitty.is_empty());
}

#[test]
fn a_non_png_draw_asks_for_no_bytes() {
    let mut app = inline_app();
    app.on_line(Line::Session(contract::Envelope {
        kind: "turn_started".to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"input": [{
            "type": "message", "source": "driver",
            "content": [
                {"type": "image", "path": "artifacts/shot.jpg",
                    "mime_type": "image/jpeg",
                    "width": 1280, "height": 800},
            ],
        }]})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }));
    app.images_drawn(&drawn(1));
    assert!(app.take_image_out().display.is_empty());
}
