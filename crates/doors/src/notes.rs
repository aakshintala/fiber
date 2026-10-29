//! A driver `propose_notes` (`docs/invocation.md`, "What each command
//! does"): gathers evidence on a thread of its own and delivers it to the
//! loop's inbox, so this reader reads on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;

use serde_json::{Map, Value};

use contract::inbox::{Ack, Delivery};
use contract::notes::{
    AllowsTally, Evidence, NotesAsk, ProposeNotesArgs, SessionTally, Topic, strip_userinfo,
};
use contract::tool::Tool;
use contract::{CommandId, ErrorCode};

use crate::client::Conn;

/// The command, for the answer when no workspace is wired.
const COMMAND: &str = "propose_notes";

/// At most one session item per prefix.
const SESSION_CAP: usize = 64;

/// Files item texts cut here.
const TEXT_CUT: usize = 4096;

/// At most this many CI configuration files.
const CI_CAP: usize = 8;

/// Takes `propose_notes`: reads the wired workspace, answering
/// `unknown_command` when none is wired, then gathers on a thread of its
/// own and delivers to the loop's inbox. A thread that cannot start
/// rejects `io_failed`; a send after the thread died answers `closing`
/// through the dropped acknowledgement.
pub(crate) fn run(conn: &mut Conn, id: CommandId, args: ProposeNotesArgs) {
    let Some(workspace) = conn.gate.notes_workspace() else {
        crate::client::reject(
            conn,
            Some(id),
            ErrorCode::UnknownCommand,
            &crate::client::not_built(COMMAND),
        );
        return;
    };
    let gate = Arc::clone(&conn.gate);
    let home = gate.home.clone();
    let dir = gate.dir.clone();
    let shell = gate.driver_shell();
    let (tx, rx) = mpsc::channel::<(ProposeNotesArgs, Ack)>();
    let spawned = thread::Builder::new()
        .name("notes".to_owned())
        .spawn(move || {
            if let Ok((args, ack)) = rx.recv() {
                let shell = shell.as_deref();
                let evidence = gather(&home, &dir, &workspace, args.topic, shell);
                gate.deliver(Delivery::ProposeNotes(NotesAsk { args, evidence }, ack));
            }
        });
    if spawned.is_err() {
        crate::client::reject(
            conn,
            Some(id),
            ErrorCode::IoFailed,
            crate::shell::COULD_NOT_START,
        );
        return;
    }
    let ack = crate::client::inbox_ack(conn, id);
    // The thread waits in `recv`, so the send fails only if it died first:
    // the dropped acknowledgement answers `closing`.
    if let Err(mpsc::SendError((_args, ack))) = tx.send((args, ack)) {
        drop(ack);
    }
}

/// Gathers `propose_notes` evidence for one topic: the project's files,
/// then the session counts, then — only for the read-only topic with a
/// shell wired — the reviewer-allow tallies for read-only shell subjects.
/// The loop appends the person's answer last. Every gather reads afresh;
/// nothing is cached across topics. It never writes, never locks and
/// never fails: an unreadable log or workspace gives what was read before
/// the failure, and a missing shell, a non-shell gate or an unreadable
/// configuration gives no allow tallies.
pub(crate) fn gather(
    home: &Path,
    dir: &Path,
    workspace: &Path,
    topic: Topic,
    shell: Option<&dyn Tool>,
) -> Vec<Evidence> {
    let mut evidence = files_evidence(workspace);
    evidence.extend(session_evidence(home, dir, workspace));
    if topic == Topic::ReadOnlyCommands {
        evidence.extend(allows_evidence(home, dir, workspace, shell));
    }
    evidence
}

/// The project's files evidence: its git remotes, its CI configuration
/// and its package registries, in that order (`docs/permissions.md`,
/// "Setting up the notes"). Every item is `strip_userinfo` of what was
/// read, cut to [`TEXT_CUT`] bytes at a character boundary. Outside a
/// repository the remotes are skipped and the rest still shows.
pub(crate) fn files_evidence(workspace: &Path) -> Vec<Evidence> {
    let root = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let mut texts = remote_texts(workspace);
    texts.extend(ci_texts(workspace, &root));
    texts.extend(registry_texts(workspace, &root));
    texts
        .into_iter()
        .map(|text| Evidence::Files { text })
        .collect()
}

/// One item per remote's fetch URL, sorted by remote name. A workspace
/// outside a repository, a `git` that cannot start, a non-zero exit and
/// unreadable output all give no item and no error.
fn remote_texts(workspace: &Path) -> Vec<String> {
    if !worktree::resolve_project(workspace).in_repository {
        return Vec::new();
    }
    let mut remotes = remote_urls(workspace);
    remotes.sort();
    remotes
        .into_iter()
        .map(|(name, url)| {
            cut_text(&format!("git remote {name}: {}", strip_userinfo(&url))).to_owned()
        })
        .collect()
}

