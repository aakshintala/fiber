//! A driver `propose_notes` (`docs/invocation.md`, "What each command
//! does"): gathers evidence on a thread of its own and delivers it to the
//! loop's inbox, so this reader reads on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;

use contract::inbox::{Ack, Delivery};
use contract::notes::{Evidence, NotesAsk, ProposeNotesArgs, SessionTally, strip_userinfo};
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
    let (tx, rx) = mpsc::channel::<(ProposeNotesArgs, Ack)>();
    let spawned = thread::Builder::new()
        .name("notes".to_owned())
        .spawn(move || {
            if let Ok((args, ack)) = rx.recv() {
                let evidence = gather(&home, &dir, &workspace);
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
/// then the session counts. The loop appends the person's answer last.
/// Every gather reads afresh; nothing is cached across topics.
pub(crate) fn gather(home: &Path, dir: &Path, workspace: &Path) -> Vec<Evidence> {
    let mut evidence = files_evidence(workspace);
    evidence.extend(session_evidence(home, dir, workspace));
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

#[cfg(test)]
#[path = "notes_tests.rs"]
mod tests;
