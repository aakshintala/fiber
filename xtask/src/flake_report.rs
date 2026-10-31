//! The flake-report decision (`docs/testing/running.md`, "Flaky tests"):
//! which closed flake issue a failure reopens, or that it files nothing.
//! It reads no API and runs no Git itself: the workflow passes each lookup
//! with an explicit completeness status, and a failed command, invalid
//! shape or incomplete pagination arrives as failed/incomplete, never as
//! an empty list. `main.rs` is its only caller.
//!
//! Selection runs in two phases so the workflow fetches only what the
//! decision needs: `event_requests` returns the unique closed issue
//! numbers whose event lists the workflow must fetch, and `decide_with`
//! turns the complete data plus one ancestry answer per selected fix
//! into one JSON-lines action per unique title. The CLI probes ancestry
//! itself with `git merge-base --is-ancestor` for only the latest
//! closed event's non-null commit; tests pass a fake probe.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value};

/// The closed set of writer actions, serialized as the JSON tags the
/// writer matches. A new action is a new variant here, never a string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Action {
    Reopen,
    FileNew,
    Stale,
    Unknown,
}

impl Action {
    fn tag(self) -> &'static str {
        match self {
            Action::Reopen => "reopen",
            Action::FileNew => "file-new",
            Action::Stale => "stale",
            Action::Unknown => "unknown",
        }
    }
}

/// A GitHub issue number. Never an event id.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct IssueNumber(pub u64);

/// A GitHub event id. Never an issue number.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct EventId(pub u64);

/// Which lookup failed, carried on unknown decisions so the summary
/// names it instead of collapsing to a generic failure.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Source {
    Open,
    Closed,
    Events,
    Ancestry,
}

impl Source {
    fn tag(self) -> &'static str {
        match self {
            Source::Open => "open",
            Source::Closed => "closed",
            Source::Events => "events",
            Source::Ancestry => "ancestry",
        }
    }
}

/// One JSON-lines decision: the unique flake title, the action the
/// writer takes, the target issue number where one applies, and the
/// failed lookup source on unknown decisions.
pub(crate) struct Decision {
    title: String,
    action: Action,
    issue: Option<IssueNumber>,
    source: Option<Source>,
}

impl Decision {
    pub(crate) fn line(&self) -> String {
        let mut map = Map::new();
        map.insert(
            "action".to_owned(),
            Value::String(self.action.tag().to_owned()),
        );
        if let Some(issue) = self.issue {
            map.insert("issue".to_owned(), Value::Number(issue.0.into()));
        }
        if let Some(source) = self.source {
            map.insert("source".to_owned(), Value::String(source.tag().to_owned()));
        }
        map.insert("title".to_owned(), Value::String(self.title.clone()));
        Value::Object(map).to_string()
    }
}

/// The unique closed issue numbers whose event lists the workflow must
/// fetch: one per unique flake title with no complete open exact-title
/// match and a closed exact-title match. Titles decided without events
/// (open match, failed lookups, no closed issue) request nothing.
pub(crate) fn event_requests(input: &Value) -> Result<Vec<IssueNumber>, String> {
    let root = object(input, "input")?;
    let flakes = titles(root)?;
    let open = lookup(field(root, "input", "open")?, "open")?;
    let closed = lookup(field(root, "input", "closed")?, "closed")?;
    let mut out = Vec::new();
    for title in &flakes {
        if !open.ok {
            continue;
        }
        if open
            .issues
            .iter()
            .any(|issue| !issue.is_pr && issue.title == *title)
        {
            continue;
        }
        if !closed.ok {
            continue;
        }
        if let Some(selected) = select_closed(&closed, title)
            && !out.contains(&selected.number)
        {
            out.push(selected.number);
        }
    }
    Ok(out)
}

