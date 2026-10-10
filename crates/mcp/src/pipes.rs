//! The stdio wire for MCP servers: request bytes written to a child's stdin
//! and response lines read from its stdout (`docs/mcp.md`, "Starting servers").
//! A full writer queue means the server stopped reading: it counts as gone.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{ChildStdin, ChildStdout};
use std::sync::{Weak, mpsc};
use std::sync::mpsc::SyncSender;

use crate::rpc::{Incoming, decode_line, encode_error, encode_result};
use crate::wait::Shared;

/// A stdout line past this long ends the reader, and the server counts as
/// gone: a flood cannot grow memory without bound.
const MAX_LINE: usize = 4 * 1024 * 1024;

/// Writes request lines to stdin. A write that never finishes blocks only
/// this thread: the waiting call still times out or cancels on the clock.
/// The thread ends when the sender is dropped, closing stdin.
pub(crate) fn write_stdin(stdin: Option<ChildStdin>, incoming: mpsc::Receiver<Vec<u8>>) {
    let Some(mut stdin) = stdin else {
        return;
    };
    for mut line in incoming {
        line.push(b'\n');
        if stdin.write_all(&line).is_err() {
            return;
        }
    }
}

/// Queues `line` for the writer thread without blocking. A full queue
/// means the server stopped reading while calls keep coming: it counts as
/// gone, waking every waiter. A disconnected queue means the writer left.
pub(crate) fn queue(writer: &SyncSender<Vec<u8>>, line: Vec<u8>, shared: &Shared) {
    match writer.try_send(line) {
        Ok(()) => {}
        Err(_) => shared.gone(),
    }
}

/// Reads response lines and routes them: responses to their id's slot,
/// `ping` answered `{}`, any other server method answered `-32601`. A line
/// that is not a JSON object is ignored. Past [`MAX_LINE`] bytes on one
/// line, or EOF, the server counts as gone.
pub(crate) fn read_stdout(stdout: ChildStdout, shared: &Shared, writer: &Weak<SyncSender<Vec<u8>>>) {
    let mut reader = BufReader::new(stdout);
    loop {
        match read_line(&mut reader) {
            ReadLine::Line(line) => match decode_line(&line) {
                Incoming::Response(response) => {
                    shared.deliver(response.id, response.outcome);
                }
                Incoming::ServerRequest(request) => {
                    let answer = match request.method.as_str() {
                        "ping" => encode_result(&request.id, &serde_json::json!({})),
                        _ => encode_error(&request.id, -32601, "Method not found"),
                    };
                    if let Some(writer) = writer.upgrade() {
                        queue(&writer, answer.into_bytes(), shared);
                    }
                }
                Incoming::Ignored => {}
            },
            ReadLine::Eof | ReadLine::TooLong => {
                shared.gone();
                return;
            }
        }
    }
}

enum ReadLine {
    Line(String),
    Eof,
    TooLong,
}

/// Reads one newline-delimited line, capped at [`MAX_LINE`] bytes: past the
/// cap the line is abandoned and the reader ends.
fn read_line(reader: &mut impl BufRead) -> ReadLine {
    let mut buf = Vec::new();
    match reader
        .by_ref()
        .take(MAX_LINE as u64 + 1)
        .read_until(b'\n', &mut buf)
    {
        Ok(0) => ReadLine::Eof,
        Ok(_) if buf.ends_with(b"\n") => {
            buf.pop();
            ReadLine::Line(String::from_utf8_lossy(&buf).into_owned())
        }
        Ok(_) if buf.len() > MAX_LINE => ReadLine::TooLong,
        Ok(_) => ReadLine::Line(String::from_utf8_lossy(&buf).into_owned()),
        Err(_) if buf.is_empty() => ReadLine::Eof,
        Err(_) => ReadLine::Line(String::from_utf8_lossy(&buf).into_owned()),
    }
}

#[cfg(test)]
#[path = "pipes_tests.rs"]
mod tests;
