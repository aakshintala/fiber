//! `cargo xtask bench-report`: judges the benchmark harness's result files
//! against the budget table in `docs/performance.md` and writes the pull
//! request comment. Exact, memory and paging growth-ratio budgets fail the
//! run; absolute timing budgets are advisory and show the head and base
//! medians ("When a budget is exceeded"). The comment also names the stripped binary's size each
//! compared file records, and the head-minus-base change when both record
//! one. Ceilings are read from the table at run time; an exact row's
//! formula is code here, so its cell text is pinned. On a pull request the
//! live base feeds only the timing columns; memory and exact rows compare
//! against the backstop's stored results for the base commit.

use crate::rules::{section, table_rows};

mod attach;
mod judge;
mod results;

use self::judge::{
    Base, Line, MEASURED, NOT_MEASURED, TEN_TURNS_BUDGET, TEN_TURNS_SKIP_NOTE, judge,
    ten_turns_skipped,
};
#[cfg(test)]
use self::judge::{Quantity, ceiling};
#[cfg(test)]
use self::results::median;
use self::results::parse_results;

/// The comment's first line, which the posting job finds it by.
pub(crate) const MARKER: &str = "<!-- fiber-benchmarks -->";
/// The idle window on a pull request, in seconds ("Measuring").
const MIN_IDLE_SECS: u64 = 10;

/// The run's trigger, from `GITHUB_EVENT_NAME`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Event {
    /// Head and base are both measured; timings compare them.
    PullRequest,
    /// The backstop on `main`: the head alone.
    Push,
}

impl Event {
    pub(crate) fn parse(name: &str) -> Result<Self, String> {
        match name {
            "pull_request" => Ok(Self::PullRequest),
            "push" => Ok(Self::Push),
            other => Err(format!("--event must be pull_request or push, not {other}")),
        }
    }
}

/// The verdict and the comment that shows it.
#[derive(Debug)]
pub(crate) struct Report {
    /// Every broken budget, head self-check and table mismatch; empty passes.
    pub(crate) failures: Vec<String>,
    /// Markdown whose first line is `MARKER`.
    pub(crate) comment: String,
}

/// The backstop's stored results for the base or its nearest stored
/// ancestor: the commit they are from, and the file's text or why it could
/// not be read.
pub(crate) struct Stored {
    pub(crate) commit: String,
    pub(crate) text: Result<String, String>,
}

