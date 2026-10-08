//! The paging workload (`docs/performance.md`, "Budgets"): the `tui`
//! crate's `paging` jig (`docs/testing.md`, "Jigs") at scale 1 and 160 by
//! 48, under GNU time, which reports its peak RSS. The jig prints five
//! frame timings and the size of the session it paged.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::json;

use crate::idle::{Ctx, Samples, Workload};
use crate::run::{self, Finished};

/// How long one run of the jig may take.
pub(crate) const JIG: Duration = Duration::from_secs(120);

/// GNU time, which the Linux runner installs as the `time` package.
const TIME: &str = "/usr/bin/time";

/// The workload, which runs only when the harness is given the jig.
pub(crate) const WORKLOADS: [Workload; 1] = [Workload {
    name: "paging",
    timing: true,
    run: paging,
}];

/// What the jig measured: five timings in milliseconds, and the session's
/// size.
#[derive(Debug, PartialEq)]
pub(crate) struct Figures {
    pub(crate) open_ms: f64,
    pub(crate) load_ms: f64,
    pub(crate) jump_ms: f64,
    pub(crate) width_ms: f64,
    pub(crate) append_ms: f64,
    pub(crate) lines: u64,
    pub(crate) turns: u64,
    pub(crate) calls: u64,
    pub(crate) pages: u64,
    pub(crate) rows: u64,
}

/// GNU time at `time` running the jig at `jig` with its workload pinned on
/// the command line, so a change to the jig's defaults cannot move what is
/// measured. The environment holds `PATH` alone, so GNU time reports in the
/// C locale.
pub(crate) fn command(time: &Path, jig: &Path, path: Option<&OsStr>) -> Command {
    let mut command = Command::new(time);
    command
        .arg("-v")
        .arg(jig)
        .args(["1", "160", "48"])
        .env_clear()
        .env("PATH", path.unwrap_or_default());
    command
}

/// The text after `label` on the line that starts with it.
fn after<'a>(stdout: &'a str, label: &str) -> Result<&'a str, String> {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(label))
        .ok_or_else(|| format!("the paging jig printed no {:?} line", label.trim_end()))
}

/// The milliseconds after `label`, up to ` ms`: finite and not negative.
fn ms(stdout: &str, label: &str) -> Result<f64, String> {
    let rest = after(stdout, label)?;
    let bad = || {
        format!(
            "the paging jig's {:?} is not a time: {rest:?}",
            label.trim_end()
        )
    };
    let (number, _) = rest.split_once(" ms").ok_or_else(bad)?;
    match number.parse::<f64>() {
        Ok(value) if value.is_finite() && value >= 0.0 => Ok(value),
        Ok(_) | Err(_) => Err(bad()),
    }
}

/// The whole number after `label`, the rest of its line.
fn count(stdout: &str, label: &str) -> Result<u64, String> {
    let rest = after(stdout, label)?;
    rest.trim().parse().map_err(|_| {
        format!(
            "the paging jig's {:?} is not a count: {rest:?}",
            label.trim_end()
        )
    })
}

/// The jig's report, from its stdout.
pub(crate) fn report(stdout: &str) -> Result<Figures, String> {
    Ok(Figures {
        open_ms: ms(stdout, "open pass and first frame: ")?,
        load_ms: ms(stdout, "slowest frame that loaded pages: ")?,
        jump_ms: ms(stdout, "slowest jump frame: ")?,
        width_ms: ms(stdout, "slowest re-count at a new width: ")?,
        append_ms: ms(stdout, "slowest append frame: ")?,
        lines: count(stdout, "lines: ")?,
        turns: count(stdout, "turns: ")?,
        calls: count(stdout, "calls: ")?,
        pages: count(stdout, "pages: ")?,
        rows: count(stdout, "rows: ")?,
    })
}

/// The peak RSS GNU time's `-v` report gives, in KiB; never zero.
pub(crate) fn max_rss_kib(stderr: &str) -> Result<u64, String> {
    const LABEL: &str = "Maximum resident set size (kbytes): ";
    let peak = stderr
        .lines()
        .find_map(|line| line.trim().strip_prefix(LABEL))
        .ok_or("GNU time printed no Maximum resident set size")?;
    match peak.trim().parse::<u64>() {
        Ok(kib) if kib > 0 => Ok(kib),
        Ok(_) | Err(_) => Err(format!(
            "GNU time's Maximum resident set size is not a peak: {peak:?}"
        )),
    }
}

/// One run's samples from the finished jig. A jig that failed, or that GNU
/// time saw killed by a signal, is an error carrying its stderr.
pub(crate) fn samples(finished: &Finished) -> Result<Samples, String> {
    let stderr = &finished.stderr;
    let killed = stderr
        .lines()
        .any(|line| line.trim().starts_with("Command terminated by signal"));
    if !finished.status.success() || killed {
        return Err(format!(
            "the paging jig exited {}; stderr: {}",
            finished.status,
            stderr.trim()
        ));
    }
    let figures = report(&finished.stdout)?;
    Ok(vec![
        ("paging_rss_kib", json!(max_rss_kib(stderr)?)),
        ("paging_open_ms", json!(figures.open_ms)),
        ("paging_load_ms", json!(figures.load_ms)),
        ("paging_jump_ms", json!(figures.jump_ms)),
        ("paging_width_ms", json!(figures.width_ms)),
        ("paging_append_ms", json!(figures.append_ms)),
        (
            "paging_counts",
            json!({
                "lines": figures.lines,
                "turns": figures.turns,
                "calls": figures.calls,
                "pages": figures.pages,
                "rows": figures.rows,
            }),
        ),
    ])
}

fn paging(ctx: &Ctx<'_>, _notes: &mut Vec<String>) -> Result<Samples, String> {
    let jig = ctx.paging.ok_or("the paging workload needs --paging")?;
    let mut command = command(Path::new(TIME), jig, ctx.path.as_deref());
    let finished = run::run_to_end(&mut command, ctx.clock, JIG, "the paging jig")?;
    samples(&finished)
}

#[cfg(test)]
#[path = "paging_tests.rs"]
mod tests;
