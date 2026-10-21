//! Tests for refusing an unknown `/name` (#3013): once the `/` list
//! holds the session's answer, a name it does not hold is refused with a
//! notice and never sent; until then, and for a path, the draft goes out
//! as written.

use super::commands_testkit::{
    SESSION, answer_commands, attached, connected, enter, reloaded, sent, turn_starts,
};
use crate::app::{App, Effect};

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
