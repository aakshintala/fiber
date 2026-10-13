use std::ffi::OsStr;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;

use serde_json::{Value, json};

use super::{Figures, command, max_rss_kib, report, samples};
use crate::run::Finished;

/// A report as the jig prints it, after its `session:` line.
const REPORT: &str = "session: scale 1, 4.70 MiB, at 160x48\n\
    lines: 8074\n\
    turns: 10\n\
    calls: 1051\n\
    pages: 120\n\
    rows: 2915\n\
    open pass and first frame: 308.04 ms\n\
    slowest frame that loaded pages: 1.30 ms, of 6 paging up\n\
    slowest jump frame: 2.61 ms, of 20 to row 2769\n\
    slowest re-count at a new width: 24.12 ms\n\
    slowest append frame: 1.85 ms, of 481; pages while appending: 120 to 124, most resident 9\n";

/// GNU time's `-v` report, after the jig's own stderr.
const TIME: &str = "a line the jig wrote\n\
    \tCommand being timed: \"paging 1 160 48\"\n\
    \tUser time (seconds): 0.31\n\
    \tMaximum resident set size (kbytes): 10612\n\
    \tExit status: 0\n";

const LABELS: [&str; 10] = [
    "lines: ",
    "turns: ",
    "calls: ",
    "pages: ",
    "rows: ",
    "open pass and first frame: ",
    "slowest frame that loaded pages: ",
    "slowest jump frame: ",
    "slowest re-count at a new width: ",
    "slowest append frame: ",
];

fn finished(code: i32, stdout: &str, stderr: &str) -> Finished {
    Finished {
        status: ExitStatus::from_raw(code << 8),
        stdout: stdout.to_owned(),
        stderr: stderr.to_owned(),
    }
}

/// A report as the new jig prints it: the absent search line sits
/// between the re-count and the append, in measurement order.
const NEW_REPORT: &str = "session: scale 1, 4.70 MiB, at 160x48\n\
    lines: 8074\n\
    turns: 10\n\
    calls: 1051\n\
    pages: 120\n\
    rows: 2915\n\
    open pass and first frame: 308.04 ms\n\
    slowest frame that loaded pages: 1.30 ms, of 6 paging up\n\
    slowest jump frame: 2.61 ms, of 20 to row 2769\n\
    slowest re-count at a new width: 24.12 ms\n\
    search of the whole log with no match: 5.43 ms, 0 matches\n\
    slowest append frame: 1.85 ms, of 481; pages while appending: 120 to 124, most resident 9\n";

/// The scale-16 report of a paired run: its own sizes and timings.
const REPORT_16: &str = "session: scale 16, 75.20 MiB, at 160x48\n\
    lines: 129184\n\
    turns: 160\n\
    calls: 16816\n\
    pages: 1920\n\
    rows: 46640\n\
    open pass and first frame: 4928.64 ms\n\
    slowest frame that loaded pages: 20.80 ms, of 6 paging up\n\
    slowest jump frame: 41.76 ms, of 20 to row 44304\n\
    slowest re-count at a new width: 385.92 ms\n\
    search of the whole log with no match: 86.88 ms, 0 matches\n\
    slowest append frame: 29.60 ms, of 481; pages while appending: 1920 to 1924, most resident 9\n";

/// GNU time's `-v` report for the scale-16 child: its own peak.
const TIME_16: &str = "a line the jig wrote\n\
    \tCommand being timed: \"paging 16 160 48\"\n\
    \tUser time (seconds): 4.96\n\
    \tMaximum resident set size (kbytes): 169872\n\
    \tExit status: 0\n";

#[test]
fn each_figure_is_read_after_its_label() {
    assert_eq!(
        report(REPORT),
        Ok(Figures {
            open_ms: 308.04,
            load_ms: 1.30,
            jump_ms: 2.61,
            width_ms: 24.12,
            append_ms: 1.85,
            search_ms: None,
            lines: 8074,
            turns: 10,
            calls: 1051,
            pages: 120,
            rows: 2915,
        })
    );
    // The new line parses with the rest, in measurement order.
    let figures = report(NEW_REPORT).expect("the new report parses");
    assert_eq!(figures.search_ms, Some(5.43));
    assert_eq!(figures.width_ms, 24.12);
    assert_eq!(figures.append_ms, 1.85);
}

