//! The `record` jig (`docs/testing.md`, "Jigs"): sends one live request and
//! saves the reply as a recorded stream: the response body's bytes only,
//! never a header or the key.
//!
//! `cargo run -p provider --example record -- PROTOCOL BASE_URL MODEL KEY REQUEST OUT [NAME:VALUE | @FILE]...`
//!
//! PROTOCOL is `openai-responses`, `anthropic-messages` or
//! `google-generative-ai`. KEY is a file holding the key, or `env:NAME` to
//! read it from that environment variable, so a live recording never writes
//! the key to disk. The key is sent as the protocol sends it. REQUEST is a
//! JSON `ModelRequest` (`contract::provider`). OUT receives the stream,
//! ready for the `decode` jig and the tests. Each NAME:VALUE is a header to
//! send, such as the `x-opencode-session` OpenCode Go requires. At most one
//! @FILE names a JSON object sent as the endpoint's `extra_body`, so a
//! recording sends what the model's provider data declares.

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
use provider::google_generative_ai::Gemini;
use provider::openai_responses::Responses;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [protocol, base_url, model, key, request, out, rest @ ..] = args.as_slice() else {
        eprintln!(
            "usage: cargo run -p provider --example record -- \
             PROTOCOL BASE_URL MODEL KEY REQUEST OUT [NAME:VALUE | @FILE]..."
        );
        return ExitCode::from(2);
    };
    let extra = match extras(rest) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("record: {message}");
            return ExitCode::from(2);
        }
    };
    match record(protocol, base_url, model, key, request, out, extra) {
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

/// The trailing arguments: headers to send, and at most one `@FILE` path
/// holding the `extra_body` object.
type Parsed = (Vec<(String, String)>, Option<String>);

/// The trailing arguments: headers, and at most one `@FILE` holding the
/// `extra_body` object. Anything else is rejected before any request is
/// sent.
fn extras(args: &[String]) -> Result<Parsed, String> {
    let mut headers = Vec::new();
    let mut body_file = None;
    for arg in args {
        if let Some(path) = arg.strip_prefix('@') {
            if path.is_empty() {
                return Err("record: @FILE names a file holding a JSON object".into());
            }
            if body_file.is_some() {
                return Err("record: one @FILE only".into());
            }
            body_file = Some(path.to_owned());
            continue;
        }
        let Some((name, value)) = arg.split_once(':') else {
            return Err("record: a header is NAME:VALUE".into());
        };
        headers.push((name.to_owned(), value.trim().to_owned()));
    }
    Ok((headers, body_file))
}

/// The `@FILE` JSON object, sent as the endpoint's `extra_body`.
fn extra_body(path: &str) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("{path}: {e}"))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| format!("record: {path} (@FILE) must hold a JSON object"))
}

fn record(
    protocol: &str,
    base_url: &str,
    model: &str,
    key: &str,
    request: &str,
    out: &str,
    extra: Parsed,
) -> Result<u64, String> {
    let key = match key.strip_prefix("env:") {
        Some(name) => std::env::var(name).map_err(|e| format!("{name}: {e}"))?,
        None => std::fs::read_to_string(key).map_err(|e| format!("{key}: {e}"))?,
    };
    let request: ModelRequest =
        serde_json::from_slice(&std::fs::read(request).map_err(|e| format!("{request}: {e}"))?)
            .map_err(|e| format!("{request}: {e}"))?;
    let extra_body = extra
        .1
        .as_deref()
        .map(extra_body)
        .transpose()?
        .unwrap_or_default();
    let endpoint = Endpoint {
        provider: "record".into(),
        model: model.into(),
        base_url: base_url.into(),
        key: Some(contract::Secret::new(key.trim().to_owned())),
        headers: extra.0,
        extra_body,
        ..Endpoint::default()
    };
    let secrets = endpoint.secrets();
    let opened = match protocol {
        "openai-responses" => Responses::new(endpoint)
            .request(&request)
            .open()
            .map(|stream| Box::new(stream) as Box<dyn Read>),
        "anthropic-messages" => Messages::new(endpoint)
            .request(&request)
            .open()
            .map(|stream| Box::new(stream) as Box<dyn Read>),
        "google-generative-ai" => Gemini::new(endpoint)
            .request(&request)
            .open()
            .map(|stream| Box::new(stream) as Box<dyn Read>),
        other => {
            return Err(format!(
                "{other} is not a protocol; use openai-responses, anthropic-messages or google-generative-ai"
            ));
        }
    };
    let mut stream = opened.map_err(|e| {
        let failure = e.failure("record", &secrets);
        serde_json::to_string(&failure).unwrap_or_else(|_| e.to_string())
    })?;
    let mut file = File::create(out).map_err(|e| format!("{out}: {e}"))?;
    std::io::copy(&mut stream, &mut file).map_err(|e| format!("{out}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::{extra_body, extras};

    #[test]
    fn extras_reads_headers_and_one_body_file() {
        let arg = |s: &str| s.to_owned();
        let parsed = extras(&[arg("x-a: 1")]).unwrap();
        assert_eq!(parsed.0, vec![("x-a".to_owned(), "1".to_owned())]);
        assert_eq!(parsed.1, None);
        let parsed = extras(&[arg("@f.json"), arg("x-a:1")]).unwrap();
        assert_eq!(parsed.0, vec![("x-a".to_owned(), "1".to_owned())]);
        assert_eq!(parsed.1, Some("f.json".to_owned()));
        assert_eq!(
            extras(&[arg("@a.json"), arg("@b.json")]).unwrap_err(),
            "record: one @FILE only"
        );
        assert_eq!(
            extras(&[arg("@")]).unwrap_err(),
            "record: @FILE names a file holding a JSON object"
        );
        assert_eq!(
            extras(&[arg("no-colon")]).unwrap_err(),
            "record: a header is NAME:VALUE"
        );
    }

    #[test]
    fn extra_body_must_be_a_json_object() {
        let dir = fakes::TempDir::new("fiber-record-extra-body");
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path.display().to_string()
        };
        let object = write("object.json", br#"{"store": false}"#);
        assert_eq!(
            extra_body(&object)
                .unwrap()
                .get("store")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        for name in ["array.json", "string.json", "null.json"] {
            let contents: &[u8] = match name {
                "array.json" => b"[1]",
                "string.json" => br#""x""#,
                _ => b"null",
            };
            let path = write(name, contents);
            assert_eq!(
                extra_body(&path).unwrap_err(),
                format!("record: {path} (@FILE) must hold a JSON object")
            );
        }
        let bad = write("bad.json", b"{oops");
        let error = extra_body(&bad).unwrap_err();
        assert!(error.starts_with(&format!("{bad}: ")), "{error}");
    }
}
