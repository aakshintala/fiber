//! A repository's hooks whose current content is approved: what runs is
//! the pinned copy in Fiber home, never the repository's files
//! (`docs/extensions.md`, "Pinning"). Gathering hashes only the paths the
//! declarations name, never the rest of the tree.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use config::ProjectKey;
use contract::clock::Clock;
use contract::events::OfferedKind;
use serde_json::Value;

use super::content::{Index, hash};
use super::declared::{Named, RepoItem, declared_items};
use super::store::{CopyHold, Decision, Store};
use crate::Error;
use crate::install::io as io_error;

/// A hook whose current content is approved: how to run the copy.
#[derive(Debug, Clone)]
#[allow(
    dead_code,
    reason = "the session keeps the approved mapping for its life"
)]
pub(crate) struct ApprovedHook {
    /// The hook's name.
    pub name: String,
    /// The approved content's hash: the copy's directory name.
    pub hash: String,
    /// Whether the repository marks it `required`: repository-owned
    /// metadata, never a merged person boolean.
    pub required: bool,
    /// The declared entry with its `command` and `args` mapped to the
    /// pinned copy: Inside to the pinned file, Outside and unmapped to
    /// the original string.
    pub entry: Value,
    /// The copy's directory.
    pub copy: PathBuf,
    /// The frozen resolutions gathered with the declaration.
    pub named: BTreeMap<String, Named>,
}

impl ApprovedHook {
    /// Maps one string to what runs: Inside to its pinned file, Outside and
    /// unmapped to the original string. No file system access.
    #[allow(
        dead_code,
        reason = "the session keeps the approved mapping for its life"
    )]
    pub(crate) fn mapped(&self, given: &str) -> String {
        map_string_for(&self.named, &self.copy, given)
    }
}

fn map_string_for(named: &BTreeMap<String, Named>, copy: &Path, given: &str) -> String {
    match named.get(given) {
        Some(Named::Inside(rel)) => copy.join(rel).display().to_string(),
        Some(Named::Outside(_)) => given.to_owned(),
        None => given.to_owned(),
    }
}

/// A hook whose approved copy could not be made.
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "the session gate reads the failed hook's name and required flag"
)]
pub(crate) struct HookFailure {
    /// The hook's name.
    pub name: String,
    /// The approved content's hash that could not be materialized.
    pub hash: String,
    /// Whether the repository marks it `required`.
    pub required: bool,
    /// What making the copy failed with.
    pub error: Error,
}

/// Whether one gathered hook may run in this session.
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "the session snapshot reads approved and unapproved outcomes"
)]
pub(crate) enum HookState {
    /// Its current content is approved, its copy ready.
    Approved(ApprovedHook),
    /// Its current content has no approval: its hash and the repository's
    /// `required`.
    Unapproved { hash: String, required: bool },
    /// This session skipped this content: never started in it, even when
    /// approved meanwhile.
    Skipped,
}

/// The hooks a load allows and the holds that keep their copies.
type HookLoaded = (
    BTreeMap<String, Result<HookState, HookFailure>>,
    Vec<CopyHold>,
);

/// Every hook the workspace's repository declares, with what its current
/// content allows and the holds that keep approved copies for the session's
/// life. A copy that cannot be made is that hook's failure only. A
/// repository with no hooks touches nothing in Fiber home.
pub(crate) fn repository_hooks(
    home: &Path,
    project: &ProjectKey,
    workspace: &Path,
    clock: Arc<dyn Clock>,
    skipped: &BTreeSet<(String, String)>,
) -> Result<HookLoaded, Error> {
    let items = declared_items(workspace, clock.as_ref())?;
    let hooks: Vec<&RepoItem> = items
        .iter()
        .filter(|item| item.kind == OfferedKind::Hook)
        .collect();
    if hooks.is_empty() {
        return Ok((BTreeMap::new(), Vec::new()));
    }
    let _root = workspace.canonicalize().map_err(io_error(workspace))?;
    let store = Store::new(home, project, clock);
    let mut index = Index::load(home);
    let mut out = BTreeMap::new();
    let mut holds = Vec::new();
    for item in hooks {
        let current = hash(&mut index, item)?;
        if skipped.contains(&(item.name.clone(), current.clone())) {
            out.insert(item.name.clone(), Ok(HookState::Skipped));
            continue;
        }
        let approved = matches!(
            store.decision(OfferedKind::Hook, &current),
            Some(Decision::Approve)
        );
        if !approved {
            out.insert(
                item.name.clone(),
                Ok(HookState::Unapproved {
                    hash: current,
                    required: item.required,
                }),
            );
            continue;
        }
        match store.ensure_copy(item, &current) {
            Ok(hold) => {
                let copy = store.copy_dir(&current);
                let entry = map_entry(item, &copy);
                holds.push(hold);
                out.insert(
                    item.name.clone(),
                    Ok(HookState::Approved(ApprovedHook {
                        name: item.name.clone(),
                        hash: current,
                        required: item.required,
                        entry,
                        copy,
                        named: item.named.clone(),
                    })),
                );
            }
            Err(error) => {
                out.insert(
                    item.name.clone(),
                    Err(HookFailure {
                        name: item.name.clone(),
                        hash: current,
                        required: item.required,
                        error,
                    }),
                );
            }
        }
    }
    index.save().unwrap_or(());
    Ok((out, holds))
}

/// The declared entry with its `command` and `args` mapped to the copy:
/// Inside to the pinned file, Outside and unmapped to the original string.
/// The mapping uses the frozen resolutions gathered with the declaration,
/// never a fresh resolution.
pub(crate) fn map_entry(item: &RepoItem, copy: &Path) -> Value {
    let Some(declaration) = item.declaration.as_ref() else {
        return Value::Null;
    };
    let Some(entry) = declaration.get("entry").cloned() else {
        return Value::Null;
    };
    let Value::Object(mut map) = entry else {
        return entry;
    };
    if let Some(command) = map
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_owned)
    {
        map.insert(
            "command".to_owned(),
            Value::String(map_string(item, copy, &command)),
        );
    }
    if let Some(args) = map.get("args").and_then(Value::as_array).cloned() {
        let mapped: Vec<Value> = args
            .into_iter()
            .map(|value| match value.as_str() {
                Some(text) => Value::String(map_string(item, copy, text)),
                None => value,
            })
            .collect();
        map.insert("args".to_owned(), Value::Array(mapped));
    }
    Value::Object(map)
}

/// One string to what runs: Inside to its pinned file, Outside and unmapped
/// to the original string, preserving absolute symlink spellings.
pub(crate) fn map_string(item: &RepoItem, copy: &Path, given: &str) -> String {
    match item.named.get(given) {
        Some(Named::Inside(rel)) => copy.join(rel).display().to_string(),
        Some(Named::Outside(_)) => given.to_owned(),
        None => given.to_owned(),
    }
}
