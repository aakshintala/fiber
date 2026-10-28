use serde_json::Value;

use super::attach::{fixture_medians, fold_per_mib, fold_rates};
use super::results::{Results, field, median, metric, run_median, runs};

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
/// The anon RSS the tenth turn may keep past the first, in KiB: 1 MiB.
const TEN_TURN_GROWTH_KIB: u64 = 1024;
/// The ten-turn pass's workload name, as the harness records it under
/// `skipped` when the selection leaves it out.
const TEN_TURNS_WORKLOAD: &str = "10 busy turns";
/// The log bytes a turn may add per tool call beyond its content.
const LOG_BYTES_PER_TOOL_CALL: u64 = 2048;
/// The row whose ceiling the `web_fetch` conversion must fit in.
const BUSY: &str = "Session, busy or resumed";
/// How the `web_fetch` row's ceiling cell starts, before the busy row's
/// ceiling cell.
const WITHIN: &str = "within the busy session's ";
/// Full CPU model strings for the timing speed classes, matched
/// case-insensitively: a model's class is the slowest cluster its stored
/// runs reach. Counts are stored result files, not samples in one file.
const FAST_CPU: &str = "AMD EPYC 9V45 96-Core Processor";
const MIDDLE_CPU: &str = "Intel(R) Xeon(R) Platinum 8370C CPU @ 2.80GHz";
const SLOW_7763_CPU: &str = "AMD EPYC 7763 64-Core Processor";
const SLOW_9V74_CPU: &str = "AMD EPYC 9V74 80-Core Processor";
/// A recorded model with no speed class: one stored file, advisory.
const UNCLASSIFIED_8573C_CPU: &str = "INTEL(R) XEON(R) PLATINUM 8573C";
const FAST_STORED: usize = 6;
const MIDDLE_STORED: usize = 2;
const SLOW_7763_STORED: usize = 13;
const SLOW_9V74_STORED: usize = 4;
const UNCLASSIFIED_8573C_STORED: usize = 1;
/// A CPU model gates only with at least this many stored result files.
const MIN_STORED_RUNS: usize = 5;
/// Why a row without a comparable same-job base timing stays advisory.
const NO_BASE_REASON: &str = "advisory: no comparable same-job base timing";
/// Why the attach rows stay advisory on a middle model: middle never
/// gates them, even with enough stored files to gate scalar rows.
const MIDDLE_ATTACH_REASON: &str = "advisory: the middle speed class never gates the attach rows";

/// A runner speed class for the timing gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpeedClass {
    Fast,
    Middle,
    Slow,
}

/// What the head CPU model means for the timing gates. Eligibility
/// derives from the stored evidence, so an eligible model always has a
/// class and enough stored runs: no flag can contradict them.
enum ModelInfo {
    /// A classified model with at least `MIN_STORED_RUNS` stored files.
    Eligible { class: SpeedClass },
    /// A classified model with fewer stored files than gate.
    FewRuns { class: SpeedClass, stored: usize },
    /// An unclassified, unknown or missing model; `stored` is the known
    /// file count when the model is recorded but unclassified.
    Advisory { stored: Option<usize> },
}

/// The head CPU model's class, stored-file count and whether it gates.
/// Matching is ASCII case-insensitive; anything else is unknown. The
/// single `stored >= MIN_STORED_RUNS` check below is the only place
/// eligibility is decided, from the class and count it just looked up.
fn model_info(cpu: Option<&str>) -> ModelInfo {
    let Some(model) = cpu.map(str::trim).filter(|m| !m.is_empty()) else {
        return ModelInfo::Advisory { stored: None };
    };
    let evidence = if model.eq_ignore_ascii_case(FAST_CPU) {
        (Some(SpeedClass::Fast), Some(FAST_STORED))
    } else if model.eq_ignore_ascii_case(MIDDLE_CPU) {
        (Some(SpeedClass::Middle), Some(MIDDLE_STORED))
    } else if model.eq_ignore_ascii_case(SLOW_7763_CPU) {
        (Some(SpeedClass::Slow), Some(SLOW_7763_STORED))
    } else if model.eq_ignore_ascii_case(SLOW_9V74_CPU) {
        (Some(SpeedClass::Slow), Some(SLOW_9V74_STORED))
    } else if model.eq_ignore_ascii_case(UNCLASSIFIED_8573C_CPU) {
        (None, Some(UNCLASSIFIED_8573C_STORED))
    } else {
        (None, None)
    };
    match evidence {
        (Some(class), Some(stored)) if stored >= MIN_STORED_RUNS => ModelInfo::Eligible { class },
        (Some(class), Some(stored)) => ModelInfo::FewRuns { class, stored },
        (_, stored) => ModelInfo::Advisory { stored },
    }
}

/// Why the head CPU model stays advisory, naming its stored evidence.
fn model_advisory(cpu: Option<&str>, info: &ModelInfo) -> String {
    let model = cpu.map(str::trim).filter(|m| !m.is_empty());
    let runs = |stored: usize| if stored == 1 { "run" } else { "runs" };
    match (model, info) {
        (Some(model), ModelInfo::FewRuns { stored, .. }) => {
            format!(
                "advisory: {model} has {stored} stored {}; at least {MIN_STORED_RUNS} are required",
                runs(*stored)
            )
        }
        (
            Some(model),
            ModelInfo::Advisory {
                stored: Some(stored),
            },
        ) => {
            format!(
                "advisory: {model} is not a classified timing model ({stored} stored {}; at least {MIN_STORED_RUNS} are required)",
                runs(*stored)
            )
        }
        _ => "advisory: CPU model is unknown or not recorded".to_owned(),
    }
}

/// The class ceilings and tolerances from a budget row's appended cells:
/// fast, middle and slow ceilings in ms, then fast and slow tolerances
/// in percent. Middle has no tolerance because no middle model gates.
struct ClassGates {
    fast_ceiling: f64,
    middle_ceiling: f64,
    slow_ceiling: f64,
    fast_tolerance: f64,
    slow_tolerance: f64,
}