#[test]
fn a_missing_label_is_an_error_naming_it() {
    for label in LABELS {
        let without: String = REPORT
            .lines()
            .filter(|line| !line.starts_with(label))
            .map(|line| format!("{line}\n"))
            .collect();
        let err = report(&without).unwrap_err();
        assert!(err.contains(label.trim_end()), "{label}: {err}");
    }
}

#[test]
fn a_missing_absent_line_is_a_legacy_report() {
    // Only the new line may be missing: every original label stays
    // required, so a legacy report still validates whole.
    let figures = report(REPORT).expect("a legacy report parses");
    assert_eq!(figures.search_ms, None);
}

#[test]
fn a_present_but_malformed_absent_line_is_an_error() {
    for bad in [
        "search of the whole log with no match: x ms, 0 matches\n",
        "search of the whole log with no match: NaN ms, 0 matches\n",
        "search of the whole log with no match: -1.00 ms, 0 matches\n",
        "search of the whole log with no match: 5.43\n",
        "search of the whole log with no match: 5.43 ms\n",
        "search of the whole log with no match: 5.43 ms, 1 matches\n",
        "search of the whole log with no match: 5.43 ms, 0 match\n",
    ] {
        let text = NEW_REPORT.replace(
            "search of the whole log with no match: 5.43 ms, 0 matches\n",
            bad,
        );
        let err = report(&text).unwrap_err();
        assert!(err.contains("with no match"), "{bad}: {err}");
    }
}

#[test]
fn a_figure_that_is_not_a_finite_number_is_an_error() {
    for bad in ["x ms", "NaN ms", "inf ms", "-1.00 ms", "ms", "1.00"] {
        let text = REPORT.replace("308.04 ms", bad);
        let err = report(&text).unwrap_err();
        assert!(err.contains("open pass and first frame"), "{bad}: {err}");
    }
    for bad in ["many", "-3", ""] {
        let text = REPORT.replace("pages: 120\n", &format!("pages: {bad}\n"));
        let err = report(&text).unwrap_err();
        assert!(err.contains("pages"), "{bad}: {err}");
    }
}

#[test]
fn the_peak_is_gnu_times_maximum_resident_set() {
    assert_eq!(max_rss_kib(TIME), Ok(10612));
    let none = TIME.replace("\tMaximum resident set size (kbytes): 10612\n", "");
    assert!(max_rss_kib(&none).unwrap_err().contains("Maximum resident"));
    let zero = TIME.replace("(kbytes): 10612", "(kbytes): 0");
    assert!(max_rss_kib(&zero).unwrap_err().contains("Maximum resident"));
    let bad = TIME.replace("(kbytes): 10612", "(kbytes): lots");
    assert!(max_rss_kib(&bad).is_err());
}

#[test]
fn a_legacy_pair_records_six_budget_metrics_and_the_counts() {
    // A consistent legacy pair keeps the existing scale-1 samples and
    // omits every additive metric.
    let recorded: Vec<(&str, Value)> = samples(
        &finished(0, REPORT, TIME),
        &finished(0, REPORT_16_LEGACY, TIME_16),
    )
    .unwrap();
    assert_eq!(
        recorded,
        vec![
            ("paging_rss_kib", json!(10612)),
            ("paging_open_ms", json!(308.04)),
            ("paging_load_ms", json!(1.30)),
            ("paging_jump_ms", json!(2.61)),
            ("paging_width_ms", json!(24.12)),
            ("paging_append_ms", json!(1.85)),
            (
                "paging_counts",
                json!({"lines": 8074, "turns": 10, "calls": 1051, "pages": 120, "rows": 2915})
            ),
        ]
    );
}

