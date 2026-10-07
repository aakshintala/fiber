use super::{
    PAGE_BYTES, RESULT_BYTES, TOOL_CALLS, TRIGGER_TOKENS, Turn, count, fdatasync_calls, filler,
    log_lines, page, turn_bytes,
};

#[test]
fn fdatasync_calls_are_counted_once_each_and_nothing_else_is() {
    let strace = "\
4101  fdatasync(3)                    = 0
4102  fdatasync(3 <unfinished ...>
4101  write(1, \"fdatasync(\", 10)    = 10
4102  <... fdatasync resumed>)        = 0
4103  fsync(4)                        = 0
4101  --- SIGCHLD {si_signo=SIGCHLD} ---
4101  +++ exited with 0 +++
fdatasync(5)                          = 0
";
    assert_eq!(fdatasync_calls(strace), Ok(3));
}

#[test]
fn no_fdatasync_call_is_an_error_not_zero() {
    let err = fdatasync_calls("4101  +++ exited with 0 +++\n").unwrap_err();
    assert!(err.contains("no fdatasync"), "{err}");
    assert!(fdatasync_calls("").is_err());
}

const LOG: &str = concat!(
    r#"{"kind":"session_started","seq":0}"#,
    "\n",
    r#"{"kind":"turn_started","seq":1}"#,
    "\n",
    r#"{"kind":"tool_call_started","seq":2}"#,
    "\n",
    r#"{"kind":"turn_completed","seq":3}"#,
    "\n",
    r#"{"kind":"turn_started","seq":4}"#,
    "\n",
    r#"{"kind":"turn_completed","seq":5}"#,
    "\n",
);

#[test]
fn the_turns_bytes_run_from_turn_started_through_turn_completed() {
    let lines = log_lines(LOG).unwrap();
    let expected = [
        r#"{"kind":"turn_started","seq":1}"#,
        r#"{"kind":"tool_call_started","seq":2}"#,
        r#"{"kind":"turn_completed","seq":3}"#,
    ]
    .iter()
    .map(|line| line.len() + 1)
    .sum::<usize>();
    assert_eq!(turn_bytes(&lines), Ok(expected));
}

#[test]
fn a_log_without_a_whole_turn_has_no_turn_bytes() {
    let started = log_lines(&LOG[..LOG.find(r#"{"kind":"turn_completed""#).unwrap()]).unwrap();
    assert!(
        turn_bytes(&started)
            .unwrap_err()
            .contains("no turn_completed")
    );
    let none = log_lines(&LOG[..LOG.find(r#"{"kind":"turn_started""#).unwrap()]).unwrap();
    assert!(turn_bytes(&none).unwrap_err().contains("no turn_started"));
    assert!(log_lines("{\"kind\":\n").is_err());
}

#[test]
fn lines_are_counted_by_kind_and_payload() {
    let log = concat!(
        r#"{"kind":"tool_call_completed","payload":{"status":"completed"}}"#,
        "\n",
        r#"{"kind":"tool_call_completed","payload":{"status":"failed"}}"#,
        "\n",
        r#"{"kind":"tool_call_started","payload":{"status":"completed"}}"#,
        "\n",
    );
    let lines = log_lines(log).unwrap();
    assert_eq!(
        count(&lines, "tool_call_completed", "status", "completed"),
        1
    );
    assert_eq!(count(&lines, "tool_call_completed", "", ""), 2);
    assert_eq!(count(&lines, "turn_completed", "", ""), 0);
}

#[test]
fn the_busy_turn_fills_the_context_to_five_percent_short_of_the_trigger() {
    let turn = Turn::new();
    assert_eq!(turn.files.len(), TOOL_CALLS);
    assert_eq!(turn.script.len(), TOOL_CALLS + 1);
    const { assert!(RESULT_BYTES < 16 * 1024) };
    let results: usize = turn.files.iter().map(|(_, body)| body.len()).sum();
    assert!(results <= TRIGGER_TOKENS * 4 * 95 / 100, "{results}");
    assert!(results > TRIGGER_TOKENS * 4 * 94 / 100, "{results}");
    // Short of the trigger with every argument and the prompt counted too.
    assert!(turn.content < TRIGGER_TOKENS * 4, "{}", turn.content);
    let arguments = r#"{"path":"f000.txt"}"#.len() * TOOL_CALLS;
    assert_eq!(
        turn.content,
        "read each file".len() + arguments + results + "every file is read".len()
    );
}

#[test]
fn filler_is_exact_plain_and_repeatable() {
    let text = filler(3543, 7);
    assert_eq!(text.len(), 3543);
    assert!(text.bytes().all(|b| b.is_ascii_lowercase() || b == b' '));
    assert_eq!(text, filler(3543, 7));
    assert_ne!(text, filler(3543, 8));
}

#[test]
fn the_page_is_exactly_the_download_cap_and_repeatable() {
    let html = page(PAGE_BYTES);
    assert_eq!(html.len(), 10_485_760);
    let text = String::from_utf8(html).unwrap();
    for part in [
        "<script>",
        "&amp;",
        "<table>",
        "<ul>",
        "data-note=\"",
        "<div><div><div>",
    ] {
        assert!(text.contains(part), "{part}");
    }
    assert!(text.ends_with("</p></body></html>"));
    assert_eq!(page(4096), page(4096));
    assert_eq!(page(4096).len(), 4096);
}
