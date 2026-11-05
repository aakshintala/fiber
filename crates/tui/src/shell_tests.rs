//! Tests for `!` commands.

use super::{answered, error_row, item, parse, ran, row};
use contract::events::{CommandResult, ShellCommand};
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
    assert_eq!(
        answered("!!ls", result()),
        Some("! ls\nonly you see this\nout\nexit 0".to_owned())
    );
    assert_eq!(answered("!ls", result()), None);
    assert_eq!(answered("ls", result()), None);
    assert_eq!(answered("!!ls", None), None);
    let other = serde_json::from_value(serde_json::json!({"tools": []})).ok();
    assert_eq!(answered("!!ls", other), None);
}

#[test]
fn a_row_names_the_command_the_note_the_output_and_the_exit() {
    assert_eq!(
        row("echo hi", "sent with your next prompt", "hi\n", &exited(0)),
        "! echo hi\nsent with your next prompt\nhi\nexit 0"
    );
    assert_eq!(
        row("echo hi", "only you see this", "hi\n", &exited(0)),
        "! echo hi\nonly you see this\nhi\nexit 0"
    );
}

#[test]
fn a_row_keeps_a_nonzero_exit_or_a_signal() {
    assert_eq!(
        row("false", "sent with your next prompt", "", &exited(1)),
        "! false\nsent with your next prompt\nexit 1"
    );
    let killed = Process {
        exit_code: None,
        signal: Some("SIGKILL".to_owned()),
        timed_out: true,
    };
    assert_eq!(
        row("sleep 9", "only you see this", "", &killed),
        "! sleep 9\nonly you see this\nsignal SIGKILL"
    );
}

#[test]
fn a_row_with_no_exit_or_signal_names_no_ending() {
    let unknown = Process {
        exit_code: None,
        signal: None,
        timed_out: false,
    };
    assert_eq!(
        row("ls", "only you see this", "a\n", &unknown),
        "! ls\nonly you see this\na"
    );
}

#[test]
fn a_row_with_empty_output_is_the_command_the_note_and_the_exit() {
    assert_eq!(
        row("true", "sent with your next prompt", "", &exited(0)),
        "! true\nsent with your next prompt\nexit 0"
    );
}

#[test]
fn an_error_row_shows_what_arrived_verbatim() {
    assert_eq!(error_row("ls", "busy", "no"), "! ls\nbusy: no");
}

#[test]
fn a_shell_command_line_is_a_sent_row() {
    let line = ShellCommand {
        command: "echo hi".to_owned(),
        output: "hi\n".to_owned(),
        artifact: None,
        process: exited(0),
    };
    assert_eq!(
        ran(&line),
        "! echo hi\nsent with your next prompt\nhi\nexit 0"
    );
}

#[test]
fn a_stop_answer_is_never_a_shell_item() {
    let stopped = Some(CommandResult::Stop {
        sessions_stopped: 2,
    });
    assert_eq!(answered("!!ls", stopped), None);
}
