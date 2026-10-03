//! Standing rules (`docs/permissions.md`, "Remembering a decision", and
//! `docs/configuration.md`, "Standing rules"): the two rules files in Fiber
//! home. Only `config` reads files in Fiber home
//! (`docs/architecture.md`, "The call rules").

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use contract::SessionId;
use contract::clock::Clock;
use contract::rules::{Rule, Rules, RulesError, StandingRules};

use crate::home::{ProjectKey, plain, read_bytes};
use crate::{ConfigError, write};

/// The standing rules in Fiber home: `rules` at the top level and
/// `projects/<key>/rules` for one project.
pub struct RulesFiles {
    /// The global rules file.
    global: PathBuf,
    /// The project's rules file.
    project: PathBuf,
    /// Wall time for `added` on a remembered rule.
    clock: Arc<dyn Clock>,
}

impl RulesFiles {
    /// Reads rules from `home`, for `project`, stamping remembered rules
    /// with `clock`'s wall time.
    pub fn new(home: PathBuf, project: ProjectKey, clock: Arc<dyn Clock>) -> Self {
        let project = home.join("projects").join(project.as_str()).join("rules");
        let global = home.join("rules");
        Self {
            global,
            project,
            clock,
        }
    }
}

impl Rules for RulesFiles {
    /// Both files, read now, in file order. A missing file holds no rules;
    /// a symlink or non-regular file, an unreadable file, or a line that
    /// does not parse is an error naming the file (and the line).
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules {
            global: read_file(&self.global)?,
            project: read_file(&self.project)?,
        })
    }

    /// Appends an allow for `tool` and `prefix` to the project's rules file,
    /// through the locked whole-file write, creating the directory.
    fn remember(&self, tool: &str, prefix: &str, session: &SessionId) -> Result<(), RulesError> {
        let added = self
            .clock
            .wall()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| RulesError(format!("the clock is before the Unix epoch: {e}")))
            .and_then(|d| {
                u64::try_from(d.as_millis())
                    .map_err(|e| RulesError(format!("the clock is past what `added` holds: {e}")))
            })?;
        let line = serde_json::to_string(&Rule {
            decision: contract::rules::RuleDecision::Allow,
            tool: tool.to_owned(),
            prefix: prefix.to_owned(),
            added: Some(added),
            session_id: Some(session.clone()),
        })
        .map_err(|e| RulesError(format!("{}: {e}", self.project.display())))?;
        write::append_line(&self.project, &line).map_err(rules_error)
    }
}

/// One rules file's lines, in order. Blank lines are skipped.
fn read_file(file: &Path) -> Result<Vec<Rule>, RulesError> {
    if !plain(file, false).map_err(rules_error)? {
        return Ok(Vec::new());
    }
    let bytes = read_bytes(file).map_err(rules_error)?.unwrap_or_default();
    let text =
        String::from_utf8(bytes).map_err(|e| RulesError(format!("{}: {e}", file.display())))?;
    let mut rules = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let rule: Rule = serde_json::from_str(line)
            .map_err(|e| RulesError(format!("{}:{}: {e}", file.display(), index + 1)))?;
        rules.push(rule);
    }
    Ok(rules)
}

fn rules_error(e: ConfigError) -> RulesError {
    RulesError(e.to_string())
}
