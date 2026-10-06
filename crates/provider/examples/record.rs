//! The `record` jig (`docs/testing.md`, "Jigs"): sends one live request and
//! saves the reply as a recorded stream: the response body's bytes only,
//! never a header or the key.
//!
//! `cargo run -p provider --example record -- PROTOCOL BASE_URL MODEL KEY REQUEST OUT [NAME:VALUE...]`
//!
//! PROTOCOL is `openai-responses` or `anthropic-messages`. KEY is a file
//! holding the key, or `env:NAME` to read it from that environment variable,
//! so a live recording never writes the key to disk. The key is sent as the
//! protocol sends it. REQUEST is a JSON `ModelRequest` (`contract::provider`).
//! OUT receives the stream, ready for the `decode` jig and the tests. Each
//! NAME:VALUE is a header to send, such as the `x-opencode-session` OpenCode
//! Go requires.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

use std::fs::File;
use std::io::Read;
use std::process::ExitCode;

use contract::provider::ModelRequest;
use provider::Endpoint;
use provider::anthropic_messages::Messages;
use provider::openai_responses::Responses;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [protocol, base_url, model, key, request, out, headers @ ..] = args.as_slice() else {
        eprintln!(
            "usage: cargo run -p provider --example record -- \
             PROTOCOL BASE_URL MODEL KEY REQUEST OUT [NAME:VALUE...]"
        );
        return ExitCode::from(2);
    };
    let Some(headers) = headers
        .iter()
        .map(|h| {
            h.split_once(':')
                .map(|(n, v)| (n.to_owned(), v.trim().to_owned()))
        })
        .collect::<Option<Vec<_>>>()
    else {
        eprintln!("record: a header is NAME:VALUE");
        return ExitCode::from(2);
    };
    match record(protocol, base_url, model, key, request, out, headers) {
        Ok(bytes) => {
            println!("saved {bytes} bytes to {out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("record: {e}");
            ExitCode::FAILURE
        }
    }
}

fn record(
    protocol: &str,
    base_url: &str,
    model: &str,
    key: &str,
    request: &str,
    out: &str,
    headers: Vec<(String, String)>,
) -> Result<u64, String> {
    let key = match key.strip_prefix("env:") {
        Some(name) => std::env::var(name).map_err(|e| format!("{name}: {e}"))?,
        None => std::fs::read_to_string(key).map_err(|e| format!("{key}: {e}"))?,
    };
    let request: ModelRequest =
        serde_json::from_slice(&std::fs::read(request).map_err(|e| format!("{request}: {e}"))?)
            .map_err(|e| format!("{request}: {e}"))?;
    let endpoint = Endpoint {
        provider: "record".into(),
        model: model.into(),
        base_url: base_url.into(),
        key: Some(key.trim().to_owned()),
        headers,
        ..Endpoint::default()
    };
    let opened = match protocol {
        "openai-responses" => Responses::new(endpoint)
            .request(&request)
            .open()
            .map(|stream| Box::new(stream) as Box<dyn Read>),
        "anthropic-messages" => Messages::new(endpoint)
            .request(&request)
            .open()
            .map(|stream| Box::new(stream) as Box<dyn Read>),
        other => {
            return Err(format!(
                "{other} is not a protocol; use openai-responses or anthropic-messages"
            ));
        }
    };
    let mut stream = opened.map_err(|e| {
        let failure = e.failure("record");
        serde_json::to_string(&failure).unwrap_or_else(|_| e.to_string())
    })?;
    let mut file = File::create(out).map_err(|e| format!("{out}: {e}"))?;
    std::io::copy(&mut stream, &mut file).map_err(|e| format!("{out}: {e}"))
}