/// The JSON-lines decisions for every unique flake title, in first-seen
/// order. A complete open exact-title match emits nothing: the writer
/// comments on it. Every other title emits exactly one line
/// (`docs/testing/running.md`, "Flaky tests"). `is_ancestor` answers
/// whether a fix commit is an ancestor of the tested commit, `None`
/// when the object is missing or Git errors: unknown, never a match.
pub(crate) fn decide_with(
    input: &Value,
    is_ancestor: &dyn Fn(&str) -> Option<bool>,
) -> Result<Vec<Decision>, String> {
    let root = object(input, "input")?;
    let flakes = titles(root)?;
    let open = lookup(field(root, "input", "open")?, "open")?;
    let closed = lookup(field(root, "input", "closed")?, "closed")?;
    let events = object(field(root, "input", "events")?, "events")?;
    let mut cache: HashMap<String, Option<bool>> = HashMap::new();
    let mut out = Vec::new();
    for title in &flakes {
        out.extend(title_decision(title, &open, &closed, events, &mut |fix| {
            *cache
                .entry(fix.to_owned())
                .or_insert_with(|| is_ancestor(fix))
        })?);
    }
    Ok(out)
}

struct Issue {
    number: IssueNumber,
    title: String,
    closed_at: String,
    is_pr: bool,
}

struct Lookup {
    ok: bool,
    issues: Vec<Issue>,
}

struct Event {
    is_closed: bool,
    id: EventId,
    commit_id: Option<String>,
    created_at: String,
}

fn titles(root: &Map<String, Value>) -> Result<Vec<String>, String> {
    let flakes = array(field(root, "input", "flakes")?, "flakes")?;
    let mut titles = Vec::new();
    for flake in flakes {
        let title = text(object(flake, "flakes")?, "flakes", "title")?;
        if !titles.iter().any(|seen| seen == title) {
            titles.push(title.to_owned());
        }
    }
    Ok(titles)
}

/// The closed exact-title match the decision reopens or skips: the
/// greatest close time wins, then the higher issue number.
fn select_closed<'a>(closed: &'a Lookup, title: &str) -> Option<&'a Issue> {
    closed
        .issues
        .iter()
        .filter(|issue| !issue.is_pr && issue.title == title)
        .max_by(|a, b| (&a.closed_at, a.number.0).cmp(&(&b.closed_at, b.number.0)))
}

/// The latest `closed` event: the greatest `created_at`, then the
/// greater event id. Only its commit can affect the decision.
fn latest_closed(events: &[Event]) -> Option<&Event> {
    events
        .iter()
        .filter(|event| event.is_closed)
        .max_by(|a, b| (&a.created_at, a.id.0).cmp(&(&b.created_at, b.id.0)))
}

fn title_decision(
    title: &str,
    open: &Lookup,
    closed: &Lookup,
    events: &Map<String, Value>,
    is_ancestor: &mut dyn FnMut(&str) -> Option<bool>,
) -> Result<Option<Decision>, String> {
    if !open.ok {
        return Ok(Some(write(
            title,
            None,
            Action::Unknown,
            Some(Source::Open),
        )));
    }
    if open
        .issues
        .iter()
        .any(|issue| !issue.is_pr && issue.title == title)
    {
        return Ok(None);
    }
    if !closed.ok {
        return Ok(Some(write(
            title,
            None,
            Action::Unknown,
            Some(Source::Closed),
        )));
    }
    let Some(selected) = select_closed(closed, title) else {
        return Ok(Some(write(title, None, Action::FileNew, None)));
    };
    let key = selected.number.0.to_string();
    let Some(found) = events.get(&key) else {
        return Ok(Some(write(
            title,
            Some(selected.number),
            Action::Unknown,
            Some(Source::Events),
        )));
    };
    let lookup = event_lookup(found, &key)?;
    if !lookup.0 {
        return Ok(Some(write(
            title,
            Some(selected.number),
            Action::Unknown,
            Some(Source::Events),
        )));
    }
    let Some(latest) = latest_closed(&lookup.1) else {
        return Ok(Some(write(
            title,
            Some(selected.number),
            Action::Unknown,
            Some(Source::Events),
        )));
    };
    let Some(fix) = latest.commit_id.as_deref() else {
        return Ok(Some(write(
            title,
            Some(selected.number),
            Action::Reopen,
            None,
        )));
    };
    match is_ancestor(fix) {
        Some(true) => Ok(Some(write(
            title,
            Some(selected.number),
            Action::Reopen,
            None,
        ))),
        Some(false) => Ok(Some(write(
            title,
            Some(selected.number),
            Action::Stale,
            None,
        ))),
        None => Ok(Some(write(
            title,
            Some(selected.number),
            Action::Unknown,
            Some(Source::Ancestry),
        ))),
    }
}

