//! `fiber sessions [--all] [--json]` (`docs/invocation.md`, "Commands and
//! flags"): one `sessions` command to the hub, then a row per session: id,
//! state, the name or first prompt, what it waits on, and spend. Inside a
//! git repository it lists that repository's project unless `--all`;
//! outside one, every project.

use std::io::{self, BufReader, Write};
use std::path::Path;

use contract::ErrorCode;
use contract::events::{SessionState, SessionStatus, WaitingKind};
use contract::shapes::Failure;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::sessions::request;
use crate::table::pad;
use crate::{fail, failed};

/// The `id` of the one command `fiber sessions` sends.
const LIST_ID: &str = "c_sessions";

/// The hub's answer to `sessions`.
#[derive(Deserialize)]
struct Answer {
    live: Vec<Live>,
    exited: Vec<Exited>,
}

/// A running session and its latest status.
#[derive(Deserialize)]
struct Live {
    session_id: String,
    status: SessionStatus,
}

/// An exited session's `recent.jsonl` row.
#[derive(Deserialize)]
struct Exited {
    session_id: String,
    name: String,
    how: String,
    #[serde(default)]
    status: Option<SessionStatus>,
}

/// One printed row. `--json` prints it as is, its fields in this order.
#[derive(Debug, PartialEq, Serialize)]
struct Row {
    id: String,
    state: String,
    name: String,
    waiting: Option<String>,
    /// US dollars.
    spend: f64,
}

/// `fiber sessions [--all] [--json]` in the current directory, through the
/// hub `connect` reaches, starting one when none runs.
pub fn list(
    all: bool,
    json: bool,
    connect: &mut dyn FnMut() -> io::Result<doors::hub::Hub>,
) -> i32 {
    let ran = std::env::current_dir()
        .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))
        .and_then(|workspace| {
            let identity = doors::project(&workspace);
            run(&workspace, &identity, all, json, &mut io::stdout(), connect)
        });
    match ran {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

/// Lists the sessions in `workspace`'s scope, `identity` its project, as
/// text or JSON Lines on `out`.
fn run(
    workspace: &Path,
    identity: &Path,
    all: bool,
    json: bool,
    out: &mut dyn Write,
    connect: &mut dyn FnMut() -> io::Result<doors::hub::Hub>,
) -> Result<(), Failure> {
    let mut args = serde_json::Map::new();
    if !all && doors::in_repository(workspace, identity) {
        args.insert(
            "project".to_owned(),
            Value::String(log::project_key(identity)),
        );
    }
    let (stream, _) =
        connect().map_err(|e| failed(ErrorCode::IoFailed, format!("the hub: {e}")))?;
    let result = request(&mut BufReader::new(stream), LIST_ID, "sessions", args)?;
    let answer: Answer = serde_json::from_value(result).map_err(|e| {
        failed(
            ErrorCode::IoFailed,
            format!("the hub's answer to `sessions`: {e}"),
        )
    })?;
    let lines = if json {
        json_lines(&rows(answer))?
    } else {
        text_lines(&rows(answer))
    };
    for line in &lines {
        writeln!(out, "{line}")
            .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))?;
    }
    Ok(())
}

/// Live rows in the hub's order, then exited rows in the hub's order.
fn rows(answer: Answer) -> Vec<Row> {
    let live = answer.live.into_iter().map(|live| Row {
        id: live.session_id,
        state: state_word(&live.status.state).to_owned(),
        name: live.status.name.clone(),
        waiting: waiting(&live.status),
        spend: spend(Some(&live.status)),
    });
    let exited = answer.exited.into_iter().map(|row| Row {
        id: row.session_id,
        state: row.how,
        name: row
            .status
            .as_ref()
            .map(|status| status.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or(row.name),
        waiting: row.status.as_ref().and_then(waiting),
        spend: spend(row.status.as_ref()),
    });
    live.chain(exited).collect()
}

/// One text cell: each embedded line break is one space.
fn one_line(cell: &str) -> String {
    cell.replace("\r\n", " ").replace(['\r', '\n'], " ")
}

/// The `state` word a live session's status names.
fn state_word(state: &SessionState) -> &'static str {
    match state {
        SessionState::Streaming => "streaming",
        SessionState::Tool { .. } => "tool",
        SessionState::Retrying => "retrying",
        SessionState::Waiting { .. } => "waiting",
        SessionState::Jobs => "jobs",
        SessionState::Idle => "idle",
    }
}

/// What the session waits on: `approval: <summary>` or
/// `question: <summary>`.
fn waiting(status: &SessionStatus) -> Option<String> {
    match &status.state {
        SessionState::Waiting { waiting } => {
            let kind = match waiting.kind {
                WaitingKind::Approval => "approval",
                WaitingKind::Question => "question",
            };
            Some(format!("{kind}: {}", waiting.summary))
        }
        SessionState::Streaming
        | SessionState::Tool { .. }
        | SessionState::Retrying
        | SessionState::Jobs
        | SessionState::Idle => None,
    }
}

/// The session's spend in US dollars: its billed cost, 0 when unknown,
/// plus its subscription cost; 0 with no status.
fn spend(status: Option<&SessionStatus>) -> f64 {
    status.map_or(0.0, |status| {
        status.spend.cost.unwrap_or(0.0) + status.spend.subscription_cost
    })
}

fn json_lines(rows: &[Row]) -> Result<Vec<String>, Failure> {
    rows.iter()
        .map(|row| {
            serde_json::to_string(row)
                .map_err(|e| failed(ErrorCode::IoFailed, format!("a session row: {e}")))
        })
        .collect()
}

/// The text table: a header row, then one row per session, a missing cell
/// `-`. A cell never spans lines: embedded line breaks become single
/// spaces, so a row stays one line; JSON keeps the original.
fn text_lines(rows: &[Row]) -> Vec<String> {
    let or_missing = |cell: &str| {
        if cell.is_empty() {
            "-".to_owned()
        } else {
            cell.to_owned()
        }
    };
    let mut table = vec![
        ["id", "state", "spend", "waits on", "name"]
            .map(str::to_owned)
            .to_vec(),
    ];
    for row in rows {
        table.push(vec![
            row.id.clone(),
            row.state.clone(),
            format!("${:.2}", row.spend),
            or_missing(&one_line(row.waiting.as_deref().unwrap_or_default())),
            or_missing(&one_line(&row.name)),
        ]);
    }
    pad(&table)
}

#[cfg(test)]
#[path = "sessions_list_tests.rs"]
mod tests;
