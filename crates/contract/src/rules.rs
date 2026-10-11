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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulesError(pub String);

impl std::fmt::Display for RulesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RulesError {}

/// Reads the standing rules and remembers an approval.
pub trait Rules: Send + Sync {
    /// Both files, read now.
    fn read(&self) -> Result<StandingRules, RulesError>;
    /// Appends an allow for `tool` and `prefix` to the project's rules file.
    fn remember(&self, tool: &str, prefix: &str, session: &SessionId) -> Result<(), RulesError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rule_round_trips_with_and_without_added_and_session() {
        let full = Rule {
            decision: RuleDecision::Allow,
            tool: "shell".into(),
            prefix: "npm test".into(),
            added: Some(1_700_000_000_000),
            session_id: Some(SessionId("s_0123456789abcdef".into())),
        };
        let line = serde_json::to_string(&full).unwrap();
        assert_eq!(
            line,
            r#"{"decision":"allow","tool":"shell","prefix":"npm test","added":1700000000000,"session_id":"s_0123456789abcdef"}"#
        );
        assert_eq!(serde_json::from_str::<Rule>(&line).unwrap(), full);

        let plain: Rule =
            serde_json::from_str(r#"{"decision":"ask","tool":"shell","prefix":"npm publish"}"#)
                .unwrap();
        assert_eq!(plain.added, None);
        assert_eq!(plain.session_id, None);
        let line = serde_json::to_string(&plain).unwrap();
        assert_eq!(
            line,
            r#"{"decision":"ask","tool":"shell","prefix":"npm publish"}"#
        );
        assert_eq!(serde_json::from_str::<Rule>(&line).unwrap(), plain);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let rule: Rule =
            serde_json::from_str(r#"{"decision":"deny","tool":"shell","prefix":"rm","later":"x"}"#)
                .unwrap();
        assert_eq!(rule.prefix, "rm");
    }

    #[test]
    fn an_unknown_decision_fails_to_read() {
        assert!(
            serde_json::from_str::<Rule>(r#"{"decision":"maybe","tool":"shell","prefix":"npm"}"#)
                .is_err()
        );
    }
}
