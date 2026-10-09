//! Tests for typing into a running `tty` job's view: the encoding table,
//! the boundaries against finished, plain and delegate views, and Esc,
//! Ctrl+C and a down link falling through as today.

use contract::clock::Clock;
use serde_json::{Value, json};

use super::super::super::{App, Effect};
use super::super::testkit::{
    DELEGATE_A, SESSION, commands, complete, home, job_started, mark_tty, opened, opened_item,
    session_line, start_delegate, start_job,
};
use super::{encode, encode_edit, is_tty};
use crate::keys::{Edit, Key};

/// Opens a running `tty` job's view, acknowledged.
fn open_tty(app: &mut App, job: &str) {
    mark_tty(app);
    job_started(app, job, "run the editor", 0, "a_1");
    opened_item(app, job);
    assert!(app.item_open());
}

/// The `job_input` lines an effect sends.
fn job_inputs(effect: Effect) -> Vec<Value> {
    match effect {
        Effect::Send(lines) => commands(lines)
            .into_iter()
            .filter(|line| line["command"] == "job_input")
            .collect(),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_) => Vec::new(),
    }
}

#[test]
fn every_key_encodes_to_the_bytes_the_job_reads() {
    let rows: Vec<(Key, &str)> = vec![
        (Key::Char('a'), "a"),
        // Multi-byte input goes as its UTF-8.
        (Key::Char('é'), "é"),
        (Key::Enter, "\r"),
        (Key::Backspace, "\x7f"),
        (Key::Tab, "\t"),
        (Key::Up, "\x1b[A"),
        (Key::Down, "\x1b[B"),
        (Key::End, "\x1b[F"),
        (Key::PageUp, "\x1b[5~"),
        (Key::PageDown, "\x1b[6~"),
        (Key::CtrlO, "\x0f"),
        (Key::CtrlG, "\x07"),
        (Key::CtrlR, "\x12"),
        (Key::CtrlF, "\x06"),
        (Key::CtrlV, "\x16"),
        (Key::CtrlL, "\x0c"),
        (Key::BackTab, "\x1b[Z"),
        (Key::F1, "\x1bOP"),
        (Key::AltA, "\x1ba"),
        (Key::AltUp, "\x1b[1;3A"),
        (Key::AltDown, "\x1b[1;3B"),
        (Key::AltX, "\x1bx"),
        (Key::AltP, "\x1bp"),
        (Key::AltR, "\x1br"),
        (Key::AltDigit(3), "\x1b3"),
    ];
    for (key, expected) in rows {
        assert_eq!(encode(&key).as_deref(), Some(expected), "{key:?}");
    }
}

#[test]
fn only_esc_and_ctrl_c_send_nothing() {
    for key in [Key::Esc, Key::CtrlC] {
        assert_eq!(encode(&key), None, "{key:?}");
    }
}

#[test]
fn every_edit_encodes_to_the_bytes_the_job_reads() {
    let rows: Vec<(Edit, &str)> = vec![
        (Edit::Left, "\x1b[D"),
        (Edit::Right, "\x1b[C"),
        (Edit::WordLeft, "\x1b[1;5D"),
        (Edit::WordRight, "\x1b[1;5C"),
        (Edit::LineStart, "\x1b[H"),
        (Edit::LineEnd, "\x1b[F"),
        (Edit::Delete, "\x1b[3~"),
        (Edit::ShiftEnter, "\r"),
        (Edit::CtrlJ, "\n"),
        (Edit::DeleteWord, "\x1b\x7f"),
        (Edit::Paste("a\nb".to_owned()), "a\rb"),
    ];
    for (edit, expected) in rows {
        assert_eq!(encode_edit(&edit).as_str(), expected, "{edit:?}");
    }
}

