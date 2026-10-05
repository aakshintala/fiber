//! Code a repository ships: what it declares, the content hash of each item,
//! approvals and pinned copies in Fiber home, and the offer a person sees
//! (`docs/extensions.md`, "Code a repository ships"). The session-side offer
//! and loading call these; nothing here reads the clock, and nothing writes
//! under the repository.

mod content;
mod declared;
mod offer;
mod store;

#[cfg(test)]
#[path = "repository/content_tests.rs"]
mod content_tests;
#[cfg(test)]
#[path = "repository/declared_tests.rs"]
mod declared_tests;
#[cfg(test)]
#[path = "repository/offer_tests.rs"]
mod offer_tests;
#[cfg(test)]
#[path = "repository/store_tests.rs"]
mod store_tests;

use contract::events::OfferedKind;

pub use content::{Index, hash};
pub use declared::{RepoItem, declared_items};
pub use offer::{Pending, pending};
pub use store::{Decision, Store};

/// The kind as `docs/events.md` and the approval files spell it:
/// `extension`, `hook` or `mcp_server`.
pub fn kind_name(kind: OfferedKind) -> &'static str {
    match kind {
        OfferedKind::Extension => "extension",
        OfferedKind::Hook => "hook",
        OfferedKind::McpServer => "mcp_server",
    }
}
