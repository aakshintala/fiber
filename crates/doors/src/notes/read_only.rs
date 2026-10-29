//! Read-only command gathering for reviewer setup (`docs/permissions.md`,
//! "Setting up the notes"): reviewer-allow tallies over shell calls,
//! grouped by classifier subject, with entry-scope disqualifiers.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value};

use contract::notes::{AllowsTally, Evidence};
use contract::tool::Tool;

/// One read-only subject's tally inputs: the allows counted here and
/// everywhere, before the threshold and cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOnlySummary {
    /// Allows from this project.
    pub here: u32,
    /// Allows from every project but this one.
    pub there: u32,
}

impl ReadOnlySummary {
    /// All matching allows, here and there.
    pub fn count(&self) -> u32 {
        self.here.saturating_add(self.there)
    }
}

/// What the gather keeps for one read-only subject: a counted tally, a
/// tally withheld with its dim reason, or nothing. The loop withholds
/// ambiguous, one-word, value-bearing and slash-containing subjects
/// itself; the gather only withholds an entry-scope disqualifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOnlyOutcome {
    /// The subject counts: the tally proposes an entry.
    Keep(ReadOnlySummary),
    /// The subject counts but a deny, block or escalation covers it: the
    /// tally stays with its withheld reason, proposing nothing.
    Withheld {
        /// The counted tally.
        summary: ReadOnlySummary,
        /// Why the subject proposes nothing.
        reason: &'static str,
    },
    /// The subject never counts.
    Drop,
}

/// The withheld reason for an entry-scope disqualifier.
const COVERED_REASON: &str = "not proposed: a call it would cover was denied, blocked or escalated";

/// The reviewer-allow tallies for read-only shell subjects: one per exact
/// classifier subject (`docs/permissions.md`, "Setting up the notes":
/// "Subjects are counted by their exact text"), capped like the session
/// counts. A missing or non-shell gate, an unreadable configuration, or
/// no shell calls give nothing.
pub(super) fn allows_evidence(
    home: &Path,
    dir: &Path,
    workspace: &Path,
    shell: Option<&dyn Tool>,
) -> Vec<Evidence> {
    let Some(shell) = shell else {
        return Vec::new();
    };
    if shell.definition().name != "shell" {
        return Vec::new();
    }
    let Some(keys) = configured_sets(home, dir) else {
        return Vec::new();
    };
    let calls = log::reviewer_shell_calls(home);
    let here = worktree::project(workspace);
    let current = super::current_key(dir);
    let mut same: HashMap<String, bool> = HashMap::new();
    // One classification per distinct command string per gather.
    let mut classified: HashMap<String, Option<(String, String)>> = HashMap::new();
    // Reviewer allows group by their classifier subject, so whitespace
    // and redirect-spelling variants of one subject count together; a
    // deny, block or escalation disqualifies by attribution below. A
    // person's allow counts for nothing, and a ran_by allow never counts.
    let mut groups: HashMap<String, (String, Vec<usize>)> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for (index, call) in calls.iter().enumerate() {
        if call.decision.as_str() != "allow"
            || call.decided_by.as_str() != "reviewer"
            || call.ran_by
        {
            continue;
        }
        let Some(raw) = call.raw.as_deref() else {
            continue;
        };
        let Some((subject, prefix)) = subject_prefix(shell, &mut classified, raw) else {
            continue;
        };
        groups
            .entry(subject.clone())
            .or_insert_with_key(|subject| {
                order.push(subject.clone());
                (prefix, Vec::new())
            })
            .1
            .push(index);
    }
    let mut kept: Vec<(String, String, ReadOnlyOutcome)> = Vec::new();
    for subject in order {
        let Some((prefix, indexes)) = groups.get(&subject) else {
            continue;
        };
        let mut here_count: u32 = 0;
        for index in indexes {
            let Some(call) = calls.get(*index) else {
                continue;
            };
            let project = current.as_deref() == Some(call.project.as_str());
            let identity = call.identity.as_deref().is_none_or(|identity| {
                *same
                    .entry(identity.to_owned())
                    .or_insert_with(|| worktree::project(Path::new(identity)) == here)
            });
            if project && identity {
                here_count = here_count.saturating_add(1);
            }
        }
        let Ok(count) = u32::try_from(indexes.len()) else {
            continue;
        };
        let Some(command) = retained_command(&subject) else {
            continue;
        };
        let outcome = outcome_for(
            Candidacy {
                shell,
                classified: &mut classified,
                calls: &calls,
                keys: &keys,
                subject: &subject,
                prefix,
                command: &command,
            },
            ReadOnlySummary {
                here: here_count,
                there: count.saturating_sub(here_count),
            },
            count,
        );
        if !matches!(outcome, ReadOnlyOutcome::Drop) {
            kept.push((subject, prefix.clone(), outcome));
        }
    }
    kept.sort_by(|left, right| {
        right
            .2
            .count()
            .cmp(&left.2.count())
            .then_with(|| left.0.cmp(&right.0))
    });
    kept.truncate(super::SESSION_CAP);
    kept.into_iter()
        .filter_map(|(subject, prefix, outcome)| {
            let (summary, reason) = match outcome {
                ReadOnlyOutcome::Keep(summary) => (summary, None),
                ReadOnlyOutcome::Withheld { summary, reason } => (summary, Some(reason)),
                ReadOnlyOutcome::Drop => return None,
            };
            let tally = AllowsTally::new(prefix, subject, summary.count(), summary.here)?;
            match reason {
                Some(reason) => Some(Evidence::Allows(tally.with_reason(reason))),
                None => Some(Evidence::Allows(tally)),
            }
        })
        .collect()
}

