//! The `probe` jig (`docs/tools.md`, "Deferral is a property of the
//! model"): runs the probe's three requests against one model and prints
//! pass or fail for `load` and `cache`.
//!
//! `cargo run -p provider --example probe -- PROTOCOL BASE_URL MODEL KEY [--bearer] [--store=false] [NAME:VALUE]...`
//!
//! PROTOCOL is `anthropic-messages` or `openai-responses`. KEY is a file
//! holding the key, or `env:NAME` to read it from that environment
//! variable, so a live run never writes the key to disk. The key is sent
//! as the protocol sends it.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

#[path = "../tests/support/probe.rs"]
mod support;

use std::process::ExitCode;

use provider::anthropic_messages::Messages;
use provider::openai_responses::Responses;
use provider::{Compat, Endpoint};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = match support::parse_args(&args) {
        Ok(parsed) => parsed,
        Err(message) => return usage(&message),
    };
    let key = match support::key_from(&parsed.key_spec, &|name: &str| std::env::var(name).ok()) {
        Ok(key) => key,
        Err(message) => return usage(&message),
    };
    let endpoint = Endpoint {
        provider: "probe".into(),
        model: parsed.model.clone(),
        base_url: parsed.base_url.clone(),
        key: Some(contract::Secret::new(key)),
        headers: parsed.headers,
        compat: Compat {
            bearer: parsed.bearer,
            store: parsed.store,
            ..Compat::default()
        },
        ..Endpoint::default()
    };
    let reference = endpoint.reference();
    let outcome = match parsed.protocol.as_str() {
        "anthropic-messages" => {
            support::run(&Messages::new(endpoint).deferred_tools(true), &reference)
        }
        "openai-responses" => {
            support::run(&Responses::new(endpoint).deferred_tools(true), &reference)
        }
        other => {
            return usage(&format!(
                "probe: {other} is not a protocol; use anthropic-messages or openai-responses"
            ));
        }
    };
    for line in support::report(&parsed.protocol, &parsed.model, &outcome) {
        println!("{line}");
    }
    ExitCode::from(support::exit_code(&outcome))
}

/// Prints `message` with the usage and exits 2.
fn usage(message: &str) -> ExitCode {
    eprintln!("{message}");
    eprintln!(
        "usage: cargo run -p provider --example probe -- PROTOCOL BASE_URL MODEL KEY [--bearer] [--store=false] [NAME:VALUE]..."
    );
    ExitCode::from(2)
}
