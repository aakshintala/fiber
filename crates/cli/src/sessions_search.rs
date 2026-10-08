//! `fiber sessions search [--all] [--json] <text>` (`docs/invocation.md`,
//! "Commands and flags"): the hits the shared scan finds for the text, as
//! the `session_search` tool sees them (`docs/tools.md`, "Searching past
//! sessions"). The scope is the terminal's session-list scope: inside a git
//! repository that repository's project, otherwise every project, and every
//! project with `--all`.

use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Weak};

use contract::ErrorCode;
use contract::clock::Wake;
use contract::session_search::{Found, Hit, LIMIT, Label, Query, Scan};
use contract::shapes::Failure;
use contract::tool::Cancel;
use serde::Serialize;

use crate::{fail, failed};

/// `fiber sessions search [--all] [--json] <text>` in the current
/// directory: the shared scan's hits on stdout, its problems on stderr.
pub fn search(text: &str, all: bool, json: bool) -> i32 {
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let workspace = std::env::current_dir()
                .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
            let in_repository = doors::resolve_project(&workspace).in_repository;
            run(
                &home,
                &workspace,
                Arc::new(doors::project),
                in_repository,
                text,
                all,
                json,
                &mut io::stdout(),
                &mut io::stderr(),
            )
        });
    match ran {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

/// The signal the command scans with: never cancelled. SIGINT, SIGTERM and
/// SIGHUP keep their default action, so the shell reports 130, 143 and 129
/// (`docs/invocation.md`, "Commands and flags"); the scan writes nothing,
/// so nothing is left half-done.
struct Uncancelled;

impl Cancel for Uncancelled {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

/// One `--json` line. `--json` prints it as is, its fields in this order.
#[derive(Debug, PartialEq, Serialize)]
struct Row {
    session_id: String,
    name: String,
    seq: u64,
    ts: u64,
    label: &'static str,
    snippet: String,
    log: String,
    artifact: Option<String>,
}

/// The command's hits: the shared scan's for the terminal's session-list
/// scope (`docs/invocation.md`, "Commands and flags"), in its order. The
/// command never re-ranks, filters or truncates them. `in_repository` is
/// whether the workspace is inside a git repository, found once by the
/// caller, so no test needs git to answer it.
#[allow(
    clippy::too_many_arguments,
    reason = "one call of the command's inputs: where, what, how, and where to write"
)]
fn run(
    home: &Path,
    workspace: &Path,
    identity: log::Identity,
    in_repository: bool,
    text: &str,
    all: bool,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let all_projects = all || !in_repository;
    let scan = log::SessionScan::new(home, workspace, identity);
    let query = Query {
        text: text.to_owned(),
        all_projects,
        limit: LIMIT,
    };
    let found = scan.scan(&query, &Uncancelled);
    if json {
        json_hits(&found, out)?;
    } else {
        text_hits(&found, text, all_projects, out)?;
    }
    problems(&found, err);
    Ok(())
}

/// The hits as JSON Lines on `out`: one object per hit, best first, nothing
/// for zero hits. Strings are the scan's own: no control-character mapping
/// (`docs/invocation.md`, "Commands and flags").
fn json_hits(found: &Found, out: &mut dyn Write) -> Result<(), Failure> {
    for hit in &found.hits {
        let line = serde_json::to_string(&row(hit))
            .map_err(|e| failed(ErrorCode::IoFailed, format!("a search hit: {e}")))?;
        writeln!(out, "{line}")
            .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))?;
    }
    Ok(())
}

/// The hits as text on `out`: a header, then one block per hit, best first.
fn text_hits(
    found: &Found,
    text: &str,
    all_projects: bool,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    if found.total == 0 {
        let scope = if all_projects {
            "any project's"
        } else {
            "this project's"
        };
        writeln!(
            out,
            "No hits for \"{}\" in {scope} sessions.",
            one_line(text)
        )
        .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))?;
        return Ok(());
    }
    writeln!(
        out,
        "{} of {} hits for \"{}\", best first.",
        found.hits.len(),
        found.total,
        one_line(text)
    )
    .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))?;
    for (at, hit) in found.hits.iter().enumerate() {
        block(out, at + 1, hit)?;
    }
    Ok(())
}

/// One hit: what and where, the artifact when the hit is in one, and the
/// snippet. Every interpolated string is one line, so a hit stays on its
/// lines and no escape sequence reaches the terminal.
fn block(out: &mut dyn Write, number: usize, hit: &Hit) -> Result<(), Failure> {
    let label = match hit.label {
        Label::Message => "message",
        Label::ToolInput => "tool_input",
        Label::ToolOutput => "tool_output",
    };
    writeln!(
        out,
        "{number}. {label}, session {} \"{}\", seq {}, {}",
        one_line(&hit.session_id.0),
        one_line(&hit.name),
        hit.seq.0,
        utc(hit.ts / 1_000)
    )
    .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))?;
    // A hit in an artifact otherwise names a `seq` whose event does not
    // hold the snippet.
    if let Some(artifact) = &hit.artifact {
        writeln!(
            out,
            "   artifact {}",
            one_line(&artifact.display().to_string())
        )
        .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))?;
    }
    writeln!(out, "   {}", one_line(&hit.snippet))
        .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))
}

/// A hit as one `--json` row.
fn row(hit: &Hit) -> Row {
    Row {
        session_id: hit.session_id.0.clone(),
        name: hit.name.clone(),
        seq: hit.seq.0,
        ts: hit.ts,
        label: match hit.label {
            Label::Message => "message",
            Label::ToolInput => "tool_input",
            Label::ToolOutput => "tool_output",
        },
        snippet: hit.snippet.clone(),
        log: hit.log.display().to_string(),
        artifact: hit.artifact.as_ref().map(|path| path.display().to_string()),
    }
}

/// The scan's problems on `err`, one line each, then how many more past the
/// scan's cap. A failed write to stderr is ignored: there is nobody else to
/// tell. Problems never change the exit code: the search ran.
fn problems(found: &Found, err: &mut dyn Write) {
    for problem in &found.problems {
        writeln!(err, "{}", one_line(problem)).unwrap_or(());
    }
    if found.more_problems > 0 {
        writeln!(err, "And {} more problems.", found.more_problems).unwrap_or(());
    }
}

/// `text` with every control character as a space, so a hit stays on its
/// lines and no escape sequence reaches the terminal.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// `seconds` since the epoch as a UTC time to the second,
/// `2026-10-07T10:00:03Z`.
fn utc(seconds: u64) -> String {
    let (year, month, day) = civil_from_days(seconds / 86_400);
    let second = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second / 3_600,
        second % 3_600 / 60,
        second % 60
    )
}

/// Proleptic Gregorian date of `days` since 1970-01-01.
// `loop` keeps its own copy (`loop::opening::civil_from_days`): `cli`
// may not call `tools`, and `contract` holds no behaviour beyond
// serialisation.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let era = (days + 719_468) / 146_097;
    let start = days + 719_468 - era * 146_097;
    let year = (start - start / 1_460 + start / 36_524 - start / 146_096) / 365;
    let ordinal = start - (365 * year + year / 4 - year / 100);
    let month = (5 * ordinal + 2) / 153;
    let day = ordinal - (153 * month + 2) / 5 + 1;
    let month = if month < 10 { month + 3 } else { month - 9 };
    let year = if month <= 2 {
        year + era * 400 + 1
    } else {
        year + era * 400
    };
    (year, month, day)
}

#[cfg(test)]
#[path = "sessions_search_tests.rs"]
mod tests;
