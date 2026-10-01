//! The `record` jig (`docs/testing.md`, "Jigs"): sends one live
//! `openai-responses` request and saves the reply as a recorded stream: the
//! response body's bytes only, never a header or the key.
//!
//! `cargo run -p provider --example record -- BASE_URL MODEL KEY_FILE REQUEST OUT [NAME:VALUE...]`
//!
//! KEY_FILE holds the key, sent as a bearer token. REQUEST is a JSON
//! `ModelRequest` (`contract::provider`). OUT receives the stream, ready for
//! the `decode` jig and the tests. Each NAME:VALUE is a header to send, such
//! as the `x-opencode-session` OpenCode Go requires.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

use std::fs::File;
use std::process::ExitCode;

use contract::provider::ModelRequest;
use provider::Endpoint;
use provider::openai_responses::Responses;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [base_url, model, key_file, request, out, headers @ ..] = args.as_slice() else {
        eprintln!(
            "usage: cargo run -p provider --example record -- \
             BASE_URL MODEL KEY_FILE REQUEST OUT [NAME:VALUE...]"
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
    match record(base_url, model, key_file, request, out, headers) {
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
    base_url: &str,
    model: &str,
    key_file: &str,
    request: &str,
    out: &str,
    headers: Vec<(String, String)>,
) -> Result<u64, String> {
    let key = std::fs::read_to_string(key_file).map_err(|e| format!("{key_file}: {e}"))?;
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
    let mut stream = Responses::new(endpoint)
        .request(&request)
        .open()
        .map_err(|e| {
            let failure = e.failure("record");
            serde_json::to_string(&failure).unwrap_or_else(|_| e.to_string())
        })?;
    let mut file = File::create(out).map_err(|e| format!("{out}: {e}"))?;
    std::io::copy(&mut stream, &mut file).map_err(|e| format!("{out}: {e}"))
}
