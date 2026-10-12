//! The `paging` jig's argument parsing, shared by the jig and its test.
//!
//! The jig stays a runnable binary (`test = false`), so the test cannot
//! call into it as a library: both include this file with `#[path]`.

use std::path::PathBuf;

pub(crate) const USAGE: &str = "usage: cargo run --release -p tui --example paging -- [scale] [width] [height]\n\
    usage: cargo run --release -p tui --example paging -- open <events-file> <width> <height> <frame_every|end>";

/// What the jig runs: its generated session, or one file's log reopened.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Mode {
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
pub(crate) fn parse_frame_every(text: &str) -> Result<usize, String> {
    if text == "end" {
        return Ok(usize::MAX);
    }
    text.parse::<usize>().map_err(|_| USAGE.to_owned())
}

/// The command line: `open` with its file, size and frame count, or the
/// positional scale, width and height, each defaulted as the jig ran
/// before the open mode existed.
pub(crate) fn parse_args(args: &[String]) -> Result<Mode, String> {
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
