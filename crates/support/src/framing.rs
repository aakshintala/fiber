//! One wire line: bytes to the next line feed (`docs/architecture.md`,
//! "The modules").

use std::io::{self, BufRead, Write};

/// The most bytes one wire line may hold before its line feed (64 MiB).
pub const MAX_LINE: usize = 64 << 20;

/// The next line, without its trailing CR and LF bytes. `Ok(None)` at end of
/// input with nothing read. A line longer than `max` is `InvalidData`.
pub fn read_line<R: BufRead>(reader: &mut R, max: usize) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let (chunk, found, eof) = {
            let buf = match reader.fill_buf() {
                Ok(buf) => buf,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            if buf.is_empty() {
                (Vec::new(), false, true)
            } else if let Some(end) = buf.iter().position(|byte| *byte == b'\n') {
                let chunk = buf.get(..end).map_or_else(Vec::new, <[u8]>::to_vec);
                (chunk, true, false)
            } else {
                (buf.to_vec(), false, false)
            }
        };
        if eof {
            if line.is_empty() {
                return Ok(None);
            }
            while line
                .last()
                .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
            {
                line.pop();
            }
            return Ok(Some(line));
        }
        if line.len() + chunk.len() > max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a line is longer than its cap",
            ));
        }
        let take = chunk.len();
        line.extend_from_slice(&chunk);
        reader.consume(take + if found { 1 } else { 0 });
        if found {
            while line
                .last()
                .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
            {
                line.pop();
            }
            return Ok(Some(line));
        }
    }
}

/// Writes `line`, one `\n`, then flushes. One `write_all` call.
pub fn write_line<W: Write + ?Sized>(writer: &mut W, line: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(line.len() + 1);
    buf.extend_from_slice(line);
    buf.push(b'\n');
    writer.write_all(&buf)?;
    writer.flush()
}

#[cfg(test)]
#[path = "framing_tests.rs"]
mod tests;
