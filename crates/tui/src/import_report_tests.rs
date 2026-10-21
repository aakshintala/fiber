//! The accepted-import report overlay (`docs/tui.md`, "Notices",
//! "Overlays"): one scrollable overlay per accepted import, no notices.
//!
//! The ticket's screen test: a 120-line import answer opens one overlay
//! with both counts and no notice rows; scrolled to the bottom the last
//! row and the footer show; Esc closes it with no notices left behind.

use ratatui::backend::TestBackend;
use serde_json::{Value, json};

use crate::app::commands::commands_testkit::{attached, enter, now, sent, session_line};
use crate::app::{App, Effect};
use crate::keys::Key;

/// The 120-line import answer: 60 copied names across the 4 kinds and
/// 60 left-behind items, each one row in the report.
fn big_result() -> Value {
    let names = |prefix: &str| -> Vec<Value> {
        (1..=15)
            .map(|n| Value::String(format!("{prefix}-{n:02}")))
            .collect()
    };
    let left: Vec<Value> = (1..=60)
        .map(|n| {
            json!({"item": format!("left-{n:02}.json"), "reason": "credentials are never imported"})
        })
        .collect();
    json!({
        "imported": {
            "mcp_servers": names("server"),
            "skills": names("skill"),
            "commands": names("command"),
            "instructions": names("AGENTS.md"),
        },
        "left_behind": left,
    })
}

/// `app` painted whole on an 80x24 in-memory screen.
fn painted(app: &mut App) -> crate::screen::Screen<TestBackend> {
    let vars = [("COLORTERM".to_owned(), "truecolor".to_owned())];
    let (look, _) = crate::look::Look::new(crate::ThemeSetting::Follow, &|name| {
        vars.iter()
            .find(|(set, _)| set == name)
            .map(|(_, value)| value.clone())
    });
    let mut screen =
        crate::screen::Screen::new(TestBackend::new(80, 24), 80, 24).expect("a screen");
    screen.set_look(look);
    screen
        .draw_with(app, None, crate::view::render)
        .expect("a draw");
    screen
}

/// A painted screen's text, one row per line.
fn painted_text(screen: &crate::screen::Screen<TestBackend>) -> String {
    let buf = screen.backend().buffer();
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn an_accepted_import_with_120_lines_opens_one_report_and_adds_no_notices() {
    let mut app = attached();
    app.set_size(80, 24);
    let lines = sent(enter(&mut app, "/import codex"));
    let id = lines[0]["id"].clone();
    let accepted = session_line(
        "command_accepted",
        json!({"command_id": id, "result": big_result()}),
    );
    assert!(app.on_line(accepted).is_empty());
    // One overlay with both counts, and no notice rows.
    let text = painted_text(&painted(&mut app));
    assert!(
        text.contains("Imported from Codex"),
        "the report names the source:\n{text}"
    );
    assert!(
        text.contains("Copied · 60"),
        "the copied section counts its items:\n{text}"
    );
    assert!(
        text.contains("Left behind · 60"),
        "the left-behind section counts its items:\n{text}"
    );
    assert!(
        app.notices().is_empty(),
        "an accepted import adds no notices"
    );
    insta::assert_snapshot!("import_report_120_lines", text);
    // Scrolled to the bottom the last row and the footer show.
    for _ in 0..30 {
        assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    }
    let bottom = painted_text(&painted(&mut app));
    assert!(
        bottom.contains("left-60.json"),
        "the last left-behind row shows at the bottom:\n{bottom}"
    );
    assert!(
        bottom.contains("Imported items apply on the next /reload · Esc closes"),
        "the footer stays visible at the bottom:\n{bottom}"
    );
    // Esc closes the overlay and leaves no notices behind.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(
        app.notices().is_empty(),
        "closing the report leaves no notices"
    );
    let closed = painted_text(&painted(&mut app));
    assert!(
        !closed.contains("Imported from Codex"),
        "Esc closes the report:\n{closed}"
    );
}
