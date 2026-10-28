//! `fiber sessions [--all] [--json]` (`docs/invocation.md`, "Commands and
//! flags"): one `sessions` command to the hub, then a row per session: id,
//! state, the name or first prompt, what it waits on, and spend. Inside a
//! git repository it lists that repository's project unless `--all`;
//! outside one, every project.

use std::io::{self, BufReader, Write};

use contract::SessionId;
use contract::commands::hub::{HubCommand, SessionsArgs};
use contract::events::{SessionState, WaitingKind};
use contract::shapes::Usage;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use support::text::one_line;

use crate::sessions::request_value;
use crate::table::pad;
use crate::{CliError, finish, write_line};

/// The `id` of the one command `fiber sessions` sends.
const LIST_ID: &str = "c_sessions";

/// Only the status fields this list prints. Flattening `SessionState`
/// here would buffer every ignored field, including delegate progress,
/// before reading the state. Read its tag and associated fields directly.
#[derive(Deserialize)]
#[serde(try_from = "StatusFields")]
struct ListStatus {
    name: String,
    state: SessionState,
    spend: Usage,
}

#[derive(Deserialize)]
struct StatusFields {
    name: String,
    state: State,
    tool: Option<Value>,
    waiting: Option<Value>,
    spend: Usage,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum State {
    Streaming,
    Tool,
    Retrying,
    Waiting,
    Jobs,
    Idle,
}

impl TryFrom<StatusFields> for ListStatus {
    type Error = serde_json::Error;

    fn try_from(fields: StatusFields) -> Result<Self, Self::Error> {
        let state = match fields.state {
            State::Streaming => SessionState::Streaming,
            State::Tool => SessionState::Tool {
                tool: serde_json::from_value(fields.tool.ok_or_else(|| {
                    <serde_json::Error as serde::de::Error>::missing_field("tool")
                })?)?,
            },
            State::Retrying => SessionState::Retrying,
            State::Waiting => SessionState::Waiting {
                waiting: serde_json::from_value(fields.waiting.ok_or_else(|| {
                    <serde_json::Error as serde::de::Error>::missing_field("waiting")
                })?)?,
            },
            State::Jobs => SessionState::Jobs,
            State::Idle => SessionState::Idle,
        };
        Ok(Self {
            name: fields.name,
            state,
            spend: fields.spend,
        })
    }
}

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
    status: ListStatus,
    /// The `ts` of the log's last line, else its opening `ts`, else
    /// `null` when neither reads: present only when asked for with
    /// `active: true`. Absent reads as `null`, so a default listing
    /// without the key still parses.
    #[serde(default)]
    active: Option<u64>,
}

/// An exited session's `recent.jsonl` row.
#[derive(Deserialize)]
struct Exited {
    session_id: String,
    name: String,
    how: String,
    #[serde(default)]
    status: Option<ListStatus>,
    /// The entry's activity, as on a live row.
    #[serde(default)]
    active: Option<u64>,
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
    connect: &mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>,
) -> i32 {
    let ran = std::env::current_dir()
        .map_err(CliError::CurrentDirectory)
        .and_then(|workspace| {
            let project = worktree::resolve_project(&workspace);
            run(&project, all, json, &mut io::stdout(), connect)
        });
    finish(ran)
}

/// Lists the sessions in `project`'s scope, as
/// text or JSON Lines on `out`.
fn run(
    project: &worktree::Project,
    all: bool,
    json: bool,
    out: &mut dyn Write,
    connect: &mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>,
) -> Result<(), CliError> {
    let command = HubCommand::Sessions(SessionsArgs {
        project: (!all && project.in_repository).then(|| log::project_key(&project.path)),
        active: None,
    });
    let (stream, _) = connect().map_err(CliError::Hub)?;
    let result = request_value(&mut BufReader::new(stream), LIST_ID, command)?;
    let answer: Answer = serde_json::from_value(result).map_err(CliError::SessionsAnswer)?;
    let lines = if json {
        json_lines(&rows(answer))?
    } else {
        text_lines(&rows(answer))
    };
    for line in &lines {
        write_line(out, line)?;
    }
    Ok(())
}

