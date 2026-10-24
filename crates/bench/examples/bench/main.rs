//! The benchmark harness behind the Linux x86_64 budgets in
//! `docs/performance.md`: runs a `fiber` binary through each workload and
//! writes the measurements, the stripped binary's size in bytes, and every
//! self-check that failed, as one result file for
//! `cargo xtask bench-report` to judge. It measures; it judges nothing.
//!
//! `bench --fiber <path> --out <file> [--runs 5] [--timing-runs <runs>] [--idle-secs 10] [--only all|timing|wheel] [--paging <jig>] [--skip-ten-turns] [--skip-rows <names>] [--base-fiber <path> --base-out <file> [--base-paging <jig>]]`
//! (`--paging` names the `tui` crate's `paging` jig; without it that workload
//! does not run; `--timing-runs` defaults to the `--runs` value;
//! `--skip-rows` is a comma-joined list of timing workloads to leave out)
//! exits 0 once the file is written (a failed workload is recorded in it),
//! 2 on a usage error or a host other than Linux, and 1 when the file
//! cannot be written. With `--base-fiber` the harness measures the head
//! and base binaries in one process, alternating head and base samples
//! (by sample for the repeated rows and the `paging` jig, by row block
//! for listing, attach and wheel), and writes the base file too; a base
//! file that cannot be written leaves the exit status alone.

#![allow(
    clippy::print_stderr,
    reason = "the harness reports usage and errors on stderr"
)]

mod attach;
mod busy;
mod debug;
mod home;
mod idle;
mod linux;
mod listing;
mod paging;
mod pty;
mod resume;
mod run;
mod screen;
mod session_list;
mod wheel;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use contract::clock::Clock;
use serde_json::{Value, json};

const USAGE: &str = "usage: bench --fiber <path> --out <file> [--runs 5] [--timing-runs <runs>] \
     [--idle-secs 10] [--only all|timing|wheel] [--paging <jig>] [--skip-ten-turns] \
     [--skip-rows <names>] [--base-fiber <path> --base-out <file> [--base-paging <jig>]]";

/// The result file's format, which `cargo xtask bench-report` checks.
const SCHEMA: u32 = 1;

/// Which workloads run: every one, or only the timing ones the base binary
/// is measured on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Only {
    All,
    Timing,
    Wheel,
}

#[derive(Debug, PartialEq, Eq)]
struct Base {
    fiber: PathBuf,
    out: PathBuf,
    paging: Option<PathBuf>,
}

/// Which side of a paired run a timed call measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Side {
    Head,
    Base,
}