#[test]
fn a_new_pair_records_both_scales_in_order() {
    let recorded: Vec<(&str, Value)> = samples(
        &finished(0, NEW_REPORT, TIME),
        &finished(0, REPORT_16, TIME_16),
    )
    .unwrap();
    assert_eq!(
        recorded,
        vec![
            ("paging_rss_kib", json!(10612)),
            ("paging_open_ms", json!(308.04)),
            ("paging_load_ms", json!(1.30)),
            ("paging_jump_ms", json!(2.61)),
            ("paging_width_ms", json!(24.12)),
            ("paging_append_ms", json!(1.85)),
            (
                "paging_counts",
                json!({"lines": 8074, "turns": 10, "calls": 1051, "pages": 120, "rows": 2915})
            ),
            ("paging_search_ms", json!(5.43)),
            ("paging_scale16_width_ms", json!(385.92)),
            ("paging_scale16_search_ms", json!(86.88)),
            (
                "paging_scale16_counts",
                json!({"lines": 129184, "turns": 160, "calls": 16816, "pages": 1920, "rows": 46640})
            ),
        ]
    );
}

/// The scale-16 report of a legacy pair: original labels only.
const REPORT_16_LEGACY: &str = "session: scale 16, 75.20 MiB, at 160x48\n\
    lines: 129184\n\
    turns: 160\n\
    calls: 16816\n\
    pages: 1920\n\
    rows: 46640\n\
    open pass and first frame: 4928.64 ms\n\
    slowest frame that loaded pages: 20.80 ms, of 6 paging up\n\
    slowest jump frame: 41.76 ms, of 20 to row 44304\n\
    slowest re-count at a new width: 385.92 ms\n\
    slowest append frame: 29.60 ms, of 481; pages while appending: 1920 to 1924, most resident 9\n";

#[test]
fn a_mixed_legacy_and_new_pair_is_malformed() {
    // Either direction: the pair must agree on legacy or new.
    for (first, second) in [(NEW_REPORT, REPORT_16_LEGACY), (REPORT, REPORT_16)] {
        let err = samples(&finished(0, first, TIME), &finished(0, second, TIME_16)).unwrap_err();
        assert!(err.contains("mixes"), "{err}");
    }
    // A present malformed new line fails the new side, not the pair.
    let bad = NEW_REPORT.replace("5.43 ms, 0 matches", "5.43 ms, 2 matches");
    let err = samples(&finished(0, &bad, TIME), &finished(0, REPORT_16, TIME_16)).unwrap_err();
    assert!(err.contains("with no match"), "{err}");
}

#[test]
fn a_failed_child_fails_with_no_partial_samples() {
    // The first child failing fails the run on its own stderr.
    let err = samples(
        &finished(1, REPORT, "paging: line 3: bad\n"),
        &finished(0, REPORT_16, TIME_16),
    )
    .unwrap_err();
    assert!(err.contains("paging: line 3: bad"), "{err}");
    // The second child failing fails the run too: its scale-1 samples
    // never record alone.
    let err = samples(
        &finished(0, NEW_REPORT, TIME),
        &finished(1, REPORT_16, "boom\n"),
    )
    .unwrap_err();
    assert!(err.contains("boom"), "{err}");
    // Either child's GNU-time peak missing or zero fails the run.
    let no_rss = TIME_16.replace("\tMaximum resident set size (kbytes): 169872\n", "");
    let err = samples(
        &finished(0, NEW_REPORT, TIME),
        &finished(0, REPORT_16, &no_rss),
    )
    .unwrap_err();
    assert!(err.contains("Maximum resident"), "{err}");
    // A jig killed by a signal fails even when time exits 0.
    let killed = format!("{TIME_16}\tCommand terminated by signal 9\n");
    let err = samples(
        &finished(0, NEW_REPORT, TIME),
        &finished(0, REPORT_16, &killed),
    )
    .unwrap_err();
    assert!(err.contains("signal 9"), "{err}");
}

#[test]
fn a_jig_that_failed_is_an_error_carrying_its_stderr() {
    // A legacy pair whose report a failing jig wrote fails the same way.
    let err = samples(
        &finished(1, REPORT, "paging: line 3: bad\n"),
        &finished(0, REPORT_16_LEGACY, TIME_16),
    )
    .unwrap_err();
    assert!(err.contains("paging: line 3: bad"), "{err}");
    // GNU time reports a jig killed by a signal; that is a failed run even
    // when time itself exits 0.
    let killed = format!("{TIME}\tCommand terminated by signal 9\n");
    let err = samples(
        &finished(0, REPORT, &killed),
        &finished(0, REPORT_16_LEGACY, TIME_16),
    )
    .unwrap_err();
    assert!(err.contains("signal 9"), "{err}");
}

