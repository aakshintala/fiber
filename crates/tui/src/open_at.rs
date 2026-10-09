//! Where the terminal opens (`docs/invocation.md`, "Commands and flags"):
//! home, home at the session list, or one session.

use contract::SessionId;

/// Where the terminal opens. The launch carries it; home acts on it once
/// and replaces it with [`OpenAt::Home`], so a reconnect never reopens it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OpenAt {
    /// Bare `fiber`: home.
    #[default]
    Home,
    /// `fiber resume` with no id: home at the session list.
    List,
    /// `fiber resume <id>` and `fiber continue`: this session.
    Session(SessionId),
}