#[test]
fn a_tty_job_is_the_one_holding_a_grid() {
    let mut app = home();
    opened(&mut app);
    mark_tty(&mut app);
    job_started(&mut app, "j_9", "run the editor", 0, "a_1");
    start_job(&mut app, "j_2");
    let jobs = &app.items.jobs;
    assert!(is_tty(&jobs[&contract::JobId("j_9".to_owned())]));
    assert!(!is_tty(&jobs[&contract::JobId("j_2".to_owned())]));
}

#[test]
fn typing_in_a_running_tty_job_view_sends_job_input_to_the_parent() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    open_tty(&mut app, "j_9");
    let lines = job_inputs(app.on_key(Key::Char('a'), clock.now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "job_input");
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(lines[0]["args"], json!({"job_id": "j_9", "text": "a"}));
    assert!(
        lines[0]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("c_")),
        "{}",
        lines[0]
    );
    let enter = job_inputs(app.on_key(Key::Enter, clock.now()));
    assert_eq!(enter.len(), 1);
    assert_eq!(enter[0]["args"], json!({"job_id": "j_9", "text": "\r"}));
}

#[test]
fn a_finished_tty_job_falls_through() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    open_tty(&mut app, "j_9");
    complete(&mut app, "j_9");
    assert!(job_inputs(app.on_key(Key::Char('a'), clock.now())).is_empty());
}

#[test]
fn a_plain_job_view_keeps_its_notice() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_job(&mut app, "j_1");
    opened_item(&mut app, "j_1");
    app.draft.set("do it");
    assert_eq!(app.on_key(Key::Enter, clock.now()), Effect::None);
    assert_eq!(app.draft(), "do it");
    assert_eq!(app.notice(), Some("A job takes no input here."));
}

#[test]
fn a_delegate_view_keeps_its_draft() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    assert!(job_inputs(app.on_key(Key::Char('x'), clock.now())).is_empty());
    assert_eq!(app.draft(), "x");
}

#[test]
fn esc_closes_the_tty_view_without_sending() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    open_tty(&mut app, "j_9");
    assert_eq!(app.on_key(Key::Esc, clock.now()), Effect::None);
    assert!(!app.item_open());
}

#[test]
fn ctrl_c_keeps_its_quit_gesture_in_a_tty_view() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    open_tty(&mut app, "j_9");
    assert!(app.draft.is_empty());
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert!(app.item_open());
}

#[test]
fn a_down_link_sends_nothing_in_a_tty_view() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    open_tty(&mut app, "j_9");
    app.connect_failed("down".to_owned());
    assert!(job_inputs(app.on_key(Key::Char('a'), clock.now())).is_empty());
    assert!(app.item_open());
}

#[test]
fn edits_in_a_running_tty_job_view_send_job_input_and_leave_the_draft() {
    let mut app = home();
    opened(&mut app);
    open_tty(&mut app, "j_9");
    let left = job_inputs(app.on_edit(Edit::Left));
    assert_eq!(left.len(), 1);
    assert_eq!(left[0]["args"], json!({"job_id": "j_9", "text": "\x1b[D"}));
    let pasted = job_inputs(app.on_edit(Edit::Paste("a\nb".to_owned())));
    assert_eq!(pasted.len(), 1);
    assert_eq!(pasted[0]["args"], json!({"job_id": "j_9", "text": "a\rb"}));
    assert!(app.draft.is_empty());
    assert!(app.item_open());
}

#[test]
fn edits_fall_through_to_the_draft_outside_a_running_tty_job() {
    let mut app = home();
    opened(&mut app);
    // A finished `tty` job: the edit moves in the draft, not the job.
    open_tty(&mut app, "j_9");
    complete(&mut app, "j_9");
    app.draft.set("ab");
    assert!(job_inputs(app.on_edit(Edit::Left)).is_empty());
    assert_eq!(app.draft(), "ab");
}

#[test]
fn edits_in_a_plain_job_view_edit_the_draft() {
    let mut app = home();
    opened(&mut app);
    start_job(&mut app, "j_1");
    opened_item(&mut app, "j_1");
    assert!(job_inputs(app.on_edit(Edit::Paste("xy".to_owned()))).is_empty());
    assert_eq!(app.draft(), "xy");
}

