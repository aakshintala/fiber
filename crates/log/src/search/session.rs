//! One session's pass (`docs/tools.md`, "Searching past sessions"): its text
//! artifacts first, then one raw pass over its log. The raw pass selects
//! every line whose decoded text could match, the lines that name the
//! session, and the lines that name a matched artifact; decoding the line
//! decides every hit.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::{Path, PathBuf};

use contract::session_search::{Hit, Label};
use contract::tool::Cancel;
use contract::{Envelope, SessionId};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use serde_json::Value;

use super::{Collect, kind_of, unreadable};
use super::fields;
use super::read::Cancelling;
use super::text::{Text, escaped, snippet};
use crate::{ARTIFACTS, EVENTS};

/// File extensions never searched as text, compared ignoring case.
const NOT_TEXT: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "pdf"];

/// One session to search.
pub(super) struct Session<'a> {
    /// Its id.
    pub(super) id: &'a SessionId,
    /// Its directory.
    pub(super) dir: &'a Path,
    /// Its `events.jsonl`, already open.
    pub(super) log: &'a File,
}

/// A text artifact the query matched.
struct Matched {
    /// The file's name in `artifacts/`.
    name: String,
    /// The file.
    path: PathBuf,
    /// The snippet around its first match.
    snippet: String,
    /// Whether a line already carries its hit.
    claimed: bool,
}

/// Searches `session`, adding its hits and problems to `out`. A cancelled
/// pass stops where it is.
pub(super) fn search(text: &Text, session: &Session<'_>, cancel: &dyn Cancel, out: &mut Collect) {
    let Some(mut matched) = artifacts(text, session.dir, cancel, out) else {
        return;
    };
    let path = session.dir.join(EVENTS);
    let mut extra: Vec<String> = matched
        .iter()
        .map(|m| escaped(&format!("{ARTIFACTS}/{}", m.name)))
        .collect();
    // Every request for the tool itself is selected, so the pass learns
    // each `session_search` call's id before its later lines even when
    // its arguments do not hold the query.
    extra.push(fields::SELF_TOOL.to_owned());
    let raw = match text.raw(&extra) {
        Ok(raw) => raw,
        Err(error) => return out.problem(unreadable(&path, &error)),
    };
    let mut log = session.log;
    if let Err(error) = log.rewind() {
        return out.problem(unreadable(&path, &error));
    }
    let mut lines = Lines {
        text,
        id: session.id,
        path: &path,
        matched: &mut matched,
        cancel,
        out,
        excluded: HashSet::new(),
        named: None,
        first_prompt: None,
    };
    let searched = SearcherBuilder::new()
        .line_number(true)
        .build()
        .search_reader(&raw, Cancelling::new(log, cancel), &mut lines);
    let name = lines
        .named
        .take()
        .or_else(|| lines.first_prompt.take())
        .unwrap_or_default();
    if let Err(error) = searched
        && !cancel.is_cancelled()
    {
        out.problem(unreadable(&path, &error));
    }
    out.name(session.id, &name);
}

/// The session's text artifacts that match, in name order: `None` when the
/// call was cancelled. A link, or anything that cannot be read, is a
/// problem.
fn artifacts(
    text: &Text,
    dir: &Path,
    cancel: &dyn Cancel,
    out: &mut Collect,
) -> Option<Vec<Matched>> {
    let path = dir.join(ARTIFACTS);
    let mut matched = Vec::new();
    let Some(kind) = kind_of(&path, &mut |problem| out.problem(problem)) else {
        return Some(matched);
    };
    if !kind.is_dir() {
        return Some(matched);
    }
    let entries = match std::fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(error) => {
            out.problem(unreadable(&path, &error));
            return Some(matched);
        }
    };
    // A name that is not UTF-8 cannot be named by a line, so it never
    // gives a hit. An entry that cannot be read is a problem.
    let mut names: Vec<String> = entries
        .filter_map(|next| super::entry(&path, next, &mut |problem| out.problem(problem)))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    for name in names {
        if cancel.is_cancelled() {
            return None;
        }
        let file = path.join(&name);
        let Some(kind) = kind_of(&file, &mut |problem| out.problem(problem)) else {
            continue;
        };
        if !kind.is_file() {
            continue;
        }
        if not_text(&name) {
            continue;
        }
        let found = File::open(&file).and_then(|open| first_match(text, open, cancel));
        match found {
            Ok(Some(snippet)) => matched.push(Matched {
                name,
                path: file,
                snippet,
                claimed: false,
            }),
            Ok(None) => {}
            Err(_) if cancel.is_cancelled() => return None,
            Err(error) => out.problem(unreadable(&file, &error)),
        }
    }
    Some(matched)
}

/// Whether `name`'s extension marks a file that is never text.
fn not_text(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| NOT_TEXT.iter().any(|skip| ext.eq_ignore_ascii_case(skip)))
}

/// The snippet around `file`'s first match: `None` when nothing matches or
/// the file holds a NUL byte. A hit counts only once the search read to the
/// file's end: the line searcher quits at a NUL before reading on, so a
/// search that reached the end found none, and a search cancelled, stopped
/// or quit before the end admits nothing.
fn first_match(text: &Text, file: File, cancel: &dyn Cancel) -> io::Result<Option<String>> {
    let mut reader = Cancelling::new(file, cancel);
    let mut first = First {
        text,
        cancel,
        snippet: None,
    };
    if text.multi_line() {
        // A search across lines holds the file in memory; the NUL check
        // covers all of it.
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        if bytes.contains(&0) {
            return Ok(None);
        }
        SearcherBuilder::new()
            .multi_line(true)
            .build()
            .search_slice(text.artifact(), &bytes, &mut first)?;
    } else {
        searcher_without_binary().search_reader(text.artifact(), &mut reader, &mut first)?;
    }
    Ok(first.snippet.filter(|_| reader.eof()))
}

