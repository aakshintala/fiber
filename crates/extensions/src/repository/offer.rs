//! What a person sees before approving: each item that has no approval for
//! its content, with what an install shows and, for a changed item, the diff
//! against the copy approved before (`docs/extensions.md`, "Code a
//! repository ships" and "What an install shows").

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use contract::events::{OfferedItem, OfferedKind};
use serde_json::Value;

use super::content::{Digest, Index, digest_file, hash};
use super::declared::RepoItem;
use super::store::{Decision, Previous, Store};
use crate::Error;
use crate::install::io as io_error;
use crate::manage::carries;

/// An item to offer, with its content hash and what the person sees.
#[derive(Debug)]
pub struct Pending {
    /// The item, to approve.
    pub item: RepoItem,
    /// The offer's entry for it.
    pub offered: OfferedItem,
}

/// The items among `items` with no approval for their content, in order. An
/// item with a never is among them: the person who asks to see it again has
/// asked on purpose. A changed item carries its diff.
pub fn pending(
    store: &Store,
    index: &mut Index,
    items: Vec<RepoItem>,
) -> Result<Vec<Pending>, Error> {
    let mut out = Vec::new();
    for item in items {
        let hash = hash(index, &item)?;
        if store.decision(item.kind, &hash) == Some(Decision::Approve) {
            continue;
        }
        let (summary, version) = summary(&item)?;
        let diff = match store.previous(item.kind, &item.name, &hash) {
            Some(previous) => Some(diff(store, &previous, &item)?).filter(|d| !d.is_empty()),
            None => None,
        };
        out.push(Pending {
            offered: OfferedItem {
                kind: item.kind,
                name: item.name.clone(),
                hash,
                required: item.required,
                summary: summary.join("\n"),
                version,
                diff,
            },
            item,
        });
    }
    Ok(out)
}

