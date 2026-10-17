//! Standing rules (`docs/permissions.md`, "Remembering a decision"): data and
//! a trait only. `contract` holds no behaviour beyond checking that a value
//! fits the vocabulary, and pure conversions of its own values.

use serde::{Deserialize, Serialize};

use crate::SessionId;

/// What a standing rule decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleDecision {
    /// Allow a matching call.
    Allow,
    /// Ask a person about a matching call.
    Ask,
    /// Deny a matching call.
    Deny,
}

/// One line of a rules file (`docs/configuration.md`, "Standing rules").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// What matching calls do.
    pub decision: RuleDecision,
    /// The tool's name.
    pub tool: String,
    /// The prefix later calls match.
    pub prefix: String,
    /// When it was added, ms since the Unix epoch; absent on a hand-written line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added: Option<u64>,
    /// The session whose approval added it; absent on a hand-written line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

/// Both rules files read now, in file order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StandingRules {
    /// Lines of the global rules file.
    pub global: Vec<Rule>,
    /// Lines of the project's rules file.
    pub project: Vec<Rule>,
}

/// Why the rules could not be read: the message names the file and line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct RulesError(pub String);

/// Reads the standing rules and remembers an approval.
pub trait Rules: Send + Sync {
    /// Both files, read now.
    fn read(&self) -> Result<StandingRules, RulesError>;
    /// Appends an allow for `tool` and `prefix` to the project's rules file.
    fn remember(&self, tool: &str, prefix: &str, session: &SessionId) -> Result<(), RulesError>;
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
