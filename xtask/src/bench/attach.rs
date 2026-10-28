use std::collections::BTreeMap;

use serde_json::Value;

use super::results::{Results, median, metric};

/// The logs the attach workload samples, by fixture name.
const ATTACH_FIXTURES: [&str; 2] = ["1 MiB", "10 MiB"];

/// Each fixture's median ms, and what the head or base column shows: one
/// `fixture (bytes): median ms` part per fixture. The flat first-frame
/// bound and the pass-end ordinary ceiling live in the table's cells, so
/// no formula renders here. A malformed metric is one failure naming `id`.
/// Each fixture's median fold ms per binary MiB of its log: one
/// `fixture (bytes): rate ms per MiB` part per fixture. Only entries
/// carrying `stage: "fold"` count; every other stage stays diagnostic.
/// Strict validation: known fixtures only, `log_bytes` and a finite
/// non-negative `ms` present, exactly five fold samples per fixture.
/// Each entry's rate divides its `ms` by its `log_bytes / 1_048_576` in
/// floating point, with no integer rounding.
pub(super) struct FoldMedians {
    /// Median fold ms per MiB of the 1 MiB fixture's log.
    pub(super) one_mib: f64,
    /// Median fold ms per MiB of the 10 MiB fixture's log.
    pub(super) ten_mib: f64,
}

/// Each fixture's median fold ms per MiB from `id`'s `stage: "fold"`
/// entries: `ms` over `log_bytes / 1_048_576` per entry, the median of
/// those rates per fixture.
pub(super) fn fold_rates(id: &str, results: Option<&Results>) -> Result<FoldMedians, String> {
    let entries = metric(results, id).and_then(|value| {
        value
            .as_array()
            .ok_or_else(|| format!("{id}: not an array"))
    })?;
    // The attach row always takes 5 samples per fixture
    // (`docs/performance.md:184-185`): a declared count of 3 is malformed,
    // even when the samples match it.
    let runs = results.map(|results| results.runs_of(id)).unwrap_or(0);
    if runs != 5 {
        return Err(format!("{id}: {runs} runs, expected 5"));
    }
    let mut one_mib = Vec::new();
    let mut ten_mib = Vec::new();
    for entry in entries {
        if entry.get("stage").and_then(Value::as_str) != Some("fold") {
            continue;
        }
        let fixture = match entry.get("fixture").and_then(Value::as_str) {
            Some(fixture) if ATTACH_FIXTURES.contains(&fixture) => fixture,
            Some(fixture) => {
                return Err(format!("{id}: unknown fixture {fixture:?}"));
            }
            None => return Err(format!("{id}: {entry} has no fixture")),
        };
        let Some(log_bytes) = entry.get("log_bytes").and_then(Value::as_u64) else {
            return Err(format!("{id}: {entry} has no log_bytes"));
        };
        let Some(ms) = entry.get("ms").and_then(Value::as_f64) else {
            return Err(format!("{id}: {entry} has no ms"));
        };
        if !ms.is_finite() || ms < 0.0 {
            return Err(format!(
                "{id}: {entry} is not a finite number at or above zero"
            ));
        }
        let mib = log_bytes as f64 / 1_048_576.0;
        let rate = ms / mib;
        if !rate.is_finite() {
            return Err(format!("{id}: {entry} has no log bytes to rate by"));
        }
        if fixture == "1 MiB" {
            one_mib.push(rate);
        } else {
            ten_mib.push(rate);
        }
    }
    let median_of = |fixture: &str, rates: &[f64]| -> Result<f64, String> {
        if rates.is_empty() {
            return Err(format!("{id}: no fold {fixture} fixture"));
        }
        if rates.len() != runs {
            return Err(format!(
                "{id}: fold {fixture}: {} runs, expected {runs}",
                rates.len()
            ));
        }
        median(rates, runs).map_err(|e| format!("{id}: fold {fixture}: {e}"))
    };
    Ok(FoldMedians {
        one_mib: median_of("1 MiB", &one_mib)?,
        ten_mib: median_of("10 MiB", &ten_mib)?,
    })
}

