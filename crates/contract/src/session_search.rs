//! The session search seam (`docs/tools.md`, "Searching past sessions"):
//! what a search asks for and what it finds. `log` implements [`Scan`], and
//! the `session_search` tool and `fiber sessions search` call it, so a change
//! to the scan changes both.

use std::path::PathBuf;

use crate::tool::Cancel;
use crate::{Seq, SessionId};

/// One search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// The text to find, as a literal, ignoring case.
    pub text: String,
    /// Every project in Fiber home instead of the session's own.
    pub all_projects: bool,
    /// The most hits returned.
    pub limit: usize,
}

/// What a hit's text is. The declaration order is the rank order among hits
/// that are otherwise equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Label {
    /// A message from the person or the model.
    Message,
    /// A tool call's input.
    ToolInput,
    /// A tool call's output, or a text artifact.
    ToolOutput,
}

/// One place the text was found. It never carries a whole event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The session.
    pub session_id: SessionId,
    /// The session's name as `session_status` gives it.
    pub name: String,
    /// The event's `seq`.
    pub seq: Seq,
    /// The event's `ts`, milliseconds since the epoch.
    pub ts: u64,
    /// What the text is.
    pub label: Label,
    /// About 200 characters around the match, as found.
    pub snippet: String,
    /// The session's `events.jsonl`.
    pub log: PathBuf,
    /// The text artifact the hit is in, when it is in one.
    pub artifact: Option<PathBuf>,
}

/// What a search found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    /// The best hits first, at most the query's `limit`.
    pub hits: Vec<Hit>,
    /// Every hit, including those past the limit.
    pub total: u64,
    /// What could not be read, at most 20 entries.
    pub problems: Vec<String>,
    /// How many more problems there were.
    pub more_problems: u64,
}

/// The scan behind a session search.
pub trait Scan: Send + Sync {
    /// The directory a search reads: the session's own project's, or every
    /// project's with `all_projects`. It ends with `/`.
    fn scope(&self, all_projects: bool) -> PathBuf;

    /// Runs `query`. It never writes and never fails: what it cannot read is
    /// listed in [`Found::problems`], and a cancelled search returns what it
    /// had found.
    fn scan(&self, query: &Query, cancel: &dyn Cancel) -> Found;
}