fn remote_urls(workspace: &Path) -> Vec<(String, String)> {
    let output = support::git::command("git")
        .arg("-C")
        .arg(workspace)
        .args(["remote", "-v"])
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(text) = String::from_utf8(output.stdout) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let name = parts.next()?;
            let url = parts.next()?;
            if parts.next() != Some("(fetch)") {
                return None;
            }
            Some((name.to_owned(), url.to_owned()))
        })
        .collect()
}

/// The CI configuration files under the workspace, sorted by path, at
/// most [`CI_CAP`]: each item `CI configuration <path>:` then its text.
fn ci_texts(workspace: &Path, root: &Path) -> Vec<String> {
    let mut paths: Vec<PathBuf> = [
        ".gitlab-ci.yml",
        ".circleci/config.yml",
        "azure-pipelines.yml",
        ".buildkite/pipeline.yml",
        "Jenkinsfile",
    ]
    .into_iter()
    .map(|rel| workspace.join(rel))
    .collect();
    if let Ok(entries) = std::fs::read_dir(workspace.join(".github/workflows")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "yml" || extension == "yaml")
            {
                paths.push(path);
            }
        }
    }
    let mut read: Vec<(PathBuf, String)> = paths
        .iter()
        .filter_map(|path| read_confined(root, path).map(|text| (path.clone(), text)))
        .collect();
    read.sort();
    read.truncate(CI_CAP);
    read.into_iter()
        .filter_map(|(path, text)| {
            let rel = path.strip_prefix(workspace).ok()?;
            Some(
                cut_text(&format!(
                    "CI configuration {}:\n{}",
                    rel.display(),
                    strip_userinfo(&text)
                ))
                .to_owned(),
            )
        })
        .collect()
}

/// Only the registry lines of the registry files under the workspace:
/// never another line, so token lines stay out.
fn registry_texts(workspace: &Path, root: &Path) -> Vec<String> {
    let mut texts = Vec::new();
    push_registries(&mut texts, workspace, root, ".npmrc", npmrc_lines);
    push_registries(&mut texts, workspace, root, ".yarnrc.yml", yarn_lines);
    push_registries(
        &mut texts,
        workspace,
        root,
        ".cargo/config.toml",
        cargo_lines,
    );
    push_registries(&mut texts, workspace, root, "pip.conf", pip_lines);
    texts
}

fn push_registries(
    texts: &mut Vec<String>,
    workspace: &Path,
    root: &Path,
    rel: &str,
    lines: fn(&str) -> Vec<String>,
) {
    let path = workspace.join(rel);
    let Some(content) = read_confined(root, &path) else {
        return;
    };
    texts.extend(
        lines(&content)
            .into_iter()
            .map(|line| cut_text(&format!("registry {rel}: {}", strip_userinfo(&line))).to_owned()),
    );
}

/// The `registry=` and `@scope:registry=` values of an `.npmrc`.
// exempted: the skip-guard `||`-to-`&&` mutants are equivalent: an empty
// line never splits on `=`, and a `#`/`;` line's key keeps its marker, so it
// can never equal `registry` or match `@scope:registry`. The `@`-scope
// `&&` stays pinned by the scope test below, without mutant enforcement.
#[cfg_attr(false, mutants::skip)]
fn npmrc_lines(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            let value = value.trim();
            if value.is_empty() {
                return None;
            }
            if key == "registry" || (key.starts_with('@') && key.ends_with(":registry")) {
                Some(value.to_owned())
            } else {
                None
            }
        })
        .collect()
}

/// The `npmRegistryServer:` value of a `.yarnrc.yml`.
// exempted: the skip-guard `||`-to-`&&` mutant is equivalent: an empty line
// never splits on `:`, and a `#` line's key keeps its `#`, so it can never
// equal `npmRegistryServer`. The value shape stays pinned by the tests below.
#[cfg_attr(false, mutants::skip)]
fn yarn_lines(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once(':')?;
            if key.trim() != "npmRegistryServer" {
                return None;
            }
            let value = value.trim().trim_matches('"').trim_matches('\'').trim();
            if value.is_empty() {
                return None;
            }
            Some(value.to_owned())
        })
        .collect()
}