/// A class ceiling cell such as `16 ms`.
fn parse_ms_cell(cell: &str, what: &str) -> Result<f64, String> {
    let trimmed = cell.trim();
    let number = trimmed
        .strip_suffix("ms")
        .map(str::trim)
        .ok_or_else(|| format!("{what}: {cell:?} is not a `<number> ms` class ceiling"))?;
    if number.is_empty() {
        return Err(format!(
            "{what}: {cell:?} is not a `<number> ms` class ceiling"
        ));
    }
    match number.replace(',', "").parse::<f64>() {
        Ok(value) if value.is_finite() && value >= 0.0 => Ok(value),
        _ => Err(format!(
            "{what}: {cell:?} is not a finite number at or above zero"
        )),
    }
}

/// A tolerance cell such as `55 %` or `55%`.
fn parse_pct_cell(cell: &str, what: &str) -> Result<f64, String> {
    let trimmed = cell.trim();
    let number = trimmed
        .strip_suffix('%')
        .map(str::trim)
        .ok_or_else(|| format!("{what}: {cell:?} is not a `<number> %` tolerance"))?;
    if number.is_empty() {
        return Err(format!("{what}: {cell:?} is not a `<number> %` tolerance"));
    }
    match number.replace(',', "").parse::<f64>() {
        Ok(value) if value.is_finite() && value >= 0.0 => Ok(value),
        _ => Err(format!(
            "{what}: {cell:?} is not a finite number at or above zero"
        )),
    }
}

/// The table row that holds a budget's class gates.
fn budget_row<'a>(budget: &str, rows: &'a [Vec<String>]) -> Result<&'a [String], String> {
    rows.iter()
        .find(|cells| cells.first().map(String::as_str) == Some(budget))
        .map(Vec::as_slice)
        .ok_or_else(|| format!("{budget}: no budget row holds its class gates"))
}

/// The class gates for `budget` from its row in `rows`: every gate number
/// comes from the doc, so there is no code mirror to drift. A malformed
/// calibration cell is a configuration failure, never implicit advisory.
fn class_gates(budget: &str, rows: &[Vec<String>]) -> Result<ClassGates, String> {
    let row = budget_row(budget, rows)?;
    let cell = |index: usize, what: &str| {
        row.get(index).ok_or_else(|| {
            format!("{budget}: missing class cell {what}; the row needs five class columns")
        })
    };
    Ok(ClassGates {
        fast_ceiling: parse_ms_cell(cell(4, "fast ceiling")?, budget)?,
        middle_ceiling: parse_ms_cell(cell(5, "middle ceiling")?, budget)?,
        slow_ceiling: parse_ms_cell(cell(6, "slow ceiling")?, budget)?,
        fast_tolerance: parse_pct_cell(cell(7, "fast tolerance")?, budget)?,
        slow_tolerance: parse_pct_cell(cell(8, "slow tolerance")?, budget)?,
    })
}

/// The class ceiling and tolerance for `class` from `gates`.
fn class_limit(gates: &ClassGates, class: SpeedClass) -> (f64, Option<f64>) {
    match class {
        SpeedClass::Fast => (gates.fast_ceiling, Some(gates.fast_tolerance)),
        SpeedClass::Middle => (gates.middle_ceiling, None),
        SpeedClass::Slow => (gates.slow_ceiling, Some(gates.slow_tolerance)),
    }
}

/// The class gates for an attach row: each cell is a gate number or `—`
/// for unavailable. The first-frame row's tolerances read `—` until one
/// model holds 5 stored files with new-regime samples, and the pass-end
/// row's every class cell reads `—` while it stays advisory. Anything
/// else is malformed configuration, never implicit advisory.
struct AttachGates {
    fast_ceiling: Option<f64>,
    middle_ceiling: Option<f64>,
    slow_ceiling: Option<f64>,
    fast_tolerance: Option<f64>,
    slow_tolerance: Option<f64>,
}

/// The attach row's class gates from its budget row: like [`class_gates`]
/// but keeping `—` cells as unavailable instead of failing on them.
fn attach_gates(budget: &str, rows: &[Vec<String>]) -> Result<AttachGates, String> {
    let row = budget_row(budget, rows)?;
    let cell = |index: usize, what: &str| {
        row.get(index).ok_or_else(|| {
            format!("{budget}: missing class cell {what}; the row needs five class columns")
        })
    };
    let ms = |index: usize, what: &str| -> Result<Option<f64>, String> {
        let got = cell(index, what)?;
        if got.trim() == "—" {
            Ok(None)
        } else {
            parse_ms_cell(got, budget).map(Some)
        }
    };
    let pct = |index: usize, what: &str| -> Result<Option<f64>, String> {
        let got = cell(index, what)?;
        if got.trim() == "—" {
            Ok(None)
        } else {
            parse_pct_cell(got, budget).map(Some)
        }
    };
    Ok(AttachGates {
        fast_ceiling: ms(4, "fast ceiling")?,
        middle_ceiling: ms(5, "middle ceiling")?,
        slow_ceiling: ms(6, "slow ceiling")?,
        fast_tolerance: pct(7, "fast tolerance")?,
        slow_tolerance: pct(8, "slow tolerance")?,
    })
}

/// The class ceiling and tolerance for `class` from attach `gates`: the
/// ceiling the absolute bound checks, and the tolerance the relative leg
/// needs (`None` disables it).
fn attach_limit(gates: &AttachGates, class: SpeedClass) -> (Option<f64>, Option<f64>) {
    match class {
        SpeedClass::Fast => (gates.fast_ceiling, gates.fast_tolerance),
        SpeedClass::Middle => (gates.middle_ceiling, None),
        SpeedClass::Slow => (gates.slow_ceiling, gates.slow_tolerance),
    }
}

/// Whether `head` fails its timing gate: strictly over the class ceiling
/// and strictly over the base median by more than the tolerance, with the
/// relative bound by multiplication. Zero base is valid: a positive head
/// over the ceiling fails while a zero head does not. An overflowing
/// bound is a configuration error, never a silent pass.
fn timing_fails(head: f64, base: f64, ceiling: f64, tolerance_pct: f64) -> Result<bool, String> {
    if head <= ceiling {
        return Ok(false);
    }
    let bound = base * (1.0 + tolerance_pct / 100.0);
    if !bound.is_finite() {
        return Err(format!(
            "relative bound {tolerance_pct}% over base {base} overflows: cannot judge the timing"
        ));
    }
    Ok(head > bound)
}

