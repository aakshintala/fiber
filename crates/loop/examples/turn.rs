//! The `turn` jig (`docs/testing.md`, "Jigs"): runs one turn against the
//! fake provider server and prints its events, one JSON line each, ephemeral
//! ones included.
//!
//! `cargo run -p loop --example turn -- [--prompt TEXT] [STREAM...]`
//!
//! Each STREAM is a response body in the `openai-responses` wire format, a
//! recorded stream (`.sse`) or a scripted one, served one per model request
//! in order. With none, the fake provider replies with reasoning and text.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

#[path = "../tests/support/mod.rs"]
mod support;

use std::process::ExitCode;

use fakes::Response;

fn main() -> ExitCode {
    let mut prompt = "Say hello.".to_owned();
    let mut script = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--prompt" {
            match args.next() {
                Some(text) => prompt = text,
                None => {
                    eprintln!("usage: turn [--prompt TEXT] [STREAM...]");
                    return ExitCode::from(2);
                }
            }
            continue;
        }
        match std::fs::read(&arg) {
            Ok(bytes) => script.push(Response::stream(bytes)),
            Err(e) => {
                eprintln!("turn: {arg}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    if script.is_empty() {
        script.push(support::reasoning_reply(
            "The person wants a greeting.",
            "Hello.",
        ));
    }
    let mut session = support::Session::new(script, None);
    if session.inbox.send(support::message(&prompt)).is_err() {
        return ExitCode::FAILURE;
    }
    session.turn();
    for line in session.lines() {
        match serde_json::to_string(&line) {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("turn: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}