fn write(
    title: &str,
    issue: Option<IssueNumber>,
    action: Action,
    source: Option<Source>,
) -> Decision {
    Decision {
        title: title.to_owned(),
        action,
        issue,
        source,
    }
}

fn lookup(input: &Value, name: &str) -> Result<Lookup, String> {
    let root = object(input, name)?;
    let ok = flag(root, name, "ok")?;
    let listed = array(field(root, name, "issues")?, "issues")?;
    let mut issues = Vec::new();
    for item in listed {
        let issue = object(item, "issues")?;
        issues.push(Issue {
            number: IssueNumber(id(field(issue, "issues", "number")?, "number")?),
            title: text(issue, "issues", "title")?.to_owned(),
            closed_at: maybe_text(issue, "closed_at")?
                .unwrap_or_default()
                .to_owned(),
            is_pr: issue
                .get("pull_request")
                .is_some_and(|pr| pr != &Value::Bool(false)),
        });
    }
    Ok(Lookup { ok, issues })
}

fn event_lookup(input: &Value, key: &str) -> Result<(bool, Vec<Event>), String> {
    let root = object(input, &format!("events[{key}]"))?;
    let ok = flag(root, &format!("events[{key}]"), "ok")?;
    let listed = array(field(root, &format!("events[{key}]"), "events")?, "events")?;
    let mut out = Vec::new();
    for item in listed {
        let event = object(item, "events")?;
        out.push(Event {
            is_closed: text(event, "events", "event")? == "closed",
            id: EventId(id(field(event, "events", "id")?, "id")?),
            commit_id: match event.get("commit_id") {
                None | Some(Value::Null) => None,
                Some(Value::String(fix)) => Some(fix.clone()),
                Some(_) => {
                    return Err("events: commit_id is not a string or null".to_owned());
                }
            },
            created_at: maybe_text(event, "created_at")?
                .unwrap_or_default()
                .to_owned(),
        });
    }
    Ok((ok, out))
}

fn object<'a>(input: &'a Value, name: &str) -> Result<&'a Map<String, Value>, String> {
    if let Value::Object(map) = input {
        Ok(map)
    } else {
        Err(format!("flake-report: {name} is not an object"))
    }
}

fn array<'a>(input: &'a Value, name: &str) -> Result<&'a Vec<Value>, String> {
    if let Value::Array(items) = input {
        Ok(items)
    } else {
        Err(format!("flake-report: {name} is not an array"))
    }
}

fn field<'a>(root: &'a Map<String, Value>, name: &str, key: &str) -> Result<&'a Value, String> {
    root.get(key)
        .ok_or_else(|| format!("flake-report: {name} has no {key}"))
}

fn text<'a>(root: &'a Map<String, Value>, name: &str, key: &str) -> Result<&'a str, String> {
    if let Value::String(text) = field(root, name, key)? {
        Ok(text)
    } else {
        Err(format!("flake-report: {name} {key} is not a string"))
    }
}

fn maybe_text<'a>(root: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, String> {
    match root.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(format!("flake-report: {key} is not a string or null")),
    }
}

fn flag(root: &Map<String, Value>, name: &str, key: &str) -> Result<bool, String> {
    if let Value::Bool(ok) = field(root, name, key)? {
        Ok(*ok)
    } else {
        Err(format!("flake-report: {name} {key} is not a boolean"))
    }
}

fn id(input: &Value, name: &str) -> Result<u64, String> {
    if let Value::Number(number) = input {
        number
            .as_u64()
            .ok_or_else(|| format!("flake-report: {name} is not a positive integer"))
    } else {
        Err(format!("flake-report: {name} is not a positive integer"))
    }
}

/// The classification of one flake line for the writer: whether the
/// failure belongs to the run's issue lookup, to a stale base or to
/// the pull request itself. Stale is checked before pull-request
/// ownership: a file both changed is stale, since the run tested the
/// old base. The writer reads this before any open-issue lookup, so a
/// stale or pull-request-owned title is skipped even when an open
/// exact-title issue exists.
/// Only a stale base carries the base commit the run tested against,
/// so a base cannot be built on any other class.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Class {
    Lookup,
    StaleBase { base: String },
    PrOwned,
}

