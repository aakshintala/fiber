use super::{
    PAGE_BYTES, RESULT_BYTES, TOOL_CALLS, TRIGGER_TOKENS, Turn, busy_turn_samples, count,
    fdatasync_calls, filler, log_lines, page, retained_anon_kib, turn_bytes,
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
fn the_busy_turn_reports_its_peak_its_retained_and_its_log() {
    let lines = log_lines(LOG).unwrap();
    let samples = busy_turn_samples(20480, 19000, &lines, 1_520_000).unwrap();
    let value = |id: &str| {
        samples
            .iter()
            .find(|(metric, _)| *metric == id)
            .unwrap_or_else(|| panic!("{id} is not sampled"))
            .1
            .clone()
    };
    assert_eq!(value("busy_turn_rss_kib"), serde_json::json!(20480));
    assert_eq!(value("busy_turn_retained_kib"), serde_json::json!(19000));
    // A different retained reading is a different sample.
    assert_ne!(
        samples,
        busy_turn_samples(20480, 19001, &lines, 1_520_000).unwrap()
    );
    let log = value("turn_log_bytes");
    assert_eq!(
        log.get("bytes"),
        Some(&serde_json::json!(turn_bytes(&lines).unwrap()))
    );
    assert_eq!(log.get("content"), Some(&serde_json::json!(1_520_000)));
    assert_eq!(log.get("tool_calls"), Some(&serde_json::json!(1)));
}

#[test]
fn a_busy_turn_without_a_whole_turn_has_no_samples() {
    assert!(busy_turn_samples(20480, 19000, &[], 1_520_000).is_err());
}

const STATUS: &str = "Name:\tfiber\nState:\tS (sleeping)\nVmHWM:\t   10840 kB\nVmRSS:\t   10812 kB\nRssAnon:\t    4100 kB\nRssFile:\t    4800 kB\n";

/// The settle waits `sleeps` records, and the order `events` saw.
#[derive(Default)]
struct RetainedRecorder {
    sleeps: Vec<std::time::Duration>,
    events: Vec<&'static str>,
}

#[test]
fn the_retained_reading_waits_the_settle_then_reads_rss_anon() {
    let recorder = std::cell::RefCell::new(RetainedRecorder::default());
    let anon = retained_anon_kib(
        || {
            recorder.borrow_mut().events.push("read");
            Ok(STATUS.to_owned())
        },
        |wait| {
            let mut recorder = recorder.borrow_mut();
            recorder.sleeps.push(wait);
            recorder.events.push("sleep");
        },
    );
    assert_eq!(anon, Ok(4100));
    let recorder = recorder.borrow();
    assert_eq!(recorder.sleeps, [std::time::Duration::from_secs(10)]);
    // The read follows the wait, so the settled memory is read.
    assert_eq!(recorder.events, vec!["sleep", "read"]);
}

#[test]
fn a_retained_reading_without_rss_anon_is_an_error_not_zero() {
    let without_anon = STATUS.replace("RssAnon:\t    4100 kB\n", "");
    let mut sleeps = 0;
    let anon = retained_anon_kib(
        || Ok(without_anon.clone()),
        |_| {
            sleeps += 1;
        },
    );
    let err = anon.unwrap_err();
    assert!(err.contains("RssAnon"), "{err}");
    assert_eq!(sleeps, 1);
}

#[test]
fn a_process_gone_during_the_settle_wait_fails_the_reading() {
    let mut sleeps = 0;
    let anon = retained_anon_kib(
        || Err("reading /proc/4101/status: no such file".to_owned()),
        |_| {
            sleeps += 1;
        },
    );
    assert!(anon.is_err());
    // The wait still ran: the process lived through the settle.
    assert_eq!(sleeps, 1);
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
