//! The benchmark harness behind the Linux x86_64 budgets in
//! `docs/performance.md`: runs a `fiber` binary through each workload and
//! writes the measurements, and every self-check that failed, as one
//! result file for `cargo xtask bench-report` to judge. It measures; it
//! judges nothing.
//!
//! `bench --fiber <path> --out <file> [--runs 5] [--idle-secs 10] [--only all|timing]`
//! exits 0 once the file is written (a failed workload is recorded in it),
//! 2 on a usage error or a host other than Linux, and 1 when the file
//! cannot be written.

#![allow(
    clippy::print_stderr,
    reason = "the harness reports usage and errors on stderr"
)]

mod home;
mod idle;
mod linux;
mod pty;
mod run;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

const USAGE: &str =
    "usage: bench --fiber <path> --out <file> [--runs 5] [--idle-secs 10] [--only all|timing]";

/// The result file's format, which `cargo xtask bench-report` checks.
const SCHEMA: u32 = 1;

/// Which workloads run: every one, or only the timing ones the base binary
/// is measured on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Only {
    All,
    Timing,
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    fiber: PathBuf,
    out: PathBuf,
    runs: u32,
    idle_secs: u64,
    only: Only,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let (mut fiber, mut out) = (None, None);
    let (mut runs, mut idle_secs, mut only) = (5, 10, Only::All);
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
            "--runs" => {
                runs = u32::try_from(number("--runs")?).map_err(|err| format!("--runs: {err}"))?;
            }
            "--idle-secs" => idle_secs = number("--idle-secs")?,
            "--only" => {
                only = match value.as_str() {
                    "all" => Only::All,
                    "timing" => Only::Timing,
                    _ => return Err(format!("--only takes all or timing, not {value:?}")),
                };
            }
            _ => return Err(format!("unknown argument {flag:?}")),
        }
    }
    Ok(Args {
        fiber: fiber.ok_or("--fiber is required")?,
        out: out.ok_or("--out is required")?,
        runs,
        idle_secs,
        only,
    })
}

/// Runs each selected workload `runs` times. A metric is recorded only when
/// every run produced it; a run that errs ends its workload and is noted.
fn bench(args: &Args) -> Value {
    let clock = run::System;
    let mut failures = Vec::new();
    let mut metrics: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    match home::Home::new(&args.fiber) {
        Err(err) => failures.push(format!("setup: {err}")),
        Ok(home) => {
            let ctx = idle::Ctx {
                home: &home,
                clock: &clock,
                idle: Duration::from_secs(args.idle_secs),
                path: std::env::var_os("PATH"),
            };
            let selected = idle::WORKLOADS
                .iter()
                .filter(|workload| args.only == Only::All || workload.timing);
            for workload in selected {
                if let Some(samples) = repeat(&ctx, workload, args.runs, &mut failures) {
                    metrics.extend(samples);
                }
            }
            if let Err(err) = home.finish() {
                failures.push(format!("cleanup: {err}"));
            }
        }
    }
    json!({
        "schema": SCHEMA,
        "runs": args.runs,
        "idle_secs": args.idle_secs,
        "metrics": metrics,
        "failures": failures,
    })
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
    if !cfg!(target_os = "linux") {
        eprintln!("bench: the benchmarks measure Linux only");
        return ExitCode::from(2);
    }
    let result = bench(&args);
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
