//! Tests for which destinations open and which program opens them
//! (`docs/tui.md`, "Links").

use std::ffi::OsString;

use super::{MAX_URL, command, valid};

/// No environment variable set.
fn no_env(_: &str) -> Option<OsString> {
    None
}

#[test]
fn valid_names_its_schemes() {
    for url in [
        "http://example.com",
        "https://example.com/a?b=c#d",
        "mailto:someone@example.com",
        "HTTP://EXAMPLE.COM",
        "HTTPS://example.com",
        "MAILTO:someone@example.com",
        "HtTp://example.com",
    ] {
        assert!(valid(url), "{url:?}");
    }
    for url in [
        "ftp://example.com",
        "file:///etc/hosts",
        "javascript:alert(1)",
        "example.com",
        "/relative/path",
        "relative",
        "",
        "http//example.com",
        "://example.com",
    ] {
        assert!(!valid(url), "{url:?}");
    }
}

#[test]
fn valid_refuses_whitespace_and_controls() {
    for url in [
        "http://example.com/a b",
        "http://example.com/\tleading",
        "http://example.com\n",
        "mailto:some one@example.com",
        "http://example.com/\u{7f}",
        "http://example.com/\u{0}",
    ] {
        assert!(!valid(url), "{url:?}");
    }
}

#[test]
fn valid_caps_the_length_at_2048_bytes() {
    let base = "http://example.com/";
    let exact = format!("{base}{}", "a".repeat(MAX_URL - base.len()));
    assert_eq!(exact.len(), MAX_URL);
    assert!(valid(&exact));
    let over = format!("{exact}a");
    assert_eq!(over.len(), MAX_URL + 1);
    assert!(!valid(&over));
}

#[test]
fn the_opener_prefers_open_then_xdg_open() {
    assert_eq!(command(no_env, |_| true), Some(vec!["open"]));
    assert_eq!(
        command(no_env, |name| name == "xdg-open"),
        Some(vec!["xdg-open"])
    );
    assert_eq!(command(no_env, |_| false), None);
}

#[test]
fn a_session_over_ssh_opens_nothing() {
    for var in ["SSH_CONNECTION", "SSH_TTY"] {
        let env = |name: &str| (name == var).then(|| OsString::from("x"));
        assert_eq!(command(env, |_| true), None, "{var}");
    }
    let other = |name: &str| (name == "TERM").then(|| OsString::from("x"));
    assert_eq!(command(other, |_| true), Some(vec!["open"]));
}
