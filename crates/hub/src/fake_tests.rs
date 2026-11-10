//! Tests for the fake session's `hold`: a torn write the fake already read
//! is recorded the way the live path records it, so a cut command's bytes
//! count however the session ended.

use super::*;
use std::collections::VecDeque;
use std::io::{self, Read};

/// One scripted fill: bytes, one stalled-read error, or nothing once the
/// script runs out.
enum Scripted {
    Bytes(Vec<u8>),
    Error,
}

/// A buffered reader replaying scripted fills: a subscribe line, a torn
/// fragment, and the stalled read, without a socket.
struct Script {
    fills: VecDeque<Scripted>,
    window: Vec<u8>,
    pos: usize,
}

impl Script {
    fn new(fills: Vec<Scripted>) -> Self {
        Self {
            fills: fills.into(),
            window: Vec::new(),
            pos: 0,
        }
    }
}

impl BufRead for Script {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.pos < self.window.len() {
            return Ok(&self.window[self.pos..]);
        }
        match self.fills.pop_front() {
            Some(Scripted::Bytes(bytes)) => {
                self.window = bytes;
                self.pos = 0;
                Ok(&self.window)
            }
            Some(Scripted::Error) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the read stalled",
            )),
            None => Ok(&[]),
        }
    }

    fn consume(&mut self, amount: usize) {
        self.pos = (self.pos + amount).min(self.window.len());
    }
}

impl Read for Script {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let window = self.fill_buf()?;
        let take = window.len().min(buf.len());
        buf[..take].copy_from_slice(&window[..take]);
        self.consume(take);
        Ok(take)
    }
}

/// `hold` keeps the EOF fragment: a torn write the fake already read
/// is recorded the way the live path records it, not discarded, so a
/// cut command's bytes count however the session ended.
#[test]
fn hold_records_the_fragment_it_reads_before_eof() {
    let received = Received::default();
    let (mut writer, reader) = UnixStream::pair().expect("a fake connection pair");
    let mut read = BufReader::new(reader);
    writer
        .write_all(b"{\"id\":\"c_2\",\"command\":\"prompt\"}")
        .expect("the torn bytes arrive");
    writer.shutdown(Shutdown::Write).expect("the session ends");
    hold(Some(&received), 0, &mut read);
    assert_eq!(
        lock(&received.lines).clone(),
        vec!["{\"id\":\"c_2\",\"command\":\"prompt\"}\n".to_owned()],
        "the torn fragment is recorded with its line feed, as live reads are"
    );
}

/// `serve_step` leaves a complete line unrecorded with its command, records
/// the stalled read's fragment the live way, then ends at end of input: the
/// torn bytes count before the divert, not after it.
#[test]
fn serve_step_records_the_error_partial_before_the_divert() {
    let received = Received::default();
    let mut read = Script::new(vec![
        Scripted::Bytes(
            b"{\"id\":\"c_1\",\"command\":\"subscribe\",\"args\":{\"level\":\"summary\"}}\n"
                .to_vec(),
        ),
        Scripted::Bytes(b"{\"id\":\"c_2\",\"command\":\"prompt\"}".to_vec()),
        Scripted::Error,
    ]);
    let Action::Line { text, command } = serve_step(&mut read, &received, 0) else {
        panic!("the subscribe line reads as a line");
    };
    assert_eq!(
        text,
        "{\"id\":\"c_1\",\"command\":\"subscribe\",\"args\":{\"level\":\"summary\"}}\n".to_owned()
    );
    assert_eq!(command, Some("subscribe".to_owned()));
    assert!(
        lock(&received.lines).clone().is_empty(),
        "a complete line stays unrecorded until the answer points"
    );
    let Action::Error = serve_step(&mut read, &received, 0) else {
        panic!("the stalled fragment reads as an error");
    };
    assert_eq!(
        lock(&received.lines).clone(),
        vec!["{\"id\":\"c_2\",\"command\":\"prompt\"}\n".to_owned()],
        "the error's partial is recorded with its line feed, as live reads are"
    );
    let Action::EndOfInput = serve_step(&mut read, &received, 0) else {
        panic!("the drained reader ends the input");
    };
    assert_eq!(
        lock(&received.lines).clone().len(),
        1,
        "end of input records nothing more"
    );
}

/// `serve_step` records nothing for an error with an empty partial: there
/// are no consumed bytes to keep.
#[test]
fn serve_step_keeps_nothing_when_the_error_kept_nothing() {
    let received = Received::default();
    let mut read = Script::new(vec![Scripted::Error]);
    let Action::Error = serve_step(&mut read, &received, 0) else {
        panic!("an immediate error reads as an error");
    };
    assert!(
        lock(&received.lines).clone().is_empty(),
        "an empty partial records nothing"
    );
    let Action::EndOfInput = serve_step(&mut read, &received, 0) else {
        panic!("the drained reader ends the input");
    };
}
