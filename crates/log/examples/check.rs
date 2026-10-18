//! The `check` jig (`docs/testing.md`, "Jigs"): checks a session directory
//! against what the directory alone shows. Every line parses as an event, and
//! `seq` runs from 0 with no gaps. Exits 1 naming the first line that fails.
//!
//! `cargo run -p log --example check -- <session directory>`

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

use std::path::Path;
use std::process::ExitCode;

#[path = "../tests/support/check.rs"]
mod support;

use support::check;

// The support file's tests read the parent's common helpers, so the
// example keeps this module for them.
#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod common;

fn main() -> ExitCode {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: cargo run -p log --example check -- <session directory>");
        return ExitCode::from(2);
    };
    match check(Path::new(&dir)) {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(problem) => {
            eprintln!("check: {problem}");
            ExitCode::FAILURE
        }
    }
}
