use super::{
    HANDOFF_NOTE, PAGE_BYTES, RESULT_BYTES, RETAINED_SETTLE, TEN_TURNS, TOOL_CALLS, TRIGGER_TOKENS,
    Turn, busy_turn_samples, count, fdatasync_calls, filler, log_lines, page, retained_anon_kib,
    ten_turn_samples, ten_turn_script, turn_bytes,
};
use crate::run::PROBE;

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

#[test]
fn the_ten_turn_run_samples_the_first_and_the_tenth() {
    assert_eq!(TEN_TURNS, 10);
    let samples = ten_turn_samples(19000, 19005);
    assert_eq!(samples.len(), 1);
    let (id, value) = &samples[0];
    assert_eq!(*id, "ten_busy_turns_retained_kib");
    assert_eq!(value.get("first"), Some(&serde_json::json!(19000)));
    assert_eq!(value.get("last"), Some(&serde_json::json!(19005)));
    // A different reading on either end is a different sample.
    assert_ne!(samples, ten_turn_samples(19001, 19005));
    assert_ne!(samples, ten_turn_samples(19000, 19006));
    // Ten turns of the busy turn's shape: ten prompts, each with its
    // calls, one closing reply and one handoff note.
    assert_eq!(TEN_TURNS * (TOOL_CALLS + 2), 4310);
    assert_eq!(TEN_TURNS * TOOL_CALLS, 4290);
    // Each prompt turn and each handoff turn ends its own turn.
    assert_eq!(2 * TEN_TURNS, 20);
}

#[test]
fn the_ten_turn_script_serves_each_turn_then_its_handoff_note() {
    // Each handoff note sits after its turn's closing reply: exactly one
    // script entry per turn holds the note, and the script serves every
    // turn's replies in order.
    let turn = Turn::new();
    let script = ten_turn_script(&turn);
    assert_eq!(script.len(), TEN_TURNS * (TOOL_CALLS + 2));
    let noted = script
        .iter()
        .filter(|reply| String::from_utf8_lossy(&reply.body).contains(HANDOFF_NOTE))
        .count();
    assert_eq!(noted, TEN_TURNS);
}

const STATUS: &str = "Name:\tfiber\nState:\tS (sleeping)\nVmHWM:\t   10840 kB\nVmRSS:\t   10812 kB\nRssAnon:\t    4100 kB\nRssFile:\t    4800 kB\n";

/// A `/proc/<pid>/status` text with `anon` KiB of `RssAnon`.
fn status_with(anon: u64) -> String {
    STATUS.replace("4100", &anon.to_string())
}

/// Runs `retained_anon_kib` over `script`'s readings: the result, the
/// readings made and the total slept. The sleeps are recorded, never
/// taken, so no test reads or sleeps on the real clock.
fn run_retained(
    script: Vec<Result<String, String>>,
) -> (Result<u64, String>, usize, std::time::Duration) {
    let readings = std::cell::Cell::new(0);
    let script = std::cell::RefCell::new(script.into_iter());
    let mut slept = std::time::Duration::ZERO;
    let got = retained_anon_kib(
        || {
            readings.set(readings.get() + 1);
            script
                .borrow_mut()
                .next()
                .unwrap_or(Err("past the script".to_owned()))
        },
        |wait| {
            slept += wait;
        },
    );
    (got, readings.get(), slept)
}

#[test]
fn the_retained_reading_returns_once_rss_anon_stops_falling() {
    let script = [5000, 4000, 4000, 1000]
        .map(status_with)
        .into_iter()
        .map(Ok)
        .collect();
    let (got, readings, slept) = run_retained(script);
    assert_eq!(got, Ok(4000));
    // The third reading is flat against the second, so the fourth never runs.
    assert_eq!(readings, 3);
    assert_eq!(slept, 2 * PROBE);
}

#[test]
fn a_rising_rss_anon_is_settled_too() {
    let script = [4000, 4100].map(status_with).into_iter().map(Ok).collect();
    let (got, readings, slept) = run_retained(script);
    // Rising is not falling: the wait ends at the second reading.
    assert_eq!(got, Ok(4100));
    assert_eq!(readings, 2);
    assert_eq!(slept, PROBE);
}

#[test]
fn a_falling_rss_anon_is_read_through_the_bound() {
    let polls =
        usize::try_from(RETAINED_SETTLE.as_millis() / PROBE.as_millis()).unwrap_or(usize::MAX);
    // Every reading falls, so the wait runs the whole bound and returns the last.
    let script = (0..=polls)
        .rev()
        .map(|n| Ok(status_with(4000 + u64::try_from(n).unwrap_or(u64::MAX))))
        .collect();
    let (got, readings, slept) = run_retained(script);
    assert_eq!(got, Ok(4000));
    assert_eq!(readings, polls + 1);
    // The pass never waits longer than the fixed settle did.
    assert_eq!(slept, RETAINED_SETTLE);
}

#[test]
fn a_retained_reading_without_rss_anon_is_an_error_not_zero() {
    let without_anon = STATUS.replace("RssAnon:\t    4100 kB\n", "");
    let (got, readings, slept) = run_retained(vec![Ok(without_anon)]);
    let err = got.unwrap_err();
    assert!(err.contains("RssAnon"), "{err}");
    // No wait ran: the first reading already failed.
    assert_eq!(readings, 1);
    assert_eq!(slept, std::time::Duration::ZERO);
}

#[test]
fn a_process_gone_before_the_first_reading_fails_the_reading() {
    let (got, readings, slept) = run_retained(vec![Err(
        "reading /proc/4101/status: no such file".to_owned()
    )]);
    assert!(got.is_err());
    assert_eq!(readings, 1);
    // No wait ran: the process was already gone.
    assert_eq!(slept, std::time::Duration::ZERO);
}

#[test]
fn a_process_gone_during_the_quiescence_wait_fails_the_reading() {
    let script = vec![
        Ok(status_with(5000)),
        Err("reading /proc/4101/status: no such file".to_owned()),
    ];
    let (got, readings, slept) = run_retained(script);
    assert!(got.is_err());
    assert_eq!(readings, 2);
    assert_eq!(slept, PROBE);
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
