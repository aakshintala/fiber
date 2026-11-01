//! The `review` jig (`docs/testing.md`, "Jigs"): prints what the loop's
//! reviewer would be sent for a session directory's calls, one JSON line
//! per replayable call, selection request and unreplayable call.
//!
//! `cargo run -p loop --example review -- [--notes FILE] [--keep-all]
//! [--sample N] [--scrub [--as-home PATH --owner-name NAME --owner-email
//! EMAIL] | --import-claude PATH | --import-pi PATH | --scan FILE]
//! [<session directory>]`
//!
//! At each completed handoff with person messages the jig prints the
//! reviewer's selection request and reads one answer line on stdin, in the
//! selection's number grammar (`none`, or comma-separated numbers). EOF
//! keeps every person message through the existing fallback, as
//! `--keep-all` does without asking. A reviewed call without its start
//! or its denial carrying its declared effects prints one `unreplayable`
//! line instead of invented input. The sampling flags instead print one
//! label-input line per replayable call; the importer flags fold native
//! session files instead of a Fiber session directory; `--scan FILE`
//! prints the final-set scan of the committed file. The source directory
//! is only read, never written.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

#[path = "../tests/support/mod.rs"]
mod support;

use std::path::Path;
use std::process::ExitCode;

/// Reports one sampling run's loud drops, then its exit.
fn sampled(report: support::review::SampleReport) -> ExitCode {
    for line in report.dropped.iter().chain(report.unsupported.iter()) {
        eprintln!("{line}");
    }
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let (rest, sample_opts) = match support::reviewer_eval::split_argv(std::env::args().skip(1)) {
        Ok(split) => split,
        Err(problem) => {
            eprintln!("{problem}");
            return ExitCode::from(2);
        }
    };
    if let Some(scan) = sample_opts.scan.clone() {
        return match support::review::run_scan(
            Path::new(&scan),
            std::io::stdout(),
            std::io::stderr(),
        ) {
            Ok(_) => ExitCode::SUCCESS,
            Err(problem) => {
                eprintln!("{problem}");
                ExitCode::FAILURE
            }
        };
    }
    if rest.is_empty()
        && (!sample_opts.import_claude.is_empty() || !sample_opts.import_pi.is_empty())
    {
        return match support::review::run_sampled(None, &sample_opts, std::io::stdout()) {
            Ok(report) => sampled(report),
            Err(problem) => {
                eprintln!("{problem}");
                ExitCode::FAILURE
            }
        };
    }
    let args = match support::review::parse_args(rest.into_iter()) {
        Ok(args) => args,
        Err(problem) => {
            eprintln!("{problem}");
            return ExitCode::from(2);
        }
    };
    if sample_opts.wants_sampled() {
        return match support::review::run_sampled(Some(&args.dir), &sample_opts, std::io::stdout())
        {
            Ok(report) => sampled(report),
            Err(problem) => {
                eprintln!("{problem}");
                ExitCode::FAILURE
            }
        };
    }
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