/// What an install shows, one line each, and an extension's version.
fn summary(item: &RepoItem) -> Result<(Vec<String>, Option<String>), Error> {
    let mut lines = Vec::new();
    let mut version = None;
    match item.kind {
        OfferedKind::Extension => {
            let manifest = config::read_manifest(&item.dir)?;
            lines.push(format!("name: {}", manifest.name));
            lines.push("from: this repository".to_owned());
            lines.push(format!("version: {}", manifest.version));
            lines.push(format!("path in the repository: {}", item.path));
            lines.push("loads in this project only".to_owned());
            for built_in in &manifest.replaces {
                lines.push(format!("replaces `{built_in}`"));
            }
            for provider in config::read_providers(&item.dir)? {
                let mut urls: Vec<String> =
                    provider.models.iter().map(|m| m.base_url.clone()).collect();
                urls.sort();
                urls.dedup();
                lines.push(format!(
                    "registers provider `{}` at {}",
                    provider.name,
                    urls.iter()
                        .map(|u| format!("`{u}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if let Some(process) = &manifest.process {
                let program = std::iter::once(process.program.as_str())
                    .chain(process.args.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(" ");
                lines.push(format!("runs the program: {program}"));
            }
            if let Some(step) = &manifest.install {
                lines.push(format!(
                    "install step: {}, which runs its dependencies' own install scripts too",
                    step.join(" ")
                ));
            }
            lines.extend(carries(&item.dir, &manifest));
            version = Some(manifest.version);
        }
        OfferedKind::Hook | OfferedKind::McpServer => {
            let entry = item.declaration.as_ref().and_then(|d| d.get("entry"));
            let word = if item.kind == OfferedKind::Hook {
                "hook"
            } else {
                "MCP server"
            };
            lines.push(format!("{word}: {}", item.name));
            lines.push(format!("declared in: {}", item.path));
            if let Some(entry) = entry {
                lines.extend(declaration_lines(entry));
            }
            if item.files.is_empty() {
                lines.push("pins no file of the repository".to_owned());
            } else {
                let rels: Vec<&str> = item.files.iter().map(|f| f.rel.as_str()).collect();
                lines.push(format!("pins: {}", rels.join(", ")));
            }
        }
    }
    for path in &item.outside {
        lines.push(format!("not pinned: {path} (outside the repository)"));
    }
    if item.required {
        lines.push("the repository marks it required".to_owned());
    }
    Ok((lines, version))
}

/// What the declaration says it runs, and when.
fn declaration_lines(entry: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    for key in ["point", "phase"] {
        if let Some(text) = entry.get(key).and_then(Value::as_str) {
            lines.push(format!("{key}: {text}"));
        }
    }
    for key in ["watch", "tools"] {
        if let Some(list) = entry.get(key).and_then(Value::as_array) {
            let names: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
            lines.push(format!("{key}: {}", names.join(", ")));
        }
    }
    if let Some(url) = entry.get("url").and_then(Value::as_str) {
        lines.push(format!("url: {url}"));
    }
    if let Some(command) = entry.get("command").and_then(Value::as_str) {
        let args = entry
            .get("args")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str);
        let words: Vec<&str> = std::iter::once(command).chain(args).collect();
        lines.push(format!("runs: {}", words.join(" ")));
    }
    lines
}

/// One side of a changed file.
enum Side {
    Text(String),
    /// Not UTF-8: its digest.
    Binary(Digest),
}

impl Side {
    fn read(path: &Path, item: &str) -> Result<Self, Error> {
        let fail = |e| Error::Pin {
            item: item.to_owned(),
            why: format!("{}: {e}", path.display()),
        };
        let mut file = File::open(path).map_err(fail)?;
        if let Ok(text) = fs::read_to_string(path) {
            return Ok(Self::Text(text));
        }
        Ok(Self::Binary(digest_file(&mut file).map_err(fail)?))
    }
}

/// The diff of an item against its earlier approved version: for a hook or a
/// server the declaration first, as pretty JSON; then each changed file, in
/// path order.
fn diff(store: &Store, previous: &Previous, item: &RepoItem) -> Result<String, Error> {
    let old_dir = store.copy_dir(&previous.hash);
    let mut out = String::new();
    if let (Some(then), Some(now)) = (&previous.declaration, &item.declaration) {
        let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default() + "\n";
        out.push_str(&text_diff(
            &pretty(then),
            &pretty(now),
            "a/declaration",
            "b/declaration",
        ));
    }
    let mut rels: BTreeSet<&str> = previous.files.iter().map(String::as_str).collect();
    rels.extend(item.files.iter().map(|f| f.rel.as_str()));
    for rel in rels {
        let new = item
            .files
            .iter()
            .find(|f| f.rel == rel)
            .map(|f| f.path.as_path());
        let old_path = old_dir.join(rel);
        let old = (previous.files.iter().any(|f| f == rel) && old_path.is_file())
            .then_some(old_path.as_path());
        out.push_str(&file_diff(&item.name, rel, old, new)?);
    }
    Ok(out)
}

fn file_diff(
    item: &str,
    rel: &str,
    old: Option<&Path>,
    new: Option<&Path>,
) -> Result<String, Error> {
    let (from, to) = (format!("a/{rel}"), format!("b/{rel}"));
    let read = |path: Option<&Path>| path.map(|p| Side::read(p, item)).transpose();
    let (old_side, new_side) = (read(old)?, read(new)?);
    Ok(match (old_side, new_side) {
        (None, None) => String::new(),
        (Some(Side::Text(a)), Some(Side::Text(b))) => {
            let text = text_diff(&a, &b, &from, &to);
            if text.is_empty() {
                mode_line(rel, old, new)?
            } else {
                text
            }
        }
        (None, Some(Side::Text(b))) if b.is_empty() => format!("{rel}: empty file added\n"),
        (Some(Side::Text(a)), None) if a.is_empty() => format!("{rel}: empty file removed\n"),
        (None, Some(Side::Text(b))) => text_diff("", &b, "/dev/null", &to),
        (Some(Side::Text(a)), None) => text_diff(&a, "", &from, "/dev/null"),
        (Some(Side::Binary(a)), Some(Side::Binary(b))) if a == b => mode_line(rel, old, new)?,
        (old_side, new_side) => {
            let kind = "binary file";
            let verb = match (old_side.is_some(), new_side.is_some()) {
                (false, _) => "added",
                (_, false) => "removed",
                (true, true) => "changed",
            };
            format!("{rel}: {kind} {verb}\n")
        }
    })
}

/// A line for a file whose bytes are the same and whose execute bit is not.
fn mode_line(rel: &str, old: Option<&Path>, new: Option<&Path>) -> Result<String, Error> {
    let exec = |path: Option<&Path>| {
        path.map(|p| {
            fs::metadata(p)
                .map(|m| m.mode() & 0o111 != 0)
                .map_err(io_error(p))
        })
        .transpose()
    };
    Ok(match (exec(old)?, exec(new)?) {
        (Some(false), Some(true)) => format!("{rel}: execute bit set\n"),
        (Some(true), Some(false)) => format!("{rel}: execute bit cleared\n"),
        _ => String::new(),
    })
}

fn text_diff(old: &str, new: &str, from: &str, to: &str) -> String {
    similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(from, to)
        .to_string()
}
