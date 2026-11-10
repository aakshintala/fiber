//! The erroring-reader input the framing jig and the framing unit tests
//! both drive: file bytes readable up to a cut point, then one stalled-read
//! error, then end of input, as a torn socket write followed by its FIN
//! looks to the wire line reader.

use std::io::{self, BufRead, Read};
use std::path::PathBuf;

/// Which wire-reader API the jig drives: the existing line reader, or the
/// partial-preserving one beside it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Old,
    New,
}

/// How the jig is invoked: the file holding the bytes, how many of them
/// arrive before the read stalls, the reader API, and the read cap when the
/// caller pins one.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Args {
    pub(crate) file: PathBuf,
    pub(crate) error_after: usize,
    pub(crate) mode: Mode,
    pub(crate) max: Option<usize>,
}

/// Reads `framing <file> --error-after <n> [--mode old|new] [--max <n>]`: the
/// file first and bare, then one value per flag. Anything else is a usage
/// error.
pub(crate) fn parse(args: impl IntoIterator<Item = String>) -> Result<Args, ()> {
    let mut args = args.into_iter();
    let file = args
        .next()
        .filter(|file| !file.starts_with('-'))
        .ok_or(())?;
    let mut error_after = None;
    let mut mode = None;
    let mut max = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--error-after" | "--max" => {
                let place = if flag == "--error-after" {
                    &mut error_after
                } else {
                    &mut max
                };
                if place.is_some() {
                    return Err(());
                }
                let value = args
                    .next()
                    .filter(|value| !value.starts_with('-'))
                    .ok_or(())?;
                *place = Some(value.parse::<usize>().map_err(|_| ())?);
            }
            "--mode" => {
                if mode.is_some() {
                    return Err(());
                }
                let value = args
                    .next()
                    .filter(|value| !value.starts_with('-'))
                    .ok_or(())?;
                mode = Some(match value.as_str() {
                    "old" => Mode::Old,
                    "new" => Mode::New,
                    _ => return Err(()),
                });
            }
            _ => return Err(()),
        }
    }
    Ok(Args {
        file: PathBuf::from(file),
        error_after: error_after.ok_or(())?,
        mode: mode.unwrap_or(Mode::Old),
        max,
    })
}

/// File bytes that stall once `error_after` of them are consumed: the first
/// fill past the cut fails the way a timed-out socket read does, and the cut
/// bytes never arrive, so every fill after the error reports end of input,
/// as the stalled connection's FIN does.
pub(crate) struct FragmentThenError {
    data: Vec<u8>,
    error_after: usize,
    pos: usize,
    errored: bool,
}

impl FragmentThenError {
    /// A reader over `data` stalling once `error_after` bytes are consumed.
    pub(crate) fn new(data: Vec<u8>, error_after: usize) -> Self {
        Self {
            data,
            error_after,
            pos: 0,
            errored: false,
        }
    }
}

impl BufRead for FragmentThenError {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.pos >= self.data.len() {
            return Ok(&[]);
        }
        if !self.errored && self.pos >= self.error_after {
            self.errored = true;
            self.data.truncate(self.pos);
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the stalled read timed out",
            ));
        }
        let end = if self.errored {
            self.data.len()
        } else {
            self.error_after.min(self.data.len())
        };
        Ok(match self.data.get(self.pos..end) {
            Some(window) => window,
            None => &[],
        })
    }

    fn consume(&mut self, amount: usize) {
        let end = if self.errored {
            self.data.len()
        } else {
            self.error_after.min(self.data.len())
        };
        self.pos = end.min(self.pos.saturating_add(amount));
    }
}

impl Read for FragmentThenError {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut taken = 0;
        for slot in buf.iter_mut() {
            let byte = match self.fill_buf()?.first() {
                Some(byte) => *byte,
                None => break,
            };
            *slot = byte;
            self.consume(1);
            taken += 1;
        }
        Ok(taken)
    }
}