impl Class {
    fn tag(&self) -> &'static str {
        match self {
            Class::Lookup => "lookup",
            Class::StaleBase { .. } => "stale-base",
            Class::PrOwned => "pr-owned",
        }
    }
}

/// The run context a classification decides under: whether the event
/// is a pull request, the tested commit every body names, the optional
/// base and head parents of the merge commit, and the two file lists
/// as raw text, one path per line.
pub(crate) struct Run<'a> {
    pub pull_request: bool,
    pub tested_commit: &'a str,
    pub base: &'a str,
    pub head: &'a str,
    pub compare_files: &'a str,
    pub pr_files: &'a str,
}

/// One flaky-list file already read: the runner its file is named for
/// and its raw text.
pub(crate) struct Listed<'a> {
    pub runner: &'a str,
    pub text: &'a str,
}

/// One classified flake title: the writer's action, the test name and
/// runner, the first occurrence's detail, the failing file when the
/// detail names one, the tested line and the issue title.
pub(crate) struct Classified {
    action: Class,
    name: String,
    runner: String,
    detail: String,
    failing: Option<String>,
    tested: String,
    title: String,
}

impl Classified {
    pub(crate) fn line(&self) -> String {
        let mut map = Map::new();
        map.insert(
            "action".to_owned(),
            Value::String(self.action.tag().to_owned()),
        );
        if let Class::StaleBase { base } = &self.action {
            map.insert("base".to_owned(), Value::String(base.clone()));
        }
        map.insert("detail".to_owned(), Value::String(self.detail.clone()));
        if let Some(failing) = &self.failing {
            map.insert("failing".to_owned(), Value::String(failing.clone()));
        }
        map.insert("name".to_owned(), Value::String(self.name.clone()));
        map.insert("runner".to_owned(), Value::String(self.runner.clone()));
        map.insert("tested".to_owned(), Value::String(self.tested.clone()));
        map.insert("title".to_owned(), Value::String(self.title.clone()));
        Value::Object(map).to_string()
    }
}

/// The issue title for one flake line: the aggregate `main` integration
/// binary is keyed by its first test-module segment; a root-level `main`
/// test falls back to `Flaky tests: main`. A binary id containing `::`,
/// such as `main::hub_exit_window`, keeps its binary title. A unit-test
/// binary is keyed by its module path without the test name or one trailing
/// `tests` module, appended to the binary id; root-level tests use the binary
/// id alone.
/// Mirrors the Collate step in .github/workflows/ci.yml; scripts/test-flake-report checks the two agree.
fn flake_title(binary: &str, test: &str) -> String {
    if binary == "main" {
        if let Some((first, _)) = test.split_once("::")
            && !first.is_empty()
        {
            return format!("Flaky tests: main::{first}");
        }
        return "Flaky tests: main".to_owned();
    }
    if binary.contains("::") {
        return format!("Flaky tests: {binary}");
    }
    let module = test.rsplit_once("::").map_or("", |(module, _)| module);
    if module == "tests" {
        return format!("Flaky tests: {binary}");
    }
    let module = module.strip_suffix("::tests").unwrap_or(module);
    if module.is_empty() {
        format!("Flaky tests: {binary}")
    } else {
        format!("Flaky tests: {binary}::{module}")
    }
}

/// One classification per distinct title, in first-seen order across
/// the lists and then the lines. A blank line carries no title and is
/// skipped; a line that is only a TAB names the empty test, and a last
/// line with no trailing newline is still read.
pub(crate) fn classify(lists: &[Listed], run: &Run) -> Vec<Classified> {
    let mut out: Vec<Classified> = Vec::new();
    for list in lists {
        for line in list.text.split('\n') {
            if line.is_empty() {
                continue;
            }
            let (name, detail) = match line.split_once('\t') {
                Some((name, detail)) => (name, detail),
                None => (line, ""),
            };
            let (binary, test) = match name.split_once(' ') {
                Some((binary, test)) => (binary, test),
                None => (name, ""),
            };
            // A legacy `Flaky test: <binary> <test>` ticket, open or
            // closed, is never matched: the lookup compares this title
            // exactly, with no legacy-title fallback.
            let title = flake_title(binary, test);
            if out.iter().any(|seen| seen.title == title) {
                continue;
            }
            let failing = failing_file(detail).map(str::to_owned);
            let action = match &failing {
                Some(file)
                    if run.pull_request
                        && !run.base.is_empty()
                        && run.compare_files.split('\n').any(|listed| listed == file) =>
                {
                    Class::StaleBase {
                        base: run.base.to_owned(),
                    }
                }
                Some(file)
                    if run.pull_request
                        && run.pr_files.split('\n').any(|listed| listed == file) =>
                {
                    Class::PrOwned
                }
                _ => Class::Lookup,
            };
            out.push(Classified {
                action,
                name: name.to_owned(),
                runner: list.runner.to_owned(),
                detail: detail.to_owned(),
                failing,
                tested: tested_line(run),
                title,
            });
        }
    }
    out
}

