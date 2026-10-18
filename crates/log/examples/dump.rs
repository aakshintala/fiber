//! The `dump` jig (`docs/testing.md`, "Jigs"): prints a session directory's
//! events, one per line, up to the last complete line.
//!
//! `cargo run -p log --example dump -- <session directory>`

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

use std::path::Path;
use std::process::ExitCode;

#[path = "../tests/support/dump.rs"]
mod support;

use support::dump;

// The support file's tests read the parent's common helpers, so the
// example keeps this module for them.
#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod common;

fn main() -> ExitCode {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: cargo run -p log --example dump -- <session directory>");
        return ExitCode::from(2);
    };
    match dump(Path::new(&dir)) {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("dump: {e}");
            ExitCode::FAILURE
        }
    }
}
