//! The debug pass (`docs/performance.md`, "Measuring"): each timing and busy
//! workload runs once more in a home whose `diagnostics.level` is `debug`,
//! and the diagnostic-log lines its homes wrote are tallied per operation.
//! The pass is never a sample: its numbers are reported and never gated.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use contract::clock::Clock;
use serde_json::{Map, Value, json};

use crate::busy;
use crate::home::{DiagnosticLogs, Home};
use crate::idle;

/// One workload's debug run.
pub(crate) struct Run {
    /// The workload's own result; an `Err` ends the workload.
    result: Result<(), String>,
    /// Notes from the workload and from the home's cleanup.
    notes: Vec<String>,
    /// The workload call's wall-clock time, in milliseconds.
    ms: f64,
    /// Every diagnostic-log text the run's homes wrote, or why one could
    /// not be read.
    logs: Vec<Result<String, String>>,
    /// The attach workload's per-sample, per-fixture span rows,
    /// harvested from its samples; every other workload leaves this
    /// empty.
    spans: idle::Samples,
}

/// How many samples the debug pass takes of `workload` per fixture: the
/// attach workload runs five, like the timed path (`docs/performance.md`,
/// "Measuring"), so its spans hold five observations per fixture; every
/// other workload runs once. One debug-only run never stands in for the
/// five timed samples.
const ATTACH_DEBUG_RUNS: u32 = 5;

/// The debug sample count of `workload`.
fn debug_runs(workload: &'static idle::Workload) -> u32 {
    if workload.name == crate::attach::SAMPLED[0].name {
        ATTACH_DEBUG_RUNS
    } else {
        1
    }
}

/// The attach workload's span rows from its samples: the `attach_span_ms`
/// values in sample order. Every other workload's samples hold none.
fn attach_spans(samples: idle::Samples) -> idle::Samples {
    samples
        .into_iter()
        .filter(|(metric, _)| *metric == crate::attach::SPAN_METRIC)
        .collect()
}

/// The workloads the debug pass repeats, in selection order: each timing
/// workload and each busy workload. `fsyncs` is not among them: it runs
/// once, under strace, and rewrites `config.json`.
pub(crate) fn debug_workloads(args: &crate::Args) -> Vec<&'static idle::Workload> {
    crate::selected(args)
        .into_iter()
        .map(|(workload, _, _)| workload)
        .filter(|workload| {
            workload.timing
                || busy::WORKLOADS
                    .iter()
                    .any(|turn| turn.name == workload.name)
        })
        .collect()
}

/// Tallies the diagnostic-log lines in `logs` into `(ops, provider_request,
/// notes)`: `ops` maps each `op_timing` operation to `{"count", "ms"}`, and
/// `provider_request` is `{"count", "total_ms"}`. Other codes are not
/// counted. A line that cannot be read is a note and is not counted.
fn tally(logs: &[Result<String, String>]) -> (Value, Value, Vec<String>) {
    let mut ops: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let (mut requests, mut total_ms) = (0_u64, 0_u64);
    let mut notes = Vec::new();
    for log in logs {
        let text = match log {
            Ok(text) => text,
            Err(err) => {
                notes.push(err.clone());
                continue;
            }
        };
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                notes.push("a diagnostic line is not JSON".to_owned());
                continue;
            };
            let data = value.get("data");
            match value.get("code").and_then(Value::as_str) {
                Some("op_timing") => {
                    let op = data.and_then(|data| data.get("op")).and_then(Value::as_str);
                    let ms = data.and_then(|data| data.get("ms")).and_then(Value::as_u64);
                    match (op, ms) {
                        (Some(op), Some(ms)) => {
                            let (count, total) = ops.entry(op.to_owned()).or_default();
                            *count = count.saturating_add(1);
                            *total = total.saturating_add(ms);
                        }
                        _ => notes.push(
                            "an op_timing line has no string data.op or whole data.ms".to_owned(),
                        ),
                    }
                }
                Some("provider_request") => {
                    match data
                        .and_then(|data| data.get("total_ms"))
                        .and_then(Value::as_u64)
                    {
                        Some(ms) => {
                            requests = requests.saturating_add(1);
                            total_ms = total_ms.saturating_add(ms);
                        }
                        None => notes
                            .push("a provider_request line has no whole data.total_ms".to_owned()),
                    }
                }
                _ => {}
            }
        }
    }
    let ops: Map<String, Value> = ops
        .into_iter()
        .map(|(op, (count, ms))| (op, json!({"count": count, "ms": ms})))
        .collect();
    (
        Value::Object(ops),
        json!({"count": requests, "total_ms": total_ms}),
        notes,
    )
}