/// Each fixture's median fold ms per MiB, and what the head or base
/// column shows: one `fixture (bytes): rate ms per MiB` part per
/// fixture. The 15 ms per MiB pin lives in the table's ceiling cell, so
/// no number renders here. A malformed metric is one failure naming `id`.
pub(super) fn fold_per_mib(id: &str, results: Option<&Results>) -> (Vec<String>, String) {
    let (one_bytes, ten_bytes) = fold_bytes(id, results);
    match fold_rates(id, results) {
        Ok(medians) => (
            Vec::new(),
            format!(
                "1 MiB ({} bytes): {:.1} ms per MiB; 10 MiB ({} bytes): {:.1} ms per MiB",
                commas(one_bytes),
                medians.one_mib,
                commas(ten_bytes),
                medians.ten_mib
            ),
        ),
        Err(e) => (vec![e], String::new()),
    }
}

/// The largest fold-stage `log_bytes` per fixture the column shows, or
/// zero when the metric is malformed: the failure names the shape, never
/// the byte count.
fn fold_bytes(id: &str, results: Option<&Results>) -> (u64, u64) {
    let mut one_bytes = 0;
    let mut ten_bytes = 0;
    let entries = metric(results, id).ok().and_then(|value| value.as_array());
    for entry in entries.into_iter().flatten() {
        if entry.get("stage").and_then(Value::as_str) != Some("fold") {
            continue;
        }
        let bytes = entry.get("log_bytes").and_then(Value::as_u64).unwrap_or(0);
        match entry.get("fixture").and_then(Value::as_str) {
            Some("1 MiB") => one_bytes = one_bytes.max(bytes),
            Some("10 MiB") => ten_bytes = ten_bytes.max(bytes),
            _ => {}
        }
    }
    (one_bytes, ten_bytes)
}

/// Each fixture's median ms, and what the head or base column shows: one
/// `fixture (bytes): median ms` part per fixture. The flat first-frame
/// bound and the pass-end ordinary ceiling live in the table's cells, so
/// no formula renders here. A malformed metric is one failure naming `id`.
/// The attach row always takes 5 samples per fixture
/// (`docs/performance.md:184-185`): any other declared count fails.
pub(super) fn fixture_medians(id: &str, results: Option<&Results>) -> (Vec<String>, String) {
    let entries = match metric(results, id).and_then(|value| {
        value
            .as_array()
            .ok_or_else(|| format!("{id}: not an array"))
    }) {
        Ok(entries) => entries,
        Err(e) => return (vec![e], String::new()),
    };
    let mut by_fixture: BTreeMap<&str, Vec<(u64, f64)>> = BTreeMap::new();
    for entry in entries {
        let fixture = match entry.get("fixture").and_then(Value::as_str) {
            Some(fixture) if ATTACH_FIXTURES.contains(&fixture) => fixture,
            Some(fixture) => {
                return (
                    vec![format!("{id}: unknown fixture {fixture:?}")],
                    String::new(),
                );
            }
            None => return (vec![format!("{id}: {entry} has no fixture")], String::new()),
        };
        let Some(log_bytes) = entry.get("log_bytes").and_then(Value::as_u64) else {
            return (
                vec![format!("{id}: {entry} has no log_bytes")],
                String::new(),
            );
        };
        let Some(ms) = entry.get("ms").and_then(Value::as_f64) else {
            return (vec![format!("{id}: {entry} has no ms")], String::new());
        };
        by_fixture.entry(fixture).or_default().push((log_bytes, ms));
    }
    let mut failures = Vec::new();
    let mut shown = Vec::new();
    // `metric` fails on `None`, so results exists past here.
    let runs = results.map(|results| results.runs_of(id)).unwrap_or(0);
    // The attach row always takes 5 samples per fixture
    // (`docs/performance.md:184-185`): a declared count of 3 is malformed,
    // even when the samples match it.
    if runs != 5 {
        return (
            vec![format!("{id}: {runs} runs, expected 5")],
            String::new(),
        );
    }
    for fixture in ATTACH_FIXTURES {
        let Some(samples) = by_fixture.get(fixture) else {
            failures.push(format!("{id}: no {fixture} fixture"));
            continue;
        };
        if samples.len() != runs {
            failures.push(format!(
                "{id}: {fixture}: {} runs, expected {runs}",
                samples.len()
            ));
            continue;
        }
        let bytes = samples.iter().map(|(bytes, _)| *bytes).max().unwrap_or(0);
        let ms: Vec<f64> = samples.iter().map(|(_, ms)| *ms).collect();
        match median(&ms, runs) {
            Ok(middle) => shown.push(format!(
                "{fixture} ({} bytes): {middle:.1} ms",
                commas(bytes)
            )),
            Err(e) => failures.push(format!("{id}: {fixture}: {e}")),
        }
    }
    (failures, shown.join("; "))
}

/// `1050231` as `"1,050,231"`, for the comment's byte counts.
fn commas(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}
