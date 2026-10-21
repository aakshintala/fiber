//! Tests for refusing an unknown `/name` (#3013): once the `/` list
//! holds the session's latest answer, a name it does not hold is refused
//! with a notice and never sent; wherever the list may lack a name the
//! session runs, and for a path, the draft goes out as written.

use super::commands_testkit::{
    SESSION, answer_commands, attached, connected, enter, reloaded, sent, session_line, turn_starts,
};
use crate::app::{App, Effect};
use serde_json::{Value, json};

/// What the notice says for `/hepl`.
const HEPL: &str = "Unknown command /hepl. Type / to see the list.";

/// An app attached to [`SESSION`] whose `/` list holds the session's
/// answer: the skill `tdd`.
fn answered() -> App {
    let mut app = attached();
    answer_commands(&mut app, &["tdd"]);
    app
}

/// Every notice showing, each as its code and text.
fn notice_texts(app: &App) -> Vec<String> {
    app.notices()
        .iter()
        .map(|line| match line {
            crate::app::NoticeLine::Notice { code, text, .. } => format!("{code} {text}"),
            crate::app::NoticeLine::More(n) => format!("+{n} more"),
        })
        .collect()
}

/// A turn start's `skills_changed` on [`SESSION`], adding `fresh`.
fn skills_changed() -> crate::link::Line {
    session_line(
        "skills_changed",
        json!({"added": [{"name": "fresh", "description": "Runs fresh.",
            "path": "/w/.fiber/skills/fresh/SKILL.md", "source": "project"}],
            "removed": []}),
    )
}

/// Answers the `commands` question in `asked`, the lines a session line
/// returned, with `rows`.
fn answer(app: &mut App, asked: &[String], rows: Value) {
    assert_eq!(asked.len(), 1);
    let asked: Value = serde_json::from_str(&asked[0]).unwrap_or_default();
    assert_eq!(asked["command"], "commands");
    let answer = session_line(
        "command_accepted",
        json!({"command_id": asked["id"], "result": {"commands": rows}}),
    );
    assert!(app.on_line(answer).is_empty());
}

/// The text of the one prompt `effect` sends.
fn prompt_text(effect: Effect) -> String {
    let lines = sent(effect);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "prompt");
    lines[0]["args"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn an_unknown_command_is_refused_with_a_notice_and_keeps_the_draft() {
    let mut app = answered();
    assert_eq!(enter(&mut app, "/hepl"), Effect::None);
    assert_eq!(app.draft(), "/hepl");
    assert_eq!(notice_texts(&app), [format!("unknown_command {HEPL}")]);
}

#[test]
fn an_unknown_command_with_text_is_refused_during_a_turn_too() {
    let mut app = answered();
    turn_starts(&mut app);
    assert_eq!(enter(&mut app, "/hepl me out"), Effect::None);
    assert_eq!(app.draft(), "/hepl me out");
    assert_eq!(app.notice(), Some(HEPL));
}

#[test]
fn rewind_is_refused_while_the_terminal_does_not_draw_it() {
    let mut app = answered();
    assert_eq!(enter(&mut app, "/rewind"), Effect::None);
    assert_eq!(
        app.notice(),
        Some("Unknown command /rewind. Type / to see the list.")
    );
}

#[test]
fn a_skill_in_the_answer_goes_out_as_typed() {
    let mut app = answered();
    assert_eq!(
        prompt_text(enter(&mut app, "/tdd the parser")),
        "/tdd the parser"
    );
    assert_eq!(app.notice(), None);
}

#[test]
fn a_path_is_no_command_and_goes_out_as_written() {
    let mut app = answered();
    assert_eq!(
        prompt_text(enter(&mut app, "/tmp/x is full")),
        "/tmp/x is full"
    );
    assert_eq!(app.notice(), None);
}

#[test]
fn a_bare_slash_is_no_command_and_goes_out_as_written() {
    let mut app = answered();
    assert_eq!(prompt_text(enter(&mut app, "/ hello")), "/ hello");
    assert_eq!(app.notice(), None);
}

#[test]
fn before_the_answer_an_unknown_command_goes_out_as_written() {
    let mut app = attached();
    assert_eq!(prompt_text(enter(&mut app, "/hepl")), "/hepl");
    // A `reloaded` asks again: until that answer lands, the list is
    // not known.
    let mut app = answered();
    assert_eq!(app.on_line(reloaded(SESSION)).len(), 1);
    assert_eq!(prompt_text(enter(&mut app, "/hepl")), "/hepl");
    assert_eq!(app.notice(), None);
}

#[test]
fn on_home_an_unknown_command_starts_a_session_with_it() {
    let mut app = connected();
    let lines = sent(enter(&mut app, "/hepl"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "start");
    // Leaving an answered session for home forgets its list.
    let mut app = answered();
    assert_eq!(enter(&mut app, "/home"), Effect::None);
    let lines = sent(enter(&mut app, "/hepl"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "start");
}

#[test]
fn after_a_skills_change_an_unlisted_name_goes_out_until_the_next_answer() {
    let mut app = answered();
    assert!(app.on_line(skills_changed()).is_empty());
    // The session runs `fresh` from this turn start on, while its
    // `commands` answer stays as it was: the session decides.
    assert_eq!(prompt_text(enter(&mut app, "/fresh")), "/fresh");
    assert_eq!(app.notice(), None);
    // A reload answers again from the reloaded skills.
    answer_commands(&mut app, &["tdd", "fresh"]);
    assert_eq!(enter(&mut app, "/hepl"), Effect::None);
    assert_eq!(app.notice(), Some(HEPL));
}

#[test]
fn a_reloads_skills_change_keeps_the_question_it_follows() {
    let mut app = answered();
    let asked = app.on_line(reloaded(SESSION));
    assert!(app.on_line(skills_changed()).is_empty());
    answer(
        &mut app,
        &asked,
        json!([{"name": "fresh", "description": "Runs fresh.", "tag": "skill"}]),
    );
    assert_eq!(prompt_text(enter(&mut app, "/fresh now")), "/fresh now");
    assert_eq!(enter(&mut app, "/hepl"), Effect::None);
    assert_eq!(app.notice(), Some(HEPL));
}

#[test]
fn extensions_loaded_asks_again_and_its_answer_runs_the_new_command() {
    let mut app = answered();
    let asked = app.on_line(session_line(
        "extensions_loaded",
        json!({"extensions": [{"name": "acme", "version": "1.0.0"}]}),
    ));
    answer(
        &mut app,
        &asked,
        json!([{"name": "deploy", "description": "Deploys.", "tag": "acme",
            "extension": true}]),
    );
    let lines = sent(enter(&mut app, "/deploy now"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "command");
    assert_eq!(lines[0]["args"], json!({"name": "deploy", "text": "now"}));
}

#[test]
fn in_a_delegates_view_an_unlisted_name_steers_the_delegate() {
    use crate::app::items::testkit::{DELEGATE_A, home, opened, opened_item, start_delegate};
    let mut app = home();
    opened(&mut app);
    answer_commands(&mut app, &["tdd"]);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.draft.set("/hepl me");
    let lines = sent(app.on_enter());
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "steer");
    assert_eq!(lines[0]["session_id"], DELEGATE_A);
    assert_eq!(
        lines[0]["args"],
        json!({"content": [{"type": "text", "text": "/hepl me"}]})
    );
    assert_eq!(app.notice(), None);
}
