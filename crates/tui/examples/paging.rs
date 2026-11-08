//! The `paging` jig (`docs/testing.md`, "Jigs"): generates a large session
//! and measures the terminal paging it (`docs/tui.md`, "History and
//! paging").
//!
//! `cargo run --release -p tui --example paging -- [--ticket] [scale] [width] [height]`
//!
//! By default, the jig keeps its budgeted session: 10 turns, 965 steps with
//! text, about 1,051 tool calls and 8,074 lines before the trailing running
//! turn. `--ticket` opts into the ticket-shaped session with 400 steps and
//! one 20,000-row message and tool output. `scale` multiplies the steps. A turn still running
//! follows the default session, which the jig appends one line a frame,
//! deltas included. The screen is 160x48 by default. Peak memory is the
//! process's, read from outside, such as with `/usr/bin/time -l`.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use support::clock::System;

#[path = "../tests/support/paging_args.rs"]
mod paging_args;

use paging_args::{Mode, parse_args};

/// One JSON line with what reopening the file cost: parsing each line,
/// folding it, and the frames drawn.
fn open_report(
    events: &str,
    width: u16,
    height: u16,
    frame_every: usize,
) -> Result<String, String> {
    let stages = tui::measure_open(events, width, height, frame_every, Arc::new(System))?;
    let ms = |took: Duration| took.as_secs_f64() * 1000.0;
    Ok(serde_json::json!({
        "parse_ms": ms(stages.parse),
        "fold_ms": ms(stages.fold),
        "frames": stages.frames,
        "frame_ms": ms(stages.frame_time),
        "fold_work": work(&stages.fold_work),
        "frame_work": work(&stages.frame_work),
    })
    .to_string())
}

/// The work counts as one JSON object, by name: the last batch's fold
/// work and the last frame's work, since nothing accumulates them.
fn work(work: &tui::work::Work) -> serde_json::Value {
    serde_json::json!({
        "reply_renders": work.reply_renders,
        "turn_rows": work.turn_rows,
        "page_counts": work.page_counts,
        "index_pages": work.index_pages,
        "usage_summaries": work.usage_summaries,
    })
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (ticket, args) = ticket_args(&args);
    let mode = match parse_args(args) {
        Ok(mode) => mode,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };
    match mode {
        Mode::Open {
            events,
            width,
            height,
            frame_every,
        } => {
            let events = match std::fs::read_to_string(&events) {
                Ok(events) => events,
                Err(error) => {
                    eprintln!("paging: reading {}: {error}", events.display());
                    return ExitCode::FAILURE;
                }
            };
            match open_report(&events, width, height, frame_every) {
                Ok(line) => {
                    println!("{line}");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("paging: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Mode::Paging {
            scale,
            width,
            height,
        } => paging_report(paging_session(ticket, scale), scale, width, height),
    }
}

fn ticket_args(args: &[String]) -> (bool, &[String]) {
    if args.first().is_some_and(|arg| arg == "--ticket") {
        (true, args.get(1..).unwrap_or_default())
    } else {
        (false, args)
    }
}

fn paging_session(ticket: bool, scale: usize) -> String {
    if ticket {
        tui::ticket_session(scale)
    } else {
        tui::paging_session(scale)
    }
}

fn paging_report(events: String, scale: usize, width: u16, height: u16) -> ExitCode {
    println!(
        "session: scale {scale}, {:.2} MiB, at {width}x{height}",
        mib(events.len())
    );
    match tui::measure_paging(&events, width, height, Arc::new(System)) {
        Ok(report) => {
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("paging: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Bytes as MiB, for the report.
fn mib(bytes: usize) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a size in a report, read to two places"
    )]
    let bytes = bytes as f64;
    bytes / 1024.0 / 1024.0
}

#[cfg(test)]
mod tests {
    use super::{paging_session, ticket_args};

    fn step_count(events: &str) -> usize {
        events
            .lines()
            .filter(|line| line.contains("\"step_started\""))
            .count()
    }

    #[test]
    fn ticket_flag_selects_the_ticket_fixture_and_default_stays_budgeted() {
        let ticket_input = vec!["--ticket".to_owned(), "1".to_owned()];
        let (ticket, rest) = ticket_args(&ticket_input);
        assert!(ticket);
        assert_eq!(rest, ["1".to_owned()]);
        assert_eq!(step_count(&paging_session(ticket, 1)), 400);

        let default_args = vec!["1".to_owned()];
        let (ticket, rest) = ticket_args(&default_args);
        assert!(!ticket);
        assert_eq!(rest, ["1".to_owned()]);
        assert_eq!(step_count(&paging_session(ticket, 1)), 965 + 30);
    }
}
