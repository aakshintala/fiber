//! The `session_search` tool (`docs/tools.md`, "Searching past sessions"):
//! the model finds a literal in this project's session logs. The scan is
//! `log`'s, injected as a [`Scan`]; this tool reads the arguments, declares
//! what the scan reads, and prints what it found.

use std::sync::Arc;

use contract::ErrorCode;
use contract::emit::Emit;
use contract::provider::ToolDefinition;
use contract::session_search::{Found, Hit, LIMIT, Label, Query, Scan};
use contract::shapes::Effect;
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::tool_util::{effects, failed, text_output};

/// The most characters of a session's name a hit shows.
const NAME: usize = 80;

/// The call's arguments, checked against the schema before the call runs.
#[derive(Debug, Deserialize)]
struct Args {
    text: String,
    #[serde(default)]
    all_projects: bool,
    limit: Option<u64>,
}

/// Searches the logs of past and running sessions.
pub struct SessionSearch {
    scan: Arc<dyn Scan>,
}

impl SessionSearch {
    /// The tool over `scan`.
    pub fn new(scan: Arc<dyn Scan>) -> Self {
        Self { scan }
    }
}

impl Tool for SessionSearch {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "session_search".to_owned(),
            description: "Finds text in the logs of this project's past and running sessions: \
                 what was decided, which command ran, which file changed, where an error \
                 appeared. The text is matched as a literal, ignoring case, in messages, tool \
                 inputs and tool outputs, including full outputs saved as artifacts. Hits come \
                 best first: messages and tool inputs before tool outputs, then newest first. \
                 Each gives the log's path and the offset to read from with `read`."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "minLength": 1,
                        "description": "The text to find, as a literal."
                    },
                    "all_projects": {
                        "type": "boolean",
                        "description": "Search every project's sessions instead of this project's. Default false."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "The most hits to return. Default 20."
                    }
                },
                "required": ["text"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        let args: Args = crate::tool_util::arguments(arguments).map_err(EffectsError::Arguments)?;
        let scope = self.scan.scope(args.all_projects);
        Ok(effects(
            vec![Effect::Reads],
            true,
            Some(vec![scope.to_string_lossy().into_owned()]),
            Some(String::new()),
            None,
        ))
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, _emit: &dyn Emit) -> Output {
        let query = match query(arguments) {
            Ok(query) => query,
            Err(message) => return failed(ErrorCode::InvalidArguments, message),
        };
        let found = self.scan.scan(&query, cancel);
        // The scan returns what it had when cancelled, which is not the
        // answer to the query (`docs/tools.md`, "Cancellation").
        if cancel.is_cancelled() {
            return text_output("Cancelled before it finished.\n".to_owned());
        }
        text_output(printed(&query, &found))
    }
}

/// The call's query, with the documented defaults.
fn query(arguments: &Map<String, Value>) -> Result<Query, String> {
    let args: Args = crate::tool_util::arguments(arguments)?;
    Ok(Query {
        text: args.text,
        all_projects: args.all_projects,
        limit: args
            .limit
            .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX))
            .unwrap_or(LIMIT),
    })
}

/// The result the model reads: a header, one block per hit, then what could
/// not be read.
fn printed(query: &Query, found: &Found) -> String {
    let wanted = one_line(&query.text);
    let mut out = String::new();
    if found.total == 0 {
        let scope = if query.all_projects {
            "any project's"
        } else {
            "this project's"
        };
        out.push_str(&format!("No hits for \"{wanted}\" in {scope} sessions.\n"));
    } else {
        out.push_str(&format!(
            "{} of {} hits for \"{wanted}\", best first.\n",
            found.hits.len(),
            found.total
        ));
    }
    for (at, hit) in found.hits.iter().enumerate() {
        block(&mut out, at + 1, hit);
    }
    for problem in &found.problems {
        out.push_str(&format!("{}\n", one_line(problem)));
    }
    if found.more_problems > 0 {
        out.push_str(&format!("And {} more problems.\n", found.more_problems));
    }
    out
}

/// One hit: what and where, the line to `read` from, the artifact when the
/// hit is in one, and the snippet.
fn block(out: &mut String, number: usize, hit: &Hit) {
    let label = match hit.label {
        Label::Message => "message",
        Label::ToolInput => "tool_input",
        Label::ToolOutput => "tool_output",
    };
    out.push_str(&format!(
        "{number}. {label}, session {} \"{}\", seq {}, {}\n",
        hit.session_id.0,
        name(&hit.name),
        hit.seq.0,
        utc(hit.ts / 1_000)
    ));
    // The log's line for an event is its `seq` + 1 (`docs/tools.md`,
    // "Searching past sessions").
    out.push_str(&format!(
        "   read {} from offset {}\n",
        hit.log.display(),
        hit.seq.0.saturating_add(1)
    ));
    if let Some(artifact) = &hit.artifact {
        out.push_str(&format!("   artifact {}\n", artifact.display()));
    }
    out.push_str(&format!("   {}\n", one_line(&hit.snippet)));
}

/// A session's name on one line, at most [`NAME`] characters and `…`.
fn name(name: &str) -> String {
    let line = one_line(name);
    if line.chars().count() > NAME {
        let mut cut: String = line.chars().take(NAME).collect();
        cut.push('…');
        cut
    } else {
        line
    }
}

/// `text` with every control character as a space, so a hit stays on its
/// lines.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// `seconds` since the epoch as a UTC time to the second,
/// `2026-10-07T10:00:03Z`.
pub(crate) fn utc(seconds: u64) -> String {
    let (year, month, day) = contract::clock::utc_date_of_secs(seconds);
    let second = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second / 3_600,
        second % 3_600 / 60,
        second % 60
    )
}

#[cfg(test)]
#[path = "session_search_tests.rs"]
mod tests;
