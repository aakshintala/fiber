//! Tests for OSC 52 and the clipboard command pick.

use super::{command, osc52};
use std::ffi::OsString;

#[test]
fn osc52_frames_the_base64_text() {
    assert_eq!(osc52("hi"), b"\x1b]52;c;aGk=\x07");
    assert_eq!(osc52(""), b"\x1b]52;c;\x07");
    assert_eq!(osc52("é\n"), b"\x1b]52;c;w6kK\x07");
}

/// No environment variable set.
fn no_env(_: &str) -> Option<OsString> {
    None
}

#[test]
fn the_first_command_found_is_picked() {
    assert_eq!(command(no_env, |_| true), Some(vec!["pbcopy"]));
    assert_eq!(
        command(no_env, |name| name == "wl-copy" || name == "xsel"),
        Some(vec!["wl-copy"])
    );
    assert_eq!(
        command(no_env, |name| name == "xclip"),
        Some(vec!["xclip", "-selection", "clipboard"])
    );
    assert_eq!(
        command(no_env, |name| name == "xsel"),
        Some(vec!["xsel", "--clipboard", "--input"])
    );
    assert_eq!(command(no_env, |_| false), None);
}

#[test]
fn a_session_over_ssh_runs_no_command() {
    for var in ["SSH_CONNECTION", "SSH_TTY"] {
        let env = |name: &str| (name == var).then(|| OsString::from("x"));
        assert_eq!(command(env, |_| true), None, "{var}");
    }
    let other = |name: &str| (name == "TERM").then(|| OsString::from("x"));
    assert_eq!(command(other, |_| true), Some(vec!["pbcopy"]));
}

#[test]
fn on_path_finds_a_program_in_a_path_directory() {
    assert!(super::on_path("sh"));
    assert!(!super::on_path("fiber-no-such-clipboard-program"));
}
