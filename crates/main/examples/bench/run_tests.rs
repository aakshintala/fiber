use std::ffi::OsStr;
use std::path::Path;

use super::{command, parse_line};

const PROXIES: [&str; 8] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

#[test]
fn a_child_gets_a_cleared_environment_with_only_the_named_variables() {
    let command = command(
        Path::new("/r/fiber"),
        Path::new("/r"),
        Path::new("/r/h"),
        Some(OsStr::new("/usr/bin")),
    );
    let mut set: Vec<(String, String)> = command
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.unwrap().to_string_lossy().into_owned(),
            )
        })
        .collect();
    set.sort();
    assert_eq!(
        set,
        [
            ("FIBER_HOME", "/r/h"),
            ("FIBER_TEST_FAKE_KEY", "sk-bench"),
            ("HOME", "/r"),
            ("PATH", "/usr/bin"),
        ]
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
    );
    for proxy in PROXIES {
        assert!(
            set.iter().all(|(key, _)| key != proxy),
            "{proxy} reached the child"
        );
    }
    // The inherited environment is cleared, so a proxy variable the
    // harness runs under is not passed on either.
    assert!(
        format!("{command:?}").contains(" env -i "),
        "the environment is not cleared: {command:?}"
    );
}

#[test]
fn a_stdout_line_that_is_not_json_is_an_error() {
    assert!(
        parse_line("panicked at main.rs")
            .unwrap_err()
            .contains("not JSON")
    );
    assert_eq!(
        parse_line(r#"{"kind":"session_started"}"#).unwrap()["kind"],
        "session_started"
    );
}
