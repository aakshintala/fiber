//! The `paging` jig (`docs/testing.md`, "Jigs"): generates a large session
//! and measures the terminal paging it (`docs/tui.md`, "History and
//! paging").
//!
//! `cargo run --release -p tui --example paging -- [scale] [width] [height]`
//!
//! The session is shaped like the prototype's heavy fixture
//! (`research/tui-prototype/LARGE.md`): 10 turns, 965 steps with text,
//! about 1,047 tool calls with long results, in Fiber's own event kinds.
//! `scale` repeats it, 1 by default. A turn still running follows it, which
//! the jig appends one line a frame, deltas included. The screen is 160x48
//! by default. Peak memory is the process's, read from outside, such as
//! with `/usr/bin/time -l`.

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

use paging_args::{Mode, USAGE, parse_args};

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
    let mode = match parse_args(&args) {
        Ok(mode) => mode,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };
    if let Mode::Open {
        events,
        width,
        height,
        frame_every,
    } = mode
    {
        let events = match std::fs::read_to_string(&events) {
            Ok(events) => events,
            Err(error) => {
                eprintln!("paging: reading {}: {error}", events.display());
                return ExitCode::FAILURE;
            }
        };
        return match open_report(&events, width, height, frame_every) {
            Ok(line) => {
                println!("{line}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("paging: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let Mode::Paging {
        scale,
        width,
        height,
    } = mode
    else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let events = tui::paging_session(scale);
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
