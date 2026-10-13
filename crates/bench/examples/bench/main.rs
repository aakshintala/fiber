//! The benchmark harness behind the Linux x86_64 budgets in
//! `docs/performance.md`: runs a `fiber` binary through each workload and
//! writes the measurements, the stripped binary's size in bytes, and every
//! self-check that failed, as one result file for
//! `cargo xtask bench-report` to judge. It measures; it judges nothing.
//!
//! `bench --fiber <path> --out <file> [--runs 5] [--timing-runs <runs>] [--idle-secs 10] [--only all|timing|wheel] [--paging <jig>]`
//! (`--paging` names the `tui` crate's `paging` jig; without it that workload
//! does not run; `--timing-runs` defaults to the `--runs` value)
//! exits 0 once the file is written (a failed workload is recorded in it),
//! 2 on a usage error or a host other than Linux, and 1 when the file
//! cannot be written.

#![allow(
    clippy::print_stderr,
    reason = "the harness reports usage and errors on stderr"
)]

mod attach;
mod busy;
mod home;
mod idle;
mod linux;
mod listing;
mod paging;
mod pty;
mod resume;
mod run;
mod screen;
mod wheel;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

const USAGE: &str = "usage: bench --fiber <path> --out <file> [--runs 5] [--timing-runs <runs>] \
     [--idle-secs 10] [--only all|timing|wheel] [--paging <jig>]";

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
struct Args {
    fiber: PathBuf,
    out: PathBuf,
    runs: u32,
    timing_runs: u32,
    idle_secs: u64,
    only: Only,
    paging: Option<PathBuf>,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let (mut fiber, mut out, mut paging) = (None, None, None);
    let (mut runs, mut timing_runs, mut idle_secs, mut only) = (5, None, 10, Only::All);
    while let Some(flag) = args.next() {
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
    })
}

/// Each workload `args` selects, with its repeats and its run count: a
/// repeated workload runs `runs` times, a sampled workload once, and one
/// pass of `busy::ONCE` is never a sample. A workload that reports a
/// memory metric keeps the full count; a timing-only workload runs the
/// pull request's timing count. Sampled workloads take their samples
/// inside one run.
fn selected(args: &Args) -> Vec<(&'static idle::Workload, u32, u32)> {
    let (runs, timing_runs) = (args.runs, args.timing_runs);
    let paging = paging::WORKLOADS.iter().filter(|_| args.paging.is_some());
    let repeated = idle::WORKLOADS
        .iter()
        .chain(&busy::WORKLOADS)
        .chain(&resume::WORKLOADS)
        .chain(paging)
        .map(|workload| {
            // `paging` reports memory, so it keeps the full count with
            // the other memory workloads.
            let count = if workload.timing && workload.name != "paging" {
                timing_runs
            } else {
                runs
            };
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
        .collect()
}

/// One workload's samples, by metric id.
type Metrics = BTreeMap<&'static str, Vec<Value>>;

/// Runs each selected workload with its repeats and run count, and records
/// each finished workload's metric ids at that count. A workload that errs
/// leaves no metrics and no counts.
fn measure_all(
    args: &Args,
    failures: &mut Vec<String>,
    mut measure: impl FnMut(&'static idle::Workload, u32, u32, &mut Vec<String>) -> Option<Metrics>,
) -> (Metrics, BTreeMap<&'static str, u32>) {
    let mut metrics: Metrics = BTreeMap::new();
    let mut metric_runs: BTreeMap<&'static str, u32> = BTreeMap::new();
    for (workload, repeats, runs) in selected(args) {
        eprintln!(
            "bench: running {} (repeats {repeats}, runs {runs})",
            workload.name
        );
        #[allow(
            clippy::let_underscore_must_use,
            reason = "a progress line must not fail the run"
        )]
        let _ = std::io::Write::flush(&mut std::io::stderr());
        if let Some(samples) = measure(workload, repeats, runs, failures) {
            for id in samples.keys() {
                metric_runs.insert(id, runs);
            }
            metrics.extend(samples);
        }
    }
    (metrics, metric_runs)
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
    let mut result = json!({
        "schema": SCHEMA,
        "runs": args.runs,
        "timing_runs": args.timing_runs,
        "cpu": cpu,
        "idle_secs": args.idle_secs,
        "metric_runs": metric_runs,
        "metrics": metrics,
        "failures": failures,
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
fn bench(args: &Args, cpu: &str) -> Value {
    let clock = run::System;
    let mut failures = Vec::new();
    let binary_size = match std::fs::metadata(&args.fiber) {
        Ok(metadata) => Some(metadata.len()),
        Err(err) => {
            failures.push(format!("binary size of {}: {err}", args.fiber.display()));
            None
        }
    };
    let (metrics, metric_runs) = match home::Home::new(&args.fiber) {
        Err(err) => {
            failures.push(format!("setup: {err}"));
            (BTreeMap::new(), BTreeMap::new())
        }
        Ok(home) => {
            let measured = measure_all(args, &mut failures, |workload, repeats, runs, failures| {
                let ctx = idle::Ctx {
                    home: &home,
                    clock: &clock,
                    idle: Duration::from_secs(args.idle_secs),
                    path: std::env::var_os("PATH"),
                    runs,
                    paging: args.paging.as_deref(),
                };
                repeat(&ctx, workload, repeats, failures)
            });
            if let Err(err) = home.finish() {
                failures.push(format!("cleanup: {err}"));
            }
            measured
        }
    };
    document(args, cpu, binary_size, metrics, metric_runs, failures)
}

fn repeat(
    ctx: &idle::Ctx<'_>,
    workload: &idle::Workload,
    runs: u32,
    failures: &mut Vec<String>,
) -> Option<BTreeMap<&'static str, Vec<Value>>> {
    let mut metrics: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
    for n in 1..=runs {
        let mut notes = Vec::new();
        let result = (workload.run)(ctx, &mut notes);
        let name = workload.name;
        failures.extend(
            notes
                .into_iter()
                .map(|note| format!("{name}, run {n}: {note}")),
        );
        match result {
            Ok(samples) => {
                for (id, sample) in samples {
                    metrics.entry(id).or_default().push(sample);
                }
            }
            Err(err) => {
                failures.push(format!("{name}, run {n}: {err}"));
                return None;
            }
        }
    }
    Some(metrics)
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
    let result = bench(&args, &host_cpu());
    match std::fs::write(&args.out, format!("{result}\n")) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("bench: writing {}: {err}", args.out.display());
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
