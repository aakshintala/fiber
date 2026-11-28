//! The vocabulary every other Fiber crate speaks (`docs/architecture.md`).
//!
//! It holds the event envelope and every event kind (`docs/events.md`), every
//! driver command (`docs/invocation.md`), the loop's inbox message and every error code
//! (`docs/errors.md`), and the provider and tool seams (`docs/architecture.md`). It
//! contains no behaviour beyond checking that a value fits the vocabulary, and
//! pure conversions of its own values, such as a wall time to milliseconds or
//! to a UTC date.

/// Environment marker set beside the hub's session environment snapshot.
/// Its presence tells downstream session code that the hub spawned this
/// process.
pub const HUB_SESSION_MARKER: &str = "FIBER_HUB_SESSION";

/// Maximum characters in one monitor line before it is marked as cut.
pub const MONITOR_LINE_LIMIT: usize = 500;
/// Maximum characters in one monitor delivery before it is marked as cut.
pub const MONITOR_DELIVERY_LIMIT: usize = 3_000;

/// A rectangle in terminal-cell coordinates, with its origin at the screen's top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutRect {
    /// The left column.
    pub x: i64,
    /// The top row.
    pub y: i64,
    /// The width in cells.
    pub width: i64,
    /// The height in cells.
    pub height: i64,
}

pub mod clock;
mod codes;
pub mod commands;
pub mod diag;
pub mod emit;
mod envelope;
pub mod events;
pub mod extension;
pub mod feedback;
pub mod files;
pub mod hook;
mod hub_event_kind;
mod ids;
pub mod images;
pub mod inbox;
pub mod jobs;
pub mod keeper;
pub mod mcp_remote;
pub mod mcp_tokens;
pub mod model;
pub mod notes;
pub use mcp_remote::McpRemote;
mod pre_session;
pub mod provider;
pub mod releases;
pub mod repository;
pub mod thinking;

pub use thinking::ThinkingLevel;
pub mod rules;
pub mod search;
mod secret;
pub mod session_argv;
pub mod session_process;
pub mod session_search;
pub mod sessions;
pub mod settings;
pub mod shapes;
pub mod signing;
pub mod skills;
pub mod steering;
pub mod tool;

#[cfg(test)]
#[path = "layout_rect_tests.rs"]
mod layout_rect_tests;

pub use codes::ErrorCode;
pub use envelope::{Envelope, HubLine, SCHEMA_VERSION};
pub use hub_event_kind::HubEventKind;
pub use ids::{
    ActionId, CommandId, GenerationId, JobId, ProviderCallId, RequestId, Seq, SessionId, TurnId,
    is_minted_session_id,
};
pub use pre_session::{PreSessionExit, PreSessionPayload};
pub use rules::{Rule, RuleDecision, Rules, RulesError, StandingRules};
pub use secret::Secret;