impl Side {
    fn tag(self) -> &'static str {
        match self {
            Side::Head => "head",
            Side::Base => "base",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    fiber: PathBuf,
    out: PathBuf,
    runs: u32,
    timing_runs: u32,
    idle_secs: u64,
    only: Only,
    paging: Option<PathBuf>,
    /// The selection did not choose the 10-busy-turns workload: CI passes
    /// this on a pull request whose diff touches nothing it measures
    /// (`docs/ci.md`, "Selection"). The results file records the skip
    /// under `skipped`, so the judge reports it instead of failing the
    /// missing metric.
    skip_ten_turns: bool,
    /// The timing workloads the base pass leaves out: CI passes this on
    /// a pull request whose diff selects only some timing rows
    /// (`docs/ci.md`, "Selection"). The results file records the skip
    /// under `skipped`, after the 10-busy-turns workload when both are
    /// skipped, so the judge reports them instead of failing the
    /// missing metrics.
    skip_rows: Vec<String>,
    /// The base side of a paired pull-request run: its binary, its
    /// result file, and its `paging` jig, if the base built one.
    base: Option<Base>,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let (mut fiber, mut out, mut paging) = (None, None, None);
    let (mut base_fiber, mut base_out, mut base_paging) = (None, None, None);
    let (mut runs, mut timing_runs, mut idle_secs, mut only) = (5, None, 10, Only::All);
    let mut skip_ten_turns = false;
    let mut skip_rows = Vec::new();
    while let Some(flag) = args.next() {
        // A switch, never a value: the selection left the workload out.
        if flag == "--skip-ten-turns" {
            skip_ten_turns = true;
            continue;
        }
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        let number = |what: &str| -> Result<u64, String> {
            match value.parse() {
                Ok(n) if n > 0 => Ok(n),
                Ok(_) | Err(_) => Err(format!(
                    "{what} takes a positive whole number, not {value:?}"
                )),
            }
        };
        match flag.as_str() {
            "--fiber" => fiber = Some(PathBuf::from(&value)),
            "--out" => out = Some(PathBuf::from(&value)),
            "--paging" => paging = Some(PathBuf::from(&value)),
            "--base-fiber" => base_fiber = Some(PathBuf::from(&value)),
            "--base-out" => base_out = Some(PathBuf::from(&value)),
            "--base-paging" => base_paging = Some(PathBuf::from(&value)),
            "--runs" => {
                runs = u32::try_from(number("--runs")?).map_err(|err| format!("--runs: {err}"))?;
            }
            "--timing-runs" => {
                timing_runs = Some(
                    u32::try_from(number("--timing-runs")?)
                        .map_err(|err| format!("--timing-runs: {err}"))?,
                );
            }
            "--idle-secs" => idle_secs = number("--idle-secs")?,
            "--skip-rows" => skip_rows = parse_skip_rows(&value)?,
            "--only" => {
                only = match value.as_str() {
                    "all" => Only::All,
                    "timing" => Only::Timing,
                    "wheel" => Only::Wheel,
                    _ => {
                        return Err(format!("--only takes all, timing or wheel, not {value:?}"));
                    }
                };
            }
            _ => return Err(format!("unknown argument {flag:?}")),
        }
    }
    // The base side needs its binary and its file together: a base file
    // without a base binary, or a base binary without its file, measures
    // nothing it can report.
    let base = match (base_fiber, base_out, base_paging) {
        (None, None, None) => None,
        (None, Some(_), _) => return Err("--base-out without --base-fiber".to_owned()),
        (None, None, Some(_)) => return Err("--base-paging without --base-fiber".to_owned()),
        (Some(_), None, _) => return Err("--base-fiber needs --base-out <file>".to_owned()),
        (Some(fiber), Some(out), paging) => Some(Base { fiber, out, paging }),
    };
    Ok(Args {
        fiber: fiber.ok_or("--fiber is required")?,
        out: out.ok_or("--out is required")?,
        runs,
        // The default resolves after every flag parses, whatever order
        // they come in.
        timing_runs: timing_runs.unwrap_or(runs),
        idle_secs,
        only,
        paging,
        skip_ten_turns,
        skip_rows,
        base,
    })
}

/// Every workload that reports only timings, in selection order: the
/// names `--skip-rows` may hold.
fn timing_names() -> Vec<&'static str> {
    idle::WORKLOADS
        .iter()
        .chain(&busy::WORKLOADS)
        .chain(&busy::ONCE)
        .chain(&resume::WORKLOADS)
        .chain(&session_list::WORKLOADS)
        .chain(&paging::WORKLOADS)
        .chain(listing::SAMPLED.iter())
        .chain(attach::SAMPLED.iter())
        .chain(wheel::SAMPLED.iter())
        .filter(|workload| workload.timing)
        .map(|workload| workload.name)
        .collect()
}

/// The workload names in a `--skip-rows` value, comma-joined, in the
/// order given; an empty value skips nothing. Every name must be a
/// timing workload, so a typo fails the run instead of silently
/// skipping nothing.
fn parse_skip_rows(value: &str) -> Result<Vec<String>, String> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let timing = timing_names();
    value
        .split(',')
        .map(|name| {
            if timing.contains(&name) {
                Ok(name.to_owned())
            } else {
                Err(format!(
                    "--skip-rows {value:?}: {name:?} is not a timing workload"
                ))
            }
        })
        .collect()
}

