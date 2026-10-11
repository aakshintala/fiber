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
        let (global, project) = files(&home, &project);
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
            global: lines_of(&self.global)?
                .into_iter()
                .map(|line| line.rule)
                .collect(),
            project: lines_of(&self.project)?
                .into_iter()
                .map(|line| line.rule)
                .collect(),
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
        write::lines::append_line(&self.project, &line).map_err(rules_error)
    }
}

/// Which rules file (`docs/configuration.md`, "Standing rules").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesScope {
    /// `rules` at the top of Fiber home.
    Global,
    /// `projects/<key>/rules` in Fiber home.
    Project,
}

/// One line of a rules file: its physical number from 1, its text as the
/// file holds it, and the rule it parses to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleLine {
    /// The line's physical number, counted from 1 over every line.
    pub line: usize,
    /// The line's text, without its line ending.
    pub text: String,
    /// The rule the line parses to.
    pub rule: Rule,
}

/// One rules file: its path, and its lines or why it cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulesListing {
    /// The file listed.
    pub file: PathBuf,
    /// The file's lines, in order; a missing file holds none.
    pub lines: Result<Vec<RuleLine>, RulesError>,
}

/// The two files' paths for `project` in `home`: the global file, then
/// the project's.
fn files(home: &Path, project: &ProjectKey) -> (PathBuf, PathBuf) {
    (
        home.join("rules"),
        home.join("projects").join(project.as_str()).join("rules"),
    )
}

/// Both rules files in `home` for `project`, read now: the global file,
/// then the project's. Each file's lines keep their physical numbers,
/// so `/rules` revokes the line it shows.
/// (`docs/configuration.md`, "Standing rules").
pub fn list_rules(home: &Path, project: &ProjectKey) -> (RulesListing, RulesListing) {
    let (global, project_file) = files(home, project);
    (
        RulesListing {
            file: global.clone(),
            lines: lines_of(&global),
        },
        RulesListing {
            file: project_file.clone(),
            lines: lines_of(&project_file),
        },
    )
}

/// Deletes line `line` of `scope`'s file when it still reads `text`:
/// `true` when it removed. A line that moved, changed or is past the
/// end, and a missing file, remove nothing and answer `false`.
/// (`docs/tui.md`, "Swapped views").
pub fn remove_rule(
    home: &Path,
    project: &ProjectKey,
    scope: RulesScope,
    line: usize,
    text: &str,
) -> Result<bool, ConfigError> {
    let (global, project_file) = files(home, project);
    let file = match scope {
        RulesScope::Global => &global,
        RulesScope::Project => &project_file,
    };
    write::lines::remove_line(file, line, text)
}

/// One rules file's lines, in order, each with its physical number and
/// text. Blank lines are skipped but still numbered, so the numbers
/// match the lines `remove_line` deletes. A missing file holds no
/// rules; a symlink or non-regular file, an unreadable file, or a line
/// that does not parse is an error naming the file (and the line).
fn lines_of(file: &Path) -> Result<Vec<RuleLine>, RulesError> {
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
        rules.push(RuleLine {
            line: index + 1,
            text: line.to_owned(),
            rule,
        });
    }
    Ok(rules)
}

fn rules_error(e: ConfigError) -> RulesError {
    RulesError(e.to_string())
}
