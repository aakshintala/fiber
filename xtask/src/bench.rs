//! `cargo xtask bench-report`: judges the benchmark harness's result files
//! against the budget table in `docs/performance.md` and writes the pull
//! request comment. Memory and exact budgets fail the run; timing budgets
//! are advisory and show the head and base medians ("When a budget is
//! exceeded"). Ceilings are read from the table at run time; an exact row's
//! formula is code here, so its cell text is pinned.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::rules::{section, table_rows};

/// The comment's first line, which the posting job finds it by.
pub(crate) const MARKER: &str = "<!-- fiber-benchmarks -->";
const SCHEMA: u64 = 1;
/// Each number is the median of this many runs ("Measuring").
const RUNS: usize = 5;
/// The idle window on a pull request, in seconds ("Measuring").
const MIN_IDLE_SECS: u64 = 10;
/// The threads every idle headless session runs with no client.
const SESSION_THREADS: u64 = 5;
/// The threads each client adds: its reader and its writer.
const THREADS_PER_CLIENT: u64 = 2;
/// The fsyncs that bracket each model request, and each tool call.
const FSYNCS_PER_REQUEST: u64 = 2;
const FSYNCS_PER_TOOL_CALL: u64 = 2;
/// The fsyncs are counted in one pass under strace, which slows the
/// process, so it is never a timing or memory sample.
const FSYNC_RUNS: usize = 1;
/// The log bytes a turn may add per tool call beyond its content.
const LOG_BYTES_PER_TOOL_CALL: u64 = 2048;
/// The attaching row's formula, 50 ms plus 10 ms per MiB of session log:
/// its base and its rate, in milliseconds.
const ATTACH_BASE_MS: f64 = 50.0;
const ATTACH_MS_PER_MIB: f64 = 10.0;
/// One MiB in bytes, which the attaching formula counts the log in.
const MIB_BYTES: f64 = 1_048_576.0;
/// The logs the attach workload samples, by fixture name.
const ATTACH_FIXTURES: [&str; 2] = ["1 MiB", "10 MiB"];
/// The row whose ceiling the `web_fetch` conversion must fit in.
const BUSY: &str = "Session, busy or resumed";
/// How the `web_fetch` row's ceiling cell starts, before the busy row's
/// ceiling cell.
const WITHIN: &str = "within the busy session's ";

