use serde_json::Value;

use super::attach::per_mib;
use super::results::{Results, field, metric, run_median, runs};

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
/// The row whose ceiling the `web_fetch` conversion must fit in.
const BUSY: &str = "Session, busy or resumed";
/// How the `web_fetch` row's ceiling cell starts, before the busy row's
/// ceiling cell.
const WITHIN: &str = "within the busy session's ";

/// What a benchmarked row checks, and the result-file metric ids it reads.
#[derive(Debug, Clone, Copy)]
pub(super) enum Check {
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
    /// A paired growth ratio per run: each run's `large` over its `small`
    /// must not exceed `times`. The row's ceiling cell must read `pin`
    /// exactly, and the pin and the multiplier change together in code.
    /// Unlike memory and exact budgets, a ratio overage never earns the
    /// stored-base exemption: it fails the run outright.
    Ratio {
        pin: &'static str,
        times: f64,
        small: &'static str,
        large: &'static str,
    },
    /// A formula held here; the row's ceiling cell must read `pin` exactly.
    Exact { pin: &'static str, rule: Rule },
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Rule {
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
pub(super) const MEASURED: &[(&str, Check)] = &[
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
        "Terminal, attaching a 10 MiB session",
        Check::Memory(&["terminal_attach_rss_kib"]),
    ),
    (
        "Session, memory kept after the busy turn",
        Check::Memory(&["busy_turn_retained_kib"]),
    ),
    (
        "Terminal, memory kept after attaching a 10 MiB session",
        Check::Memory(&["terminal_attach_retained_kib"]),
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
    (
        "`paging` jig at scale 16, slowest re-count at a new width",
        Check::Ratio {
            pin: "16 times the same run's scale-1 re-count",
            times: 16.0,
            small: "paging_width_ms",
            large: "paging_scale16_width_ms",
        },
    ),
    (
        "`paging` jig at scale 16, a search of the whole session with no match",
        Check::Ratio {
            pin: "16 times the same run's scale-1 search",
            times: 16.0,
            small: "paging_search_ms",
            large: "paging_scale16_search_ms",
        },
    ),
    (
        "Terminal, wheel to frame, the `paging` jig's session at scale 1, 160 by 48",
        Check::Timing("wheel_paging_ms"),
    ),
    (
        "Terminal, wheel to frame, one turn with an opened `read` of 300 lines, 160 by 48",
        Check::Timing("wheel_read_ms"),
    ),
];

/// Each row with no benchmark yet, by its Budget cell, and what owns it.
pub(super) const NOT_MEASURED: &[(&str, &str)] = &[
    (
        "`session_list`, waiting for every running session's status",
        "#581 (`session_list`)",
    ),
    (
        "Session, memory kept after 10 busy turns",
        "#2188 (the status fold's tool names)",
    ),
];

/// The idle rows' RSS-split diagnostics, by Budget cell: the anon and file
/// metric ids whose head medians the comment shows after the peak RSS.
/// Diagnostics are shown, never judged: a missing one is omitted.
const RSS_SPLIT: &[(&str, (&str, &str))] = &[
    (
        "Session, idle, headless",
        ("session_idle_rss_anon_kib", "session_idle_rss_file_kib"),
    ),
    (
        "Terminal, idle",
        ("terminal_idle_rss_anon_kib", "terminal_idle_rss_file_kib"),
    ),
];

/// A ceiling cell's first quantity, in KiB or milliseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Quantity {
    Kib(f64),
    Ms(f64),
}

/// The first `<number> KiB|MiB|ms|s` in a ceiling cell; commas are allowed
/// in the number.
pub(super) fn ceiling(cell: &str) -> Result<Quantity, String> {
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

/// The base's state for the timing column.
pub(super) enum Base {
    /// A push to `main`: no base column.
    None,
    /// The base binary failed; timings show why instead of a median.
    Failed(String),
    Ok(Results),
}

/// The rule's failures, and what the head column shows.
fn exact(rule: Rule, head: Option<&Results>) -> (Vec<String>, String) {
    match rule {
        Rule::IdleSwitches(ids) => per_thread(List::Switches, ids, head),
        Rule::Threads(id) => per_thread(List::Threads, &[id], head),
        Rule::Fsyncs(id) => per_entry(id, FSYNC_RUNS, head, fsyncs),
        Rule::LogBytes(id) => {
            // `per_entry` fails on the missing metric first, so the count
            // is only read when results exist.
            let count = head.map(|results| results.runs_of(id)).unwrap_or(0);
            per_entry(id, count, head, log_bytes)
        }
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
                    Ok(Some(broken)) => {
                        failures.push(format!("{OVER}{id}: run {}: {broken}", i + 1));
                    }
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
                    failures.push(format!("{OVER}{id}: run {}: {broken}", i + 1));
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
pub(super) struct Line {
    pub(super) budget: String,
    pub(super) ceiling: String,
    pub(super) head: String,
    pub(super) base: String,
    pub(super) result: &'static str,
}

/// The head's results, the stored results when they parsed, and the commit
/// the stored results are from.
pub(super) type Measured<'a> = (Option<&'a Results>, Option<&'a Results>, &'a str);

/// Starts each failure that is a measurement over its budget, as opposed to
/// a result that is missing or malformed.
const OVER: &str = "over budget: ";

/// Whether `failures` are non-empty and all measured violations. Only these
/// can be excused by a base that has them too.
fn all_over(failures: &[String]) -> bool {
    !failures.is_empty() && failures.iter().all(|f| f.starts_with(OVER))
}

/// One row's failures and its comment line. A memory or exact budget the base
/// fails too does not fail the row: it reads "over at base".
pub(super) fn judge(
    budget: &str,
    cell: &str,
    check: Check,
    (head, base_results, base_commit): Measured<'_>,
    base: &Base,
    rows: &[Vec<String>],
) -> (Vec<String>, Line) {
    let mut measured = Vec::new();
    let mut within = None;
    let mut failures = Vec::new();
    let mut line = Line {
        budget: budget.to_owned(),
        ceiling: cell.to_owned(),
        head: String::new(),
        base: String::new(),
        result: "pass",
    };
    match check {
        Check::Memory(ids) => {
            line.head = memory(cell, ids, head, &mut measured);
            if let Some((anon, file)) = RSS_SPLIT
                .iter()
                .find(|(row, _)| *row == budget)
                .map(|(_, ids)| *ids)
            {
                line.head.push_str(&rss_split(head, anon, file));
            }
        }
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
                    line.head = memory(other, &[id], head, &mut measured);
                    within = Some((other.clone(), id));
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
            measured.extend(broken);
            line.head = observed;
        }
        Check::Ratio {
            pin,
            times,
            small,
            large,
        } => {
            // The ceiling text is code: a changed multiplier or wording
            // fails here with the budget, never silently keeping 16x
            // enforcement. A ratio overage fails the run outright below:
            // it never reaches the stored-base exemption.
            if cell != pin {
                failures.push(format!(
                    "the ceiling reads {cell:?}; it must read {pin:?} for budget {budget:?}: change xtask/src/bench/judge.rs with it"
                ));
            }
            match paired_ratio(head, small, large, times) {
                Ok((shown, None)) => line.head = shown,
                Ok((shown, Some(over))) => {
                    line.head = shown;
                    failures.push(over);
                }
                Err(malformed) => failures.push(malformed),
            }
            // The base ratio is a diagnostic, never authority to waive a
            // head failure; a base predating the workload shows
            // unavailable.
            line.base = match base {
                Base::None => String::new(),
                Base::Failed(why) => format!("base failed: {why}"),
                Base::Ok(results)
                    if !results.metrics.contains_key(small)
                        || !results.metrics.contains_key(large) =>
                {
                    "unavailable".to_owned()
                }
                Base::Ok(results) => match paired_ratio(Some(results), small, large, times) {
                    Ok((shown, _)) => shown,
                    Err(first) => format!("base failed: {first}"),
                },
            };
        }
    }
    if !measured.is_empty() {
        let base_failures = base_results.map(|b| match check {
            Check::Memory(ids) => {
                let mut f = Vec::new();
                memory(cell, ids, Some(b), &mut f);
                f
            }
            Check::Within { .. } => {
                let mut f = Vec::new();
                if let Some((other, id)) = &within {
                    memory(other, &[id], Some(b), &mut f);
                }
                f
            }
            Check::Exact { rule, .. } => exact(rule, Some(b)).0,
            Check::Timing(_) | Check::PerMib { .. } | Check::Ratio { .. } => Vec::new(),
        });
        if all_over(&measured) && base_failures.is_some_and(|f| all_over(&f)) {
            line.base = format!("over at base {base_commit}");
            line.result = "over at base";
        } else {
            failures.extend(measured);
        }
    }
    if !failures.is_empty() {
        line.result = "fail";
    }
    (failures, line)
}

/// One idle row's RSS-split suffix for the head column: the anon and file
/// medians after the peak, ` (anon 4100, file 4800)`. A diagnostic that is
/// missing or malformed is omitted, never a failure.
fn rss_split(head: Option<&Results>, anon: &str, file: &str) -> String {
    let part = |id: &str, name: &str| {
        run_median(head, id)
            .ok()
            .map(|value| format!("{name} {value:.0}"))
    };
    let parts: Vec<String> = [part(anon, "anon"), part(file, "file")]
        .into_iter()
        .flatten()
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

/// One metric's per-run numbers: exactly its declared run count, each a
/// finite number at or above zero. A missing, misshapen or out-of-count
/// metric is a configuration failure, never a slow sample.
fn samples_of(results: Option<&Results>, id: &str) -> Result<Vec<f64>, String> {
    let values = metric(results, id)?
        .as_array()
        .ok_or_else(|| format!("{id}: not an array"))?;
    let runs = results.map(|results| results.runs_of(id)).unwrap_or(0);
    if values.len() != runs {
        return Err(format!("{id}: {} runs, expected {runs}", values.len()));
    }
    values
        .iter()
        .map(|value| match value.as_f64() {
            Some(value) if value.is_finite() && value >= 0.0 => Ok(value),
            Some(_) | None => Err(format!(
                "{id}: {value} is not a finite number at or above zero"
            )),
        })
        .collect()
}

/// The median paired growth of `large` over `small`: each run's large
/// against the same run's small, sorted without dividing, judged as
/// `large <= times * small` on the median pair. Returns what the column
/// shows, and the overage when the median pair breaks the ceiling. Zero
/// over zero is zero growth; positive over zero is infinite growth and
/// fails. A non-finite `times * small` bound is a configuration failure,
/// never silently accepted overflow.
fn paired_ratio(
    results: Option<&Results>,
    small: &str,
    large: &str,
    times: f64,
) -> Result<(String, Option<String>), String> {
    let small = samples_of(results, small)?;
    let large = samples_of(results, large)?;
    if small.len() != large.len() {
        return Err(format!(
            "paired samples differ: {} runs of one metric, {} of the other",
            small.len(),
            large.len()
        ));
    }
    if small.is_empty() {
        return Err("no runs to pair".to_owned());
    }
    let mut pairs: Vec<(f64, f64)> = small.into_iter().zip(large).collect();
    // Least growth first, without dividing: zero growth (0/0), then
    // finite ratios by cross-multiplication, then infinite growth
    // (+/0), so zeros and infinities sort without a single NaN.
    pairs.sort_by(|(s1, l1), (s2, l2)| {
        let class = |small: f64, large: f64| {
            if small == 0.0 {
                if large == 0.0 { 0 } else { 2 }
            } else {
                1
            }
        };
        match (class(*s1, *l1), class(*s2, *l2)) {
            (a, b) if a != b => a.cmp(&b),
            (1, 1) => {
                let (left, right) = (l1 * s2, l2 * s1);
                match (left.is_finite(), right.is_finite()) {
                    (true, true) => left.total_cmp(&right),
                    // Overflowed cross products fall back to the
                    // quotient, which stays ordered (never NaN: small is
                    // nonzero and both sides finite here).
                    _ => (l1 / s1).total_cmp(&(l2 / s2)),
                }
            }
            _ => std::cmp::Ordering::Equal,
        }
    });
    let Some(&(median_small, median_large)) = pairs.get(pairs.len() / 2) else {
        return Err("no runs to pair".to_owned());
    };
    let shown = if median_small == 0.0 {
        if median_large == 0.0 {
            format!("0.0x of {times:.0}x allowed")
        } else {
            format!("infinite of {times:.0}x allowed")
        }
    } else {
        format!("{:.1}x of {times:.0}x allowed", median_large / median_small)
    };
    if median_small == 0.0 && median_large == 0.0 {
        return Ok((shown, None));
    }
    let bound = times * median_small;
    if !bound.is_finite() {
        return Err(format!(
            "{times}x of {median_small} overflows: cannot judge the paired growth"
        ));
    }
    if median_large <= bound {
        Ok((shown, None))
    } else {
        Ok((
            shown,
            Some(format!(
                "{OVER}median paired growth is over {times:.0}x: {median_large:.1} ms over {median_small:.1} ms"
            )),
        ))
    }
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
                        "{OVER}median {value:.0} KiB of {id} is over {limit:.0} KiB"
                    ));
                }
            }
            Err(e) => failures.push(e),
        }
    }
    shown.join(", ")
}