/// The `flaky-tests-<runner>.txt` names among `names`, as
/// `(runner, file name)` pairs sorted by file-name byte order. The
/// runner is the text between the prefix and the suffix, empty for
/// `flaky-tests-.txt`; anything shorter than the prefix plus the
/// suffix holds no runner and is skipped.
pub(crate) fn runner_lists(names: &[String]) -> Vec<(String, String)> {
    const PREFIX: &str = "flaky-tests-";
    const SUFFIX: &str = ".txt";
    let mut kept: Vec<(String, String)> = names
        .iter()
        .filter_map(|name| {
            let runner = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?.to_owned();
            Some((runner, name.clone()))
        })
        .collect();
    kept.sort_by(|a, b| a.1.cmp(&b.1));
    kept
}

/// The runner lists under `dir`: its regular files named
/// `flaky-tests-<runner>.txt`, in file-name byte order, each read as
/// lossy UTF-8. A missing or unreadable directory, entry or file is an
/// error naming what failed.
pub(crate) fn read_lists(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("flake-classify: {}: {e}", dir.display()))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("flake-classify: {}: {e}", dir.display()))?;
        let kind = entry.file_type().map(|kind| kind.is_file());
        if !entry_is_file(&entry.path(), kind)? {
            continue;
        }
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    let mut out = Vec::new();
    for (runner, name) in runner_lists(&names) {
        let path = dir.join(&name);
        let bytes =
            std::fs::read(&path).map_err(|e| format!("flake-classify: {}: {e}", path.display()))?;
        out.push((runner, String::from_utf8_lossy(&bytes).into_owned()));
    }
    Ok(out)
}

/// Whether the entry at `path` is a regular file, given the result of
/// reading its file type: a failed read is an error naming the entry,
/// never "not a file", so no runner list is silently skipped.
fn entry_is_file(path: &Path, kind: std::io::Result<bool>) -> Result<bool, String> {
    kind.map_err(|e| format!("flake-classify: {}: {e}", path.display()))
}

/// The file a failure names: `None` unless the detail carries a panic,
/// else the first `path.rs:line` run with the trailing line number
/// removed. The run is the maximal run of path characters ending in
/// `.rs` and followed by a colon and a digit; at least one character
/// must precede `.rs`.
fn failing_file(detail: &str) -> Option<&str> {
    if !detail.contains("panicked at") {
        return None;
    }
    let bytes = detail.as_bytes();
    let mut i = 0;
    while let Some(&byte) = bytes.get(i) {
        if !is_path_char(byte) {
            i += 1;
            continue;
        }
        let start = i;
        while bytes.get(i).is_some_and(|byte| is_path_char(*byte)) {
            i += 1;
        }
        let run = &detail[start..i];
        if run.len() > ".rs".len()
            && run.ends_with(".rs")
            && bytes.get(i) == Some(&b':')
            && bytes.get(i + 1).is_some_and(|byte| byte.is_ascii_digit())
        {
            return Some(run);
        }
    }
    None
}

fn is_path_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'/' | b'-')
}

/// The tested line every body names: the merge commit equals main plus
/// head on a pull request run that knows both parents, else the tested
/// commit alone.
fn tested_line(run: &Run) -> String {
    if run.pull_request && !run.base.is_empty() && !run.head.is_empty() {
        format!(
            "tested {} = main {} + head {}",
            run.tested_commit, run.base, run.head
        )
    } else {
        format!("tested {}", run.tested_commit)
    }
}

#[cfg(test)]
#[path = "flake_report_tests.rs"]
mod tests;
