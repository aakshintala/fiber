//! Code a repository ships: what it declares, the content hash of each item,
//! approvals and pinned copies in Fiber home, and the offer a person sees
//! (`docs/extensions.md`, "Code a repository ships"). The session-side offer
//! and loading call these; nothing here reads the clock, and nothing writes
//! under the repository.

mod content;
mod declared;
mod hooks;
mod offer;
mod project;
mod servers;
mod session;
mod store;

#[cfg(test)]
#[path = "repository/content_tests.rs"]
mod content_tests;
#[cfg(test)]
#[path = "repository/declared_tests.rs"]
mod declared_tests;
#[cfg(test)]
#[path = "repository/hooks_tests.rs"]
mod hooks_tests;
#[cfg(test)]
#[path = "repository/offer_tests.rs"]
mod offer_tests;
#[cfg(test)]
#[path = "repository/servers_tests.rs"]
mod servers_tests;
#[cfg(test)]
#[path = "repository/session_tests.rs"]
mod session_tests;
#[cfg(test)]
#[path = "repository/store_sync_tests.rs"]
mod store_sync_tests;
#[cfg(test)]
#[path = "repository/store_tests.rs"]
mod store_tests;

use contract::events::OfferedKind;

pub use content::{Index, hash};
pub use declared::{RepoItem, declared_items, mcp_server_item};
pub(crate) use hooks::{ApprovedHook, HookFailure, HookState};
pub use offer::{Pending, pending};
pub use project::{ApprovedExtension, RepositorySet, approved_extension, approved_extensions};
pub(crate) use project::{Candidate, candidates};
pub use servers::{ApprovedServer, ServerApproval, SpawnGuard, repository_servers};
pub use session::SessionOffer;
pub use store::{CopyHold, CopyRemoval, Decision, Fetch, KeptCopy, Store};

/// The kind as `docs/events.md` and the approval files spell it:
/// `extension`, `hook` or `mcp_server`.
pub fn kind_name(kind: OfferedKind) -> &'static str {
    match kind {
        OfferedKind::Extension => "extension",
        OfferedKind::Hook => "hook",
        OfferedKind::McpServer => "mcp_server",
    }
}