#[test]
fn edits_in_a_delegate_view_edit_the_draft() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    assert!(job_inputs(app.on_edit(Edit::Paste("xy".to_owned()))).is_empty());
    assert_eq!(app.draft(), "xy");
}

#[test]
fn edits_with_the_link_down_edit_the_draft() {
    let mut app = home();
    opened(&mut app);
    open_tty(&mut app, "j_9");
    app.connect_failed("down".to_owned());
    assert!(job_inputs(app.on_edit(Edit::Paste("xy".to_owned()))).is_empty());
    assert_eq!(app.draft(), "xy");
}

/// An approval request on the attached session, opening the approval panel.
fn approval(app: &mut App) {
    app.on_line(session_line(
        "permission_requested",
        json!({"request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "standing_ask",
            "standing_rule": {"scope": "global", "prefix": "p"}}),
    ));
}

#[test]
fn edits_with_the_approval_panel_open_reach_the_panel() {
    let mut app = home();
    opened(&mut app);
    open_tty(&mut app, "j_9");
    approval(&mut app);
    assert!(app.panel().is_some());
    assert!(job_inputs(app.on_edit(Edit::Left)).is_empty());
    assert!(job_inputs(app.on_edit(Edit::Paste("xy".to_owned()))).is_empty());
    // The paste went to the panel's feedback, not the draft or the job.
    assert!(app.draft.is_empty());
    assert!(app.panel().is_some());
}

#[test]
fn keys_with_the_approval_panel_open_do_not_send() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    open_tty(&mut app, "j_9");
    approval(&mut app);
    assert!(app.panel().is_some());
    // End falls through the panel to the screen behind: without the
    // guard it would type into the job.
    assert!(job_inputs(app.on_key(Key::End, clock.now())).is_empty());
    assert!(app.panel().is_some());
}

#[test]
fn rail_keys_in_a_running_tty_job_view_send_job_input() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    open_tty(&mut app, "j_9");
    let alt_r = job_inputs(app.on_key(Key::AltR, clock.now()));
    assert_eq!(alt_r.len(), 1);
    assert_eq!(alt_r[0]["args"], json!({"job_id": "j_9", "text": "\x1br"}));
    let alt_3 = job_inputs(app.on_key(Key::AltDigit(3), clock.now()));
    assert_eq!(alt_3.len(), 1);
    assert_eq!(alt_3[0]["args"], json!({"job_id": "j_9", "text": "\x1b3"}));
}

#[test]
fn rail_keys_fall_through_to_the_rail_outside_a_running_tty_job() {
    let clock = fakes::clock::FakeClock::new();
    // A finished `tty` job, a plain job and a delegate: the rail keeps
    // Alt+R and Alt+3 as today, sending nothing.
    for open in ["tty", "plain", "delegate"] {
        let mut app = home();
        opened(&mut app);
        match open {
            "tty" => {
                open_tty(&mut app, "j_9");
                complete(&mut app, "j_9");
            }
            "plain" => {
                start_job(&mut app, "j_1");
                opened_item(&mut app, "j_1");
            }
            _ => {
                start_delegate(&mut app, "j_1", DELEGATE_A);
                opened_item(&mut app, "j_1");
            }
        }
        assert_eq!(app.on_key(Key::AltR, clock.now()), Effect::None, "{open}");
        assert_eq!(
            app.on_key(Key::AltDigit(3), clock.now()),
            Effect::None,
            "{open}"
        );
    }
}

#[test]
fn rail_keys_with_the_approval_panel_open_do_not_send() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    open_tty(&mut app, "j_9");
    approval(&mut app);
    assert!(app.panel().is_some());
    assert_eq!(app.on_key(Key::AltR, clock.now()), Effect::None);
    assert_eq!(app.on_key(Key::AltDigit(3), clock.now()), Effect::None);
    assert!(app.panel().is_some());
}