/// One workload's `op_breakdown` entry: its name, its run's wall-clock
/// time, the tallied operations and provider requests, and its notes. An
/// `error` key is added when the workload erred. The attach workload's
/// entry also carries its per-sample, per-fixture `spans`: the Phase 1
/// evidence, separate from the tallied totals. A successful attach run
/// with no spans fails the diagnostic result with an error.
fn entry(workload: &str, run: Run) -> Value {
    let (ops, provider_request, tallied) = tally(&run.logs);
    let mut notes = run.notes;
    notes.extend(tallied);
    let mut entry = json!({
        "workload": workload,
        "ms": run.ms,
        "ops": ops,
        "provider_request": provider_request,
        "notes": notes
    });
    if let Err(err) = run.result
        && let Some(fields) = entry.as_object_mut()
    {
        fields.insert("error".to_owned(), json!(err));
    }
    if workload == crate::attach::SAMPLED[0].name {
        let spans: Vec<Value> = run.spans.into_iter().map(|(_, value)| value).collect();
        if let Some(fields) = entry.as_object_mut() {
            // Missing spans fail the diagnostic result rather than passing
            // as a spanless attach: without them there is no Phase 1
            // evidence.
            if spans.is_empty() && !fields.contains_key("error") {
                fields.insert("error".to_owned(), json!("no attach spans were recorded"));
            }
            fields.insert("spans".to_owned(), Value::Array(spans));
        }
    }
    entry
}

/// Runs each debug workload once through `run` and records them as
/// `op_breakdown`, one entry per workload in selection order. Only
/// `--only all` runs the pass, and nothing it finds reaches `head.failures`.
pub(crate) fn debug_pass(
    head: &mut crate::Pass,
    mut run: impl FnMut(&'static idle::Workload) -> Run,
) {
    if head.args.only != crate::Only::All {
        return;
    }
    let entries: Vec<Value> = debug_workloads(&head.args)
        .into_iter()
        .map(|workload| entry(workload.name, run(workload)))
        .collect();
    head.metrics.insert("op_breakdown", entries);
    head.metric_runs.insert("op_breakdown", 1);
}

/// One workload's debug run: the workload, timed on `clock`, in a debug
/// home of its own. The attach workload runs five samples per fixture,
/// like the timed path, so its spans hold five observations per fixture;
/// every other workload runs once. The home's logs are drained after it
/// is gone.
pub(crate) fn run_debug(
    fiber: &Path,
    clock: &dyn Clock,
    path: Option<OsString>,
    idle: Duration,
    paging: Option<&Path>,
    workload: &'static idle::Workload,
) -> Run {
    let (home, sink): (Home, DiagnosticLogs) = match Home::debug(fiber) {
        Ok(built) => built,
        Err(err) => {
            return Run {
                result: Err(err),
                notes: Vec::new(),
                ms: 0.0,
                logs: Vec::new(),
                spans: Vec::new(),
            };
        }
    };
    let ctx = idle::Ctx {
        home: &home,
        clock,
        idle,
        path,
        runs: debug_runs(workload),
        paging,
    };
    let mut notes = Vec::new();
    let started = clock.now();
    let (result, spans) = match crate::run_once(&ctx, workload, &mut notes) {
        Ok(samples) => (Ok(()), attach_spans(samples)),
        Err(err) => (Err(err), Vec::new()),
    };
    let ms = idle::ms(clock, started).as_f64().unwrap_or_default();
    if let Err(err) = home.finish() {
        notes.push(format!("cleanup: {err}"));
    }
    let logs = std::mem::take(&mut *sink.borrow_mut());
    Run {
        result,
        notes,
        ms,
        logs,
        spans,
    }
}

#[cfg(test)]
#[path = "debug_tests.rs"]
mod tests;
