//! The `framing` jig (`docs/testing/fakes.md`, "Jigs"): reads one file
//! through the wire line reader with a stalled read after a cut point,
//! printing what the read returns and what a follow-up read finds.
//!
//! `cargo run -p support --example framing -- <file> --error-after <n> [--max <n>]`

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

#[path = "../tests/support/framing_input.rs"]
mod framing_input;

use std::io;
use std::process::ExitCode;

use framing_input::FragmentThenError;

const USAGE: &str =
    "usage: cargo run -p support --example framing -- <file> --error-after <n> [--max <n>]";

fn main() -> ExitCode {
    let args = match framing_input::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(()) => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let data = match std::fs::read(&args.file) {
        Ok(data) => data,
        Err(error) => {
            eprintln!("cannot read {}: {error}", args.file.display());
            return ExitCode::from(1);
        }
    };
    let max = args.max.unwrap_or(support::framing::MAX_LINE);
    let mut read = FragmentThenError::new(data, args.error_after);
    print_outcome("first", support::framing::read_line(&mut read, max));
    print_outcome("second", support::framing::read_line(&mut read, max));
    ExitCode::SUCCESS
}

/// One read's outcome on its own line: the line without its feed, `none`
/// at end of input, or the error kind with nothing returned.
fn print_outcome(tag: &str, outcome: io::Result<Option<Vec<u8>>>) {
    match outcome {
        Ok(Some(line)) => println!("{tag}: ok {}", String::from_utf8_lossy(&line)),
        Ok(None) => println!("{tag}: none"),
        Err(error) => println!("{tag}: err {:?}", error.kind()),
    }
}