/// Judges `head` (and `base` on a pull request) against the budget table in
/// `performance`, the text of `docs/performance.md`. `base` is the base
/// result file's text, or why it could not be read. `stored` is the
/// backstop's stored results the memory and exact rows compare against.
/// `base_commit` names the commit the base binary is from.
pub(crate) fn report(
    performance: &str,
    head: &str,
    base: Option<Result<String, String>>,
    stored: Option<Stored>,
    event: Event,
    base_commit: &str,
) -> Report {
    let mut failures = Vec::new();
    let head = match parse_results(head) {
        Ok(results) => Some(results),
        Err(e) => {
            failures.push(format!("head results: {e}"));
            None
        }
    };
    // The stored results feed only over-at-base, and only on a pull
    // request: the backstop must fail on a budget the base fails.
    let (stored_results, stored_why): (Option<results::Results>, Option<String>) =
        match (event, &stored) {
            (Event::PullRequest, Some(stored)) => match &stored.text {
                Ok(text) => match parse_results(text) {
                    Ok(results) => (Some(results), None),
                    Err(e) => (None, Some(e)),
                },
                Err(e) => (None, Some(e.clone())),
            },
            (Event::Push, _) | (_, None) => (None, None),
        };
    if let Some(results) = &head {
        failures.extend(results.failures.iter().map(|f| format!("self-check: {f}")));
        if results.idle_secs < MIN_IDLE_SECS {
            failures.push(format!(
                "idle window of {} s, expected at least {MIN_IDLE_SECS} s",
                results.idle_secs
            ));
        }
    }
    let base = match (event, base) {
        (Event::Push, _) => Base::None,
        (Event::PullRequest, None) => {
            failures.push("a pull request run needs --base".to_owned());
            Base::Failed("no base file".to_owned())
        }
        (Event::PullRequest, Some(Err(e))) => Base::Failed(e),
        (Event::PullRequest, Some(Ok(text))) => match parse_results(&text) {
            Ok(results) if results.failures.is_empty() => Base::Ok(results),
            Ok(results) => Base::Failed(results.failures.join("; ")),
            Err(e) => Base::Failed(e),
        },
    };

    let rows = section(performance, "Budgets")
        .map(|lines| table_rows(&lines))
        .unwrap_or_default();
    if rows.is_empty() {
        failures.push("docs/performance.md has no table under \"Budgets\"".to_owned());
    }
    let mut lines = Vec::new();
    // Over-at-base names the stored commit: the live base feeds only the
    // timing columns.
    let stored_commit = stored.as_ref().map(|s| s.commit.as_str()).unwrap_or("");
    for cells in &rows {
        let budget = cells.first().map_or("", String::as_str);
        let cell = cells.get(1).map_or("", String::as_str);
        if let Some((_, check)) = MEASURED.iter().find(|(b, _)| *b == budget) {
            let (broken, line) = judge(
                budget,
                cell,
                *check,
                (head.as_ref(), stored_results.as_ref(), stored_commit),
                &base,
                &rows,
            );
            failures.extend(broken.into_iter().map(|f| format!("{budget}: {f}")));
            lines.push(line);
        } else if !NOT_MEASURED.iter().any(|(b, _)| *b == budget) {
            failures.push(format!(
                "{budget}: a budget row with no benchmark and no \"not measured\" entry in xtask/src/bench.rs"
            ));
        }
    }
    for budget in MEASURED
        .iter()
        .map(|(b, _)| *b)
        .chain(NOT_MEASURED.iter().map(|(b, _)| *b))
    {
        if !rows
            .iter()
            .any(|cells| cells.first().map(String::as_str) == Some(budget))
        {
            failures.push(format!(
                "{budget}: listed in xtask/src/bench.rs but not a row of the table in docs/performance.md"
            ));
        }
    }

    let idle_secs = head.as_ref().map(|h| h.idle_secs);
    let mut cpus = cpu_lines(
        head.as_ref(),
        &base,
        base_commit,
        stored_results.as_ref(),
        stored_commit,
    );
    cpus.extend(size_lines(head.as_ref(), &base, base_commit));
    let comment = comment(
        &lines,
        &failures,
        &base,
        idle_secs,
        head.as_ref(),
        stored_line(
            &stored,
            stored_results.as_ref(),
            stored_why.as_deref(),
            event,
        )
        .as_deref(),
        &cpus,
    );
    Report { failures, comment }
}

/// The run-count sentence for the head file: the full count, and the
/// timing-only count when it differs.
fn run_sentence(head: Option<&results::Results>) -> String {
    match head {
        None => "Each number is the median of its runs on Linux x86_64 (`docs/performance.md`)."
            .to_owned(),
        Some(results) if results.timing_runs != results.runs => format!(
            "Each number is the median of {} runs on Linux x86_64, a timing-only workload's of {} (`docs/performance.md`).",
            results.runs, results.timing_runs
        ),
        Some(results) => format!(
            "Each number is the median of {} runs on Linux x86_64 (`docs/performance.md`).",
            results.runs
        ),
    }
}

/// The stored-results sentence on a pull request: which stored results the
/// memory and exact rows compare against, or why they are judged against
/// the table alone. A push judges the head alone and carries none.
fn stored_line(
    stored: &Option<Stored>,
    parsed: Option<&results::Results>,
    why: Option<&str>,
    event: Event,
) -> Option<String> {
    match event {
        Event::Push => None,
        Event::PullRequest => Some(match (stored, parsed, why) {
            (Some(stored), Some(_), _) => format!(
                "Memory and exact rows compare against the backstop's stored results for {}.",
                stored.commit
            ),
            (Some(stored), None, Some(why)) => format!(
                "The stored results for {} could not be read ({why}): memory and exact rows are judged against the table alone.",
                stored.commit
            ),
            (Some(stored), None, None) => format!(
                "The stored results for {} could not be read (no reason given): memory and exact rows are judged against the table alone.",
                stored.commit
            ),
            (None, _, _) => "No stored results for the base or its 9 nearest first-parent ancestors: memory and exact rows are judged against the table alone."
                .to_owned(),
        }),
    }
}