/// What a benchmarked row checks, and the result-file metric ids it reads.
#[derive(Debug, Clone, Copy)]
enum Check {
    /// Peak RSS in KiB per run, one metric per workload; each median must
    /// not exceed the ceiling.
    Memory(&'static [&'static str]),
    /// Peak RSS in KiB per run, held to `row`'s ceiling: the cell must read
    /// `within the busy session's ` and that row's ceiling cell.
    Within { row: &'static str, id: &'static str },
    /// Milliseconds per run; head and base medians, never failing.
    Timing(&'static str),
    /// Milliseconds per run with each run's log size, judged per fixture
    /// against a formula: head and base medians per fixture, never
    /// failing on a slow sample, failing on a malformed metric.
    PerMib { pin: &'static str, id: &'static str },
    /// A formula held here; the row's ceiling cell must read `pin` exactly.
    Exact { pin: &'static str, rule: Rule },
}

#[derive(Debug, Clone, Copy)]
enum Rule {
    /// Per run, each thread's context switches in the idle window: all zero.
    IdleSwitches(&'static [&'static str]),
    /// Per run, the thread count at each client count.
    Threads(&'static str),
    /// One pass: `fdatasync` calls against the log's model requests and
    /// tool calls.
    Fsyncs(&'static str),
    /// Per run, the turn's log bytes against its content and tool calls.
    LogBytes(&'static str),
}

/// Each benchmarked row of the table, by its Budget cell.
const MEASURED: &[(&str, Check)] = &[
    (
        "Session, idle, headless",
        Check::Memory(&["session_idle_rss_kib"]),
    ),
    ("Terminal, idle", Check::Memory(&["terminal_idle_rss_kib"])),
    (
        BUSY,
        Check::Memory(&[
            "busy_turn_rss_kib",
            "resume_20k_rss_kib",
            "resume_2m_rss_kib",
        ]),
    ),
    (
        "`web_fetch` converting a 10 MiB HTML page, the download cap",
        Check::Within {
            row: BUSY,
            id: "web_fetch_rss_kib",
        },
    ),
    (
        "Idle CPU, session and terminal",
        Check::Exact {
            pin: "zero context switches in the idle window, on every thread",
            rule: Rule::IdleSwitches(&["session_idle_switches", "terminal_idle_switches"]),
        },
    ),
    (
        "Threads, idle headless session",
        Check::Exact {
            pin: "5, plus 2 per client, plus 1 per Lua extension in use",
            rule: Rule::Threads("session_threads"),
        },
    ),
    (
        "fsyncs",
        Check::Exact {
            pin: "2 per model request, 2 per tool call",
            rule: Rule::Fsyncs("fsyncs"),
        },
    ),
    (
        "Log bytes, 429-call turn",
        Check::Exact {
            pin: "the turn's content plus 2 KiB per tool call",
            rule: Rule::LogBytes("turn_log_bytes"),
        },
    ),
    (
        "Session start, the internal session command to its first line, no hub",
        Check::Timing("session_start_ms"),
    ),
    (
        "Terminal to its first frame, new session",
        Check::Timing("terminal_first_frame_ms"),
    ),
    (
        "Terminal to its first frame, attaching",
        Check::PerMib {
            pin: "50 ms plus 10 ms per MiB of session log",
            id: "terminal_attach_ms",
        },
    ),
    (
        "Listing 1,000 sessions in one project, warm cache",
        Check::Timing("sessions_list_ms"),
    ),
    (
        "`paging` jig, its session at scale 1 and 160 by 48",
        Check::Memory(&["paging_rss_kib"]),
    ),
    (
        "`paging` jig, open pass and first frame",
        Check::Timing("paging_open_ms"),
    ),
    (
        "`paging` jig, slowest frame that loaded pages",
        Check::Timing("paging_load_ms"),
    ),
    (
        "`paging` jig, slowest jump frame",
        Check::Timing("paging_jump_ms"),
    ),
    (
        "`paging` jig, slowest re-count at a new width",
        Check::Timing("paging_width_ms"),
    ),
    (
        "`paging` jig, slowest append frame",
        Check::Timing("paging_append_ms"),
    ),
];

/// Each row with no benchmark yet, by its Budget cell, and what owns it.
const NOT_MEASURED: &[(&str, &str)] = &[(
    "`session_list`, waiting for every running session's status",
    "#581 (`session_list`)",
)];

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

/// A ceiling cell's first quantity, in KiB or milliseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Quantity {
    Kib(f64),
    Ms(f64),
}

/// The first `<number> KiB|MiB|ms|s` in a ceiling cell; commas are allowed
/// in the number.
pub(crate) fn ceiling(cell: &str) -> Result<Quantity, String> {
    let words: Vec<&str> = cell.split_whitespace().collect();
    words
        .windows(2)
        .find_map(|pair| {
            let [number, unit] = pair else { return None };
            if !number
                .chars()
                .all(|c| c.is_ascii_digit() || c == ',' || c == '.')
            {
                return None;
            }
            let n: f64 = number.replace(',', "").parse().ok()?;
            match *unit {
                "KiB" => Some(Quantity::Kib(n)),
                "MiB" => Some(Quantity::Kib(n * 1024.0)),
                "ms" => Some(Quantity::Ms(n)),
                "s" => Some(Quantity::Ms(n * 1000.0)),
                _ => None,
            }
        })
        .ok_or_else(|| format!("no `<number> KiB|MiB|ms|s` in {cell:?}"))
}

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

/// The verdict and the comment that shows it.
#[derive(Debug)]
pub(crate) struct Report {
    /// Every broken budget, head self-check and table mismatch; empty passes.
    pub(crate) failures: Vec<String>,
    /// Markdown whose first line is `MARKER`.
    pub(crate) comment: String,
}

/// A parsed result file.
struct Results {
    idle_secs: u64,
    metrics: Map<String, Value>,
    failures: Vec<String>,
}

fn parse_results(text: &str) -> Result<Results, String> {
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

/// The base's state for the timing column.
enum Base {
    /// A push to `main`: no base column.
    None,
    /// The base binary failed; timings show why instead of a median.
    Failed(String),
    Ok(Results),
}

fn metric<'a>(results: Option<&'a Results>, id: &str) -> Result<&'a Value, String> {
    results
        .ok_or("no results")?
        .metrics
        .get(id)
        .ok_or_else(|| format!("{id}: missing"))
}

fn run_median(results: Option<&Results>, id: &str) -> Result<f64, String> {
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
fn runs<'a>(results: Option<&'a Results>, id: &str) -> Result<Vec<&'a [Value]>, String> {
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

fn field(entry: &Value, id: &str, name: &str) -> Result<u64, String> {
    entry
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{id}: {entry} has no {name}"))
}

/// The rule's failures, and what the head column shows.
fn exact(rule: Rule, head: Option<&Results>) -> (Vec<String>, String) {
    match rule {
        Rule::IdleSwitches(ids) => per_thread(List::Switches, ids, head),
        Rule::Threads(id) => per_thread(List::Threads, &[id], head),
        Rule::Fsyncs(id) => per_entry(id, FSYNC_RUNS, head, fsyncs),
        Rule::LogBytes(id) => per_entry(id, RUNS, head, log_bytes),
    }
}

/// What each entry of a per-run list is: a thread's switches, or a thread
/// count.
#[derive(Debug, Clone, Copy)]
enum List {
    Switches,
    Threads,
}

/// A rule whose runs each hold a list of entries.
fn per_thread(list: List, ids: &[&str], head: Option<&Results>) -> (Vec<String>, String) {
    let mut failures = Vec::new();
    let mut observed = Vec::new();
    for id in ids {
        let runs = match runs(head, id) {
            Ok(runs) => runs,
            Err(e) => {
                failures.push(e);
                continue;
            }
        };
        let mut switches = 0;
        for (i, run) in runs.iter().enumerate() {
            for entry in *run {
                let checked = match list {
                    List::Switches => idle_thread(entry, id).map(|(thread, v, n)| {
                        switches += v + n;
                        (v != 0 || n != 0).then(|| {
                            format!("thread {thread}: {v} voluntary, {n} involuntary switches")
                        })
                    }),
                    List::Threads => thread_count(entry, id).map(|(clients, threads)| {
                        let expected = SESSION_THREADS + THREADS_PER_CLIENT * clients;
                        if i == 0 {
                            observed.push(format!("{threads} at {clients} clients"));
                        }
                        (threads != expected).then(|| {
                            format!("{threads} threads at {clients} clients, expected {expected}")
                        })
                    }),
                };
                match checked {
                    Ok(None) => {}
                    Ok(Some(broken)) => failures.push(format!("{id}: run {}: {broken}", i + 1)),
                    Err(e) => failures.push(e),
                }
            }
        }
        if matches!(list, List::Switches) {
            observed.push(format!("{id}: {switches} switches"));
        }
    }
    (failures, observed.join(", "))
}

/// What one entry shows in the head column, and why it breaks its rule.
type Entry = (String, Option<String>);

/// A rule whose metric holds exactly `count` entries, one per run, each
/// judged by `check`. The head column shows the first.
fn per_entry(
    id: &str,
    count: usize,
    head: Option<&Results>,
    check: fn(&Value, &str) -> Result<Entry, String>,
) -> (Vec<String>, String) {
    let entries = match metric(head, id).and_then(|value| {
        value
            .as_array()
            .ok_or_else(|| format!("{id}: not an array"))
    }) {
        Ok(entries) => entries,
        Err(e) => return (vec![e], String::new()),
    };
    if entries.len() != count {
        return (
            vec![format!("{id}: {} runs, expected {count}", entries.len())],
            String::new(),
        );
    }
    let mut failures = Vec::new();
    let mut observed = String::new();
    for (i, entry) in entries.iter().enumerate() {
        match check(entry, id) {
            Ok((shown, broken)) => {
                if i == 0 {
                    observed = shown;
                }
                if let Some(broken) = broken {
                    failures.push(format!("{id}: run {}: {broken}", i + 1));
                }
            }
            Err(e) => failures.push(e),
        }
    }
    (failures, observed)
}

/// Two fsyncs per model request and two per tool call, and at least one.
fn fsyncs(entry: &Value, id: &str) -> Result<Entry, String> {
    let requests = field(entry, id, "model_requests")?;
    let calls = field(entry, id, "tool_calls")?;
    let counted = field(entry, id, "fdatasync")?;
    let expected = FSYNCS_PER_REQUEST * requests + FSYNCS_PER_TOOL_CALL * calls;
    let shown = format!("{counted} fdatasync for {requests} model requests and {calls} tool calls");
    let broken = if counted != expected {
        Some(format!("{shown}, expected {expected}"))
    } else if counted == 0 {
        Some("no fdatasync call was counted".to_owned())
    } else {
        None
    };
    Ok((shown, broken))
}

/// The turn's log bytes: at most its content plus 2 KiB per tool call.
fn log_bytes(entry: &Value, id: &str) -> Result<Entry, String> {
    let bytes = field(entry, id, "bytes")?;
    let content = field(entry, id, "content")?;
    let calls = field(entry, id, "tool_calls")?;
    let limit = content + LOG_BYTES_PER_TOOL_CALL * calls;
    let shown = format!("{bytes} bytes for {content} bytes of content and {calls} tool calls");
    Ok((
        shown.clone(),
        (bytes > limit).then(|| format!("{shown}, over {limit}")),
    ))
}

/// The thread as `tid` or `tid (name)`, and its voluntary and involuntary
/// switches. `name` is optional: older result files carry only `tid`.
fn idle_thread(entry: &Value, id: &str) -> Result<(String, u64, u64), String> {
    let tid = field(entry, id, "tid")?;
    let thread = match entry.get("name") {
        None => tid.to_string(),
        Some(name) => {
            let name = name
                .as_str()
                .ok_or_else(|| format!("{id}: {entry} has a name that is not a string"))?;
            format!("{tid} ({name})")
        }
    };
    Ok((
        thread,
        field(entry, id, "voluntary")?,
        field(entry, id, "involuntary")?,
    ))
}

fn thread_count(entry: &Value, id: &str) -> Result<(u64, u64), String> {
    Ok((field(entry, id, "clients")?, field(entry, id, "threads")?))
}

/// One table row in the comment.
struct Line {
    budget: String,
    ceiling: String,
    head: String,
    base: String,
    result: &'static str,
}

/// Judges `head` (and `base` on a pull request) against the budget table in
/// `performance`, the text of `docs/performance.md`. `base` is the base
/// result file's text, or why it could not be read.
pub(crate) fn report(
    performance: &str,
    head: &str,
    base: Option<Result<String, String>>,
    event: Event,
) -> Report {
    let mut failures = Vec::new();
    let head = match parse_results(head) {
        Ok(results) => Some(results),
        Err(e) => {
            failures.push(format!("head results: {e}"));
            None
        }
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
    for cells in &rows {
        let budget = cells.first().map_or("", String::as_str);
        let cell = cells.get(1).map_or("", String::as_str);
        if let Some((_, check)) = MEASURED.iter().find(|(b, _)| *b == budget) {
            let (broken, line) = judge(budget, cell, *check, head.as_ref(), &base, &rows);
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
    let comment = comment(&lines, &failures, &base, idle_secs);
    Report { failures, comment }
}

/// One row's failures and its comment line.
fn judge(
    budget: &str,
    cell: &str,
    check: Check,
    head: Option<&Results>,
    base: &Base,
    rows: &[Vec<String>],
) -> (Vec<String>, Line) {
    let mut failures = Vec::new();
    let mut line = Line {
        budget: budget.to_owned(),
        ceiling: cell.to_owned(),
        head: String::new(),
        base: String::new(),
        result: "pass",
    };
    match check {
        Check::Memory(ids) => line.head = memory(cell, ids, head, &mut failures),
        Check::Within { row, id } => {
            let other = rows
                .iter()
                .find(|cells| cells.first().map(String::as_str) == Some(row))
                .and_then(|cells| cells.get(1));
            match other {
                None => failures.push(format!("no {row:?} row holds its ceiling")),
                Some(other) => {
                    let expected = format!("{WITHIN}{other}");
                    if cell != expected {
                        failures.push(format!(
                            "the ceiling reads {cell:?}; it must read {expected:?}, the {row:?} row's ceiling"
                        ));
                    }
                    line.head = memory(other, &[id], head, &mut failures);
                }
            }
        }
        Check::PerMib { pin, id } => {
            line.result = "advisory";
            if cell != pin {
                failures.push(format!(
                    "the ceiling reads {cell:?}; its formula is code, so change xtask/src/bench.rs with it"
                ));
            }
            let (broken, shown) = per_mib(id, head);
            failures.extend(broken);
            line.head = shown;
            line.base = match base {
                Base::None => String::new(),
                Base::Failed(why) => format!("base failed: {why}"),
                Base::Ok(results) if !results.metrics.contains_key(id) => "unavailable".to_owned(),
                Base::Ok(results) => {
                    let (broken, shown) = per_mib(id, Some(results));
                    match broken.first() {
                        None => shown,
                        Some(first) => format!("base failed: {first}"),
                    }
                }
            };
        }
        Check::Timing(id) => {
            line.result = "advisory";
            match ceiling(cell) {
                Ok(Quantity::Ms(_)) => {}
                Ok(Quantity::Kib(_)) => failures.push(format!("{cell:?} is not a timing ceiling")),
                Err(e) => failures.push(e),
            }
            match run_median(head, id) {
                Ok(value) => line.head = format!("{value:.1} ms"),
                Err(e) => failures.push(e),
            }
            line.base = match base {
                Base::None => String::new(),
                Base::Failed(why) => format!("base failed: {why}"),
                // A base that ran cleanly but predates the workload has
                // nothing to compare.
                Base::Ok(results) if !results.metrics.contains_key(id) => "unavailable".to_owned(),
                Base::Ok(results) => run_median(Some(results), id)
                    .map_or_else(|e| format!("base failed: {e}"), |v| format!("{v:.1} ms")),
            };
        }
        Check::Exact { pin, rule } => {
            if cell != pin {
                failures.push(format!(
                    "the ceiling reads {cell:?}; its formula is code, so change xtask/src/bench.rs with it"
                ));
            }
            let (broken, observed) = exact(rule, head);
            failures.extend(broken);
            line.head = observed;
        }
    }
    if !failures.is_empty() {
        line.result = "fail";
    }
    (failures, line)
}

/// Each metric's median against the memory ceiling in `cell`, and what the
/// head column shows.
fn memory(cell: &str, ids: &[&str], head: Option<&Results>, failures: &mut Vec<String>) -> String {
    let limit = match ceiling(cell) {
        Ok(Quantity::Kib(limit)) => limit,
        Ok(Quantity::Ms(_)) => {
            failures.push(format!("{cell:?} is not a memory ceiling"));
            return String::new();
        }
        Err(e) => {
            failures.push(e);
            return String::new();
        }
    };
    let mut shown = Vec::new();
    for id in ids {
        match run_median(head, id) {
            Ok(value) => {
                shown.push(format!("{id}: {value:.0} KiB"));
                if value > limit {
                    failures.push(format!(
                        "median {value:.0} KiB of {id} is over {limit:.0} KiB"
                    ));
                }
            }
            Err(e) => failures.push(e),
        }
    }
    shown.join(", ")
}

/// Each fixture's samples against the attaching formula, and what the
/// head or base column shows: one `fixture (bytes): median ms of ceiling
/// ms` part per fixture. A slow median is advisory and adds no failure;
/// a malformed metric is one failure naming `id`.
fn per_mib(id: &str, results: Option<&Results>) -> (Vec<String>, String) {
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
    for fixture in ATTACH_FIXTURES {
        let Some(samples) = by_fixture.get(fixture) else {
            failures.push(format!("{id}: no {fixture} fixture"));
            continue;
        };
        if samples.len() != RUNS {
            failures.push(format!(
                "{id}: {fixture}: {} runs, expected {RUNS}",
                samples.len()
            ));
            continue;
        }
        let bytes = samples.iter().map(|(bytes, _)| *bytes).max().unwrap_or(0);
        let ms: Vec<f64> = samples.iter().map(|(_, ms)| *ms).collect();
        match median(&ms) {
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

fn comment(lines: &[Line], failures: &[String], base: &Base, idle_secs: Option<u64>) -> String {
    let with_base = !matches!(base, Base::None);
    let mut out = format!("{MARKER}\n## Benchmarks\n\n");
    out.push_str(
        "Exact and memory budgets fail the run; timing budgets are advisory. \
         Each number is the median of 5 runs on Linux x86_64 (`docs/performance.md`).\n",
    );
    if let Some(secs) = idle_secs {
        out.push_str(&format!("The idle window was {secs} s.\n"));
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
        out.push_str(&format!("- {budget}: {owner}\n"));
    }
    out
}

#[cfg(test)]
#[path = "bench_tests.rs"]
mod tests;
