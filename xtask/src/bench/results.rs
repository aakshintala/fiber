use std::collections::BTreeMap;

use serde_json::{Map, Value};

pub(super) const SCHEMA: u64 = 1;

/// The median of exactly `runs` values: the run count a metric declares.
pub(super) fn median(values: &[f64], runs: usize) -> Result<f64, String> {
    if values.len() != runs {
        return Err(format!("{} runs, expected {runs}", values.len()));
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted
        .get(sorted.len() / 2)
        .copied()
        .ok_or_else(|| "no runs".to_owned())
}

/// A parsed result file.
#[derive(Clone, Debug)]
pub(super) struct Results {
    pub(super) idle_secs: u64,
    /// The run count every metric without its own entry declares.
    pub(super) runs: usize,
    /// The pull request's timing count; the full count elsewhere.
    pub(super) timing_runs: usize,
    /// Each recorded metric id's run count, when the file declares one.
    pub(super) metric_runs: BTreeMap<String, usize>,
    /// The workloads the selection left out, by name: the judge reports
    /// their rows as skipped instead of failing their missing metrics.
    /// Files the harness wrote before the flag carry none.
    pub(super) skipped: Vec<String>,
    /// The runner's CPU model; files the backstop wrote before it carry none.
    pub(super) cpu: Option<String>,
    /// The stripped binary's size in bytes; files written before it carry none.
    pub(super) binary_size_bytes: Option<u64>,
    pub(super) metrics: Map<String, Value>,
    pub(super) failures: Vec<String>,
}

impl Results {
    /// The run count `id`'s median is taken over: its declared count, else
    /// the file's.
    pub(super) fn runs_of(&self, id: &str) -> usize {
        self.metric_runs.get(id).copied().unwrap_or(self.runs)
    }
}

/// A whole number of at least 1 in `value`, for a run count.
fn count(value: &Value, what: &str) -> Result<usize, String> {
    match value.as_u64() {
        Some(n) if n >= 1 => {
            usize::try_from(n).map_err(|_| format!("{what} is not a positive whole number"))
        }
        Some(_) | None => Err(format!("{what} is not a positive whole number")),
    }
}

pub(super) fn parse_results(text: &str) -> Result<Results, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let schema = value.get("schema").and_then(Value::as_u64);
    if schema != Some(SCHEMA) {
        return Err(format!("schema is {schema:?}, expected {SCHEMA}"));
    }
    let runs = match value.get("runs") {
        None => return Err("no runs".to_owned()),
        Some(value) => count(value, "runs")?,
    };
    let timing_runs = match value.get("timing_runs") {
        None => runs,
        Some(value) => count(value, "timing_runs")?,
    };
    let mut metric_runs = BTreeMap::new();
    if let Some(declared) = value.get("metric_runs") {
        let declared = declared.as_object().ok_or("metric_runs is not an object")?;
        for (id, value) in declared {
            metric_runs.insert(id.clone(), count(value, &format!("metric_runs.{id}"))?);
        }
    }
    let skipped = match value.get("skipped") {
        None => Vec::new(),
        Some(skipped) => skipped
            .as_array()
            .ok_or("skipped is not an array")?
            .iter()
            .map(|name| {
                name.as_str()
                    .map(str::to_owned)
                    .ok_or("a skipped workload is not a string")
            })
            .collect::<Result<_, _>>()?,
    };
    let cpu = match value.get("cpu") {
        None => None,
        Some(Value::String(model)) => Some(model.clone()),
        Some(_) => return Err("cpu is not a string".to_owned()),
    };
    let binary_size_bytes = match value.get("binary_size_bytes") {
        None => None,
        Some(size) => Some(
            size.as_u64()
                .ok_or("binary_size_bytes is not a whole number of bytes")?,
        ),
    };
    let idle_secs = value
        .get("idle_secs")
        .and_then(Value::as_u64)
        .ok_or("no idle_secs")?;
    let metrics = value
        .get("metrics")
        .and_then(Value::as_object)
        .ok_or("no metrics object")?
        .clone();
    let failures = value
        .get("failures")
        .and_then(Value::as_array)
        .ok_or("no failures array")?
        .iter()
        .map(|f| {
            f.as_str()
                .map(str::to_owned)
                .ok_or("a failure is not a string")
        })
        .collect::<Result<_, _>>()?;
    Ok(Results {
        idle_secs,
        runs,
        timing_runs,
        metric_runs,
        skipped,
        cpu,
        binary_size_bytes,
        metrics,
        failures,
    })
}

