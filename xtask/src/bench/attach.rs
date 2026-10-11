use std::collections::BTreeMap;

use serde_json::Value;

use super::results::{Results, median, metric};

/// The attaching row's formula, 50 ms plus 10 ms per MiB of session log:
/// its base and its rate, in milliseconds.
const ATTACH_BASE_MS: f64 = 50.0;
const ATTACH_MS_PER_MIB: f64 = 10.0;
/// One MiB in bytes, which the attaching formula counts the log in.
const MIB_BYTES: f64 = 1_048_576.0;
/// The logs the attach workload samples, by fixture name.
const ATTACH_FIXTURES: [&str; 2] = ["1 MiB", "10 MiB"];

/// Each fixture's samples against the attaching formula, and what the
/// head or base column shows: one `fixture (bytes): median ms of ceiling
/// ms` part per fixture. A slow median is advisory and adds no failure;
/// a malformed metric is one failure naming `id`.
pub(super) fn per_mib(id: &str, results: Option<&Results>) -> (Vec<String>, String) {
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
                "{fixture} ({} bytes): {middle:.1} ms of {:.1} ms",
                commas(bytes),
                ceiling_ms(bytes)
            )),
            Err(e) => failures.push(format!("{id}: {fixture}: {e}")),
        }
    }
    (failures, shown.join("; "))
}

/// The attaching ceiling for a fixture whose largest log is `bytes`:
/// 50 ms plus 10 ms per MiB.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a session log under 9 PiB holds exactly in an f64"
)]
fn ceiling_ms(bytes: u64) -> f64 {
    ATTACH_BASE_MS + ATTACH_MS_PER_MIB * (bytes as f64) / MIB_BYTES
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