/// Whether the live base gives a comparable same-job timing: a base with
/// a different CPU key, or with no key, cannot establish the same-job
/// premise, so the row stays advisory. Case-only differences compare
/// normally; model equality alone is not claimed to prove runner identity.
fn base_cpu_comparable(head_cpu: Option<&str>, base: &Base) -> bool {
    let Base::Ok(results) = base else {
        return false;
    };
    let head = head_cpu.map(str::trim).filter(|m| !m.is_empty());
    let base_cpu = results
        .cpu
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty());
    match (head, base_cpu) {
        (Some(head), Some(base_cpu)) => head.eq_ignore_ascii_case(base_cpu),
        _ => false,
    }
}

/// One scalar timing median from validated samples: exactly its declared
/// run count, each finite and at or above zero. Unsorted arrays with
/// distinct middles, so a first-sample mutant cannot pass.
fn timing_median(head: Option<&Results>, id: &str) -> Result<f64, String> {
    let samples = samples_of(head, id)?;
    let runs = head.map(|results| results.runs_of(id)).unwrap_or(0);
    median(&samples, runs).map_err(|e| format!("{id}: {e}"))
}

/// The class name the comment shows, so the active ceiling is
/// distinguishable from the old picked ceiling or formula.
fn class_name(class: SpeedClass) -> &'static str {
    match class {
        SpeedClass::Fast => "fast",
        SpeedClass::Middle => "middle",
        SpeedClass::Slow => "slow",
    }
}

/// Each attach fixture's median ms from strictly validated entries: known
/// fixtures only, `log_bytes` and `ms` present, exactly five runs per
/// fixture (`docs/performance.md:184-185`), each `ms` finite and at or
/// above zero. `fixture_medians` itself does not reject negatives, so
/// this runs before any advisory handling: a negative or non-finite `ms`
/// is a named configuration failure even on an advisory model.
struct AttachMedians {
    /// Median ms of the 1 MiB fixture.
    one_mib: f64,
    /// Median ms of the 10 MiB fixture.
    ten_mib: f64,
}

fn attach_medians(results: Option<&Results>, id: &str) -> Result<AttachMedians, String> {
    let entries = metric(results, id)?
        .as_array()
        .ok_or_else(|| format!("{id}: not an array"))?;
    let runs = results.map(|results| results.runs_of(id)).unwrap_or(0);
    // The attach row always takes 5 samples per fixture
    // (`docs/performance.md:184-185`): a declared count of 3 is malformed,
    // even when the samples match it.
    if runs != 5 {
        return Err(format!("{id}: {runs} runs, expected 5"));
    }
    let mut one_mib = Vec::new();
    let mut ten_mib = Vec::new();
    for entry in entries {
        let fixture = entry
            .get("fixture")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{id}: {entry} has no fixture"))?;
        let samples = if fixture == "1 MiB" {
            &mut one_mib
        } else if fixture == "10 MiB" {
            &mut ten_mib
        } else {
            return Err(format!("{id}: unknown fixture {fixture:?}"));
        };
        entry
            .get("log_bytes")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{id}: {entry} has no log_bytes"))?;
        let ms = entry
            .get("ms")
            .and_then(Value::as_f64)
            .ok_or_else(|| format!("{id}: {entry} has no ms"))?;
        if !ms.is_finite() || ms < 0.0 {
            return Err(format!(
                "{id}: {entry} is not a finite number at or above zero"
            ));
        }
        samples.push(ms);
    }
    let median_of = |fixture: &str, samples: &[f64]| -> Result<f64, String> {
        if samples.is_empty() {
            return Err(format!("{id}: no {fixture} fixture"));
        }
        if samples.len() != runs {
            return Err(format!(
                "{id}: {fixture}: {} runs, expected {runs}",
                samples.len()
            ));
        }
        median(samples, runs).map_err(|e| format!("{id}: {fixture}: {e}"))
    };
    Ok(AttachMedians {
        one_mib: median_of("1 MiB", &one_mib)?,
        ten_mib: median_of("10 MiB", &ten_mib)?,
    })
}

/// What the head column shows for a gated scalar timing: the median with
/// its class ceiling and tolerance, so the decision follows the doc value.
fn gated_shown(median: f64, class: SpeedClass, ceiling: f64, tolerance: f64) -> String {
    format!(
        "{median:.1} ms ({} ceiling {ceiling:.1} ms, +{tolerance}% over base)",
        class_name(class)
    )
}