/// A line searcher that quits at the first NUL byte, as the shell's
/// `grep -I`.
fn searcher_without_binary() -> Searcher {
    SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(0))
        .build()
}

/// The sink that keeps an artifact's first match and reads on to the end.
struct First<'a> {
    /// The query.
    text: &'a Text,
    /// The call's signal.
    cancel: &'a dyn Cancel,
    /// The first match's snippet.
    snippet: Option<String>,
}

impl Sink for First<'_> {
    type Error = io::Error;

    fn matched(&mut self, _: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, io::Error> {
        if self.cancel.is_cancelled() {
            return Ok(false);
        }
        if self.snippet.is_none() {
            let found = String::from_utf8_lossy(mat.bytes());
            let found = found.strip_suffix('\n').unwrap_or(&found);
            let start = self.text.find(found).map_or(0, |(start, _)| start);
            self.snippet = Some(snippet(found, start));
        }
        Ok(true)
    }
}

/// The sink of the raw pass over one log.
struct Lines<'a, 'b> {
    /// The query.
    text: &'a Text,
    /// The session.
    id: &'a SessionId,
    /// Its log.
    path: &'a Path,
    /// Its matched artifacts.
    matched: &'a mut [Matched],
    /// The call's signal.
    cancel: &'a dyn Cancel,
    /// Where hits and problems go.
    out: &'b mut Collect,
    /// The call ids of this log's `session_search` calls, seen on their
    /// request lines. No line of such a call gives a hit.
    excluded: HashSet<String>,
    /// The latest `session_named`'s name; `None` when it was cleared.
    named: Option<String>,
    /// The first turn's first message, once the first turn was read.
    first_prompt: Option<String>,
}

impl Sink for Lines<'_, '_> {
    type Error = io::Error;

    fn matched(&mut self, _: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, io::Error> {
        if self.cancel.is_cancelled() {
            return Ok(false);
        }
        let bytes = mat.bytes();
        // A last line with no newline is a running session's torn tail.
        if !bytes.ends_with(b"\n") {
            return Ok(true);
        }
        match serde_json::from_slice::<Envelope>(bytes) {
            Ok(line) => self.line(&line),
            Err(error) => self.out.problem(format!(
                "Could not read: {}, line {}: {error}",
                self.path.display(),
                mat.line_number().unwrap_or_default()
            )),
        }
        Ok(true)
    }
}

impl Lines<'_, '_> {
    /// One selected complete line.
    fn line(&mut self, line: &Envelope) {
        let Some(seq) = line.seq else {
            return;
        };
        let kind = line.kind.as_str();
        let payload = &line.payload;
        match kind {
            "session_named" => {
                self.named = payload
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            "turn_started" if self.first_prompt.is_none() => {
                self.first_prompt = Some(first_prompt(payload));
            }
            _ => {}
        }
        // A search never returns `session_search`'s own calls or their
        // results (`docs/tools.md`, "Searching past sessions"): a request
        // for the tool itself marks its call, and no line of a marked call
        // gives a hit or names an artifact. A completed line never precedes
        // its request, so one pass in file order marks every call in time.
        if fields::self_request(kind, payload) {
            if let Some(id) = &line.action_id {
                self.excluded.insert(id.0.clone());
            }
            return;
        }
        if line
            .action_id
            .as_ref()
            .is_some_and(|id| self.excluded.contains(&id.0))
        {
            return;
        }
        // One hit per label: the first of its strings that holds the query.
        let mut snippets = Snippets::default();
        for (label, text) in fields::labelled(kind, payload) {
            let slot = snippets.slot(label);
            if slot.is_none()
                && let Some((start, _)) = self.text.find(text)
            {
                *slot = Some(snippet(text, start));
            }
        }
        let mut artifact = None;
        if let Some(named) = fields::names(kind, payload).and_then(|n| n.strip_prefix("artifacts/"))
            && let Some(found) = self
                .matched
                .iter_mut()
                .find(|m| !m.claimed && m.name == named)
        {
            found.claimed = true;
            // The artifact's hit replaces the line's own cut output.
            snippets.tool_output = Some(found.snippet.clone());
            artifact = Some(found.path.clone());
        }
        let found = [
            (Label::Message, snippets.message, None),
            (Label::ToolInput, snippets.tool_input, None),
            (Label::ToolOutput, snippets.tool_output, artifact),
        ];
        for (label, snippet, artifact) in found {
            let Some(snippet) = snippet else {
                continue;
            };
            self.out.hit(Hit {
                session_id: self.id.clone(),
                name: String::new(),
                seq,
                ts: line.ts,
                label,
                snippet,
                log: self.path.to_owned(),
                artifact,
            });
        }
    }
}

/// One line's snippet for each label.
#[derive(Default)]
struct Snippets {
    /// The `message` hit's.
    message: Option<String>,
    /// The `tool_input` hit's.
    tool_input: Option<String>,
    /// The `tool_output` hit's.
    tool_output: Option<String>,
}

impl Snippets {
    /// The snippet for `label`.
    fn slot(&mut self, label: Label) -> &mut Option<String> {
        match label {
            Label::Message => &mut self.message,
            Label::ToolInput => &mut self.tool_input,
            Label::ToolOutput => &mut self.tool_output,
        }
    }
}

/// The joined text parts of a `turn_started`'s first message; `""` when it
/// had none.
fn first_prompt(payload: &serde_json::Map<String, Value>) -> String {
    let items = payload.get("input").and_then(Value::as_array);
    let message = items
        .into_iter()
        .flatten()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("message"));
    message
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect()
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