pub(super) fn metric<'a>(results: Option<&'a Results>, id: &str) -> Result<&'a Value, String> {
    results
        .ok_or("no results")?
        .metrics
        .get(id)
        .ok_or_else(|| format!("{id}: missing"))
}

pub(super) fn run_median(results: Option<&Results>, id: &str) -> Result<f64, String> {
    let values = metric(results, id)?
        .as_array()
        .ok_or_else(|| format!("{id}: not an array"))?
        .iter()
        .map(|v| {
            v.as_f64()
                .ok_or_else(|| format!("{id}: {v} is not a number"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    // `metric` fails on `None`, so results exists past here.
    let runs = results.map(|results| results.runs_of(id)).unwrap_or(0);
    median(&values, runs).map_err(|e| format!("{id}: {e}"))
}

/// The per-run arrays of an exact metric: one entry per declared run, none
/// empty.
pub(super) fn runs<'a>(results: Option<&'a Results>, id: &str) -> Result<Vec<&'a [Value]>, String> {
    let results = results.ok_or("no results")?;
    let runs = results.runs_of(id);
    let all = results
        .metrics
        .get(id)
        .ok_or_else(|| format!("{id}: missing"))?
        .as_array()
        .ok_or_else(|| format!("{id}: not an array"))?;
    if all.len() != runs {
        return Err(format!("{id}: {} runs, expected {runs}", all.len()));
    }
    all.iter()
        .enumerate()
        .map(|(i, run)| match run.as_array() {
            Some(entries) if !entries.is_empty() => Ok(entries.as_slice()),
            Some(_) | None => Err(format!("{id}: run {}: nothing recorded", i + 1)),
        })
        .collect()
}

/// Appends the remeasure run's samples to the first pass's for every timing
/// metric of `rows`, so the judge medians all 7 per side. `rows` holds
/// `ROW_METRICS` row names from the first pass's breaches; other metrics
/// keep their first-pass samples, and ids ending in `_kib` (memory) or
/// `_counts` (counts) are never merged. The same function merges the head
/// pair and the base pair. Any problem with the extra file is an error and
/// the caller keeps the first pass: a different `cpu` (a different runner),
/// a non-empty `failures` (a failed run, not slow samples), a breached-row
/// metric missing from the extra file, or one declaring a count other than
/// `REMEASURE_RUNS`.
pub(super) fn merge_remeasure(
    first: &Results,
    extra: &Results,
    rows: &[&str],
) -> Result<Results, String> {
    if first.cpu != extra.cpu {
        return Err(format!(
            "cpu {:?} differs from the first pass's {:?}: not the same runner",
            extra.cpu, first.cpu
        ));
    }
    if !extra.failures.is_empty() {
        return Err(format!(
            "extra run failed, not a slow sample: {}",
            extra.failures.join("; ")
        ));
    }
    let mut merged = first.clone();
    for row in rows {
        let (_, ids) = super::judge::ROW_METRICS
            .iter()
            .find(|(name, _)| name == row)
            .ok_or_else(|| format!("{row}: no ROW_METRICS row holds it"))?;
        for id in *ids {
            if id.ends_with("_kib") || id.ends_with("_counts") {
                continue;
            }
            let extra_values = extra
                .metrics
                .get(*id)
                .ok_or_else(|| format!("{id}: missing from the extra run"))?;
            let extra_samples = extra_values
                .as_array()
                .ok_or_else(|| format!("{id}: not an array in the extra run"))?;
            let declared = extra.runs_of(id);
            if declared != super::judge::REMEASURE_RUNS {
                return Err(format!(
                    "{id}: extra run declares {declared} runs, expected {}",
                    super::judge::REMEASURE_RUNS
                ));
            }
            if extra_samples.len() != super::judge::REMEASURE_RUNS {
                return Err(format!(
                    "{id}: extra run holds {} samples, expected {}",
                    extra_samples.len(),
                    super::judge::REMEASURE_RUNS
                ));
            }
            let first_samples = merged
                .metrics
                .get_mut(*id)
                .and_then(Value::as_array_mut)
                .ok_or_else(|| format!("{id}: missing from the first pass"))?;
            first_samples.extend(extra_samples.iter().cloned());
            merged.metric_runs.insert(
                (*id).to_owned(),
                first.runs_of(id) + super::judge::REMEASURE_RUNS,
            );
        }
    }
    Ok(merged)
}

pub(super) fn field(entry: &Value, id: &str, name: &str) -> Result<u64, String> {
    entry
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{id}: {entry} has no {name}"))
}
