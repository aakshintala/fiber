//! The `hover` jig (`docs/testing.md`, "Jigs"): measures what hover costs
//! the terminal (`docs/tui.md`, "Mouse and hover").
//!
//! `cargo run --release -p tui --example hover -- <events.jsonl> [width] [height]`
//!
//! It folds the events file, puts the request the panel shows aside so it
//! waits on the badge, then sends motion reports through the terminal's
//! own screen, four ways: a still pointer (one cell of the badge, over and
//! over); one target (along the badge's cells); a change of target on
//! every report (on and off the badge); and a fast sweep (one row down and
//! 7 columns right per report, over the whole screen). For each it prints
//! the reports, the frames and bytes they wrote, and the mean time per
//! report, the median of 5 runs less the time to fold and draw the first
//! frame. `examples/hover.jsonl` is a session with one request waiting.
//! The size defaults to 160 by 48.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str =
    "usage: cargo run --release -p tui --example hover -- <events.jsonl> [width] [height]";

/// Reports per case.
const REPORTS: u16 = 20_000;
/// Runs per case; the median is printed.
const RUNS: usize = 5;
/// The badge's width: `! 1 waiting · /approvals or ⌥A`.
const BADGE: u16 = 30;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let width = args.get(1).map_or("160", String::as_str);
    let height = args.get(2).map_or("48", String::as_str);
    let (Ok(width), Ok(height)) = (width.parse::<u16>(), height.parse::<u16>()) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if width < BADGE || height < 2 {
        eprintln!("hover: the screen must be at least {BADGE} by 2");
        return ExitCode::from(2);
    }
    let events = match std::fs::read_to_string(path) {
        Ok(events) => events,
        Err(error) => {
            eprintln!("hover: {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    // The badge is on the row above the input line.
    let row = height - 2;
    let cases: [(&str, Vec<(u16, u16)>); 4] = [
        ("still pointer", (0..REPORTS).map(|_| (3, row)).collect()),
        (
            "one target",
            (0..REPORTS).map(|at| (at % BADGE, row)).collect(),
        ),
        (
            "target change",
            (0..REPORTS)
                .map(|at| if at % 2 == 0 { (3, row) } else { (3, 0) })
                .collect(),
        ),
        (
            "fast sweep",
            (0..REPORTS)
                .map(|at| {
                    let at = u32::from(at);
                    let col = at.saturating_mul(7) % u32::from(width);
                    let line = at % u32::from(height);
                    (
                        u16::try_from(col).unwrap_or(0),
                        u16::try_from(line).unwrap_or(0),
                    )
                })
                .collect(),
        ),
    ];
    let base = match median(|| tui::hover_frames(&events, width, height, &[])) {
        Ok((time, _)) => time,
        Err(error) => {
            eprintln!("hover: {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("{width}x{height}, {RUNS} runs per case, median");
    println!("case            reports  frames    bytes  time/report");
    for (name, pointer) in cases {
        let (time, bytes) = match median(|| tui::hover_frames(&events, width, height, &pointer)) {
            Ok(measured) => measured,
            Err(error) => {
                eprintln!("hover: {path}: {error}");
                return ExitCode::FAILURE;
            }
        };
        let frames = bytes.iter().filter(|written| **written > 0).count();
        let total: usize = bytes.iter().sum();
        let per = time.saturating_sub(base) / u32::from(REPORTS);
        println!(
            "{name:<15} {reports:>7} {frames:>7} {total:>8} {micros:>9.1} µs",
            reports = pointer.len(),
            micros = per.as_secs_f64() * 1e6,
        );
    }
    ExitCode::SUCCESS
}

/// Runs `measure` [`RUNS`] times, returning the median time and the last
/// run's bytes per report.
fn median(
    measure: impl Fn() -> Result<Vec<usize>, String>,
) -> Result<(Duration, Vec<usize>), String> {
    let mut times = Vec::with_capacity(RUNS);
    let mut bytes = Vec::new();
    for _ in 0..RUNS {
        let (time, written) = timed(&measure);
        bytes = written?;
        times.push(time);
    }
    times.sort();
    Ok((times.get(RUNS / 2).copied().unwrap_or_default(), bytes))
}

/// The wall time of one call of `measure`, and its result.
#[allow(
    clippy::disallowed_methods,
    reason = "the jig measures wall time; nothing else reads it"
)]
fn timed<T>(measure: impl Fn() -> T) -> (Duration, T) {
    let start = std::time::Instant::now();
    let result = measure();
    (start.elapsed(), result)
}