/// Each workload `args` selects, with its repeats and its run count: a
/// repeated workload runs the timing count when it reports only timings
/// and the full count otherwise, a sampled workload once, and one
/// pass of `busy::ONCE` is never a sample. `terminal attach` reports
/// memory, so it keeps the full count; the other sampled workloads are
/// timing-only. Sampled workloads take their samples
/// inside one run.
fn selected(args: &Args) -> Vec<(&'static idle::Workload, u32, u32)> {
    let (runs, timing_runs) = (args.runs, args.timing_runs);
    let paging = paging::WORKLOADS.iter().filter(|_| args.paging.is_some());
    let repeated = idle::WORKLOADS
        .iter()
        .chain(&busy::WORKLOADS)
        .chain(&resume::WORKLOADS)
        .chain(&session_list::WORKLOADS)
        .chain(paging)
        .map(|workload| {
            let count = if workload.timing { timing_runs } else { runs };
            (workload, count, count)
        });
    let once = busy::ONCE.iter().map(|workload| (workload, 1, 1));
    let sampled = listing::SAMPLED
        .iter()
        .chain(attach::SAMPLED.iter())
        .chain(wheel::SAMPLED.iter())
        .map(|workload| {
            // `terminal attach` reports memory, so it keeps the full
            // count; the other sampled workloads are timing-only.
            let count = if workload.name == "terminal attach" {
                runs
            } else {
                timing_runs
            };
            (workload, 1, count)
        });
    repeated
        .chain(once)
        .chain(sampled)
        .filter(|(workload, _, _)| match args.only {
            Only::All => true,
            Only::Timing => workload.timing,
            Only::Wheel => wheel::SAMPLED
                .first()
                .is_some_and(|wheel| wheel.name == workload.name),
        })
        // The selection runs the 10-busy-turns workload only on a pull
        // request whose diff touches what it measures, and always on a
        // push (`docs/ci.md`, "Selection"). The base pass leaves out
        // the timing rows the diff did not select, after the `only` filter.
        .filter(|(workload, _, _)| !(args.skip_ten_turns && workload.name == busy::TEN_TURNS_NAME))
        .filter(|(workload, _, _)| !args.skip_rows.iter().any(|name| name == workload.name))
        .collect()
}

/// One workload's samples, by metric id.
type Metrics = BTreeMap<&'static str, Vec<Value>>;

/// One side's run: the selection it walks follows its `args`, and its
/// failures, metrics and counts land in its own result file.
struct Pass {
    side: Side,
    args: Args,
    failures: Vec<String>,
    metrics: Metrics,
    metric_runs: BTreeMap<&'static str, u32>,
}

impl Pass {
    fn new(side: Side, args: Args) -> Self {
        Self {
            side,
            args,
            failures: Vec::new(),
            metrics: BTreeMap::new(),
            metric_runs: BTreeMap::new(),
        }
    }
}

/// The head's selection and, with `--base-fiber`, the base's. The base
/// runs the timing rows the selection keeps, so `--skip-rows` filters
/// the base list only and the head runs every row at its place in head
/// order; a missing base list leaves the single run's selection, and its
/// `--skip-rows` filtering, exactly as before.
fn sides(args: &Args) -> (Args, Option<Args>) {
    let rebuild = |fiber: &PathBuf,
                   out: &PathBuf,
                   only: Only,
                   paging: &Option<PathBuf>,
                   skip_ten_turns: bool,
                   skip_rows: Vec<String>| {
        Args {
            fiber: fiber.clone(),
            out: out.clone(),
            runs: args.runs,
            timing_runs: args.timing_runs,
            idle_secs: args.idle_secs,
            only,
            paging: paging.clone(),
            skip_ten_turns,
            skip_rows,
            base: None,
        }
    };
    match &args.base {
        None => (
            rebuild(
                &args.fiber,
                &args.out,
                args.only,
                &args.paging,
                args.skip_ten_turns,
                args.skip_rows.clone(),
            ),
            None,
        ),
        Some(base) => (
            rebuild(
                &args.fiber,
                &args.out,
                args.only,
                &args.paging,
                args.skip_ten_turns,
                Vec::new(),
            ),
            Some(rebuild(
                &base.fiber,
                &base.out,
                Only::Timing,
                &base.paging,
                false,
                args.skip_rows.clone(),
            )),
        ),
    }
}

/// Whether the row takes every sample in its own timed call: listing,
/// attach and wheel prepare their fixtures once per side, then time one
/// sample per call, so the scheduler alternates the sides per sample.
/// Every other row times one run per call.
fn is_sampled(workload: &idle::Workload) -> bool {
    listing::SAMPLED
        .iter()
        .chain(attach::SAMPLED.iter())
        .chain(wheel::SAMPLED.iter())
        .any(|sampled| sampled.name == workload.name)
}

