//! Per-call temporary grep metadata and its strict reader.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::io::Read;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(super) struct MatchDir {
    path: PathBuf,
}

impl MatchDir {
    pub(super) fn create() -> Option<Self> {
        let path = std::env::temp_dir().join(format!(
            "fiber-grep-{:016x}",
            RandomState::new().hash_one(())
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&path).ok()?;
        Some(Self { path })
    }

    pub(super) fn file(&self) -> PathBuf {
        self.path.join("matches.jsonl")
    }
}

impl Drop for MatchDir {
    fn drop(&mut self) {
        remove(&self.path);
    }
}

fn remove(dir: &Path) {
    let gone = dir.with_extension("gone");
    // Stop every later grep opening the old path before removing entries.
    let path = match std::fs::rename(dir, &gone) {
        Ok(()) => gone.as_path(),
        Err(_) => dir,
    };
    std::fs::remove_dir_all(path).unwrap_or(());
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    path: String,
    line: u64,
    text: String,
    spans: Vec<[usize; 2]>,
    #[serde(rename = "context")]
    _context: bool,
}

#[derive(Serialize)]
struct Group {
    path: String,
    lines: Vec<Line>,
}

#[derive(Serialize)]
struct Line {
    line: u64,
    text: String,
    spans: Vec<[usize; 2]>,
}

pub(super) fn read(path: &Path, limit: u64) -> Option<Value> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > limit {
        return None;
    }
    let mut text = String::new();
    file.take(limit).read_to_string(&mut text).ok()?;
    let mut groups: Vec<Group> = Vec::new();
    if text.is_empty() {
        return Some(Value::Array(Vec::new()));
    }
    for raw in text.strip_suffix('\n')?.split('\n') {
        let record: Record = serde_json::from_str(raw).ok()?;
        for [start, end] in &record.spans {
            if start > end
                || *end > record.text.len()
                || !record.text.is_char_boundary(*start)
                || !record.text.is_char_boundary(*end)
            {
                return None;
            }
        }
        let line = Line {
            line: record.line,
            text: record.text,
            spans: record.spans,
        };
        match groups.last_mut().filter(|group| group.path == record.path) {
            Some(group) => group.lines.push(line),
            None => groups.push(Group {
                path: record.path,
                lines: vec![line],
            }),
        }
    }
    serde_json::to_value(groups).ok()
}

#[cfg(test)]
#[path = "matches_tests.rs"]
mod tests;