/// The CPU models each compared file records, in order: the head's, the
/// live base's, the stored results'. A file without one shows `not
/// recorded`; a file that did not parse shows no line.
fn cpu_lines(
    head: Option<&results::Results>,
    base: &Base,
    base_commit: &str,
    stored: Option<&results::Results>,
    stored_commit: &str,
) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(results) = head {
        lines.push(format!(
            "- Head CPU: {}",
            results.cpu.as_deref().unwrap_or("not recorded")
        ));
    }
    if let Base::Ok(results) = base {
        let cpu = results.cpu.as_deref().unwrap_or("not recorded");
        if base_commit.is_empty() {
            lines.push(format!("- Base CPU: {cpu}"));
        } else {
            lines.push(format!("- Base CPU ({base_commit}): {cpu}"));
        }
    }
    if let Some(results) = stored {
        lines.push(format!(
            "- Stored results CPU ({stored_commit}): {}",
            results.cpu.as_deref().unwrap_or("not recorded")
        ));
    }
    lines
}

/// The stripped binary sizes each compared file records, in order: the
/// head's, the live base's, and the head-minus-base change when both
/// record a size. A file without one shows `not recorded`; a file that
/// did not parse shows no line.
fn size_lines(head: Option<&results::Results>, base: &Base, base_commit: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let head_size = head.and_then(|results| results.binary_size_bytes);
    if head.is_some() {
        match head_size {
            Some(size) => lines.push(format!("- Head size: {size} bytes")),
            None => lines.push("- Head size: not recorded".to_owned()),
        }
    }
    if let Base::Ok(results) = base {
        let shown = results
            .binary_size_bytes
            .map_or("not recorded".to_owned(), |size| format!("{size} bytes"));
        if base_commit.is_empty() {
            lines.push(format!("- Base size: {shown}"));
        } else {
            lines.push(format!("- Base size ({base_commit}): {shown}"));
        }
        if let (Some(head), Some(base)) = (head_size, results.binary_size_bytes) {
            let change = i128::from(head) - i128::from(base);
            lines.push(format!("- Size change, head - base: {change:+} bytes"));
        }
    }
    lines
}

fn comment(
    lines: &[Line],
    failures: &[String],
    base: &Base,
    idle_secs: Option<u64>,
    head: Option<&results::Results>,
    stored_line: Option<&str>,
    cpus: &[String],
) -> String {
    let ten_turns_skipped = ten_turns_skipped(head);
    let run_sentence = run_sentence(head);
    let with_base = !matches!(base, Base::None);
    let mut out = format!("{MARKER}\n## Benchmarks\n\n");
    out.push_str("Exact, memory and paging growth-ratio budgets fail the run; absolute timing budgets are advisory. ");
    out.push_str(&run_sentence);
    out.push('\n');
    if let Some(secs) = idle_secs {
        out.push_str(&format!("The idle window was {secs} s.\n"));
    }
    if let Some(line) = stored_line {
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    for cpu in cpus {
        out.push_str(cpu);
        out.push('\n');
    }
    out.push('\n');
    if with_base {
        out.push_str("| Budget | Ceiling | Head | Base | Result |\n|---|---|---|---|---|\n");
    } else {
        out.push_str("| Budget | Ceiling | Head | Result |\n|---|---|---|---|\n");
    }
    for line in lines {
        out.push_str(&format!(
            "| {} | {} | {} |",
            line.budget, line.ceiling, line.head
        ));
        if with_base {
            out.push_str(&format!(" {} |", line.base));
        }
        out.push_str(&format!(" {} |\n", line.result));
    }
    if !failures.is_empty() {
        out.push_str("\n### Failures\n\n");
        for failure in failures {
            out.push_str(&format!("- {failure}\n"));
        }
    }
    out.push_str("\n### Not measured\n\n");
    for (budget, owner) in NOT_MEASURED {
        if *budget == TEN_TURNS_BUDGET && ten_turns_skipped {
            // The row does not gate, but the selection's skip is still
            // recorded in the head file, so the summary says so
            // (`docs/ci.md`, "Selection").
            out.push_str(&format!("- {budget}: {owner} ({TEN_TURNS_SKIP_NOTE})\n"));
        } else {
            out.push_str(&format!("- {budget}: {owner}\n"));
        }
    }
    out
}

#[cfg(test)]
#[path = "bench_tests.rs"]
mod tests;
