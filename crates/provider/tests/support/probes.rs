//! Reads a probe recording from `research/*-probe/raw/` as the responses it
//! holds. Shared by the provider tests and the `decode` jig.
//!
//! The probes saved their exchanges in four shapes:
//!
//! - a `.sse` file: the response stream's bytes
//! - a wrapper with the stream's bytes in `raw_sse`, or in `body` beside a
//!   `status` (`opencode-probe`)
//! - a wrapper with the stream as parsed `events`, each an `event` name and
//!   its `data` (`codex-responses-probe`); its bytes are rebuilt from them
//! - a non-streamed response object in `response` (`openai-responses-probe`);
//!   its stream is rebuilt as one `response.output_item.done` per output
//!   item, then `response.completed`
//!
//! A file holds one wrapper or a list of them.

#![allow(dead_code, reason = "each user of this module needs only part of it")]

use std::path::Path;

use serde_json::{Value, json};

/// What a recorded exchange answered.
pub(crate) enum Recorded {
    /// A 200 stream's bytes.
    Stream(Vec<u8>),
    /// Another status, with its body.
    Status(u16, Vec<u8>),
}

/// One recorded exchange.
pub(crate) struct Exchange {
    /// Where it came from: the file, and its label within the file.
    pub(crate) label: String,
    /// What the provider answered.
    pub(crate) response: Recorded,
    /// The wrapper as saved, for the facts it records beside the stream.
    pub(crate) wrapper: Value,
}

/// The exchanges `path` holds.
pub(crate) fn read(path: &Path) -> Result<Vec<Exchange>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path.display().to_string();
    if path.extension().is_some_and(|e| e == "sse") {
        return Ok(vec![Exchange {
            label: name,
            response: Recorded::Stream(bytes),
            wrapper: Value::Null,
        }]);
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("{name}: not JSON ({e})"))?;
    let wrappers = if let Value::Array(list) = value {
        list
    } else {
        vec![value]
    };
    wrappers
        .into_iter()
        .enumerate()
        .map(|(n, wrapper)| {
            let label = match wrapper.get("label").or_else(|| wrapper.get("name")) {
                Some(Value::String(l)) => format!("{name} [{n}] {l}"),
                _ => format!("{name} [{n}]"),
            };
            let response = response(&wrapper).ok_or_else(|| format!("{label}: no stream"))?;
            Ok(Exchange {
                label,
                response,
                wrapper,
            })
        })
        .collect()
}

fn response(wrapper: &Value) -> Option<Recorded> {
    let status = wrapper
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|s| u16::try_from(s).ok())
        .unwrap_or(200);
    if let Some(Value::String(raw)) = wrapper.get("raw_sse") {
        return Some(Recorded::Stream(raw.clone().into_bytes()));
    }
    if let Some(Value::String(body)) = wrapper.get("body") {
        let body = body.clone().into_bytes();
        return Some(if status == 200 {
            Recorded::Stream(body)
        } else {
            Recorded::Status(status, body)
        });
    }
    if let Some(Value::Array(events)) = wrapper.get("events") {
        let mut stream = String::new();
        for event in events {
            let name = event.get("event")?.as_str()?;
            stream.push_str(&format!("event: {name}\ndata: {}\n\n", event.get("data")?));
        }
        return Some(Recorded::Stream(stream.into_bytes()));
    }
    let response = wrapper.get("response")?;
    if let Some(status) = response.get("http_error").and_then(Value::as_u64) {
        let body = response.get("body").cloned().unwrap_or(Value::Null);
        return Some(Recorded::Status(
            u16::try_from(status).ok()?,
            body.to_string().into_bytes(),
        ));
    }
    let mut stream = String::new();
    for item in response.get("output")?.as_array()? {
        let done = json!({"type": "response.output_item.done", "item": item});
        stream.push_str(&format!("data: {done}\n\n"));
    }
    let completed = json!({"type": "response.completed", "response": response});
    stream.push_str(&format!("data: {completed}\n\n"));
    Some(Recorded::Stream(stream.into_bytes()))
}