/// `fiber resume <selector> --hub <name>`: the session the selector names
/// on that hub, over the hub's `sessions` answer across every project.
/// A full id wins over a longer id it prefixes, else a unique prefix;
/// an empty selector matches nothing. The command carries no `project`
/// argument, as the list outside a repository does, and never reads the
/// current directory.
pub fn resolve_listed(
    selector: &str,
    hub: &str,
    connect: &mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>,
) -> Result<SessionId, CliError> {
    if selector.is_empty() {
        return Err(not_found(selector, hub));
    }
    let (stream, _) = connect().map_err(CliError::Hub)?;
    let result = request_value(
        &mut BufReader::new(stream),
        LIST_ID,
        HubCommand::Sessions(SessionsArgs {
            project: None,
            active: None,
        }),
    )?;
    let answer: Answer = serde_json::from_value(result).map_err(CliError::SessionsAnswer)?;
    let ids = answer
        .live
        .into_iter()
        .map(|row| row.session_id)
        .chain(answer.exited.into_iter().map(|row| row.session_id));
    let ids: Vec<String> = ids.collect();
    if ids.iter().any(|id| id == selector) {
        return Ok(SessionId(selector.to_owned()));
    }
    let mut matches: Vec<String> = ids
        .into_iter()
        .filter(|id| id.starts_with(selector))
        .collect();
    matches.sort();
    match matches.len() {
        1 => Ok(SessionId(matches.remove(0))),
        0 => Err(not_found(selector, hub)),
        _ => {
            let error = log::Error::Ambiguous {
                selector: selector.to_owned(),
                matches,
            };
            Err(CliError::Log(error))
        }
    }
}

/// `fiber continue --hub <name>`: the latest session on that hub, across
/// every project: the entry with the greatest (`active`, `id`) among
/// entries whose `active` is not `null`. `None` when no entry qualifies.
/// The command carries `active: true` and no `project`, and never reads
/// the current directory.
pub fn latest_listed(
    connect: &mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>,
) -> Result<Option<SessionId>, CliError> {
    let (stream, _) = connect().map_err(CliError::Hub)?;
    let result = request_value(
        &mut BufReader::new(stream),
        LIST_ID,
        HubCommand::Sessions(SessionsArgs {
            project: None,
            active: Some(true),
        }),
    )?;
    let answer: Answer = serde_json::from_value(result).map_err(CliError::SessionsAnswer)?;
    Ok(answer
        .live
        .into_iter()
        .map(|row| (row.session_id, row.active))
        .chain(
            answer
                .exited
                .into_iter()
                .map(|row| (row.session_id, row.active)),
        )
        .filter_map(|(id, active)| active.map(|active| (active, id)))
        .max()
        .map(|(_, id)| SessionId(id)))
}

/// No session on `hub` answers `selector`: the local sessions path does
/// not exist here, so the sentence names the hub instead.
fn not_found(selector: &str, hub: &str) -> CliError {
    CliError::SessionNotFoundOnHub {
        selector: selector.to_owned(),
        hub: hub.to_owned(),
    }
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
/// `question: <summary>` or `offer: <summary>`.
fn waiting(status: &ListStatus) -> Option<String> {
    match &status.state {
        SessionState::Waiting { waiting } => {
            let kind = match waiting.kind {
                WaitingKind::Approval => "approval",
                WaitingKind::Question => "question",
                WaitingKind::Offer => "offer",
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
fn spend(status: Option<&ListStatus>) -> f64 {
    status.map_or(0.0, |status| {
        status.spend.cost.unwrap_or(0.0) + status.spend.subscription_cost
    })
}

fn json_lines(rows: &[Row]) -> Result<Vec<String>, CliError> {
    rows.iter()
        .map(|row| serde_json::to_string(row).map_err(CliError::SessionRow))
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
