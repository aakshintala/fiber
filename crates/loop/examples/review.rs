//! The `review` jig (`docs/testing.md`, "Jigs"): prints what the loop's
//! reviewer would be sent for a session directory's calls, one JSON line
//! per replayable call, selection request and unreplayable call.
//!
//! `cargo run -p loop --example review -- [--notes FILE] [--keep-all]
//! <session directory>`
//!
//! At each completed handoff with person messages the jig prints the
//! reviewer's selection request and reads one answer line on stdin, in the
//! selection's number grammar (`none`, or comma-separated numbers). EOF
//! keeps every person message through the existing fallback, as
//! `--keep-all` does without asking. A reviewed call without logged
//! started effects prints one `unreplayable` line instead of invented
//! input. The source directory is only read, never written.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

#[path = "../tests/support/mod.rs"]
mod support;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args = support::review::parse_args(std::env::args().skip(1));
    let args = match args {
        Ok(args) => args,
        Err(problem) => {
            eprintln!("{problem}");
            return ExitCode::from(2);
        }
    };
    match support::review::run(
        args,
        std::io::BufReader::new(std::io::stdin()),
        std::io::stdout(),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(problem) => {
            eprintln!("{problem}");
            ExitCode::FAILURE
        }
    }
}
