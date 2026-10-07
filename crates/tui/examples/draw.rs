//! The `draw` jig (`docs/testing.md`, "Jigs"): draws an events file at a
//! given width and prints the screen as text.
//!
//! `cargo run -p tui --example draw -- <events.jsonl> <width> [height]`
//!
//! Each line is one envelope of a session's stream, as the session log
//! holds it. The height defaults to 24.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

use std::process::ExitCode;

const USAGE: &str = "usage: cargo run -p tui --example draw -- <events.jsonl> <width> [height]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(path), Some(width)) = (args.first(), args.get(1)) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let height = args.get(2).map_or("24", String::as_str);
    let (Ok(width), Ok(height)) = (width.parse::<u16>(), height.parse::<u16>()) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let events = match std::fs::read_to_string(path) {
        Ok(events) => events,
        Err(error) => {
            eprintln!("draw: {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    match tui::draw(&events, width, height) {
        Ok(screen) => {
            print!("{screen}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("draw: {path}: {error}");
            ExitCode::FAILURE
        }
    }
}
