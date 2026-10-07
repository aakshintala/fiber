//! Tests for `!` commands.

use super::{answered, item, parse};
use contract::shapes::Process;

/// A process that exited with `code`.
fn exited(code: i32) -> Process {
    Process {
        exit_code: Some(code),
        signal: None,
        timed_out: false,
    }
}

#[test]
fn one_bang_sends_and_two_show_only() {
    assert_eq!(parse("!ls"), Some(("ls", true)));
    assert_eq!(parse("!!ls"), Some(("ls", false)));
    assert_eq!(parse("  !! git status  "), Some(("git status", false)));
    assert_eq!(parse("! ls -l"), Some(("ls -l", true)));
    assert_eq!(parse("!!!x"), Some(("!x", false)));
}

#[test]
fn a_bang_with_no_command_or_no_bang_is_a_prompt() {
    assert_eq!(parse("!"), None);
    assert_eq!(parse("!!"), None);
    assert_eq!(parse("!!  "), None);
    assert_eq!(parse(" ! "), None);
    assert_eq!(parse("ls !"), None);
    assert_eq!(parse(""), None);
}

#[test]
fn an_item_is_the_command_then_its_output() {
    assert_eq!(item("ls", "a\nb\n", &exited(0)), "! ls\na\nb");
    assert_eq!(item("true", "", &exited(0)), "! true");
}

#[test]
fn an_item_ends_with_a_nonzero_exit_or_a_signal() {
    assert_eq!(item("false", "", &exited(1)), "! false\nexit 1");
    assert_eq!(item("x", "out", &exited(-1)), "! x\nout\nexit -1");
    let killed = Process {
        exit_code: None,
        signal: Some("SIGKILL".to_owned()),
        timed_out: true,
    };
    assert_eq!(item("sleep 9", "", &killed), "! sleep 9\nsignal SIGKILL");
}

#[test]
fn only_a_show_only_answer_with_its_output_is_an_item() {
    let result = || {
        serde_json::from_value(serde_json::json!({"output": "out",
            "process": {"exit_code": 0, "timed_out": false}}))
        .ok()
    };
    assert_eq!(answered("!!ls", result()), Some("! ls\nout".to_owned()));
    assert_eq!(answered("!ls", result()), None);
    assert_eq!(answered("ls", result()), None);
    assert_eq!(answered("!!ls", None), None);
    let other = serde_json::from_value(serde_json::json!({"tools": []})).ok();
    assert_eq!(answered("!!ls", other), None);
}
