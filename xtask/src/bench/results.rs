use serde_json::{Map, Value};

pub(super) const SCHEMA: u64 = 1;
/// Each number is the median of this many runs ("Measuring").
pub(super) const RUNS: usize = 5;

/// The median of exactly `RUNS` values.
pub(crate) fn median(values: &[f64]) -> Result<f64, String> {
    if values.len() != RUNS {
        return Err(format!("{} runs, expected {RUNS}", values.len()));
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted
        .get(sorted.len() / 2)
        .copied()
        .ok_or_else(|| "no runs".to_owned())
}

/// A parsed result file.
pub(super) struct Results {
    pub(super) idle_secs: u64,
    pub(super) metrics: Map<String, Value>,
    pub(super) failures: Vec<String>,
}

pub(super) fn parse_results(text: &str) -> Result<Results, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let schema = value.get("schema").and_then(Value::as_u64);
    if schema != Some(SCHEMA) {
        return Err(format!("schema is {schema:?}, expected {SCHEMA}"));
    }
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
    median(&values).map_err(|e| format!("{id}: {e}"))
}

/// The per-run arrays of an exact metric: exactly `RUNS`, none empty.
pub(super) fn runs<'a>(results: Option<&'a Results>, id: &str) -> Result<Vec<&'a [Value]>, String> {
    let all = metric(results, id)?
        .as_array()
        .ok_or_else(|| format!("{id}: not an array"))?;
    if all.len() != RUNS {
        return Err(format!("{id}: {} runs, expected {RUNS}", all.len()));
    }
    all.iter()
        .enumerate()
        .map(|(i, run)| match run.as_array() {
            Some(entries) if !entries.is_empty() => Ok(entries.as_slice()),
            Some(_) | None => Err(format!("{id}: run {}: nothing recorded", i + 1)),
        })
        .collect()
}

pub(super) fn field(entry: &Value, id: &str, name: &str) -> Result<u64, String> {
    entry
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{id}: {entry} has no {name}"))
}