impl ReadOnlyOutcome {
    /// All matching allows the outcome counts, here and there.
    fn count(&self) -> u32 {
        match self {
            Self::Keep(summary) | Self::Withheld { summary, .. } => summary.count(),
            Self::Drop => 0,
        }
    }
}

/// One candidate subject with everything the outcome needs: the wired
/// shell, the per-gather classification cache, the session's shell calls,
/// the configured keys, the exact counted subject with its classifier
/// prefix, and the retained command path the entry checks run against.
struct Candidacy<'a> {
    shell: &'a dyn Tool,
    classified: &'a mut HashMap<String, Option<(String, String)>>,
    calls: &'a [log::ShellCall],
    keys: &'a [String],
    subject: &'a str,
    prefix: &'a str,
    command: &'a str,
}

/// What the gather keeps for one exact subject: the filters drop silently
/// — disallowed redirects, write-shaped flags, built-in commands and
/// already-configured ones — while an entry-scope disqualifier withholds
/// with its reason. Only a built-in refusal drops here: every other typed
/// refusal stays for the loop, which withholds it with its own reason.
/// Subjects the classifier cannot match never reach this: the caller
/// drops them first.
fn outcome_for(candidate: Candidacy, summary: ReadOnlySummary, count: u32) -> ReadOnlyOutcome {
    let Candidacy {
        shell,
        classified,
        calls,
        keys,
        subject,
        prefix,
        command,
    } = candidate;
    if command
        .split_whitespace()
        .filter(|word| word.starts_with('-'))
        .any(|flag| {
            let name = flag.split('=').next().unwrap_or(flag);
            contract::notes::WRITE_SHAPED_FLAGS.contains(&name)
        })
    {
        return ReadOnlyOutcome::Drop;
    }
    if matches!(
        config::validate_read_only_entry(command, &[]),
        Err(config::ReadOnlyEntryProblem::Builtin)
    ) {
        return ReadOnlyOutcome::Drop;
    }
    if overlaps_configured(command, keys) {
        return ReadOnlyOutcome::Drop;
    }
    if count < 5 {
        return ReadOnlyOutcome::Drop;
    }
    if covers_call(shell, classified, calls, subject, prefix, command) {
        return ReadOnlyOutcome::Withheld {
            summary,
            reason: COVERED_REASON,
        };
    }
    ReadOnlyOutcome::Keep(summary)
}

/// The classifier's subject and prefix for `raw`, once per distinct
/// command string: every verdict counts, not only a read-only one. A
/// command the classifier executes owns no rule for it, which is exactly
/// the candidate pool (C19); the built-in and configured checks below keep
/// only commands nothing owns. A typed error or a missing subject closes
/// the subject.
fn subject_prefix(
    shell: &dyn Tool,
    classified: &mut HashMap<String, Option<(String, String)>>,
    raw: &str,
) -> Option<(String, String)> {
    if !classified.contains_key(raw) {
        let mut arguments = Map::new();
        arguments.insert("command".to_owned(), Value::String(raw.to_owned()));
        let verdict = shell
            .effects(&arguments)
            .ok()
            .and_then(|effects| Some((effects.subject?, effects.prefix?)));
        classified.insert(raw.to_owned(), verdict);
    }
    classified.get(raw).cloned().flatten()
}

/// The command the subject keeps: the subject without its allowed
/// redirects, or nothing when a redirect outside the allow-list stays.
fn retained_command(subject: &str) -> Option<String> {
    let mut kept = Vec::new();
    for word in subject.split_whitespace() {
        if contract::notes::ALLOWED_REDIRECT_TOKENS.contains(&word) {
            continue;
        }
        if let Some(destination) = word.strip_prefix("0<").or_else(|| word.strip_prefix('<')) {
            if destination.starts_with("/dev/tcp/") || destination.starts_with("/dev/udp/") {
                return None;
            }
            continue;
        }
        if word.contains('>') || word.contains('<') {
            return None;
        }
        kept.push(word);
    }
    Some(kept.join(" "))
}