/// The `index =` and `registry =` values under `[registries.*]` and
/// `[registry]` of a cargo config.
fn cargo_lines(content: &str) -> Vec<String> {
    let mut kept = Vec::new();
    let mut in_registries = false;
    for line in content.lines() {
        let line = line.trim();
        // Empty lines never split on `=` and `#` lines never match a section
        // or a key, so both fall through with no effect and need no guard.
        if line.starts_with('[') {
            let section = line
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .trim_matches('"');
            in_registries = section == "registry" || section.starts_with("registries.");
            continue;
        }
        if !in_registries {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key != "index" && key != "registry" {
            continue;
        }
        let value = value.trim().trim_matches('"').trim_matches('\'').trim();
        if !value.is_empty() {
            kept.push(value.to_owned());
        }
    }
    kept
}

/// The `index-url` and `extra-index-url` values of a `pip.conf`.
// exempted: the three skip-guard `||`-to-`&&` mutants are equivalent: an
// empty line never splits on `=`, and a `#`/`;`/`[` line's key keeps its
// marker, so it can never equal `index-url` or `extra-index-url`. The kept
// values stay pinned by the tests below.
#[cfg_attr(false, mutants::skip)]
fn pip_lines(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty()
                || line.starts_with('#')
                || line.starts_with(';')
                || line.starts_with('[')
            {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            if key != "index-url" && key != "extra-index-url" {
                return None;
            }
            let value = value.trim();
            if value.is_empty() {
                return None;
            }
            Some(value.to_owned())
        })
        .collect()
}

/// The file's text when its canonical path is a regular file under the
/// canonical workspace. A symlink resolving outside, a directory, a FIFO
/// and anything else give nothing, and nothing is opened before the
/// check, so a FIFO is never blocked on.
fn read_confined(root: &Path, path: &Path) -> Option<String> {
    let canonical = std::fs::canonicalize(path).ok()?;
    if !canonical.starts_with(root) {
        return None;
    }
    if !std::fs::metadata(&canonical).is_ok_and(|meta| meta.is_file()) {
        return None;
    }
    std::fs::read(&canonical)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

/// The text cut to [`TEXT_CUT`] bytes at a character boundary: the longest
/// boundary-terminated prefix, found by walking down with an iterator so a
/// mutated step still terminates (docs/testing/mutants.md, "Code is written
/// so a mutant fails fast").
fn cut_text(text: &str) -> &str {
    let end = (0..=TEXT_CUT.min(text.len()))
        .rev()
        .find(|end| text.is_char_boundary(*end))
        .unwrap_or(0);
    &text[..end]
}

/// One item per counted prefix: `<count> escalations answered allow for
/// `<prefix>``, sorted by count descending, then prefix ascending, at most
/// [`SESSION_CAP`]. `here` counts the escalations whose recorded identity
/// resolves to this session's project; each distinct identity resolves
/// once. An escalation from another `projects/<key>` directory is another
/// project's without running `git`; one with no identity, or whose
/// identity no longer resolves, counts there too.
fn session_evidence(home: &Path, dir: &Path, workspace: &Path) -> Vec<Evidence> {
    let mut tallies: HashMap<String, (usize, usize)> = HashMap::new();
    let here = worktree::project(workspace);
    let current = current_key(dir);
    let mut same: HashMap<String, bool> = HashMap::new();
    for escalation in log::allowed_escalations(home) {
        let tally = tallies.entry(escalation.prefix.clone()).or_insert((0, 0));
        tally.0 += 1;
        let identity = escalation.identity.as_deref();
        let is_here = identity.is_some_and(|identity| {
            current.as_deref() == Some(escalation.project.as_str())
                && *same
                    .entry(identity.to_owned())
                    .or_insert_with(|| worktree::project(Path::new(identity)) == here)
        });
        if is_here {
            tally.1 += 1;
        }
    }
    let mut prefixes: Vec<(String, usize, usize)> = tallies
        .into_iter()
        .map(|(prefix, (count, here))| (prefix, count, here))
        .collect();
    prefixes.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    prefixes.truncate(SESSION_CAP);
    prefixes
        .into_iter()
        .filter_map(|(prefix, count, here)| {
            let count = u32::try_from(count).ok()?;
            let here = u32::try_from(here).ok()?;
            SessionTally::new(prefix, count, here)
        })
        .map(Evidence::Session)
        .collect()
}

/// The `projects/<key>` directory name holding the session in `dir`:
/// `None` when `dir` is not laid out as one.
fn current_key(dir: &Path) -> Option<String> {
    dir.parent()?
        .parent()?
        .file_name()
        .map(|key| key.to_string_lossy().into_owned())
}

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
/// command, capped like the session counts. A missing or non-shell gate,
/// an unreadable configuration, or no shell calls give nothing.
fn allows_evidence(
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
    let current = current_key(dir);
    let mut same: HashMap<String, bool> = HashMap::new();
    // One classification per distinct command string per gather.
    let mut classified: HashMap<String, Option<(String, String)>> = HashMap::new();
    // Reviewer allows group by their exact command; a deny, block or
    // escalation disqualifies by attribution below. A person's allow
    // counts for nothing, and a ran_by allow never counts.
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
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
        if !groups.contains_key(raw) {
            order.push(raw.to_owned());
        }
        groups.entry(raw.to_owned()).or_default().push(index);
    }
    let mut kept: Vec<(String, String, ReadOnlyOutcome)> = Vec::new();
    for raw in order {
        let Some(indexes) = groups.get(&raw) else {
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
        let Some((subject, prefix)) = subject_prefix(shell, &mut classified, &raw) else {
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
                raw: &raw,
                command: &command,
            },
            ReadOnlySummary {
                here: here_count,
                there: count.saturating_sub(here_count),
            },
            count,
        );
        if !matches!(outcome, ReadOnlyOutcome::Drop) {
            kept.push((command, prefix, outcome));
        }
    }
    kept.sort_by(|left, right| {
        right
            .2
            .count()
            .cmp(&left.2.count())
            .then_with(|| left.0.cmp(&right.0))
    });
    kept.truncate(SESSION_CAP);
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

/// One candidate command with everything the outcome needs: the wired
/// shell, the per-gather classification cache, the session's shell calls,
/// the configured keys, the exact logged command and its retained
/// command path.
struct Candidacy<'a> {
    shell: &'a dyn Tool,
    classified: &'a mut HashMap<String, Option<(String, String)>>,
    calls: &'a [log::ShellCall],
    keys: &'a [String],
    raw: &'a str,
    command: &'a str,
}

