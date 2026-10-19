//! The hub's `prompt_history` command (`docs/invocation.md`, "What the hub
//! speaks"): a page of a project's `history.jsonl` (`docs/state.md`, "Prompt
//! history"), newest first, read from the tail without a lock.
//!
//! `before` is a byte offset into the file. A page holds the newest whole
//! lines that end at or before it; the answer's `before` is where the
//! oldest returned line starts. Appends only add bytes past any offset a
//! client holds, so paging stays stable while sessions append.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use contract::ErrorCode;
use serde_json::{Map, Value};

/// The most prompts one page holds: the session `history` command's cap.
const PAGE: usize = 256;

/// How many bytes one backward read takes: 64 KiB. A literal, since the
/// size changes no answer, only how many reads one takes.
pub(crate) const CHUNK: usize = 65_536;

const UNFIT: &str = "The arguments do not fit this command.";
const UNREADABLE: &str = "The prompt history could not be read.";

use crate::connection::Refused;

/// A page of prompts, newest first, and the offset of the oldest one's
/// start when older lines remain.
type Page = (Vec<Value>, Option<u64>);

/// Answers `prompt_history` with `args` against Fiber home `home`: the
/// `result` object, or why it is rejected.
pub(crate) fn answer(home: &Path, args: &Map<String, Value>) -> Result<Value, Refused> {
    let (project, before) = parse(args).ok_or(Refused {
        code: ErrorCode::InvalidArguments,
        message: UNFIT.to_owned(),
    })?;
    let file = home.join("projects").join(project).join("history.jsonl");
    let (prompts, before) = page(&file, before).map_err(|_| Refused {
        code: ErrorCode::IoFailed,
        message: UNREADABLE.to_owned(),
    })?;
    let mut result = Map::new();
    result.insert("prompts".to_owned(), Value::Array(prompts));
    if let Some(before) = before {
        result.insert("before".to_owned(), Value::from(before));
    }
    Ok(Value::Object(result))
}

/// `project` (a project key, required) and `before` (a non-negative
/// integer, optional). A wrong, missing or extra key, or an explicit
/// `null`, is `None`.
fn parse(args: &Map<String, Value>) -> Option<(&str, Option<u64>)> {
    if args.keys().any(|key| key != "project" && key != "before") {
        return None;
    }
    let project = args.get("project")?.as_str()?;
    if !valid_key(project) {
        return None;
    }
    let before = match args.get("before") {
        None => None,
        Some(before) => Some(before.as_u64()?),
    };
    Some((project, before))
}

/// Whether `key` is one path component under `projects/`, so a key never
/// reaches a file outside its project directory.
fn valid_key(key: &str) -> bool {
    !key.is_empty() && !key.contains('/') && !key.contains('\0') && key != "." && key != ".."
}

/// The page of `file` that ends at `before`, or at the file's end. A file
/// that does not exist is an empty page.
fn page(file: &Path, before: Option<u64>) -> io::Result<Page> {
    let mut opened = match File::open(file) {
        Ok(opened) => opened,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), None)),
        Err(error) => return Err(error),
    };
    let len = opened.metadata()?.len();
    let end = before.map_or(len, |before| before.min(len));
    read_back(&mut opened, end, CHUNK, PAGE, |line| {
        serde_json::from_slice(line)
            .ok()
            .filter(|value| matches!(value, Value::Object(_)))
    })
}

/// Reads `file` backward from `end`, `chunk` bytes at a time, collecting
/// the whole lines that end at or before `end`, newest first, until
/// `limit` values are held or the file's start is reached. The bytes
/// after the last newline before `end` are a fragment, a write in
/// progress or a mid-line `before`, and are skipped. A line `parse`
/// skips is skipped and does not count toward `limit`. An empty file
/// (`end == 0`) is empty; `limit` and `chunk` are caller constants, so a
/// zero one is a bug and debug-asserted.
pub(crate) fn read_back<T>(
    file: &mut (impl Read + Seek),
    end: u64,
    chunk: usize,
    limit: usize,
    parse: impl Fn(&[u8]) -> Option<T>,
) -> io::Result<(Vec<T>, Option<u64>)> {
    debug_assert!(limit > 0);
    debug_assert!(chunk > 0);
    if end == 0 {
        return Ok((Vec::new(), None));
    }
    let mut found = Vec::new();
    // `held` is the bytes from `start` up to the newline that ends the
    // next line to collect, or up to `end` while no newline is found.
    let mut held: Vec<u8> = Vec::new();
    let mut start = end;
    let mut ended = false;
    loop {
        while let Some(at) = held.iter().rposition(|byte| *byte == b'\n') {
            // `line` is the newline at `at` and the line after it.
            let line = held.split_off(at);
            if ended {
                if let Some(value) = parse(line.get(1..).unwrap_or_default()) {
                    found.push(value);
                }
                if found.len() == limit {
                    return Ok((found, Some(start + at as u64 + 1)));
                }
            }
            ended = true;
        }
        if start == 0 {
            if ended && let Some(value) = parse(&held) {
                found.push(value);
            }
            return Ok((found, None));
        }
        let take = start.min(chunk as u64);
        start -= take;
        let mut bytes = vec![0; usize::try_from(take).unwrap_or(chunk)];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut bytes)?;
        bytes.extend_from_slice(&held);
        held = bytes;
    }
}

#[cfg(test)]
#[path = "prompt_history_tests.rs"]
mod tests;