/// What a benchmarked row checks, and the result-file metric ids it reads.
#[derive(Debug, Clone, Copy)]
pub(super) enum Check {
    /// Peak RSS in KiB per run, one metric per workload; each median must
    /// not exceed the ceiling.
    Memory(&'static [&'static str]),
    /// Peak RSS in KiB per run, held to `row`'s ceiling: the cell must read
    /// `within the busy session's ` and that row's ceiling cell.
    Within { row: &'static str, id: &'static str },
    /// Milliseconds per run, gated per runner speed class: the head median
    /// fails only when strictly over its class ceiling and strictly over
    /// the same-job base median by more than its tolerance; anything
    /// without a comparable base stays advisory with its reason.
    Timing(&'static str),
    /// Milliseconds per run with each run's log size, judged per fixture
    /// against the row's flat class ceiling on the fast and slow classes:
    /// a median over the ceiling fails even with a slower same-job base.
    /// Middle never gates. A `—` tolerance disables only the
    /// base-relative leg, never the absolute bound. A malformed metric
    /// fails even on an advisory model.
    FirstFrame { pin: &'static str, id: &'static str },
    /// Milliseconds per run with each run's log size, advisory reference
    /// only: every class cell reads `—` until the follow-up gates them, so
    /// samples never fail the row. A malformed metric still fails.
    PassEnd { id: &'static str },
    /// Fold milliseconds per run with each run's log size, judged per
    /// fixture against the pin's rate on the eligible fast and slow
    /// classes: a median rate over it fails even with a slower same-job
    /// base. Middle never gates. Every class cell reads `—`, and the pin
    /// carries the rate, so the ceiling text is code. A missing or
    /// malformed fold stage fails the head, never skips; a missing or
    /// malformed base reads unavailable without waiving the head's cap.
    FoldPerMib { pin: &'static str, id: &'static str },
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
    /// One pass per run: the median tenth-turn anon RSS growth over the
    /// first's, across the harness's repeats.
    RetainedGrowth(&'static str),
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
        Check::FirstFrame {
            pin: "50 ms",
            id: "terminal_attach_ms",
        },
    ),
    (
        "Terminal attaching, to the end of the opening pass",
        Check::PassEnd {
            id: "terminal_attach_pass_ms",
        },
    ),
    (
        "Terminal attaching, fold per MiB of session log",
        Check::FoldPerMib {
            pin: "15 ms per MiB",
            id: "attach_stage_ms",
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
        TEN_TURNS_BUDGET,
        Check::Exact {
            pin: "the memory kept after the first, plus 1 MiB RssAnon",
            rule: Rule::RetainedGrowth("ten_busy_turns_retained_kib"),
        },
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
        "`session_list`, waiting for every running session's status",
        Check::Timing("session_list_ms"),
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
            pin: "24 times the same run's scale-1 re-count",
            times: 24.0,
            small: "paging_width_ms",
            large: "paging_scale16_width_ms",
        },
    ),
    (
        "`paging` jig at scale 16, a search of the whole session with no match",
        Check::Ratio {
            pin: "24 times the same run's scale-1 search",
            times: 24.0,
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

/// Each selectable timing row with the result-file metric ids its budgets
/// read, in `BASE_ROWS` order (`xtask/src/select/plan.rs`): a head file
/// that skips one of these rows leaves those budgets unmeasured. Memory
/// rows that ride on a timing row (`terminal attach`, `paging`) are
/// skipped with it; every other memory and exact budget keeps running.
pub(super) const ROW_METRICS: &[(&str, &[&str])] = &[
    ("session start", &["session_start_ms"]),
    ("sessions list", &["sessions_list_ms"]),
    ("terminal first frame", &["terminal_first_frame_ms"]),
    (
        "terminal attach",
        &[
            "terminal_attach_ms",
            "terminal_attach_pass_ms",
            "attach_stage_ms",
            "terminal_attach_rss_kib",
            "terminal_attach_retained_kib",
        ],
    ),
    ("terminal wheel", &["wheel_paging_ms", "wheel_read_ms"]),
    (
        "paging",
        &[
            "paging_rss_kib",
            "paging_open_ms",
            "paging_load_ms",
            "paging_jump_ms",
            "paging_width_ms",
            "paging_append_ms",
            "paging_search_ms",
            "paging_scale16_width_ms",
            "paging_scale16_search_ms",
        ],
    ),
];

/// Extra samples per side when a gated timing row breaches: 3 + 4 = 7 is
/// odd, so the merged median is a real sample and a breach needs 4 of 7
/// over. A deterministic slowdown puts all 7 over and still fails; a
/// one-off load spike is outvoted by 4 clean extras. The CI remeasure run
/// passes `--runs 4 --timing-runs 4`, which must match: `merge_remeasure`
/// rejects an extra metric declaring any other count.
pub(super) const REMEASURE_RUNS: usize = 4;

/// Every harness timing workload (`crates/bench/examples/bench/main.rs`
/// `timing_names`): the remeasure run skips all but the breached rows, so
/// the skip list is these minus the breached rows, joined with `,`.
pub(super) const TIMING_WORKLOADS: &[&str] = &[
    "session start",
    "terminal first frame",
    "sessions list",
    "terminal attach",
    "paging",
    "terminal wheel",
    "session_list",
];

/// The `ROW_METRICS` row a breach of metric `id` remeasures, when
/// remeasuring it can change the verdict: timing rows only, never
/// `terminal attach` (it already takes 5 samples and nests samples by
/// fixture) and never an id no row lists (such as `session_list_ms`, which
/// the selector never skips).
pub(super) fn remeasured_row(id: &str) -> Option<&'static str> {
    ROW_METRICS
        .iter()
        .find(|(row, ids)| *row != "terminal attach" && ids.contains(&id))
        .map(|(row, _)| *row)
}

/// The ten-turn pass's Budget cell, here and in `docs/performance.md`.
pub(super) const TEN_TURNS_BUDGET: &str =
    "Session, memory kept after 10 busy turns, each followed by its handoff";
/// What the summary says when the selection left the ten-turn pass out:
/// the run is judged on the other rows (`docs/ci.md`, "Selection").
pub(super) const TEN_TURNS_SKIP_NOTE: &str =
    "skipped: the selection did not run the 10-busy-turns workload";

/// Each row with no benchmark yet, by its Budget cell, and what owns it.
pub(super) const NOT_MEASURED: &[(&str, &str)] = &[];

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
#[derive(Clone, Debug)]
pub(super) enum Base {
    /// A push to `main`: no base column.
    None,
    /// The base binary failed; timings show why instead of a median.
    Failed(String),
    Ok(Results),
}

/// Whether the head file records the 10-busy-turns workload as skipped:
/// a pull request the selection did not choose it for is judged on the
/// other rows. `pub` for the summary, which names the skip even though
/// the row does not gate (`docs/ci.md`, "Selection").
pub(super) fn ten_turns_skipped(head: Option<&Results>) -> bool {
    head.is_some_and(|results| {
        results
            .skipped
            .iter()
            .any(|name| name == TEN_TURNS_WORKLOAD)
    })
}

/// What the head column shows for a budget the selection left
/// unmeasured: the skipped row's name follows (`docs/ci.md`,
/// "Selection").
const HEAD_SKIP_NOTE: &str = "skipped: the selection did not run ";

/// The head file's skipped timing rows, in file order: the names in
/// `head.skipped` that name a `ROW_METRICS` row. `10 busy turns` and
/// unknown names are not returned; `None` returns empty. `pub` for the
/// summary, which names the skip (`docs/ci.md`, "Selection").
pub(super) fn head_skipped_rows(head: Option<&Results>) -> Vec<&'static str> {
    match head {
        None => Vec::new(),
        Some(results) => results
            .skipped
            .iter()
            .filter_map(|name| {
                ROW_METRICS
                    .iter()
                    .find(|(row, _)| name == row)
                    .map(|(row, _)| *row)
            })
            .collect(),
    }
}

/// The first head-skipped timing row one of whose metric ids `check`
/// reads, or `None`. A row matches when its name is in the head file's
/// `skipped` list and one of the check's ids is in the row's
/// `ROW_METRICS` entry: memory checks read their ids, timing and
/// per-MiB checks their one id, ratios either side, and exact checks
/// read none, so an exact budget never skips. `10 busy turns` and names
/// outside `ROW_METRICS` never match: the selector never emits them.
// Exempt: the ratio arm's `||` (`ids.contains(&small) || ids.contains(&large)`)
// becomes `&&` without a behaviour change in the shipped table, since both ids
// of every ratio row belong to the `paging` row (`ROW_METRICS`). A test cannot
// reach this private function from `bench_tests.rs` without widening it.
#[cfg_attr(false, mutants::skip)]
fn head_skipped_row(check: Check, head: Option<&Results>) -> Option<&'static str> {
    let head = head?;
    for (row, ids) in ROW_METRICS {
        if !head.skipped.iter().any(|name| name == row) {
            continue;
        }
        let reads = match check {
            Check::Memory(check_ids) => check_ids.iter().any(|id| ids.contains(id)),
            Check::Within { id, .. }
            | Check::Timing(id)
            | Check::FirstFrame { id, .. }
            | Check::PassEnd { id }
            | Check::FoldPerMib { id, .. } => ids.contains(&id),
            Check::Ratio { small, large, .. } => ids.contains(&small) || ids.contains(&large),
            Check::Exact { .. } => false,
        };
        if reads {
            return Some(row);
        }
    }
    None
}

/// The table-only checks for a budget the selection left unmeasured on
/// the head: the ceiling cell must still read what the measured arms
/// require, since a table edit is checked even when nothing is measured
/// (`docs/ci.md`, "Selection"). Each message reuses its measured arm's
/// wording, so a failure reads the same measured or skipped.
fn table_failures(budget: &str, cell: &str, check: Check, rows: &[Vec<String>]) -> Vec<String> {
    let mut failures = Vec::new();
    match check {
        Check::Memory(_) => match ceiling(cell) {
            Ok(Quantity::Kib(_)) => {}
            Ok(Quantity::Ms(_)) => failures.push(format!("{cell:?} is not a memory ceiling")),
            Err(e) => failures.push(e),
        },
        Check::Within { .. } => {}
        Check::FirstFrame { pin, .. } => {
            if cell != pin {
                failures.push(format!(
                    "the ceiling reads {cell:?}; it must read {pin:?} for budget {budget:?}: change xtask/src/bench/judge.rs with it"
                ));
            }
            if let Err(e) = attach_gates(budget, rows) {
                failures.push(e);
            }
        }
        Check::PassEnd { .. } => {
            match ceiling(cell) {
                Ok(Quantity::Ms(_)) => {}
                Ok(Quantity::Kib(_)) => {
                    failures.push(format!("{cell:?} is not a timing ceiling"));
                }
                Err(e) => failures.push(e),
            }
            match attach_gates(budget, rows) {
                Ok(gates)
                    if [gates.fast_ceiling, gates.middle_ceiling, gates.slow_ceiling, gates.fast_tolerance, gates.slow_tolerance]
                        .iter().all(Option::is_none) => {}
                Ok(_) => failures.push(format!(
                    "{budget}: a class cell holds a number; the pass-end row stays advisory with `—` in every class cell until the follow-up gates them"
                )),
                Err(e) => failures.push(e),
            }
        }
        Check::FoldPerMib { pin, .. } => {
            if cell != pin {
                failures.push(format!(
                    "the ceiling reads {cell:?}; it must read {pin:?} for budget {budget:?}: change xtask/src/bench/judge.rs with it"
                ));
            }
            match attach_gates(budget, rows) {
                Ok(gates)
                    if [gates.fast_ceiling, gates.middle_ceiling, gates.slow_ceiling, gates.fast_tolerance, gates.slow_tolerance]
                        .iter().all(Option::is_none) => {}
                Ok(_) => failures.push(format!(
                    "{budget}: a class cell holds a number; the fold-per-MiB row gates on its pin with `—` in every class cell"
                )),
                Err(e) => failures.push(e),
            }
        }
        Check::Timing(_) => {
            match ceiling(cell) {
                Ok(Quantity::Ms(_)) => {}
                Ok(Quantity::Kib(_)) => failures.push(format!("{cell:?} is not a timing ceiling")),
                Err(e) => failures.push(e),
            }
            if let Err(e) = class_gates(budget, rows) {
                failures.push(e);
            }
        }
        Check::Exact { .. } => {}
        Check::Ratio { pin, .. } => {
            if cell != pin {
                failures.push(format!(
                    "the ceiling reads {cell:?}; it must read {pin:?} for budget {budget:?}: change xtask/src/bench/judge.rs with it"
                ));
            }
        }
    }
    failures
}

/// The rule's failures, and what the head column shows.
fn exact(rule: Rule, head: Option<&Results>) -> (Vec<String>, String) {
    match rule {
        Rule::IdleSwitches(ids) => per_thread(List::Switches, ids, head),
        Rule::Threads(id) => per_thread(List::Threads, &[id], head),
        Rule::Fsyncs(id) => per_entry(id, FSYNC_RUNS, head, fsyncs),
        Rule::RetainedGrowth(id) => {
            // `per_entry` fails on the missing metric first, so the count
            // is only read when results exist.
            let count = head.map(|results| results.runs_of(id)).unwrap_or(0);
            retained_growth_median(id, count, head)
        }
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

/// One pass's growth: its tenth turn's anon RSS minus its first's,
/// floored at zero, since a smaller tenth turn is no leak.
fn growth(entry: &Value, id: &str) -> Result<f64, String> {
    let first = field(entry, id, "first")?;
    let last = field(entry, id, "last")?;
    Ok(last.saturating_sub(first) as f64)
}

/// The median growth over the run's ten-turn passes: at most 1 MiB.
/// One leaking pass moves the median only when most passes leak.
fn retained_growth_median(id: &str, count: usize, head: Option<&Results>) -> (Vec<String>, String) {
    // The selection left the workload out: the run is judged on the
    // other rows, and the comment's row says so.
    if ten_turns_skipped(head) {
        return (Vec::new(), TEN_TURNS_SKIP_NOTE.to_owned());
    }
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
    let mut growths = Vec::with_capacity(entries.len());
    let mut failures = Vec::new();
    for entry in entries {
        match growth(entry, id) {
            Ok(grown) => growths.push(grown),
            Err(e) => failures.push(e),
        }
    }
    if !failures.is_empty() {
        return (failures, String::new());
    }
    let median_growth = match median(&growths, count) {
        Ok(median_growth) => median_growth,
        Err(e) => return (vec![format!("{id}: {e}")], String::new()),
    };
    let limit = TEN_TURN_GROWTH_KIB as f64;
    // The first pass stands in for the shape; the verdict is the median.
    // Unreachable when empty: no entries is a count failure above, and an
    // empty median fails before this line.
    let Some(first_entry) = entries.first() else {
        return (vec![format!("{id}: no runs recorded")], String::new());
    };
    let (first, last) = match (
        field(first_entry, id, "first"),
        field(first_entry, id, "last"),
    ) {
        (Ok(first), Ok(last)) => (first, last),
        (Err(e), _) | (_, Err(e)) => return (vec![e], String::new()),
    };
    let shown = format!(
        "median growth {median_growth:.0} KiB over {count} runs; run 1: {last} KiB RssAnon after the tenth, {first} KiB after the first"
    );
    let broken = (median_growth > limit).then(|| {
        format!("{OVER}{id}: median growth {median_growth:.0} KiB is over {limit:.0} KiB")
    });
    match broken {
        Some(broken) => (vec![broken], shown),
        None => (Vec::new(), shown),
    }
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
    /// The `ROW_METRICS` row to remeasure when this line's gated timing
    /// breached: set only in the scalar `Check::Timing` arm when
    /// `timing_fails` holds, for a row other than `terminal attach`.
    pub(super) breached_row: Option<&'static str>,
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
        breached_row: None,
    };
    // A pull request leaves an unselected timing row unmeasured on the
    // head as well as the base (`docs/ci.md`, "Selection"): its budgets
    // read skipped, table-checked but never measured, and the stored
    // results' over-at-base path is never reached for them. A push
    // judges the head alone and ignores `skipped` entirely, so a stray
    // `--skip-rows` on a push cannot silently skip a gating row in the
    // backstop.
    if !matches!(base, Base::None)
        && let Some(skipped) = head_skipped_row(check, head)
    {
        line.head = format!("{HEAD_SKIP_NOTE}{skipped}");
        line.result = "skipped";
        let table = table_failures(budget, cell, check, rows);
        // The closing rule below: a table failure fails the row.
        if !table.is_empty() {
            line.result = "fail";
        }
        return (table, line);
    }
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
        Check::FirstFrame { pin, id } => {
            if cell != pin {
                failures.push(format!(
                    "the ceiling reads {cell:?}; it must read {pin:?} for budget {budget:?}: change xtask/src/bench/judge.rs with it"
                ));
            }
            let (broken, shown) = fixture_medians(id, head);
            failures.extend(broken);
            line.head = shown;
            line.base = match base {
                Base::None => String::new(),
                Base::Failed(why) => format!("base failed: {why}"),
                Base::Ok(results) if !results.metrics.contains_key(id) => "unavailable".to_owned(),
                Base::Ok(results) => {
                    let (broken, shown) = fixture_medians(id, Some(results));
                    match broken.first() {
                        None => shown,
                        Some(first) => format!("base failed: {first}"),
                    }
                }
            };
            // The typed fixture medians with strict validation: negatives
            // and non-finite samples fail even on an advisory model.
            let head_medians = attach_medians(head, id);
            let gates = attach_gates(budget, rows);
            let head_cpu = head.and_then(|results| results.cpu.as_deref());
            let info = model_info(head_cpu);
            match (head_medians, gates) {
                (Err(e), _) | (_, Err(e)) => {
                    // `fixture_medians` already reported the same shape; naming it
                    // twice would double-count, so only add what it missed.
                    if !failures.iter().any(|f| f.contains(&e)) {
                        failures.push(e);
                    }
                }
                (Ok(medians), Ok(gates)) => match info {
                    // Fast and slow gate; middle never does, even with
                    // enough stored files, and anything without enough
                    // evidence stays advisory.
                    ModelInfo::Eligible {
                        class: class @ (SpeedClass::Fast | SpeedClass::Slow),
                    } => match attach_limit(&gates, class) {
                        (None, _) => failures.push(format!(
                            "{budget}: {} ceiling is `—`: the first-frame row carries its flat ceiling in every class cell",
                            class_name(class)
                        )),
                        (Some(ceiling), tolerance) => {
                            let usable_base = match base {
                                Base::Ok(results) if base_cpu_comparable(head_cpu, base) => {
                                    attach_medians(Some(results), id).ok()
                                }
                                Base::None | Base::Failed(_) | Base::Ok(_) => None,
                            };
                            for (fixture, median, base_median) in [
                                ("1 MiB", medians.one_mib, usable_base.as_ref().map(|b| b.one_mib)),
                                ("10 MiB", medians.ten_mib, usable_base.as_ref().map(|b| b.ten_mib)),
                            ] {
                                line.head.push_str(&format!(
                                    "; {fixture}: {median:.1} ms ({} ceiling {ceiling:.1} ms, absolute bound)",
                                    class_name(class)
                                ));
                                if let (Some(base_median), Some(tolerance)) = (base_median, tolerance) {
                                    line.head.push_str(&format!("; {fixture}: +{tolerance}% over base {base_median:.1} ms"));
                                }
                                // Either leg fails; an absolute breach takes precedence
                                // so one fixture never produces duplicate failures.
                                if median > ceiling {
                                    failures.push(format!(
                                        "{OVER}median {median:.1} ms of {id} {fixture} is over {ceiling:.1} ms {} ceiling (absolute bound)",
                                        class_name(class)
                                    ));
                                } else if let (Some(base_median), Some(tolerance)) = (base_median, tolerance) {
                                    // Samples are nonnegative: a zero floor checks only
                                    // the relative leg, independently of the flat ceiling.
                                    match timing_fails(median, base_median, 0.0, tolerance) {
                                        Ok(true) => failures.push(format!(
                                            "{OVER}median {median:.1} ms of {id} {fixture} is over base {base_median:.1} ms by more than {tolerance}% (relative bound)"
                                        )),
                                        Ok(false) => {},
                                        Err(e) => failures.push(format!("{id} {fixture}: {e}")),
                                    }
                                }
                            }
                            // Missing comparisons remain advisory only when the
                            // absolute check above has not failed the row.
                            if usable_base.is_none() {
                                line.result = "advisory";
                                let why = match base {
                                    Base::Failed(_) => line.base.as_str(),
                                    Base::None | Base::Ok(_) => NO_BASE_REASON,
                                };
                                line.head.push_str(&format!("; {why}"));
                            }
                        }
                    },
                    ModelInfo::Eligible { .. }
                    | ModelInfo::FewRuns { .. }
                    | ModelInfo::Advisory { .. } => {
                        line.result = "advisory";
                        // No stored-count combination yields an eligible
                        // middle model today (middle holds 2 stored files),
                        // so the middle arm below is defensive: it names the
                        // ruling until middle holds 5 stored files.
                        let why = match &info {
                            ModelInfo::Eligible { .. } => MIDDLE_ATTACH_REASON.to_owned(),
                            ModelInfo::FewRuns { .. } | ModelInfo::Advisory { .. } => {
                                model_advisory(head_cpu, &info)
                            }
                        };
                        line.head.push_str(&format!(
                            "; 1 MiB: {:.1} ms ({why}); 10 MiB: {:.1} ms ({why})",
                            medians.one_mib, medians.ten_mib
                        ));
                    }
                },
            }
        }
        Check::PassEnd { id } => {
            let (broken, shown) = fixture_medians(id, head);
            failures.extend(broken);
            line.head = shown;
            line.base = match base {
                Base::None => String::new(),
                Base::Failed(why) => format!("base failed: {why}"),
                Base::Ok(results) if !results.metrics.contains_key(id) => "unavailable".to_owned(),
                Base::Ok(results) => {
                    let (broken, shown) = fixture_medians(id, Some(results));
                    match broken.first() {
                        None => shown,
                        Some(first) => format!("base failed: {first}"),
                    }
                }
            };
            // Strict validation even though the row stays advisory: a
            // negative or non-finite sample is a named configuration
            // failure.
            if let Err(e) = attach_medians(head, id)
                && !failures.iter().any(|f| f.contains(&e))
            {
                failures.push(e);
            }
            // Advisory reference in this PR: every class cell reads `—`.
            line.result = "advisory";
            line.head.push_str(&format!(
                "; ordinary ceiling {cell} (advisory reference: every class cell is `—`)"
            ));
        }
        Check::FoldPerMib { pin, id } => {
            if cell != pin {
                failures.push(format!(
                    "the ceiling reads {cell:?}; it must read {pin:?} for budget {budget:?}: change xtask/src/bench/judge.rs with it"
                ));
            }
            let (broken, shown) = fold_per_mib(id, head);
            failures.extend(broken);
            line.head = shown;
            line.base = match base {
                Base::None => String::new(),
                Base::Failed(why) => format!("base failed: {why}"),
                Base::Ok(results) if !results.metrics.contains_key(id) => "unavailable".to_owned(),
                Base::Ok(results) => {
                    let (broken, shown) = fold_per_mib(id, Some(results));
                    match broken.first() {
                        None => shown,
                        Some(first) => format!("base failed: {first}"),
                    }
                }
            };
            // The pin carries the rate: no class cell holds one.
            let bound = match ceiling(pin) {
                Ok(Quantity::Ms(bound)) if bound.is_finite() && bound >= 0.0 => Some(bound),
                _ => {
                    failures.push(format!(
                        "{pin:?} is not a `<number> ms per MiB` rate ceiling"
                    ));
                    None
                }
            };
            // The typed fold-stage medians with strict validation: a
            // missing fold stage, missing log bytes, or a negative or
            // non-finite ms fails even on an advisory model.
            let head_rates = fold_rates(id, head);
            let gates = attach_gates(budget, rows);
            let head_cpu = head.and_then(|results| results.cpu.as_deref());
            let info = model_info(head_cpu);
            match (head_rates, gates, bound) {
                (Err(e), _, _) | (_, Err(e), _) => {
                    // `fold_per_mib` already reported the same shape;
                    // naming it twice would double-count, so only add
                    // what it missed.
                    if !failures.iter().any(|f| f.contains(&e)) {
                        failures.push(e);
                    }
                }
                (_, _, None) => {}
                (Ok(rates), Ok(_), Some(bound)) => match info {
                    // Fast and slow gate; middle never does, even with
                    // enough stored files, and anything without enough
                    // evidence stays advisory.
                    ModelInfo::Eligible {
                        class: class @ (SpeedClass::Fast | SpeedClass::Slow),
                    } => {
                        // No relative leg: the base column is a
                        // diagnostic, and a missing or malformed base
                        // reads unavailable without waiving the head's
                        // absolute cap.
                        let usable_base = match base {
                            Base::Ok(results) if base_cpu_comparable(head_cpu, base) => {
                                fold_rates(id, Some(results)).ok()
                            }
                            Base::None | Base::Failed(_) | Base::Ok(_) => None,
                        };
                        for (fixture, rate) in [("1 MiB", rates.one_mib), ("10 MiB", rates.ten_mib)]
                        {
                            line.head.push_str(&format!(
                                "; {fixture}: {rate:.1} ms per MiB ({} ceiling {bound:.1} ms per MiB, absolute bound)",
                                class_name(class)
                            ));
                            if rate > bound {
                                failures.push(format!(
                                    "{OVER}median {rate:.1} ms per MiB of {id} {fixture} is over {bound:.1} ms per MiB {} ceiling (absolute bound)",
                                    class_name(class)
                                ));
                            }
                        }
                        // Missing comparisons remain advisory only when
                        // the absolute check above has not failed the row.
                        if usable_base.is_none() {
                            line.result = "advisory";
                            let why = match base {
                                Base::Failed(_) => line.base.as_str(),
                                Base::None | Base::Ok(_) => NO_BASE_REASON,
                            };
                            line.head.push_str(&format!("; {why}"));
                        }
                    }
                    ModelInfo::Eligible { .. }
                    | ModelInfo::FewRuns { .. }
                    | ModelInfo::Advisory { .. } => {
                        line.result = "advisory";
                        // No stored-count combination yields an eligible
                        // middle model today (middle holds 2 stored files),
                        // so the middle arm below is defensive: it names the
                        // ruling until middle holds 5 stored files.
                        let why = match &info {
                            ModelInfo::Eligible { .. } => MIDDLE_ATTACH_REASON.to_owned(),
                            ModelInfo::FewRuns { .. } | ModelInfo::Advisory { .. } => {
                                model_advisory(head_cpu, &info)
                            }
                        };
                        line.head.push_str(&format!(
                            "; 1 MiB: {:.1} ms per MiB ({why}); 10 MiB: {:.1} ms per MiB ({why})",
                            rates.one_mib, rates.ten_mib
                        ));
                    }
                },
            }
        }
        Check::Timing(id) => {
            match ceiling(cell) {
                Ok(Quantity::Ms(_)) => {}
                Ok(Quantity::Kib(_)) => failures.push(format!("{cell:?} is not a timing ceiling")),
                Err(e) => failures.push(e),
            }
            let head_median = timing_median(head, id);
            let gates = class_gates(budget, rows);
            let head_cpu = head.and_then(|results| results.cpu.as_deref());
            let info = model_info(head_cpu);
            match (head_median, gates) {
                (Err(e), _) | (_, Err(e)) => {
                    if !failures.iter().any(|f| f.contains(&e)) {
                        failures.push(e);
                    }
                    line.base = match base {
                        Base::None => String::new(),
                        Base::Failed(why) => format!("base failed: {why}"),
                        Base::Ok(results) if !results.metrics.contains_key(id) => {
                            "unavailable".to_owned()
                        }
                        Base::Ok(results) => timing_median(Some(results), id)
                            .map_or_else(|e| format!("base failed: {e}"), |v| format!("{v:.1} ms")),
                    };
                }
                (Ok(head_value), Ok(gates)) => {
                    line.base = match base {
                        Base::None => String::new(),
                        Base::Failed(why) => format!("base failed: {why}"),
                        Base::Ok(results) if !results.metrics.contains_key(id) => {
                            "unavailable".to_owned()
                        }
                        Base::Ok(results) => timing_median(Some(results), id)
                            .map_or_else(|e| format!("base failed: {e}"), |v| format!("{v:.1} ms")),
                    };
                    match info {
                        ModelInfo::FewRuns { class, .. } => {
                            line.result = "advisory";
                            let why = model_advisory(head_cpu, &info);
                            let (recorded, _) = class_limit(&gates, class);
                            // The advisory line names the same effective
                            // ceiling the gate judges: a recorded ceiling
                            // under 2 ms shows the 2.0 ms floor.
                            let ceil = recorded.max(2.0);
                            line.head = format!(
                                "{head_value:.1} ms ({} ceiling {ceil:.1} ms; {why})",
                                class_name(class)
                            );
                        }
                        ModelInfo::Advisory { .. } => {
                            line.result = "advisory";
                            let why = model_advisory(head_cpu, &info);
                            line.head = format!("{head_value:.1} ms ({why})");
                        }
                        ModelInfo::Eligible { class } => {
                            let (recorded, tolerance) = class_limit(&gates, class);
                            let tolerance = tolerance.unwrap_or(0.0);
                            // A recorded class ceiling under 2 ms gates on
                            // an effective 2.0 ms ceiling with a 1.0 ms
                            // absolute allowance over the base, so one tick
                            // of runner jitter on a 1 ms row passes while a
                            // real regression still fails both strictly-over
                            // legs. No other row takes this path.
                            let quantum = recorded < 2.0;
                            let ceiling = recorded.max(2.0);
                            line.head = if quantum {
                                format!(
                                    "{head_value:.1} ms ({} ceiling {ceiling:.1} ms, +1.0 ms absolute over base)",
                                    class_name(class)
                                )
                            } else {
                                gated_shown(head_value, class, ceiling, tolerance)
                            };
                            let base_value = match base {
                                Base::Ok(results)
                                    if results.metrics.contains_key(id)
                                        && timing_median(Some(results), id).is_ok()
                                        && base_cpu_comparable(head_cpu, base) =>
                                {
                                    timing_median(Some(results), id).ok()
                                }
                                Base::None | Base::Failed(_) | Base::Ok(_) => None,
                            };
                            match (base, base_value) {
                                // A failed base never reaches this arm: its own
                                // arm below matches first, so no guard is needed.
                                (Base::None, _) | (Base::Ok(_), None) => {
                                    line.result = "advisory";
                                    line.head.push_str(&format!("; {NO_BASE_REASON}"));
                                }
                                (Base::Failed(_), _) => {
                                    line.result = "advisory";
                                }
                                _ => {
                                    let base_median = base_value.unwrap_or(0.0);
                                    if quantum {
                                        let bound = base_median + 1.0;
                                        if !bound.is_finite() {
                                            failures.push(format!(
                                                "{id}: relative bound 1 ms over base {base_median} overflows: cannot judge the timing"
                                            ));
                                        } else if head_value > ceiling && head_value > bound {
                                            failures.push(format!(
                                        "{OVER}median {head_value:.1} ms of {id} is over {ceiling:.1} ms {} ceiling and over base {base_median:.1} ms by more than 1 ms absolute allowance",
                                        class_name(class)
                                    ));
                                            line.breached_row = remeasured_row(id);
                                        }
                                    } else {
                                        match timing_fails(
                                            head_value,
                                            base_median,
                                            ceiling,
                                            tolerance,
                                        ) {
                                            Err(e) => {
                                                failures.push(format!("{id}: {e}"));
                                            }
                                            Ok(true) => {
                                                failures.push(format!(
                                            "{OVER}median {head_value:.1} ms of {id} is over {ceiling:.1} ms {} ceiling and over base {base_median:.1} ms by more than {tolerance}%",
                                            class_name(class)
                                        ));
                                                line.breached_row = remeasured_row(id);
                                            }
                                            Ok(false) => {}
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
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
            // A skipped ten-turns run leaves the row out of the verdict.
            if matches!(rule, Rule::RetainedGrowth(_)) && ten_turns_skipped(head) {
                line.result = "skipped";
            }
        }
        Check::Ratio {
            pin,
            times,
            small,
            large,
        } => {
            // The ceiling text is code: a changed multiplier or wording
            // fails here with the budget, never silently keeping 24x
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
            Check::Timing(_)
            | Check::FirstFrame { .. }
            | Check::PassEnd { .. }
            | Check::FoldPerMib { .. }
            | Check::Ratio { .. } => Vec::new(),
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
                // Overflowed cross products fall back to the quotient,
                // which stays ordered (never NaN: small is nonzero and
                // both sides finite here). A nested `if` would collapse
                // and a match would lose an arm to mutants no test can
                // catch, so the fallback decision chains `then` instead.
                let quotient = (l1 / s1).total_cmp(&(l2 / s2));
                let crossed = if left.is_finite() {
                    right.is_finite()
                } else {
                    false
                };
                if crossed {
                    left.total_cmp(&right)
                } else {
                    quotient
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
    // Zero over zero passes through the bound below (`0 <= times * 0`),
    // so no early return remains to mutate; positive over zero still
    // fails there as infinite growth.
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
