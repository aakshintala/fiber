//! The bounded, per-call grep match side file.

use std::borrow::Cow;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

pub(crate) const MATCHES_ENV: &str = "FIBER_GREP_MATCHES";
pub(crate) const LIMIT: u64 = 1_048_576;

pub(crate) struct MatchFile {
    file: Option<File>,
    path: PathBuf,
    remaining: u64,
}

#[derive(Serialize)]
struct Record<'a> {
    path: Cow<'a, str>,
    line: u64,
    text: Cow<'a, str>,
    spans: Vec<[usize; 2]>,
    context: bool,
}

impl MatchFile {
    pub(crate) fn create(path: &Path, limit: u64) -> Option<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .ok()?;
        Some(Self {
            file: Some(file),
            path: path.to_owned(),
            remaining: limit,
        })
    }

    pub(crate) fn record(
        &mut self,
        path: &[u8],
        line: u64,
        raw: &[u8],
        spans: &[(usize, usize)],
        context: bool,
    ) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let spans = match std::str::from_utf8(raw) {
            Ok(text) => spans
                .iter()
                .filter(|(start, end)| start < end)
                .map(|(start, end)| {
                    [
                        text.floor_char_boundary(*start),
                        text.ceil_char_boundary(*end),
                    ]
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        let record = Record {
            path: String::from_utf8_lossy(path),
            line,
            text: String::from_utf8_lossy(raw),
            spans,
            context,
        };
        // These strings and integers have no fallible JSON representation.
        let mut bytes = serde_json::to_vec(&record).unwrap_or_default();
        bytes.push(b'\n');
        let len = bytes.len() as u64;
        if len > self.remaining {
            self.file = None;
            return;
        }
        if file.write_all(&bytes).is_err() {
            self.file = None;
            std::fs::remove_file(&self.path).unwrap_or(());
            return;
        }
        self.remaining -= len;
    }

    pub(crate) fn finish(mut self) {
        if let Some(file) = self.file.as_mut()
            && file.flush().is_err()
        {
            self.file = None;
            std::fs::remove_file(&self.path).unwrap_or(());
        }
    }
}

#[cfg(test)]
#[path = "matches_tests.rs"]
mod tests;
