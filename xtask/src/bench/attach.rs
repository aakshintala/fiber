use std::collections::BTreeMap;

use serde_json::Value;

use super::results::{Results, median, metric};

/// The logs the attach workload samples, by fixture name.
const ATTACH_FIXTURES: [&str; 2] = ["1 MiB", "10 MiB"];

/// Each fixture's median ms, and what the head or base column shows: one
/// `fixture (bytes): median ms` part per fixture. The flat first-frame
/// bound and the pass-end ordinary ceiling live in the table's cells, so
/// no formula renders here. A malformed metric is one failure naming `id`.
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
