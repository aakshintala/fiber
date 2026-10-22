use std::cell::Cell;

use serde_json::{Value, json};

use super::{Run, debug_pass, debug_workloads, tally};
use crate::idle;
use crate::{Pass, Side};

/// The parsed arguments: a fixed `--fiber` and `--out`, then `extra`.
fn args(extra: &[&str]) -> crate::Args {
    let words = ["--fiber", "f", "--out", "o"]
        .into_iter()
        .chain(extra.iter().copied());
    crate::parse(words.map(str::to_owned)).unwrap()
}

fn names(workloads: &[&'static idle::Workload]) -> Vec<&'static str> {
    workloads.iter().map(|workload| workload.name).collect()
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

/// A log file holding `lines`, as `read_logs` returns it.
fn text(lines: Vec<String>) -> Result<String, String> {
    Ok(lines.join("\n"))
}

/// A debug run whose workload ran, with the given notes and logs.
fn run(result: Result<(), String>, notes: Vec<String>, logs: Vec<Result<String, String>>) -> Run {
    Run {
        result,
        notes,
        ms: 4.0,
        logs,
    }
}

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
    let (ops, provider, notes) = tally(&logs);
    assert_eq!(
        ops,
        json!({
            "startup.config": {"count": 2, "ms": 5},
            "tool.run": {"count": 2, "ms": 9},
            "tui.future_op": {"count": 1, "ms": 7}
        })
    );
    assert_eq!(provider, json!({"count": 0, "total_ms": 0}));
    assert!(notes.is_empty(), "{notes:?}");
}

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
    let (ops, provider, notes) = tally(&logs);
    assert_eq!(ops, json!({"startup.config": {"count": 1, "ms": 4}}));
    assert_eq!(provider, json!({"count": 0, "total_ms": 0}));
    assert!(notes.is_empty(), "{notes:?}");
}

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
    assert_eq!(notes.len(), 4, "{notes:?}");
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
    assert_eq!(notes, ["reading x.log: boom".to_owned()]);
}

#[test]
fn debug_workloads_are_the_timing_and_busy_workloads_in_order() {
    assert_eq!(
        names(&debug_workloads(&args(&["--paging", "j"]))),
        [
            "session start",
            "terminal first frame",
            "busy turn",
            "web_fetch",
            "10 busy turns",
            "session_list",
            "paging",
            "sessions list",
            "terminal attach",
            "terminal wheel",
        ]
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

#[test]
fn debug_workloads_never_hold_fsyncs_idle_or_resume() {
    let names = names(&debug_workloads(&args(&["--paging", "j"])));
    assert!(names.contains(&"busy turn"), "{names:?}");
    for excluded in ["fsyncs", "session idle", "terminal idle"] {
        assert!(!names.contains(&excluded), "{excluded} in {names:?}");
    }
    assert!(
        names.iter().all(|name| !name.starts_with("resume")),
        "{names:?}"
    );
}

#[test]
fn debug_pass_has_one_entry_per_workload_in_selection_order() {
    let mut head = Pass::new(Side::Head, args(&["--paging", "j"]));
    head.metrics.insert("attach_stage_ms", vec![json!(1)]);
    head.metric_runs.insert("attach_stage_ms", 5);
    let expected = names(&debug_workloads(&head.args));
    debug_pass(&mut head, |_| run(Ok(()), Vec::new(), Vec::new()));
    let entries = head.metrics.get("op_breakdown").expect("op_breakdown");
    let got: Vec<&str> = entries
        .iter()
        .filter_map(|entry| entry.get("workload")?.as_str())
        .collect();
    assert_eq!(got, expected);
    assert_eq!(head.metrics.get("attach_stage_ms"), Some(&vec![json!(1)]));
    assert_eq!(head.metric_runs.get("attach_stage_ms"), Some(&5));
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
