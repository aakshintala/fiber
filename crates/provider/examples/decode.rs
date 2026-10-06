//! The `decode` jig (`docs/testing.md`, "Jigs"): runs a recorded stream
//! through the `openai-responses` decoder and prints what it produced.
//!
//! `cargo run -p provider --example decode -- FILE...`
//!
//! FILE is a recorded stream (`.sse`) or a probe's JSON wrapper
//! (`research/*-probe/raw/`). Each exchange prints a `# label` line, then one
//! line per thing produced, as `kind {payload}` in the event vocabulary
//! (`docs/events.md`): each delta as it arrived, then the reply's actions,
//! its text and its usage, or the failure.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

#[path = "../tests/support/probes.rs"]
mod probes;

use std::path::Path;
use std::process::ExitCode;

use contract::provider::{Delta, ReplyAction};
use provider::openai_responses::decode;
use serde_json::json;

use probes::Recorded;

fn main() -> ExitCode {
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        eprintln!("usage: cargo run -p provider --example decode -- FILE...");
        return ExitCode::from(2);
    }
    for file in &files {
        let exchanges = match probes::read(Path::new(file)) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("decode: {e}");
                return ExitCode::FAILURE;
            }
        };
        for exchange in exchanges {
            println!("# {}", exchange.label);
            match exchange.response {
                Recorded::Stream(bytes) => show(&bytes),
                Recorded::Status(status, body) => {
                    println!("http_status {status} {}", String::from_utf8_lossy(&body));
                }
            }
        }
    }
    ExitCode::SUCCESS
}

fn show(bytes: &[u8]) {
    let mut sink = |delta: Delta| {
        let (kind, payload) = match delta {
            Delta::Text(d) => (
                "assistant_message_delta",
                serde_json::to_value(&d).unwrap_or_default(),
            ),
            Delta::Reasoning(d) => (
                "reasoning_delta",
                serde_json::to_value(&d).unwrap_or_default(),
            ),
            Delta::ToolCallArguments(d) => (
                "tool_call_arguments_delta",
                serde_json::to_value(&d).unwrap_or_default(),
            ),
        };
        println!("{kind} {payload}");
    };
    match decode(bytes, &mut sink) {
        Ok(reply) => {
            for action in &reply.actions {
                match action {
                    ReplyAction::Text(part) => println!(
                        "text_completed {}",
                        serde_json::to_value(part).unwrap_or_default()
                    ),
                    ReplyAction::Reasoning(r) => println!(
                        "reasoning_completed {}",
                        serde_json::to_value(r).unwrap_or_default()
                    ),
                    ReplyAction::ToolCall(c) => println!(
                        "tool_call_requested {}",
                        serde_json::to_value(c).unwrap_or_default()
                    ),
                    ReplyAction::Hosted(h) => println!(
                        "hosted tool_call_requested {} tool_call_completed {}",
                        serde_json::to_value(&h.call).unwrap_or_default(),
                        serde_json::to_value(&h.completed).unwrap_or_default()
                    ),
                }
            }
            println!(
                "assistant_message_completed {}",
                json!({"outcome": "completed", "text": reply.text(), "finish": format!("{:?}", reply.finish)})
            );
            println!(
                "usage_recorded {}",
                json!({"generation_id": reply.generation_id, "tokens": reply.tokens})
            );
        }
        Err(e) => println!(
            "assistant_message_completed {}",
            json!({"outcome": "failed", "error": e.failure("recording")})
        ),
    }
}