/// How many timed calls one side makes of a sampled row with `runs`
/// samples per fixture or session: listing times one sample per run,
/// while attach times each of its fixtures and wheel each of its
/// sessions `runs` times.
fn sample_total(workload: &idle::Workload, runs: u32) -> u32 {
    // The scheduler calls this only for sampled rows; any other row
    // keeps its run count.
    if attach::SAMPLED
        .iter()
        .any(|sampled| sampled.name == workload.name)
    {
        return runs.saturating_mul(attach::FIXTURE_COUNT);
    }
    if wheel::SAMPLED
        .iter()
        .any(|sampled| sampled.name == workload.name)
    {
        return runs.saturating_mul(wheel::SESSION_COUNT);
    }
    runs
}

/// Runs each selected workload with its repeats and run count, head and
/// base calls in alternation: head call n, then the base call n when the
/// base runs the row, for n in 1..=`repeats`, so runner drift lands on
/// both sides. A repeated row therefore alternates by sample, and a
/// sampled row alternates by sample too: head sample s, then the base
/// sample s, for s in 1..=the row's samples, with no other row's call
/// between. A row runs on the base only when the base's selection holds
/// it; any other row runs on the head alone, at its place in head order.
/// A side whose call errs ends that side's row, its samples for the row
/// discarded, while the other side still runs its remaining calls of
/// the row, without the failed side.
fn measure_all(
    head: &mut Pass,
    mut base: Option<&mut Pass>,
    mut measure: impl FnMut(
        Side,
        &'static idle::Workload,
        u32,
        u32,
        &mut Vec<String>,
    ) -> Result<idle::Samples, String>,
) {
    if head.args.skip_ten_turns {
        eprintln!(
            "bench: skipping {} (the selection did not choose it)",
            busy::TEN_TURNS_NAME
        );
    }
    if !head.args.skip_rows.is_empty() {
        eprintln!(
            "bench: skipping {} (the selection did not choose them)",
            head.args.skip_rows.join(", ")
        );
    }
    if let Some(base) = &base
        && !base.args.skip_rows.is_empty()
    {
        eprintln!(
            "bench: skipping {} on the base (the selection did not choose them)",
            base.args.skip_rows.join(", ")
        );
    }
    let rows = selected(&head.args);
    // The rows the base runs, by name: the walk below holds the head's
    // order, and a row the base list lacks runs on the head alone.
    let base_rows = base.as_ref().map(|pass| {
        selected(&pass.args)
            .iter()
            .map(|(workload, _, _)| workload.name)
            .collect::<Vec<_>>()
    });
    for (workload, repeats, runs) in rows {
        let paired = base_rows
            .as_ref()
            .is_some_and(|names| names.contains(&workload.name));
        // Both selections share the run counts, so the head tuple's runs
        // is each side's run count.
        let mut head_samples: Metrics = BTreeMap::new();
        let mut base_samples: Metrics = BTreeMap::new();
        let mut head_alive = true;
        let mut base_alive = paired;
        // A sampled row times one sample per call, alternating the sides
        // per sample; every other row times one run per call. The row's
        // nominal run count still labels its metrics below.
        let (calls, each) = if is_sampled(workload) {
            (sample_total(workload, runs), 1)
        } else {
            (repeats, runs)
        };
        for n in 1..=calls {
            // debt: the head and base side-measurement branches duplicate progress
            // reporting, measurement, note collection, sample accumulation and failure
            // handling; extract one per-side measurement operation called in head/base
            // order on the next change to the side-measurement branches.
            if head_alive {
                eprintln!(
                    "bench: running {} (run {n} of {calls}, samples {each}) [{}]",
                    workload.name,
                    head.side.tag()
                );
                #[allow(
                    clippy::let_underscore_must_use,
                    reason = "a progress line must not fail the run"
                )]
                let _ = std::io::Write::flush(&mut std::io::stderr());
                let mut notes = Vec::new();
                let result = measure(head.side, workload, each, n, &mut notes);
                head.failures.extend(
                    notes
                        .into_iter()
                        .map(|note| format!("{}, run {n}: {note}", workload.name)),
                );
                match result {
                    Ok(samples) => {
                        for (id, sample) in samples {
                            head_samples.entry(id).or_default().push(sample);
                        }
                    }
                    Err(err) => {
                        head.failures
                            .push(format!("{}, run {n}: {err}", workload.name));
                        head_alive = false;
                        head_samples.clear();
                    }
                }
            }
            if base_alive && let Some(base) = base.as_mut() {
                eprintln!(
                    "bench: running {} (run {n} of {calls}, samples {each}) [{}]",
                    workload.name,
                    base.side.tag()
                );
                #[allow(
                    clippy::let_underscore_must_use,
                    reason = "a progress line must not fail the run"
                )]
                let _ = std::io::Write::flush(&mut std::io::stderr());
                let mut notes = Vec::new();
                let result = measure(base.side, workload, each, n, &mut notes);
                base.failures.extend(
                    notes
                        .into_iter()
                        .map(|note| format!("{}, run {n}: {note}", workload.name)),
                );
                match result {
                    Ok(samples) => {
                        for (id, sample) in samples {
                            base_samples.entry(id).or_default().push(sample);
                        }
                    }
                    Err(err) => {
                        base.failures
                            .push(format!("{}, run {n}: {err}", workload.name));
                        base_alive = false;
                        base_samples.clear();
                    }
                }
            }
        }
        // A metric is recorded only when every call of its side's row
        // produced it, as the single-side run did.
        if head_alive {
            for id in head_samples.keys() {
                head.metric_runs.insert(id, runs);
            }
            head.metrics.extend(head_samples);
        }
        if base_alive && let Some(base) = base.as_mut() {
            for id in base_samples.keys() {
                base.metric_runs.insert(id, runs);
            }
            base.metrics.extend(base_samples);
        }
    }
}

