//! The `turn` jig (`docs/testing.md`, "Jigs"): runs one turn against the
//! scripted fake provider and prints its events, one JSON line each,
//! ephemeral ones included.
//!
//! `cargo run -p loop --example turn -- [--prompt TEXT] [--tool NAME]...`
//!
//! The fake provider replies with reasoning and text. Each `--tool NAME`
//! makes its first reply call that tool instead, with `{"city": "Paris"}`,
//! and the turn takes another step. One test tool is registered,
//! `get_weather`, which only reads; any other name fails `unknown_tool`.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

#[path = "../tests/support/mod.rs"]
mod support;

use std::process::ExitCode;
use std::sync::Arc;

fn main() -> ExitCode {
    let mut prompt = "Say hello.".to_owned();
    let mut tools = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next()) {
            ("--prompt", Some(text)) => prompt = text,
            ("--tool", Some(name)) => tools.push(name),
            _ => {
                eprintln!("usage: turn [--prompt TEXT] [--tool NAME]...");
                return ExitCode::from(2);
            }
        }
    }
    let mut script = Vec::new();
    if !tools.is_empty() {
        let names: Vec<&str> = tools.iter().map(String::as_str).collect();
        script.push(support::tool_call_reply("Let me check.", &names));
    }
    script.push(support::reasoning_reply(
        "The person wants a greeting.",
        "Hello.",
    ));
    let weather = support::TestTool::reads("get_weather", "Sunny in Paris.");
    let mut session = support::Session::with_tools(script, None, vec![Arc::new(weather)]);
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
