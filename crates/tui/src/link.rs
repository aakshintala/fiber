//! Hub lines out and in: command lines written one per line, and the
//! reader that splits hub lines from session envelopes.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;

use contract::{Envelope, HubLine};

use crate::Input;

/// One line read from the hub.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Line {
    /// A line for the connection: `hub_hello`, or the answer to a hub
    /// command.
    Hub(HubLine),
    /// A session's line, relayed by the hub.
    Session(Envelope),
}

/// Splits one line: a session's lines carry `session_id`, the hub's own do
/// not. A line that is neither is `None`, and the reader drops it.
pub(crate) fn parse_line(text: &str) -> Option<Line> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    if value.get("session_id").is_some() {
        serde_json::from_value(value).ok().map(Line::Session)
    } else {
        serde_json::from_value(value).ok().map(Line::Hub)
    }
}

/// Whether `line` answers command `id`: the accepted or rejected answer
/// carrying its id.
pub(crate) fn answers(line: &Envelope, id: &str) -> bool {
    matches!(line.kind.as_str(), "command_accepted" | "command_rejected")
        && line
            .payload
            .get("command_id")
            .and_then(serde_json::Value::as_str)
            == Some(id)
}

/// Reads a `history` answer: its durable lines on `command_accepted`,
/// or why there are none on `command_rejected` or an unreadable answer.
/// The loop and the search both read answers through it, so the two
/// cannot read one differently (`docs/tui.md`, "History and paging").
pub(crate) fn history_answer(line: &Envelope) -> Result<Vec<Envelope>, String> {
    if line.kind != "command_accepted" {
        return Err(line
            .payload
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("rejected")
            .to_owned());
    }
    line.payload
        .get("result")
        .and_then(|result| result.get("lines"))
        .cloned()
        .and_then(|lines| serde_json::from_value(lines).ok())
        .ok_or_else(|| "the answer could not be read".to_owned())
}

/// Writes one command line.
pub(crate) fn write_line(mut stream: &UnixStream, line: &str) -> std::io::Result<()> {
    stream.write_all(format!("{line}\n").as_bytes())?;
    stream.flush()
}

/// Reads the hub stream, sending each line to the loop, then
/// [`Input::Disconnected`] once the stream ends.
pub(crate) fn read_lines(stream: impl std::io::Read, tx: &Sender<Input>) {
    let mut read = BufReader::new(stream);
    let mut buf = String::new();
    loop {
        buf.clear();
        match read.read_line(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if let Some(line) = parse_line(buf.trim_end_matches(['\r', '\n']))
            && tx.send(Input::Hub(line)).is_err()
        {
            return;
        }
    }
    drop(tx.send(Input::Disconnected));
}

#[cfg(test)]
#[path = "link_tests.rs"]
mod tests;
