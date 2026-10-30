use std::cell::Cell;
use std::fs;

use serde_json::{Value, json};

use super::{Run, debug_pass, debug_workloads, run_debug, tally};
use crate::idle;
use crate::{Pass, Side};

/// The parsed arguments: a fixed `--fiber` and `--out`, then `extra`.
fn args(extra: &[&str]) -> crate::Args {
    let words = ["--fiber", "f", "--out", "o"]
        .into_iter()
        .chain(extra.iter().copied());
    crate::parse(words.map(str::to_owned)).unwrap()
}

/// The workload names, sorted: `docs/performance.md` promises each workload
/// once, not an order, so the tests compare sets.
fn names(workloads: &[&'static idle::Workload]) -> Vec<&'static str> {
    let mut names: Vec<_> = workloads.iter().map(|workload| workload.name).collect();
    names.sort_unstable();
    names
}

/// One diagnostic-log line with `code` and `data`.
fn line(code: &str, data: Value) -> String {
    json!({
        "ts": 1_700_000_000_000_u64,
        "level": "debug",
        "process": "session",
        "session_id": "s_test",
        "code": code,
        "message": "A line.",
        "data": data
    })
    .to_string()
}

fn op(op: &str, ms: u64) -> String {
    line("op_timing", json!({"op": op, "ms": ms}))
}

/// A fixture workload for `run_debug`: a child home writes one diagnostic line.
fn fixture_workload(ctx: &idle::Ctx<'_>, _: &mut Vec<String>) -> Result<idle::Samples, String> {
    let child = ctx.home.child([])?;
    let config: Value = serde_json::from_slice(
        &fs::read(child.home().join("config.json")).map_err(|err| err.to_string())?,
    )
    .map_err(|err| err.to_string())?;
    assert_eq!(config["diagnostics"]["level"], "debug");
    let logs = child.home().join("logs");
    fs::create_dir_all(&logs).map_err(|err| err.to_string())?;
    fs::write(logs.join("session.log"), op("startup.config", 7)).map_err(|err| err.to_string())?;
    drop(child);
    Ok(Vec::new())
}

const FIXTURE_WORKLOAD: idle::Workload = idle::Workload {
    name: "session start",
    timing: true,
    run: fixture_workload,
};

/// A log file holding `lines`, as `read_logs` returns it.
fn text(lines: Vec<String>) -> Result<String, String> {
    Ok(lines.join("\n"))
}

/// A debug run whose workload ran, with the given notes and logs.
fn run(result: Result<(), String>, notes: Vec<String>, logs: Vec<Result<String, String>>) -> Run {
    run_spans(result, notes, logs, Vec::new())
}

/// A debug run whose workload ran, with the given notes, logs and span
/// rows.
fn run_spans(
    result: Result<(), String>,
    notes: Vec<String>,
    logs: Vec<Result<String, String>>,
    spans: crate::idle::Samples,
) -> Run {
    Run {
        result,
        notes,
        ms: 4.0,
        logs,
        spans,
    }
}

/// `docs/performance.md`, "Measuring": each `op_timing` operation's count and
/// total `ms`.
#[test]
fn tally_sums_op_timing_lines_by_operation() {
    let logs = [
        text(vec![
            op("startup.config", 2),
            line(
                "op_timing",
                json!({"op": "tool.run", "ms": 4, "tool": "read"}),
            ),
        ]),
        text(vec![
            op("startup.config", 3),
            line(
                "op_timing",
                json!({"op": "tool.run", "ms": 5, "tool": "shell"}),
            ),
            op("tui.future_op", 7),
        ]),
    ];
    let (ops, provider, _) = tally(&logs);
    assert_eq!(
        ops,
        json!({
            "startup.config": {"count": 2, "ms": 5},
            "tool.run": {"count": 2, "ms": 9},
            "tui.future_op": {"count": 1, "ms": 7}
        })
    );
    assert_eq!(provider, json!({"count": 0, "total_ms": 0}));
}

/// `docs/performance.md`, "Measuring": the terminal's four operations
/// land as one row each, like every other `op_timing` operation. The
/// tally reads names from the log, so no bench production change covers
/// the new rows.
#[test]
fn tally_sums_terminal_operations_by_operation() {
    let logs = [text(vec![
        op("tui.first_render", 7),
        op("tui.log_read", 3),
        op("tui.transcript", 5),
        op("tui.frame", 21),
        op("tui.frame", 19),
    ])];
    let (ops, provider, _) = tally(&logs);
    assert_eq!(
        ops,
        json!({
            "tui.first_render": {"count": 1, "ms": 7},
            "tui.log_read": {"count": 1, "ms": 3},
            "tui.transcript": {"count": 1, "ms": 5},
            "tui.frame": {"count": 2, "ms": 40}
        })
    );
    assert_eq!(provider, json!({"count": 0, "total_ms": 0}));
}

/// `docs/performance.md`, "Measuring": `provider_request`'s count and total
/// `total_ms`.
#[test]
fn tally_sums_provider_request_total_ms() {
    let logs = [text(vec![
        line("provider_request", json!({"total_ms": 10, "headers_ms": 1})),
        line("provider_request", json!({"total_ms": 32})),
    ])];
    let (ops, provider, _) = tally(&logs);
    assert_eq!(ops, json!({}));
    assert_eq!(provider, json!({"count": 2, "total_ms": 42}));
}

#[test]
fn tally_does_not_count_other_codes() {
    let logs = [text(vec![
        op("startup.config", 4),
        line("info", json!({"op": "startup.config", "ms": 5})),
        line("peak_memory", json!({"bytes": 1})),
        line("session_started", json!({"op": "tool.run", "ms": 3})),
    ])];
    let (ops, provider, _) = tally(&logs);
    assert_eq!(ops, json!({"startup.config": {"count": 1, "ms": 4}}));
    assert_eq!(provider, json!({"count": 0, "total_ms": 0}));
}

/// Unreadable lines are notes; the readable lines around them still count.
#[test]
fn tally_notes_each_unreadable_line_and_counts_the_rest() {
    let logs = [text(vec![
        "not json".to_owned(),
        line("op_timing", json!({"op": "tool.run", "ms": "3"})),
        line("op_timing", json!({"ms": 3})),
        line("provider_request", json!({"headers_ms": 1})),
        op("startup.config", 6),
    ])];
    let (ops, provider, notes) = tally(&logs);
    assert_eq!(ops, json!({"startup.config": {"count": 1, "ms": 6}}));
    assert_eq!(provider, json!({"count": 0, "total_ms": 0}));
    for note in [
        "a diagnostic line is not JSON",
        "an op_timing line has no string data.op or whole data.ms",
        "a provider_request line has no whole data.total_ms",
    ] {
        assert!(
            notes.iter().any(|found| found == note),
            "{note} in {notes:?}"
        );
    }
}

#[test]
fn tally_saturates_at_u64_max() {
    let logs = [text(vec![op("tool.run", u64::MAX), op("tool.run", 1)])];
    let (ops, _, _) = tally(&logs);
    assert_eq!(ops, json!({"tool.run": {"count": 2, "ms": u64::MAX}}));
}

#[test]
fn tally_notes_a_log_that_could_not_be_read() {
    let logs = [Err("reading x.log: boom".to_owned())];
    let (ops, _, notes) = tally(&logs);
    assert_eq!(ops, json!({}));
    assert!(
        notes.iter().any(|note| note == "reading x.log: boom"),
        "{notes:?}"
    );
}

/// Each timing and busy workload runs once (`docs/performance.md`,
/// "Measuring"); `fsyncs`, idle and resume workloads never do. Compared as
/// sets: the doc promises no order.
#[test]
fn debug_workloads_are_the_timing_and_busy_workloads() {
    let got = names(&debug_workloads(&args(&["--paging", "j"])));
    let mut want = vec![
        "session start",
        "terminal first frame",
        "busy turn",
        "web_fetch",
        "10 busy turns",
        "session_list",
        "paging",
        "sandbox setup",
        "sessions list",
        "terminal attach",
        "terminal wheel",
    ];
    want.sort_unstable();
    assert_eq!(got, want);
    for excluded in ["fsyncs", "session idle", "terminal idle"] {
        assert!(!got.contains(&excluded), "{excluded} in {got:?}");
    }
    assert!(
        got.iter().all(|name| !name.starts_with("resume")),
        "{got:?}"
    );
}

#[test]
fn debug_workloads_without_ten_turns_when_skipped() {
    let names = names(&debug_workloads(&args(&[
        "--paging",
        "j",
        "--skip-ten-turns",
    ])));
    assert!(!names.contains(&"10 busy turns"), "{names:?}");
    assert!(names.contains(&"busy turn"), "{names:?}");
}

#[test]
fn debug_workloads_without_paging_when_no_jig() {
    let names = names(&debug_workloads(&args(&[])));
    assert!(!names.contains(&"paging"), "{names:?}");
    assert!(names.contains(&"session_list"), "{names:?}");
}

/// Each selected workload gets one entry (`docs/performance.md`, "Measuring").
#[test]
fn debug_pass_has_one_entry_per_workload() {
    let mut head = Pass::new(Side::Head, args(&["--paging", "j"]));
    head.metrics.insert("attach_stage_ms", vec![json!(1)]);
    head.metric_runs.insert("attach_stage_ms", 5);
    let expected = names(&debug_workloads(&head.args));
    debug_pass(&mut head, |_| run(Ok(()), Vec::new(), Vec::new()));
    let entries = head.metrics.get("op_breakdown").expect("op_breakdown");
    let mut got: Vec<&str> = entries
        .iter()
        .filter_map(|entry| entry.get("workload")?.as_str())
        .collect();
    got.sort_unstable();
    assert_eq!(got, expected);
    assert_eq!(head.metric_runs.get("op_breakdown"), Some(&1));
    assert_eq!(head.metrics.get("attach_stage_ms"), Some(&vec![json!(1)]));
    assert_eq!(head.metric_runs.get("attach_stage_ms"), Some(&5));
}

#[test]
fn debug_pass_tallies_a_child_homes_diagnostic_log() {
    let skip_rows = crate::timing_names()
        .into_iter()
        .filter(|name| *name != "session start")
        .collect::<Vec<_>>()
        .join(",");
    let args = args(&[
        "--only",
        "all",
        "--skip-ten-turns",
        "--skip-rows",
        &skip_rows,
    ]);
    let workloads = debug_workloads(&args);
    assert_eq!(
        names(&workloads),
        ["busy turn", "session start", "web_fetch"]
    );

    let fiber = std::env::current_exe().unwrap();
    let clock = crate::run::System;
    let mut head = Pass::new(Side::Head, args);
    debug_pass(&mut head, |workload| {
        if workload.name != "session start" {
            return run(Ok(()), Vec::new(), Vec::new());
        }
        run_debug(
            &fiber,
            &clock,
            None,
            std::time::Duration::ZERO,
            None,
            &FIXTURE_WORKLOAD,
        )
    });

    let entries = head.metrics.get("op_breakdown").expect("op_breakdown");
    assert_eq!(entries.len(), 3);
    let start = entries
        .iter()
        .find(|entry| entry["workload"] == "session start")
        .expect("session start entry");
    assert_eq!(
        start["ops"],
        json!({"startup.config": {"count": 1, "ms": 7}})
    );
    assert_eq!(
        start["provider_request"],
        json!({"count": 0, "total_ms": 0})
    );
}

#[test]
fn debug_pass_notes_an_error_and_never_fails_the_run() {
    let mut head = Pass::new(Side::Head, args(&["--paging", "j"]));
    debug_pass(&mut head, |workload| {
        if workload.name == "busy turn" {
            run(
                Err("boom".to_owned()),
                vec!["n1".to_owned()],
                vec![text(vec![op("tool.run", 3)])],
            )
        } else {
            run(Ok(()), Vec::new(), Vec::new())
        }
    });
    assert!(head.failures.is_empty(), "{:?}", head.failures);
    let entries = head.metrics.get("op_breakdown").expect("op_breakdown");
    let busy = entries
        .iter()
        .find(|entry| entry.get("workload") == Some(&json!("busy turn")))
        .expect("busy turn entry");
    assert_eq!(busy.get("error"), Some(&json!("boom")));
    assert_eq!(busy.get("notes"), Some(&json!(["n1"])));
    assert_eq!(
        busy.get("ops"),
        Some(&json!({"tool.run": {"count": 1, "ms": 3}}))
    );
}

#[test]
fn debug_pass_is_skipped_unless_only_all() {
    for extra in [&["--only", "timing"][..], &["--only", "wheel"][..]] {
        let mut head = Pass::new(Side::Head, args(extra));
        let calls = Cell::new(0);
        debug_pass(&mut head, |_| {
            calls.set(calls.get() + 1);
            run(Ok(()), Vec::new(), Vec::new())
        });
        assert_eq!(calls.get(), 0, "{extra:?}");
        assert!(!head.metrics.contains_key("op_breakdown"), "{extra:?}");
    }
    let mut head = Pass::new(Side::Head, args(&["--only", "all"]));
    let calls = Cell::new(0);
    debug_pass(&mut head, |_| {
        calls.set(calls.get() + 1);
        run(Ok(()), Vec::new(), Vec::new())
    });
    assert!(calls.get() > 0);
    assert!(head.metrics.contains_key("op_breakdown"));
}

/// The attach workload's entry carries its per-sample, per-fixture spans
/// in no promised order, separate from the tallied totals, with no error
/// when spans exist. The one-line-per-process attach spans are documented
/// in `docs/state.md:423-446`.
#[test]
fn debug_pass_attach_entry_carries_per_sample_spans() {
    let mut head = Pass::new(Side::Head, args(&["--paging", "j"]));
    debug_pass(&mut head, |workload| {
        if workload.name == crate::attach::SAMPLED[0].name {
            run_spans(
                Ok(()),
                Vec::new(),
                vec![text(vec![op("tui.tail_wait", 11)])],
                vec![
                    (
                        crate::attach::SPAN_METRIC,
                        json!({"fixture": "1 MiB", "sample": 0, "op": "tui.tail_wait", "ms": 11}),
                    ),
                    (
                        crate::attach::SPAN_METRIC,
                        json!({"fixture": "10 MiB", "sample": 0, "op": "tui.tail_wait", "ms": 13}),
                    ),
                ],
            )
        } else {
            run(Ok(()), Vec::new(), Vec::new())
        }
    });
    let entries = head.metrics.get("op_breakdown").expect("op_breakdown");
    let attach = entries
        .iter()
        .find(|entry| entry.get("workload") == Some(&json!("terminal attach")))
        .expect("attach entry");
    let spans = attach
        .get("spans")
        .and_then(|spans| spans.as_array())
        .expect("attach entry carries a spans array");
    assert_eq!(spans.len(), 2, "{attach}");
    // `docs/state.md:423-446` promises counts and scopes, not sample
    // order, so the comparison sorts both sides: the exact rows and
    // their count still match, in any order.
    let mut got = spans.clone();
    let mut want = vec![
        json!({"fixture": "1 MiB", "sample": 0, "op": "tui.tail_wait", "ms": 11}),
        json!({"fixture": "10 MiB", "sample": 0, "op": "tui.tail_wait", "ms": 13}),
    ];
    let key = |span: &Value| serde_json::to_string(span).unwrap_or_default();
    got.sort_by_key(|span| key(span));
    want.sort_by_key(|span| key(span));
    assert_eq!(got, want, "{attach}");
    assert!(attach.get("error").is_none(), "{attach}");
    // The tallied totals still stand beside the spans.
    assert_eq!(
        attach.get("ops"),
        Some(&json!({"tui.tail_wait": {"count": 1, "ms": 11}})),
        "{attach}"
    );
}

/// A successful attach run with no spans fails the diagnostic result:
/// without spans there is no Phase 1 evidence.
#[test]
fn debug_pass_attach_entry_errors_without_spans() {
    let mut head = Pass::new(Side::Head, args(&["--paging", "j"]));
    debug_pass(&mut head, |_| run(Ok(()), Vec::new(), Vec::new()));
    let entries = head.metrics.get("op_breakdown").expect("op_breakdown");
    let attach = entries
        .iter()
        .find(|entry| entry.get("workload") == Some(&json!("terminal attach")))
        .expect("attach entry");
    assert_eq!(
        attach.get("error"),
        Some(&json!("no attach spans were recorded")),
        "{attach}"
    );
    assert_eq!(attach.get("spans"), Some(&json!([])), "{attach}");
}

/// A failed attach run keeps the workload's error, and still carries
/// whatever spans it recorded.
#[test]
fn debug_pass_attach_entry_keeps_the_workload_error() {
    let mut head = Pass::new(Side::Head, args(&["--paging", "j"]));
    debug_pass(&mut head, |workload| {
        if workload.name == crate::attach::SAMPLED[0].name {
            run_spans(
                Err("boom".to_owned()),
                Vec::new(),
                Vec::new(),
                vec![(
                    crate::attach::SPAN_METRIC,
                    json!({"fixture": "1 MiB", "sample": 0, "op": "tui.tail_wait", "ms": 11}),
                )],
            )
        } else {
            run(Ok(()), Vec::new(), Vec::new())
        }
    });
    let entries = head.metrics.get("op_breakdown").expect("op_breakdown");
    let attach = entries
        .iter()
        .find(|entry| entry.get("workload") == Some(&json!("terminal attach")))
        .expect("attach entry");
    assert_eq!(attach.get("error"), Some(&json!("boom")), "{attach}");
    assert_eq!(
        attach.get("spans"),
        Some(&json!([
            {"fixture": "1 MiB", "sample": 0, "op": "tui.tail_wait", "ms": 11},
        ])),
        "{attach}"
    );
}

use std::sync::atomic::{AtomicU32, Ordering};

static PROBE_RUNS: AtomicU32 = AtomicU32::new(u32::MAX);
static PROBE_OTHER_RUNS: AtomicU32 = AtomicU32::new(u32::MAX);

/// A workload recording how many samples the debug pass asked of it.
fn probe_runs(ctx: &idle::Ctx<'_>, _: &mut Vec<String>) -> Result<idle::Samples, String> {
    PROBE_RUNS.store(ctx.runs, Ordering::SeqCst);
    Ok(Vec::new())
}

fn probe_other_runs(ctx: &idle::Ctx<'_>, _: &mut Vec<String>) -> Result<idle::Samples, String> {
    PROBE_OTHER_RUNS.store(ctx.runs, Ordering::SeqCst);
    Ok(Vec::new())
}

/// The debug pass runs the attach workload five samples per fixture, like
/// the timed path, and every other workload once: one debug-only run
/// never stands in for the five timed samples.
#[test]
fn run_debug_runs_attach_five_times_and_others_once() {
    assert_eq!(crate::attach::SAMPLED[0].name, "terminal attach");
    const ATTACH_PROBE: idle::Workload = idle::Workload {
        name: "terminal attach",
        timing: true,
        run: probe_runs,
    };
    const OTHER_PROBE: idle::Workload = idle::Workload {
        name: "session start",
        timing: true,
        run: probe_other_runs,
    };
    let fiber = std::env::current_exe().unwrap();
    let clock = crate::run::System;
    run_debug(
        &fiber,
        &clock,
        None,
        std::time::Duration::ZERO,
        None,
        &ATTACH_PROBE,
    );
    run_debug(
        &fiber,
        &clock,
        None,
        std::time::Duration::ZERO,
        None,
        &OTHER_PROBE,
    );
    assert_eq!(PROBE_RUNS.load(Ordering::SeqCst), 5);
    assert_eq!(PROBE_OTHER_RUNS.load(Ordering::SeqCst), 1);
}
