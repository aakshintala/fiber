//! A repository's servers whose current content is approved: what runs is
//! the pinned copy in Fiber home, never the repository's files
//! (`docs/mcp.md`, "A repository's servers"; `docs/extensions.md`,
//! "Pinning"). Gathering hashes only the paths the declarations name,
//! never the rest of the tree.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use config::ProjectKey;
use contract::clock::Clock;
use contract::events::OfferedKind;
use serde_json::Value;

use super::content::{Index, hash};
use super::declared::{Named, declared_servers};
use super::store::{CopyHold, Store};
use crate::Error;
use crate::install::io as io_error;

/// Whether one gathered server may start in this session.
#[derive(Debug)]
pub enum ServerApproval {
    /// Its current content is approved, its copy ready and held.
    Approved(ApprovedServer),
    /// Its current content has no approval: its hash and the repository's
    /// `required`.
    Unapproved {
        /// The current content's hash.
        hash: String,
        /// Whether the repository marks it `required`.
        required: bool,
    },
    /// This session skipped this content: never started in it, even when
    /// approved meanwhile.
    Skipped,
}

/// A server whose current content is approved: how to run the copy.
/// `entry` holds the declared command, arguments and environment, so it
/// never prints (`docs/code-quality.md`, "What a reviewer checks").
pub struct ApprovedServer {
    /// The server's name.
    pub name: String,
    /// The repository's checked entry as the approval hashed it.
    pub entry: Value,
    /// The approved content's hash: the copy's directory name.
    pub hash: String,
    copy: PathBuf,
    root: PathBuf,
    named: BTreeMap<String, Named>,
    hold: Arc<CopyHold>,
}

// The checked entry holds the declaration's command, arguments and
// environment values, so only the server's name and hash print
// (`docs/code-quality.md`, "What a reviewer checks").
impl std::fmt::Debug for ApprovedServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovedServer")
            .field("name", &self.name)
            .field("entry", &"redacted")
            .field("hash", &self.hash)
            .field("copy", &self.copy)
            .field("root", &self.root)
            .field("named", &self.named.len())
            .field("hold", &"<guard>")
            .finish()
    }
}

impl ApprovedServer {
    /// Maps one string the merged configuration gives to the program that
    /// runs: a string the declaration named that resolved inside the
    /// repository becomes its copy; one that resolved outside becomes the
    /// target recorded at gathering; any other string stays as written. No
    /// file system access: retargeting a link after gathering cannot change
    /// what runs.
    pub fn arg(&self, given: &str) -> String {
        match self.named.get(given) {
            Some(Named::Inside(rel)) => self.copy.join(rel).display().to_string(),
            Some(Named::Outside(target)) => target.display().to_string(),
            None => given.to_owned(),
        }
    }

    /// The guard for one spawn: the strings of `given` the frozen
    /// resolutions do not cover, re-resolved on every check. It keeps the
    /// copy held for as long as the spec that owns it lives.
    pub fn guard(&self, given: &[String]) -> SpawnGuard {
        SpawnGuard {
            name: self.name.clone(),
            root: self.root.clone(),
            open: given
                .iter()
                .filter(|s| !self.named.contains_key(s.as_str()))
                .cloned()
                .collect(),
            hold: Arc::clone(&self.hold),
        }
    }
}

/// What one spawn re-checks: every string no frozen resolution covers must
/// still name no regular file inside the repository.
#[derive(Clone)]
pub struct SpawnGuard {
    name: String,
    root: PathBuf,
    open: Vec<String>,
    // Never read: dropping it releases the copy's shared lock, which is
    // the whole point of keeping it for the spec's life.
    #[allow(dead_code, reason = "the lock releases when the guard drops")]
    hold: Arc<CopyHold>,
}

// An argument can hold a token, so only its count prints
// (`docs/code-quality.md`, "Errors").
impl std::fmt::Debug for SpawnGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnGuard")
            .field("name", &self.name)
            .field("root", &self.root)
            .field("open", &self.open.len())
            .field("hold", &"<guard>")
            .finish()
    }
}

impl SpawnGuard {
    /// Re-resolves every open string, symbolic links followed, and refuses
    /// one that is now a regular file inside the repository: the approval
    /// does not cover it. Caches nothing; never reads a file's bytes.
    pub fn check(&self) -> Result<(), Error> {
        for given in &self.open {
            let candidate = self.root.join(given);
            match fs::metadata(&candidate) {
                Ok(meta) if meta.is_file() => {}
                Ok(_) => continue,
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::NotFound
                            | ErrorKind::NotADirectory
                            | ErrorKind::InvalidInput
                            | ErrorKind::InvalidFilename
                    ) =>
                {
                    continue;
                }
                Err(e) => {
                    return Err(Error::Pin {
                        item: self.name.clone(),
                        why: format!("{}: {e}", candidate.display()),
                    });
                }
            }
            let resolved = candidate.canonicalize().map_err(|e| Error::Pin {
                item: self.name.clone(),
                why: format!("{}: {e}", candidate.display()),
            })?;
            if resolved.starts_with(&self.root) {
                return Err(Error::Pin {
                    item: self.name.clone(),
                    why: format!(
                        "it names `{given}`, a repository file this approval does not cover. \
                         Run `fiber approve` in the repository."
                    ),
                });
            }
        }
        Ok(())
    }
}

/// Every server the workspace's repository declares, with what its current
/// content allows: approved, unapproved, or skipped for this session.
/// `skipped` holds the `(name, content hash)` of each `mcp_server` content
/// the session skipped, decided before any approval read or copy, so a
/// copy error never defeats a skip. Failing to gather is the outer error;
/// a copy that cannot be made is that server's error only. A repository
/// with no servers touches nothing in Fiber home.
pub fn repository_servers(
    home: &Path,
    project: &ProjectKey,
    workspace: &Path,
    clock: Arc<dyn Clock>,
    skipped: &BTreeSet<(String, String)>,
) -> Result<BTreeMap<String, Result<ServerApproval, Error>>, Error> {
    let items = declared_servers(workspace)?;
    if items.is_empty() {
        return Ok(BTreeMap::new());
    }
    let root = workspace.canonicalize().map_err(io_error(workspace))?;
    let store = Store::new(home, project, clock);
    let mut index = Index::load(home);
    let mut out = BTreeMap::new();
    for item in &items {
        debug_assert_eq!(item.kind, OfferedKind::McpServer);
        let current = hash(&mut index, item)?;
        if skipped.contains(&(item.name.clone(), current.clone())) {
            out.insert(item.name.clone(), Ok(ServerApproval::Skipped));
            continue;
        }
        let approved = matches!(
            store.decision(OfferedKind::McpServer, &current),
            Some(super::store::Decision::Approve)
        );
        if !approved {
            out.insert(
                item.name.clone(),
                Ok(ServerApproval::Unapproved {
                    hash: current,
                    required: item.required,
                }),
            );
            continue;
        }
        match store.ensure_copy(item, &current) {
            Ok(hold) => {
                out.insert(
                    item.name.clone(),
                    Ok(ServerApproval::Approved(ApprovedServer {
                        name: item.name.clone(),
                        entry: item.declaration.clone().unwrap_or(Value::Null),
                        hash: current.clone(),
                        copy: store.copy_dir(&current),
                        root: root.clone(),
                        named: item.named.clone(),
                        hold: Arc::new(hold),
                    })),
                );
            }
            Err(error) => {
                out.insert(item.name.clone(), Err(error));
            }
        }
    }
    // The index only saves a re-hash; failing to write it costs nothing else.
    index.save().unwrap_or(());
    Ok(out)
}
