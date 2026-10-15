//! Standing rules that remember nothing, and ones that record what was
//! remembered (`contract::rules::Rules`).

use std::sync::Mutex;

use contract::SessionId;
use contract::rules::{Rules, RulesError, StandingRules};

/// Standing rules that hold nothing: `read` is empty, and `remember`
/// succeeds without writing.
pub struct NoRules;

impl Rules for NoRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules::default())
    }

    fn remember(&self, _: &str, _: &str, _: &SessionId) -> Result<(), RulesError> {
        Ok(())
    }
}

/// Standing rules that are empty but record every `remember` call as
/// (`tool`, `prefix`, `session`), in order.
pub struct RecordingRules {
    remembered: Mutex<Vec<(String, String, SessionId)>>,
}

impl RecordingRules {
    /// Rules that have remembered nothing.
    pub fn new() -> Self {
        Self {
            remembered: Mutex::new(Vec::new()),
        }
    }

    /// Every (`tool`, `prefix`, `session`) remembered, in order.
    pub fn calls(&self) -> Vec<(String, String, SessionId)> {
        self.remembered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Default for RecordingRules {
    fn default() -> Self {
        Self::new()
    }
}

impl Rules for RecordingRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules::default())
    }

    fn remember(&self, tool: &str, prefix: &str, session: &SessionId) -> Result<(), RulesError> {
        self.remembered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((tool.into(), prefix.into(), session.clone()));
        Ok(())
    }
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
