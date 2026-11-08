//! The break-glass audit's decision (`docs/ci.md`, "The break-glass audit"):
//! which merges to `main` in the last 48 hours had no green `CI` on their
//! pull request head, and whether each one declared break-glass. It runs
//! no `gh` and reads no API itself: the `break-glass audit` job in
//! `scheduled.yml` fetches each response and passes them together as one
//! JSON object, each wrapped as `{"ok": true, "body": ...}` or
//! `{"ok": false}`. Every response must carry the shape read here; a
//! failed lookup or a wrong shape is an error, never an empty list, so a
//! skipped read cannot pass. `main.rs` is the only caller.

use serde_json::Value;

/// The audited window: twice the daily cadence, so one skipped run leaves
/// no gap.
const WINDOW_SECS: u64 = 172_800;

/// The exact prefix a pull request body starts with to declare break-glass
/// (`docs/workflow.md`, "Break-glass merge").
const DECLARATION: &str = "Break-glass:";

/// The one check run name that counts; `CI (draft)` does not.
const CHECK: &str = "CI";

/// The audit's outcome: one line per merge with no green `CI`, and the
/// counts the last line reports.
#[derive(Debug, Default)]
pub(crate) struct Report {
    lines: Vec<String>,
    audited: usize,
    without_green: usize,
    undeclared: usize,
}

impl Report {
    /// The run passes when every merge with no green `CI` declared it.
    pub(crate) fn passed(&self) -> bool {
        self.undeclared == 0
    }

    /// The stdout lines: the listed merges, then the counts.
    pub(crate) fn render(&self) -> Vec<String> {
        let mut out = self.lines.clone();
        out.push(format!(
            "audited {} merges, {} without green CI, {} undeclared",
            self.audited, self.without_green, self.undeclared
        ));
        out
    }

    /// The listed merges that fail the run.
    pub(crate) fn failing(&self) -> impl Iterator<Item = &String> {
        self.lines.iter().filter(|l| l.ends_with(" undeclared"))
    }
}

/// The command line: `[--since] [--now EPOCH]`.
#[derive(Debug)]
pub(crate) struct Args {
    pub(crate) since: bool,
    pub(crate) now: Option<u64>,
}

pub(crate) fn parse_args(rest: &[String]) -> Result<Args, String> {
    const USAGE: &str = "usage: cargo xtask break-glass-audit [--since] [--now EPOCH]";
    let mut args = Args {
        since: false,
        now: None,
    };
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--since" => args.since = true,
            "--now" => {
                let epoch = rest.next().ok_or(USAGE)?;
                args.now = Some(epoch.parse().map_err(|_| USAGE)?);
            }
            _ => return Err(USAGE.to_owned()),
        }
    }
    Ok(args)
}

/// The stdin object, or the error naming the command.
pub(crate) fn parse_input(text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| format!("break-glass-audit: {e}"))
}

/// The cutoff, `now` minus the window, as `YYYY-MM-DDTHH:MM:SSZ`.
pub(crate) fn window_since(now: u64) -> String {
    let secs = now.saturating_sub(WINDOW_SECS);
    let (days, time) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (proleptic Gregorian).
    let z = i64::try_from(days).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3_600,
        time % 3_600 / 60,
        time % 60
    )
}

/// Decides the audit over the lookups in `input` at epoch `now`.
pub(crate) fn audit(input: &Value, now: u64) -> Result<Report, String> {
    let cutoff = window_since(now);
    let mut report = Report::default();
    for (sha, date) in commits(input)? {
        // Fixed-width UTC timestamps order as text.
        if date < cutoff.as_str() {
            continue;
        }
        report.audited += 1;
        let pulls = read_pulls(lookup_body(input, "pulls", sha)?)?;
        let Some(pull) = pulls.iter().find(|p| p.merged_on_main) else {
            report.without_green += 1;
            report.undeclared += 1;
            report
                .lines
                .push(format!("merge {sha} head none PR none ticket - undeclared"));
            continue;
        };
        if has_green_ci(lookup_body(input, "checks", pull.head)?)? {
            continue;
        }
        let declared = declared(pull.body);
        report.without_green += 1;
        report.undeclared += usize::from(!declared);
        report.lines.push(format!(
            "merge {sha} head {} PR #{} ticket {} {}",
            pull.head,
            pull.number,
            ticket(pull.body),
            if declared { "declared" } else { "undeclared" }
        ));
    }
    Ok(report)
}

/// The body of a successful lookup.
fn body<'a>(lookup: &'a Value, what: &str) -> Result<&'a Value, String> {
    match lookup.get("ok").and_then(Value::as_bool) {
        Some(true) => lookup
            .get("body")
            .ok_or_else(|| format!("{what}: the lookup has no body")),
        Some(false) => Err(format!("{what}: the gh lookup failed")),
        None => Err(format!("{what}: the lookup has no ok flag")),
    }
}