/// The result file: the schema, the run counts, the runner's CPU model,
/// the stripped binary's size in bytes when it could be read, the metrics
/// with each id's count, and every failure the run noted.
fn document(
    args: &Args,
    cpu: &str,
    binary_size: Option<u64>,
    metrics: Metrics,
    metric_runs: BTreeMap<&'static str, u32>,
    failures: Vec<String>,
) -> Value {
    let mut skipped = Vec::new();
    if args.skip_ten_turns {
        skipped.push(busy::TEN_TURNS_NAME.to_owned());
    }
    skipped.extend(args.skip_rows.iter().cloned());
    let mut result = json!({
        "schema": SCHEMA,
        "runs": args.runs,
        "timing_runs": args.timing_runs,
        "cpu": cpu,
        "idle_secs": args.idle_secs,
        "metric_runs": metric_runs,
        "metrics": metrics,
        "failures": failures,
        // The workloads the selection left out, by name: the judge
        // reports them instead of failing their missing metrics.
        "skipped": skipped,
    });
    if let Some(size) = binary_size
        && let Value::Object(map) = &mut result
    {
        map.insert("binary_size_bytes".to_owned(), json!(size));
    }
    result
}

/// The first `model name` value in Linux's `/proc/cpuinfo`, trimmed; `None`
/// when no such line holds a value.
// The macOS binary never reads cpuinfo: only its tests call this.
#[cfg_attr(
    target_os = "macos",
    allow(dead_code, reason = "used on Linux and by tests")
)]
fn model_name(cpuinfo: &str) -> Option<&str> {
    cpuinfo.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.trim() != "model name" {
            return None;
        }
        let value = value.trim();
        (!value.is_empty()).then_some(value)
    })
}

/// The macOS CPU brand string, read through an injected runner so tests
/// never spawn without a deadline; `unknown` when it cannot be read.
#[cfg(target_os = "macos")]
fn brand_string(
    run: impl FnOnce(&mut std::process::Command) -> Option<std::process::Output>,
) -> String {
    let mut command = std::process::Command::new("sysctl");
    command.args(["-n", "machdep.cpu.brand_string"]);
    match run(&mut command) {
        Some(output) => match String::from_utf8(output.stdout) {
            Ok(brand) if !brand.trim().is_empty() => brand.trim().to_owned(),
            Ok(_) | Err(_) => "unknown".to_owned(),
        },
        None => "unknown".to_owned(),
    }
}

/// The runner's CPU model: the `model name` value on Linux, the brand
/// string on macOS, `unknown` when unreadable or empty.
fn host_cpu() -> String {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|cpuinfo| model_name(&cpuinfo).map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_owned())
    }
    #[cfg(target_os = "macos")]
    {
        brand_string(|command| command.output().ok())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        "unknown".to_owned()
    }
}

