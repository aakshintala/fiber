//! One server-sent event stream (`docs/dependencies.md`, "Written
//! ourselves"): the `text/event-stream` answers of a streamable HTTP MCP
//! server, read event by event. Each event's `data` is one JSON-RPC
//! message; the event name, `id` and `retry` are ignored, except that the
//! legacy probe reads the first event's name. A line or one event's data
//! past [`crate::pipes::MAX_LINE`] ends the read: a flood cannot grow
//! memory without bound, as on the stdio wire.

use std::io::{BufRead, Read};

/// One event: its name, and its `data` lines joined with `\n`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Event {
    /// The `event:` name, empty when the stream named none.
    pub(crate) name: String,
    /// The `data:` lines joined with `\n`.
    pub(crate) data: String,
}

/// What reading one event found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Next {
    /// One event's name and data.
    Event(Event),
    /// The stream ended, mid-event or between events: no more events come.
    /// An unreadable stream ends the same way: its connection failed while
    /// an answer was still awaited.
    End,
    /// One line, or one event's data, ran past
    /// [`crate::pipes::MAX_LINE`].
    TooLong,
}

/// Reads one event: `data:` lines joined with `\n`, the `event:` name
/// kept, comments and `id`/`retry` skipped, `\r\n` endings stripped. A
/// field is the text before the first `:` (the whole line when it holds
/// none); only one leading space follows it into the value, so a field
/// named `database` is not `data`. An event the stream ends inside is not
/// returned. At exactly [`crate::pipes::MAX_LINE`] bytes a line and an
/// event's data still pass; one byte past either ends [`Next::TooLong`].
pub(crate) fn next(reader: &mut impl BufRead) -> Next {
    let mut name = String::new();
    let mut data = String::new();
    let mut open = false;
    loop {
        match read_line(reader) {
            ReadLine::Line(line) => {
                if line.is_empty() {
                    if open {
                        return Next::Event(Event { name, data });
                    }
                    continue;
                }
                let (field, value) = match line.find(':') {
                    Some(at) => (
                        &line[..at],
                        line[at + 1..]
                            .strip_prefix(' ')
                            .unwrap_or(&line[at + 1..])
                            .to_owned(),
                    ),
                    None => (line.as_str(), String::new()),
                };
                match field {
                    "data" => {
                        open = true;
                        if !data.is_empty() {
                            data.push('\n');
                        }
                        data.push_str(&value);
                        if data.len() > crate::pipes::MAX_LINE {
                            return Next::TooLong;
                        }
                    }
                    "event" => {
                        open = true;
                        name = value;
                    }
                    _ => {}
                }
            }
            ReadLine::Eof => return Next::End,
            ReadLine::TooLong => return Next::TooLong,
        }
    }
}

enum ReadLine {
    Line(String),
    Eof,
    TooLong,
}

/// Reads one newline-delimited line, capped at
/// [`crate::pipes::MAX_LINE`] bytes: past the cap the line is abandoned
/// and the read ends, as on the stdio wire.
fn read_line(reader: &mut impl BufRead) -> ReadLine {
    let mut buf = Vec::new();
    match reader
        .by_ref()
        .take(crate::pipes::MAX_LINE as u64 + 1)
        .read_until(b'\n', &mut buf)
    {
        Ok(0) => ReadLine::Eof,
        Ok(_) if buf.ends_with(b"\n") => {
            buf.pop();
            if buf.ends_with(b"\r") {
                buf.pop();
            }
            ReadLine::Line(String::from_utf8_lossy(&buf).into_owned())
        }
        Ok(_) if buf.len() > crate::pipes::MAX_LINE => ReadLine::TooLong,
        Ok(_) => {
            if buf.ends_with(b"\r") {
                buf.pop();
            }
            ReadLine::Line(String::from_utf8_lossy(&buf).into_owned())
        }
        // An unreadable stream ends the read: no more events come.
        Err(_) => ReadLine::Eof,
    }
}

#[cfg(test)]
#[path = "sse_tests.rs"]
mod tests;
