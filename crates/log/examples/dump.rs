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

/// The session's events, one JSON line each.
fn dump(dir: &Path) -> Result<String, log::Error> {
    let mut out = String::new();
    for line in log::read(dir)? {
        out.push_str(&serde_json::to_string(&line)?);
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod common;

#[cfg(test)]
mod tests {
    use std::fs;

    use super::common::*;
    use super::*;
    use log::Log;

    #[test]
    fn it_prints_the_log_a_session_left_one_event_per_line() {
        let tmp = TestDir::new("dump");
        let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
        log.append(&session_started(), None, None).unwrap();
        log.append(&delta("x"), None, None).unwrap();
        log.append(&empty("step_started"), None, None).unwrap();
        let dir = tmp.session(&id("s_1"));
        let out = dump(&dir).unwrap();
        assert_eq!(out, fs::read_to_string(dir.join("events.jsonl")).unwrap());
        assert_eq!(out.lines().count(), 2);
    }

    #[test]
    fn a_missing_directory_fails() {
        let tmp = TestDir::new("dump-missing");
        assert!(dump(&tmp.session(&id("s_none"))).is_err());
    }
}
