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

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime};

use contract::clock::{Clock, Wake};

const USAGE: &str = "usage: cargo run --release -p tui --example paging -- [scale] [width] [height]\n\
    usage: cargo run --release -p tui --example paging -- open <events-file> <width> <height> <frame_every|end>";

/// What the jig runs: its generated session, or one file's log reopened.
#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Paging {
        scale: usize,
        width: u16,
        height: u16,
    },
    Open {
        events: PathBuf,
        width: u16,
        height: u16,
        frame_every: usize,
    },
}

/// How many lines each frame folds: a number, or `end` for the single
/// final frame. The bench prints the single frame's count as its number,
/// so both spell it.
fn parse_frame_every(text: &str) -> Result<usize, String> {
    if text == "end" {
        return Ok(usize::MAX);
    }
    text.parse::<usize>().map_err(|_| USAGE.to_owned())
}

/// The command line: `open` with its file, size and frame count, or the
/// positional scale, width and height, each defaulted as the jig ran
/// before the open mode existed.
fn parse_args(args: &[String]) -> Result<Mode, String> {
    if args.first().is_some_and(|first| first == "open") {
        let rest = args.get(1..).unwrap_or_default();
        let [file, width, height, frame_every] = rest else {
            return Err(USAGE.to_owned());
        };
        let width = width.parse::<u16>().map_err(|_| USAGE.to_owned())?;
        let height = height.parse::<u16>().map_err(|_| USAGE.to_owned())?;
        return Ok(Mode::Open {
            events: PathBuf::from(file),
            width,
            height,
            frame_every: parse_frame_every(frame_every)?,
        });
    }
    let arg = |at: usize, default: &str| args.get(at).map_or(default, String::as_str).to_owned();
    let (Ok(scale), Ok(width), Ok(height)) = (
        arg(0, "1").parse::<usize>(),
        arg(1, "160").parse::<u16>(),
        arg(2, "48").parse::<u16>(),
    ) else {
        return Err(USAGE.to_owned());
    };
    Ok(Mode::Paging {
        scale,
        width,
        height,
    })
}

/// One JSON line with what reopening the file cost: parsing each line,
/// folding it, and the frames drawn.
fn open_report(
    events: &str,
    width: u16,
    height: u16,
    frame_every: usize,
) -> Result<String, String> {
    let stages = tui::measure_open(events, width, height, frame_every, Arc::new(ProcessClock))?;
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
    match tui::measure_paging(&events, width, height, Arc::new(ProcessClock)) {
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

/// The process clock. `tui` cannot depend on `main`, which owns the one
/// the binary uses, so the jig keeps a small one.
struct ProcessClock;

impl Clock for ProcessClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "the paging jig times frames on the process clock; tui cannot depend on main"
    )]
    fn now(&self) -> Instant {
        Instant::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the paging jig reads the process clock; tui cannot depend on main"
    )]
    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }

    fn sleep(&self, _duration: Duration) {}

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        wait(until.map(|until| until.saturating_duration_since(self.now())));
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}