/// Runs each selected workload `runs` times, and each workload that runs
/// once, once. A metric is recorded only when every run produced it; a run
/// that errs ends its workload and is noted.
///
/// A side's binary size and home: every failure lands in that side's
/// file, as a missing-binary run does today.
fn setup(pass: &mut Pass) -> (Option<u64>, Option<home::Home>) {
    let size = match std::fs::metadata(&pass.args.fiber) {
        Ok(metadata) => Some(metadata.len()),
        Err(err) => {
            pass.failures.push(format!(
                "binary size of {}: {err}",
                pass.args.fiber.display()
            ));
            None
        }
    };
    let home = match home::Home::new(&pass.args.fiber) {
        Ok(home) => Some(home),
        Err(err) => {
            pass.failures.push(format!("setup: {err}"));
            None
        }
    };
    (size, home)
}

/// One timed call: the workload runs once against `ctx`, its notes kept
/// for the caller to record.
fn run_once(
    ctx: &idle::Ctx<'_>,
    workload: &'static idle::Workload,
    notes: &mut Vec<String>,
) -> Result<idle::Samples, String> {
    (workload.run)(ctx, notes)
}

/// One side's prepared sampled row: both sides' fixtures are ready
/// before the first timed sample, and the row's setup notes join its
/// first call.
struct SampledRow {
    state: Result<SampledState, String>,
    setup_notes: Vec<String>,
    total: u32,
    per_group: u32,
}

/// A sampled row's prepared fixtures, by workload.
enum SampledState {
    Listing(Box<listing::ListingPrepared>),
    Attach(attach::AttachPrepared),
    Wheel(Box<wheel::WheelPrepared>),
}

/// Prepares the side's sampled rows before any timed sample, in
/// selection order. A failed prepare is stored, so the row's calls
/// report it exactly where the single-call run did.
fn prepare_sampled(
    side: Side,
    home: &home::Home,
    args: &Args,
    clock: &dyn Clock,
    path: &Option<OsString>,
    sampled: &mut BTreeMap<(Side, &'static str), SampledRow>,
) {
    for (workload, _, runs) in selected(args) {
        if !is_sampled(workload) {
            continue;
        }
        let ctx = idle::Ctx {
            home,
            clock,
            idle: Duration::from_secs(args.idle_secs),
            path: path.clone(),
            runs,
            paging: args.paging.as_deref(),
        };
        let mut setup_notes = Vec::new();
        let state = if listing::SAMPLED
            .iter()
            .any(|sampled| sampled.name == workload.name)
        {
            listing::prepare_listing(&ctx, &mut setup_notes)
                .map(Box::new)
                .map(SampledState::Listing)
        } else if attach::SAMPLED
            .iter()
            .any(|sampled| sampled.name == workload.name)
        {
            attach::prepare_attach(&ctx, &mut setup_notes).map(SampledState::Attach)
        } else {
            wheel::prepare_wheel(&ctx)
                .map(Box::new)
                .map(SampledState::Wheel)
        };
        sampled.insert(
            (side, workload.name),
            SampledRow {
                state,
                setup_notes,
                total: sample_total(workload, runs),
                per_group: runs,
            },
        );
    }
}

/// What one timed sample borrows: the harness clock, PATH, and the
/// side's idle window and paging jig.
struct SampleParts<'a> {
    clock: &'a dyn Clock,
    path: &'a Option<OsString>,
    idle_secs: u64,
    paging: Option<&'a Path>,
}