#[test]
fn the_jig_runs_under_gnu_time_with_its_workload_pinned_and_only_path_set() {
    // Scale 1 first, then scale 16: every pair runs in order at 160x48.
    for (scale, pinned) in [(1u32, "1"), (16u32, "16")] {
        let command = command(
            Path::new("/usr/bin/time"),
            Path::new("/r/paging"),
            Some(OsStr::new("/usr/bin")),
            scale,
        );
        assert_eq!(command.get_program(), "/usr/bin/time");
        let args: Vec<&OsStr> = command.get_args().collect();
        assert_eq!(args, ["-v", "/r/paging", pinned, "160", "48"]);
    }
    let command = command(
        Path::new("/usr/bin/time"),
        Path::new("/r/paging"),
        Some(OsStr::new("/usr/bin")),
        1,
    );
    let set: Vec<(&OsStr, Option<&OsStr>)> = command.get_envs().collect();
    assert_eq!(set, [(OsStr::new("PATH"), Some(OsStr::new("/usr/bin")))]);
    assert!(
        format!("{command:?}").starts_with("env -i "),
        "the environment is not cleared: {command:?}"
    );
}

/// The report the `tui` crate's jig code prints, over a short session,
/// parses: renaming one of its labels fails here before the release job.
#[test]
fn the_tui_crates_own_report_parses() {
    let mut seq = 0u64;
    let mut events = String::new();
    let mut line = |kind: &str, action: Option<&str>, payload: Value, durable: bool| {
        let mut envelope = json!({
            "kind": kind,
            "session_id": "s_paging0000000000",
            "ts": 1_790_604_120_000_u64 + seq,
            "schema_version": contract::SCHEMA_VERSION,
            "payload": payload,
        });
        if let Some(action) = action {
            envelope["action_id"] = json!(action);
        }
        if durable {
            envelope["seq"] = json!(seq);
            seq += 1;
        }
        events.push_str(&format!("{envelope}\n"));
    };
    let input = |text: &str| json!({"input": [{"type": "message", "source": "driver", "content": [{"type": "text", "text": text}]}]});
    line(
        "fiber_started",
        None,
        json!({"version": "0.0.0", "resumed": false}),
        true,
    );
    line("session_started", None, json!({"workspace": "/w"}), true);
    line("turn_started", None, input("read it"), true);
    line("step_started", None, json!({}), true);
    line("assistant_message_started", Some("a_m1"), json!({}), true);
    line(
        "text_completed",
        Some("a_m1"),
        json!({"text": "reading"}),
        true,
    );
    line(
        "tool_call_requested",
        Some("a_t1"),
        json!({"name": "read", "arguments": {"path": "src/lib.rs"}}),
        true,
    );
    line(
        "assistant_message_completed",
        Some("a_m1"),
        json!({"outcome": "completed"}),
        true,
    );
    line("tool_call_started", Some("a_t1"), json!({}), true);
    line(
        "tool_call_completed",
        Some("a_t1"),
        json!({"status": "completed", "content": [{"type": "text", "text": "ok"}]}),
        true,
    );
    line(
        "turn_completed",
        None,
        json!({"outcome": "completed"}),
        true,
    );
    line("turn_started", None, input("more"), true);
    line(
        "assistant_message_delta",
        Some("a_live"),
        json!({"text": "streaming"}),
        false,
    );
    let printed = tui::measure_paging(&events, 160, 48, fakes::clock::FakeClock::new())
        .unwrap_or_else(|err| panic!("{err}"));
    let figures = report(&printed).unwrap_or_else(|err| panic!("{err}:\n{printed}"));
    assert_eq!(
        (figures.lines, figures.turns, figures.calls),
        (12, 1, 1),
        "{printed}"
    );
    // The jig's own absent search parses as a zero-match sample.
    assert!(
        figures.search_ms.is_some(),
        "the jig reports its absent search: {printed}"
    );
}