/// The body of `input[group][key]`; a missing lookup is an error.
fn lookup_body<'a>(input: &'a Value, group: &str, key: &str) -> Result<&'a Value, String> {
    let what = if group == "checks" {
        "check-runs"
    } else {
        group
    };
    let lookup = input
        .get(group)
        .and_then(|lookups| lookups.get(key))
        .ok_or_else(|| format!("{what}: no lookup for {key}"))?;
    body(lookup, &format!("{what} {key}"))
}

/// Each commit's sha and committer date, pages flattened.
fn commits(input: &Value) -> Result<Vec<(&str, &str)>, String> {
    let lookup = input.get("commits").ok_or("commits: no lookup")?;
    let list = body(lookup, "commits")?
        .as_array()
        .ok_or("commits: the response is not an array")?;
    let mut out = Vec::new();
    for item in list {
        let page = item
            .as_array()
            .map_or_else(|| std::slice::from_ref(item), Vec::as_slice);
        for commit in page {
            let sha = commit
                .get("sha")
                .and_then(Value::as_str)
                .ok_or("commits: a commit has no sha")?;
            let date = commit
                .pointer("/commit/committer/date")
                .and_then(Value::as_str)
                .filter(|d| d.len() == 20 && d.ends_with('Z'))
                .ok_or_else(|| format!("commits: {sha} has no UTC committer date"))?;
            out.push((sha, date));
        }
    }
    Ok(out)
}

/// A pull request as the audit reads it.
struct Pull<'a> {
    number: u64,
    body: Option<&'a str>,
    head: &'a str,
    merged_on_main: bool,
}

/// Every entry of a `commits/<sha>/pulls` response, shape-checked.
fn read_pulls(response: &Value) -> Result<Vec<Pull<'_>>, String> {
    let list = response
        .as_array()
        .ok_or("pulls: the response is not an array")?;
    list.iter().map(read_pull).collect()
}

fn read_pull(pull: &Value) -> Result<Pull<'_>, String> {
    let bad = |field: &str| format!("pulls: a pull request has no valid {field}");
    let merged = match pull.get("merged_at") {
        Some(Value::Null) => false,
        Some(Value::String(_)) => true,
        _ => return Err(bad("merged_at")),
    };
    let body = match pull.get("body") {
        Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.as_str()),
        _ => return Err(bad("body")),
    };
    let base = pull
        .pointer("/base/ref")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("base.ref"))?;
    Ok(Pull {
        number: pull
            .get("number")
            .and_then(Value::as_u64)
            .ok_or_else(|| bad("number"))?,
        body,
        head: pull
            .pointer("/head/sha")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("head.sha"))?,
        merged_on_main: merged && base == "main",
    })
}

/// Whether the newest `CI` run in a check-runs response completed with
/// success. The response must carry its shape: a `check_runs` array and a
/// `total_count` no larger than the runs read.
fn has_green_ci(response: &Value) -> Result<bool, String> {
    let bad = |what: &str| format!("check-runs: {what}");
    let runs = response
        .get("check_runs")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("no check_runs array"))?;
    let total = response
        .get("total_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| bad("no numeric total_count"))?;
    if usize::try_from(total).map_or(true, |total| total > runs.len()) {
        return Err(bad("total_count exceeds the runs read"));
    }
    // The newest run (highest id) and whether it is green.
    let mut newest: Option<(u64, bool)> = None;
    for run in runs {
        let name = run
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("a run has no name"))?;
        let status = run
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("a run has no status"))?;
        let conclusion = match run.get("conclusion") {
            Some(Value::Null) => None,
            Some(Value::String(text)) => Some(text.as_str()),
            _ => return Err(bad("a run has no valid conclusion")),
        };
        let id = run
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| bad("a run has no numeric id"))?;
        if name == CHECK && newest.is_none_or(|(newest_id, _)| id > newest_id) {
            newest = Some((id, status == "completed" && conclusion == Some("success")));
        }
    }
    Ok(newest.is_some_and(|(_, green)| green))
}

/// Whether the body starts with the exact declaration.
fn declared(body: Option<&str>) -> bool {
    body.is_some_and(|text| text.starts_with(DECLARATION))
}

/// The first `#<digits>` in the body, or `-`.
fn ticket(body: Option<&str>) -> String {
    let text = body.unwrap_or_default();
    text.match_indices('#')
        .find_map(|(at, _)| {
            let digits: String = text[at + 1..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            (!digits.is_empty()).then(|| format!("#{digits}"))
        })
        .unwrap_or_else(|| "-".to_owned())
}

#[cfg(test)]
#[path = "break_glass_audit_tests.rs"]
mod tests;
