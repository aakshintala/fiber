//! A session's view of the code its repository ships: what has no approval
//! for its current content, and recording a person's decision on one item
//! (`docs/extensions.md`, "Code a repository ships"). Nothing is kept
//! between calls: each gathers the repository again, through the index, so
//! an unchanged file is not read.

use std::path::{Path, PathBuf};

use config::ProjectKey;
use contract::events::{OfferDecision, OfferedItem};
use contract::repository::{Decided, RepositoryCode, Unapproved};
use contract::shapes::Failure;

use super::content::{Index, hash};
use super::declared::{RepoItem, declared_items};
use super::kind_name;
use super::offer::pending;
use super::store::Store;
use crate::Error;

/// The code one session's repository declares, with the approvals in Fiber
/// home for its project.
#[derive(Debug, Clone)]
pub struct SessionOffer {
    home: PathBuf,
    store: Store,
    workspace: PathBuf,
}

impl SessionOffer {
    /// The repository at `workspace`, with approvals in `home` for `project`.
    pub fn new(home: &Path, project: &ProjectKey, workspace: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            store: Store::new(home, project),
            workspace: workspace.to_path_buf(),
        }
    }
}

/// `Store::approve` or `Store::never`.
type Record = fn(&Store, &RepoItem, &str) -> Result<(), Error>;

fn failure(e: &Error) -> Failure {
    failed(e, e.to_string())
}

fn failed(e: &Error, message: String) -> Failure {
    Failure {
        code: e.code(),
        message,
        retry_after_ms: None,
        provider: None,
    }
}

impl RepositoryCode for SessionOffer {
    fn unapproved(&self) -> Result<Vec<Unapproved>, Failure> {
        let items = declared_items(&self.workspace).map_err(|e| failure(&e))?;
        // A repository that ships nothing touches nothing in Fiber home.
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let mut index = Index::load(&self.home);
        let pending = pending(&self.store, &mut index, items).map_err(|e| failure(&e))?;
        // The index only saves a re-hash; failing to write it costs nothing else.
        index.save().unwrap_or(());
        Ok(pending
            .into_iter()
            .map(|p| Unapproved {
                offered: p.offered,
                never: p.never,
            })
            .collect())
    }

    fn decide(&self, item: &OfferedItem, decision: OfferDecision) -> Result<Decided, Failure> {
        let (record, verb): (Record, &str) = match decision {
            OfferDecision::Approve => (Store::approve, "approve"),
            OfferDecision::Never => (Store::never, "record never for"),
            OfferDecision::Skip => return Ok(Decided::Obsolete),
        };
        let items = declared_items(&self.workspace).map_err(|e| failure(&e))?;
        let Some(current) = items
            .into_iter()
            .find(|i| i.kind == item.kind && i.name == item.name)
        else {
            return Ok(Decided::Obsolete);
        };
        let mut index = Index::load(&self.home);
        let now = hash(&mut index, &current).map_err(|e| failure(&e))?;
        index.save().unwrap_or(());
        // A decision applies only to the content offered.
        if now != item.hash {
            return Ok(Decided::Obsolete);
        }
        record(&self.store, &current, &now).map_err(|e| {
            failed(
                &e,
                format!(
                    "could not {verb} {} {}: {e}",
                    kind_name(current.kind),
                    current.name
                ),
            )
        })?;
        Ok(Decided::Recorded)
    }
}
