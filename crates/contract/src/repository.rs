//! The seam between the loop and the code a repository ships
//! (`docs/extensions.md`, "Code a repository ships").

use crate::events::{OfferDecision, OfferedItem};
use crate::shapes::Failure;

/// One declared item with no approval for its current content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unapproved {
    /// The offer's entry for it.
    pub offered: OfferedItem,
    /// A person recorded never for this content.
    pub never: bool,
}

/// What `decide` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decided {
    /// The approval and its pinned copy, or the never, are in Fiber home.
    Recorded,
    /// The content offered is no longer what the repository declares:
    /// nothing recorded.
    Obsolete,
}

/// Code a repository ships, as the loop reads and records it
/// (`docs/extensions.md`, "Code a repository ships").
pub trait RepositoryCode: Send + Sync {
    /// Every declared item with no approval for its content, in offer order.
    ///
    /// # Errors
    ///
    /// When the declaration or a declared file cannot be read.
    fn unapproved(&self) -> Result<Vec<Unapproved>, Failure>;

    /// The text of the workspace file at `path`, relative to the workspace,
    /// read now. The caller has checked that a pending offer lists `path`.
    ///
    /// # Errors
    ///
    /// `invalid_arguments` when it resolves outside the workspace, is not a
    /// regular file, or is not UTF-8; `io_failed` when it cannot be read. The
    /// message names `path`.
    fn file(&self, path: &str) -> Result<String, Failure>;

    /// Records `approve` or `never` for the content `item` names, after
    /// gathering the repository again. `skip` is never passed.
    ///
    /// # Errors
    ///
    /// When the repository cannot be gathered or the decision cannot be
    /// recorded; the message names the item.
    fn decide(&self, item: &OfferedItem, decision: OfferDecision) -> Result<Decided, Failure>;
}