/// One timed sample of a prepared row: fixture or session group `s`
/// within its group, alternating head and base. The row's first call
/// reports its setup notes. The last call finalizes the row, so a
/// failed stop discards its buffered samples as the single-call run
/// did; any other failed call tears the row down without its tail
/// checks and reports the sample's error.
fn sample_prepared(
    sampled: &mut BTreeMap<(Side, &'static str), SampledRow>,
    parts: &SampleParts<'_>,
    side: Side,
    workload: &'static idle::Workload,
    s: u32,
    notes: &mut Vec<String>,
) -> Result<idle::Samples, String> {
    let key = (side, workload.name);
    let Some(row) = sampled.get_mut(&key) else {
        return Err(format!("bench: no prepared fixture for {}", workload.name));
    };
    if s == 1 {
        notes.extend(std::mem::take(&mut row.setup_notes));
    }
    let total = row.total;
    let group = usize::try_from(s.saturating_sub(1) / row.per_group)
        .map_err(|err| format!("counting sample groups: {err}"))?;
    let result = match &mut row.state {
        Err(err) => Err(err.clone()),
        Ok(SampledState::Listing(prepared)) => {
            let ctx = idle::Ctx {
                home: &prepared.home,
                clock: parts.clock,
                idle: Duration::from_secs(parts.idle_secs),
                path: parts.path.clone(),
                runs: 1,
                paging: parts.paging,
            };
            listing::sample_listing(prepared, &ctx, notes)
        }
        Ok(SampledState::Attach(prepared)) => {
            let Some(home) = prepared.home(group) else {
                return Err(format!("bench: no attach fixture at index {group}"));
            };
            let ctx = idle::Ctx {
                home,
                clock: parts.clock,
                idle: Duration::from_secs(parts.idle_secs),
                path: parts.path.clone(),
                runs: 1,
                paging: parts.paging,
            };
            attach::sample_attach(prepared, group, &ctx, notes)
        }
        Ok(SampledState::Wheel(prepared)) => wheel::sample_wheel(prepared, group, parts.clock),
    };
    match result {
        // The row stays up for its remaining samples.
        Ok(one) if s != total => Ok(one),
        Ok(one) => {
            let finished = match sampled.remove(&key) {
                Some(SampledRow {
                    state: Ok(SampledState::Listing(prepared)),
                    ..
                }) => listing::finish_listing(*prepared, parts.clock),
                Some(SampledRow {
                    state: Ok(SampledState::Attach(prepared)),
                    ..
                }) => attach::finish_attach(prepared, parts.clock, notes, true),
                Some(SampledRow {
                    state: Ok(SampledState::Wheel(prepared)),
                    ..
                }) => wheel::finish_wheel(*prepared, parts.clock, notes, true),
                _ => Ok(()),
            };
            finished.map(|()| one)
        }
        Err(err) => {
            if let Some(row) = sampled.remove(&key) {
                let mut dropped = Vec::new();
                match row.state {
                    Ok(SampledState::Listing(prepared)) => {
                        let _finished = listing::finish_listing(*prepared, parts.clock);
                    }
                    Ok(SampledState::Attach(prepared)) => {
                        let _finished =
                            attach::finish_attach(prepared, parts.clock, &mut dropped, false);
                    }
                    Ok(SampledState::Wheel(prepared)) => {
                        let _finished =
                            wheel::finish_wheel(*prepared, parts.clock, &mut dropped, false);
                    }
                    Err(_) => {}
                }
            }
            Err(err)
        }
    }
}

/// Measures the head side and, with `--base-fiber`, the base side in
/// alternation, and hands back each side's result file: `None` without a
/// base. A side whose home failed setup runs zero timed calls while the
/// other side runs the whole list alone.
fn bench(args: &Args, cpu: &str) -> (Value, Option<Value>) {
    let clock = run::System;
    let (head_args, base_args) = sides(args);
    let mut head = Pass::new(Side::Head, head_args);
    let mut base = base_args.map(|args| Pass::new(Side::Base, args));
    let (head_size, head_home) = setup(&mut head);
    let (base_size, base_home) = match base.as_mut() {
        Some(pass) => setup(pass),
        None => (None, None),
    };
    // Both sides' sampled fixtures are ready before the first timed
    // sample, so no setup lands between one side's sample and the
    // other's. The per-call closure builds its own context per call for
    // every other row, exactly as the single-side run did.
    let path = std::env::var_os("PATH");
    let mut sampled: BTreeMap<(Side, &'static str), SampledRow> = BTreeMap::new();
    match (head_home, base_home) {
        (Some(head_home), base_home) => {
            prepare_sampled(
                Side::Head,
                &head_home,
                &head.args,
                &clock,
                &path,
                &mut sampled,
            );
            let head_idle = head.args.idle_secs;
            let head_paging = head.args.paging.clone();
            // The base side's home and context pieces, when its setup
            // held: the walk hands the base pass over exactly then, so
            // the closure finds them here.
            let base_ctx = match (&base_home, &base) {
                (Some(home), Some(pass)) => {
                    prepare_sampled(Side::Base, home, &pass.args, &clock, &path, &mut sampled);
                    Some((home, pass.args.idle_secs, pass.args.paging.clone()))
                }
                _ => None,
            };
            measure_all(
                &mut head,
                base.as_mut().filter(|_| base_ctx.is_some()),
                |side, workload, runs, s, notes| {
                    if is_sampled(workload) {
                        let (idle_secs, paging) = match side {
                            Side::Head => (head_idle, head_paging.as_deref()),
                            Side::Base => match &base_ctx {
                                Some((_, idle_secs, paging)) => (*idle_secs, paging.as_deref()),
                                None => {
                                    return Err("bench: the base home is missing".to_owned());
                                }
                            },
                        };
                        let parts = SampleParts {
                            clock: &clock,
                            path: &path,
                            idle_secs,
                            paging,
                        };
                        return sample_prepared(&mut sampled, &parts, side, workload, s, notes);
                    }
                    let (home, idle_secs, paging) = match side {
                        Side::Head => (&head_home, head_idle, head_paging.as_deref()),
                        Side::Base => match &base_ctx {
                            Some((home, idle_secs, paging)) => {
                                (*home, *idle_secs, paging.as_deref())
                            }
                            None => return Err("bench: the base home is missing".to_owned()),
                        },
                    };
                    let ctx = idle::Ctx {
                        home,
                        clock: &clock,
                        idle: Duration::from_secs(idle_secs),
                        path: path.clone(),
                        runs,
                        paging,
                    };
                    run_once(&ctx, workload, notes)
                },
            );
            let head_fiber = head.args.fiber.clone();
            debug::debug_pass(&mut head, |workload| {
                debug::run_debug(
                    &head_fiber,
                    &clock,
                    path.clone(),
                    Duration::from_secs(head_idle),
                    head_paging.as_deref(),
                    workload,
                )
            });
            if let Err(err) = head_home.finish() {
                head.failures.push(format!("cleanup: {err}"));
            }
            if let Some(home) = base_home
                && let Err(err) = home.finish()
                && let Some(pass) = base.as_mut()
            {
                pass.failures.push(format!("cleanup: {err}"));
            }
        }
        (None, Some(base_home)) => {
            // The head's setup failed, so the base runs the whole list alone.
            if let Some(pass) = base.as_mut() {
                prepare_sampled(
                    Side::Base,
                    &base_home,
                    &pass.args,
                    &clock,
                    &path,
                    &mut sampled,
                );
                let idle_secs = pass.args.idle_secs;
                let paging = pass.args.paging.clone();
                measure_all(pass, None, |side, workload, runs, s, notes| {
                    if is_sampled(workload) {
                        let parts = SampleParts {
                            clock: &clock,
                            path: &path,
                            idle_secs,
                            paging: paging.as_deref(),
                        };
                        return sample_prepared(&mut sampled, &parts, side, workload, s, notes);
                    }
                    let ctx = idle::Ctx {
                        home: &base_home,
                        clock: &clock,
                        idle: Duration::from_secs(idle_secs),
                        path: path.clone(),
                        runs,
                        paging: paging.as_deref(),
                    };
                    run_once(&ctx, workload, notes)
                });
                if let Err(err) = base_home.finish() {
                    pass.failures.push(format!("cleanup: {err}"));
                }
            }
        }
        (None, None) => {}
    }
    let Pass {
        args: head_args,
        failures: head_failures,
        metrics: head_metrics,
        metric_runs: head_runs,
        ..
    } = head;
    let head_document = document(
        &head_args,
        cpu,
        head_size,
        head_metrics,
        head_runs,
        head_failures,
    );
    let base_document = base.map(|pass| {
        let Pass {
            args,
            failures,
            metrics,
            metric_runs,
            ..
        } = pass;
        document(&args, cpu, base_size, metrics, metric_runs, failures)
    });
    (head_document, base_document)
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(err) => {
            eprintln!("bench: {err}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if !cfg!(target_os = "linux") && args.only != Only::Wheel {
        eprintln!("bench: the benchmarks measure Linux only");
        return ExitCode::from(2);
    }
    let (head_result, base_result) = bench(&args, &host_cpu());
    if let Err(err) = std::fs::write(&args.out, format!("{head_result}\n")) {
        eprintln!("bench: writing {}: {err}", args.out.display());
        return ExitCode::from(1);
    }
    // A base file that cannot be written leaves the exit status alone:
    // the missing file is reported by `bench-report`, not by this step.
    if let (Some(base), Some(result)) = (&args.base, base_result)
        && let Err(err) = std::fs::write(&base.out, format!("{result}\n"))
    {
        eprintln!("bench: writing {}: {err}", base.out.display());
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
