//! The paging workload (`docs/performance.md`, "Budgets"): the `tui`
//! crate's `paging` jig (`docs/testing.md`, "Jigs") at scales 1 and 16,
//! each at 160 by 48, under GNU time, which reports its peak RSS. Each
//! invocation runs both scales in order, so every scale-16 sample pairs
//! with the same run's scale-1 sample and the ratio, not the machine,
//! decides. The jig prints five frame timings, a no-match search timing
//! and the size of the session it paged.

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

/// What the jig measured: five timings in milliseconds, the absent
/// search's timing, and the session's size. Reports older than the
/// absent search carry no `search_ms`: they are legacy reports.
#[derive(Debug, PartialEq)]
pub(crate) struct Figures {
    pub(crate) open_ms: f64,
    pub(crate) load_ms: f64,
    pub(crate) jump_ms: f64,
    pub(crate) width_ms: f64,
    pub(crate) append_ms: f64,
    /// The whole-log search with no match, in milliseconds: `None` on a
    /// legacy report, which predates it.
    pub(crate) search_ms: Option<f64>,
    pub(crate) lines: u64,
    pub(crate) turns: u64,
    pub(crate) calls: u64,
    pub(crate) pages: u64,
    pub(crate) rows: u64,
}

/// GNU time at `time` running the jig at `jig` at `scale` with its
/// workload pinned on the command line, so a change to the jig's defaults
/// cannot move what is measured. The environment holds `PATH` alone, so
/// GNU time reports in the C locale.
pub(crate) fn command(time: &Path, jig: &Path, path: Option<&OsStr>, scale: u32) -> Command {
    let mut command = Command::new(time);
    command
        .arg("-v")
        .arg(jig)
        .args([scale.to_string(), "160".to_owned(), "48".to_owned()])
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

/// The jig's report, from its stdout. The absent-search line is
/// optional only at this boundary: a report without it is a legacy one,
/// and a report with a malformed one is an error.
pub(crate) fn report(stdout: &str) -> Result<Figures, String> {
    Ok(Figures {
        open_ms: ms(stdout, "open pass and first frame: ")?,
        load_ms: ms(stdout, "slowest frame that loaded pages: ")?,
        jump_ms: ms(stdout, "slowest jump frame: ")?,
        width_ms: ms(stdout, "slowest re-count at a new width: ")?,
        append_ms: ms(stdout, "slowest append frame: ")?,
        search_ms: absent_ms(stdout)?,
        lines: count(stdout, "lines: ")?,
        turns: count(stdout, "turns: ")?,
        calls: count(stdout, "calls: ")?,
        pages: count(stdout, "pages: ")?,
        rows: count(stdout, "rows: ")?,
    })
}

/// The absent search's milliseconds, or `None` when its line is missing:
/// a consistent report without it is a legacy one. A present line must
/// read `<ms> ms, 0 matches`: any other count is an error, never a
/// sample, since the jig only prints the line for a zero-match scan.
fn absent_ms(stdout: &str) -> Result<Option<f64>, String> {
    const LABEL: &str = "search of the whole log with no match: ";
    let Some(line) = stdout.lines().find_map(|line| line.strip_prefix(LABEL)) else {
        return Ok(None);
    };
    let bad = || {
        format!(
            "the paging jig's {:?} is not a zero-match time: {line:?}",
            LABEL.trim_end()
        )
    };
    let (number, matches) = line.split_once(" ms, ").ok_or_else(bad)?;
    if matches != "0 matches" {
        return Err(bad());
    }
    match number.parse::<f64>() {
        Ok(value) if value.is_finite() && value >= 0.0 => Ok(Some(value)),
        Ok(_) | Err(_) => Err(bad()),
    }
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

/// One run's samples from its finished jig: scale 1's, then scale 16's.
/// A consistent legacy pair keeps the existing scale-1 samples and omits
/// the additive metrics, so the base judge shows only new comparisons
/// unavailable; a mixed pair is malformed. A jig that failed, or that GNU
/// time saw killed by a signal, is an error carrying its stderr, with no
/// partial samples after a failed child.
pub(crate) fn samples(first: &Finished, second: &Finished) -> Result<Samples, String> {
    let one = finished(first)?;
    let sixteen = finished(second)?;
    match (one.figures.search_ms, sixteen.figures.search_ms) {
        (None, None) => Ok(samples_scale1(&one)),
        (Some(search_ms), Some(scale16_search_ms)) => {
            let mut samples = samples_scale1(&one);
            samples.push(("paging_search_ms", json!(search_ms)));
            samples.push(("paging_scale16_width_ms", json!(sixteen.figures.width_ms)));
            samples.push(("paging_scale16_search_ms", json!(scale16_search_ms)));
            samples.push((
                "paging_scale16_counts",
                json!({
                    "lines": sixteen.figures.lines,
                    "turns": sixteen.figures.turns,
                    "calls": sixteen.figures.calls,
                    "pages": sixteen.figures.pages,
                    "rows": sixteen.figures.rows,
                }),
            ));
            Ok(samples)
        }
        _ => Err("the paging pair mixes a legacy report with a new one".to_owned()),
    }
}

/// A finished jig's validated figures and peak RSS: a jig that failed,
/// or that GNU time saw killed by a signal, is an error carrying its
/// stderr.
struct Checked {
    figures: Figures,
    rss_kib: u64,
}

fn finished(finished: &Finished) -> Result<Checked, String> {
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
    Ok(Checked {
        figures: report(&finished.stdout)?,
        rss_kib: max_rss_kib(stderr)?,
    })
}

/// Scale 1's samples: the six budget metrics and the counts, as before.
fn samples_scale1(one: &Checked) -> Samples {
    let figures = &one.figures;
    vec![
        ("paging_rss_kib", json!(one.rss_kib)),
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
    ]
}

fn paging(ctx: &Ctx<'_>, _notes: &mut Vec<String>) -> Result<Samples, String> {
    let jig = ctx.paging.ok_or("the paging workload needs --paging")?;
    // Both scales in order, at 160 by 48 under the existing child
    // runner: every scale-16 sample pairs with the same run's scale-1
    // sample. A failed child fails the run with no partial samples.
    let mut first = command(Path::new(TIME), jig, ctx.path.as_deref(), 1);
    let one = run::run_to_end(&mut first, ctx.clock, JIG, "the paging jig")?;
    let mut second = command(Path::new(TIME), jig, ctx.path.as_deref(), 16);
    let sixteen = run::run_to_end(&mut second, ctx.clock, JIG, "the paging jig")?;
    samples(&one, &sixteen)
}

#[cfg(test)]
#[path = "paging_tests.rs"]
mod tests;