/// Whether the deny, block or escalation on `calls` covers the candidate:
/// a judged rule subject covers its own command and narrower ones, never
/// a wider or outside one; a classified call covers the candidate only
/// when its command path equals or extends the candidate prefix with
/// flags the candidate already carries, never when the candidate words
/// merely appear somewhere in its subject; otherwise the raw text
/// withholds when it names the candidate command whole-word, covering
/// wrapped and compound calls and ran_by shell calls that no longer
/// classify.
fn covers_call(
    shell: &dyn Tool,
    classified: &mut HashMap<String, Option<(String, String)>>,
    calls: &[log::ShellCall],
    subject: &str,
    prefix: &str,
    command: &str,
) -> bool {
    let covered = calls
        .iter()
        .filter(|call| call.decision.as_str() == "deny" || call.escalated);
    for call in covered {
        if let Some(rule) = call.rule_subject.as_deref() {
            if command == rule || command.starts_with(&format!("{rule} ")) {
                return true;
            }
            continue;
        }
        let Some(call_raw) = call.raw.as_deref() else {
            continue;
        };
        match subject_prefix(shell, classified, call_raw) {
            Some((denied_subject, denied_prefix)) => {
                if denied_subject == subject
                    || covers_subject(&denied_subject, &denied_prefix, subject, prefix)
                {
                    return true;
                }
            }
            None => {
                // The call no longer classifies: the raw text still
                // withholds when it names the candidate command.
                if raw_contains_command_word(call_raw, command) {
                    return true;
                }
            }
        }
    }
    false
}

/// Whether a classified deny, block or escalation covers the candidate:
/// its command path equals or extends the candidate prefix, and every
/// flag it carries is one the candidate subject carries. An outside
/// command whose subject merely contains the candidate words never
/// covers (`docs/permissions.md`, "Setting up the notes": "An entry
/// always names a subcommand path, and is not proposed when any call it
/// would cover was denied, blocked or escalated").
fn covers_subject(denied_subject: &str, denied_prefix: &str, subject: &str, prefix: &str) -> bool {
    if denied_prefix != prefix && !denied_prefix.starts_with(&format!("{prefix} ")) {
        return false;
    }
    let carried: Vec<&str> = subject
        .split_whitespace()
        .skip(prefix.split_whitespace().count())
        .filter(|word| word.starts_with('-'))
        .collect();
    denied_subject
        .split_whitespace()
        .skip(denied_prefix.split_whitespace().count())
        .filter(|word| word.starts_with('-'))
        .all(|flag| carried.contains(&flag))
}

/// Whether `raw` names the candidate `command`: the command's first word
/// appears delimited by non-name characters, so a longer name merely
/// containing it never counts against the entry. An unparseable deny,
/// block or escalation fails closed against every entry whose command
/// name appears in it (`docs/permissions.md`, "Setting up the notes").
fn raw_contains_command_word(raw: &str, command: &str) -> bool {
    let Some(name) = command.split_whitespace().next() else {
        return false;
    };
    let mut rest = raw;
    while let Some(at) = rest.find(name) {
        let bounded = |edge: Option<char>| {
            edge.is_none_or(|edge| {
                !(edge.is_alphanumeric() || edge == '-' || edge == '_' || edge == '.')
            })
        };
        if bounded(rest[..at].chars().next_back())
            && bounded(rest[at + name.len()..].chars().next())
        {
            return true;
        }
        rest = &rest[at + name.len()..];
    }
    false
}

/// Whether `command` falls under any configured key: equal to one, or
/// extending one or extended by one, word for word.
fn overlaps_configured(command: &str, keys: &[String]) -> bool {
    keys.iter().any(|key| config::overlaps(key, command))
}

/// The union of both layers' configured keys for `dir`'s project, read
/// through `config`: a missing file covers nothing, while a
/// present-but-unreadable file fails the whole gather. Every configured
/// key suppresses, per the ticket's overlap rule.
fn configured_sets(home: &Path, dir: &Path) -> Option<Vec<String>> {
    let key = super::current_key(dir)?;
    let project = config::ProjectKey::new(key).ok()?;
    config::read_only_keys(home, &project).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_count_reports_the_tally_not_a_constant() {
        let summary = ReadOnlySummary { here: 3, there: 2 };
        assert_eq!(ReadOnlyOutcome::Keep(summary.clone()).count(), 5);
        assert_eq!(
            ReadOnlyOutcome::Withheld {
                summary,
                reason: COVERED_REASON,
            }
            .count(),
            5
        );
        assert_eq!(ReadOnlyOutcome::Drop.count(), 0);
    }

    #[test]
    fn retained_command_rejects_each_redirect_delimiter_alone() {
        assert_eq!(retained_command("acme-cli query>out"), None);
        assert_eq!(retained_command("acme-cli query<input"), None);
    }

    #[test]
    fn raw_command_name_is_not_bounded_by_a_dot() {
        assert!(!raw_contains_command_word(
            "wrapper.acme-cli query --json",
            "acme-cli query"
        ));
    }
}