/// What the gather keeps for one exact command: the filters drop silently
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
        raw,
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
    if covers_call(shell, classified, calls, raw, command) {
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

/// Whether the deny, block or escalation on `calls` covers the candidate
/// `command`: a judged rule subject covers its own command and narrower
/// ones, never a wider or outside one; otherwise the command-only
/// classification of each covering call's own words, whole-word, or the
/// raw text when the call no longer classifies.
fn covers_call(
    shell: &dyn Tool,
    classified: &mut HashMap<String, Option<(String, String)>>,
    calls: &[log::ShellCall],
    raw: &str,
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
            Some((subject, _)) => {
                let denied: Vec<&str> = subject.split_whitespace().collect();
                let full: Vec<&str> = raw.split_whitespace().collect();
                if denied == full || full.iter().all(|word| denied.contains(word)) {
                    return true;
                }
            }
            None => {
                // The call no longer classifies: the raw text still
                // withholds when it holds the whole candidate command.
                if call_raw.contains(command) {
                    return true;
                }
            }
        }
    }
    false
}

/// Whether two command paths overlap: they are equal, or one extends the
/// other word for word.
fn overlaps(first: &str, second: &str) -> bool {
    first == second
        || first.starts_with(&format!("{second} "))
        || second.starts_with(&format!("{first} "))
}

/// Whether `command` falls under any configured key: equal to one, or
/// extending one or extended by one, word for word.
fn overlaps_configured(command: &str, keys: &[String]) -> bool {
    keys.iter().any(|key| overlaps(key, command))
}

/// The union of both layers' configured keys for `dir`'s project: a
/// missing file covers nothing, while a present-but-unreadable file fails
/// the whole gather. Every configured key suppresses, per the ticket's
/// overlap rule.
fn configured_sets(home: &Path, dir: &Path) -> Option<Vec<String>> {
    let key = current_key(dir)?;
    let project = config::ProjectKey::new(key).ok()?;
    let mut keys = Vec::new();
    for file in [
        config::global_config_file(home),
        config::project_config_file(home, &project),
    ] {
        let bytes = match std::fs::read(&file) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        keys.extend(configured_keys(&bytes)?);
    }
    Some(keys)
}

/// The configured command paths in `bytes`, read as the nested objects
/// dotted paths name: `None` for anything but an object root with an
/// object `shell`, holding an object `read_only` of entry objects with
/// string-array flags. A missing `shell` or `read_only` covers nothing,
/// like the save's own key read. Every configured key suppresses,
/// whether or not the shared validator would refuse it.
fn configured_keys(bytes: &[u8]) -> Option<Vec<String>> {
    let root: Value = serde_json::from_slice(bytes).ok()?;
    let Some(shell) = root.as_object()?.get("shell") else {
        return Some(Vec::new());
    };
    let Some(entries) = shell.as_object()?.get("read_only") else {
        return Some(Vec::new());
    };
    let entries = entries.as_object()?;
    let mut keys = Vec::new();
    for (command, value) in entries {
        let flags = value.get("flags")?.as_array()?;
        for flag in flags {
            flag.as_str()?;
        }
        keys.push(command.to_owned());
    }
    Some(keys)
}

#[cfg(test)]
#[path = "notes_tests.rs"]
mod tests;
