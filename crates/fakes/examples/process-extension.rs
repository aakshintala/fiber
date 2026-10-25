//! The `process-extension` fixture (`docs/testing/fakes.md`): a checked-in
//! Rust program speaking the process extension wire over its stdin and
//! stdout, with protocol modes and acknowledged holds.
//!
//! `cargo run -p fakes --example process-extension -- <mode>`, where mode is
//! one of:
//!
//! - `echo`: writes each stdin line back to stdout verbatim, until EOF.
//! - `hold`: reads one stdin line, prints `{"held":true}`, then waits for
//!   EOF and prints `{"released":true}` before exiting 0. The `held` line
//!   acknowledges the hold is engaged; closing stdin releases it.
//!
//! Every stdout line is one JSON line, flushed at once. Diagnostics go to
//! stderr, never stdout. Any other use exits 2.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a fixture's output is its interface"
)]

use std::io::{BufRead, BufReader, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "echo" => echo(),
        "hold" => hold(),
        _ => {
            eprintln!("usage: process-extension <echo|hold>");
            ExitCode::from(2)
        }
    }
}

/// Writes each stdin line back to stdout verbatim (flushed), until EOF.
fn echo() -> ExitCode {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in BufReader::new(stdin.lock()).lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                eprintln!("process-extension: stdin: {err}");
                return ExitCode::FAILURE;
            }
        };
        if out.write_all(line.as_bytes()).is_err() || out.write_all(b"\n").is_err() {
            return ExitCode::FAILURE;
        }
        if out.flush().is_err() {
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// Reads one stdin line, acknowledges the hold, then waits for EOF before
/// releasing and exiting. Closing stdin releases the hold.
fn hold() -> ExitCode {
    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let mut first = String::new();
    match input.read_line(&mut first) {
        Ok(0) => {
            eprintln!("process-extension: hold ended before its line");
            return ExitCode::from(2);
        }
        Ok(_) => {}
        Err(err) => {
            eprintln!("process-extension: stdin: {err}");
            return ExitCode::FAILURE;
        }
    }
    let mut out = std::io::stdout().lock();
    if out.write_all(b"{\"held\":true}\n").is_err() || out.flush().is_err() {
        return ExitCode::FAILURE;
    }
    for line in input.lines() {
        if let Err(err) = line {
            eprintln!("process-extension: stdin: {err}");
            return ExitCode::FAILURE;
        }
    }
    if out.write_all(b"{\"released\":true}\n").is_err() || out.flush().is_err() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
