//! Server-sent events, written here rather than taken from a crate
//! (`docs/dependencies.md`, "Written ourselves"). Only `data` matters to
//! Fiber's protocols: each names its event inside the JSON.

use std::io::BufRead;

use crate::Error;

/// Reads `stream` and passes each event's data to `on_data`, its `data`
/// lines joined with `\n`, until `on_data` returns `true` or the stream
/// ends. Comment lines and other fields are skipped. An event the stream
/// ends inside is never dispatched.
pub(crate) fn read(
    mut stream: impl BufRead,
    mut on_data: impl FnMut(&str) -> Result<bool, Error>,
) -> Result<(), Error> {
    let mut line = String::new();
    let mut data = String::new();
    let mut has_data = false;
    loop {
        line.clear();
        let read = stream
            .read_line(&mut line)
            .map_err(|e| Error::Connection(e.to_string()))?;
        if read == 0 {
            return Ok(());
        }
        let field = line.strip_suffix('\n').unwrap_or(&line);
        let field = field.strip_suffix('\r').unwrap_or(field);
        if field.is_empty() {
            if has_data && on_data(&data)? {
                return Ok(());
            }
            data.clear();
            has_data = false;
        } else if let Some(value) = field.strip_prefix("data") {
            if !value.is_empty() && !value.starts_with(':') {
                // A field named `database` is not `data`.
                continue;
            }
            let value = value.strip_prefix(':').unwrap_or(value);
            if has_data {
                data.push('\n');
            }
            data.push_str(value.strip_prefix(' ').unwrap_or(value));
            has_data = true;
        }
    }
}

#[cfg(test)]
#[path = "sse_tests.rs"]
mod tests;
