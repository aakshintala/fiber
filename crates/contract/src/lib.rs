//! The vocabulary every other Fiber crate speaks (`docs/architecture.md`).
//!
//! It holds the event envelope and every event kind (`docs/events.md`), every
//! driver command (`docs/invocation.md`), the loop's inbox message and every error code
//! (`docs/errors.md`), and the provider and tool seams (`docs/architecture.md`). It
//! contains no behaviour beyond serialisation.

pub mod clock;
mod codes;
pub mod commands;
mod envelope;
pub mod events;
mod ids;
pub mod inbox;
mod pre_session;
pub mod provider;
pub mod rules;
pub mod shapes;
pub mod signing;
pub mod tool;

pub use codes::ErrorCode;
pub use envelope::{Envelope, SCHEMA_VERSION};
pub use ids::{
    ActionId, CommandId, GenerationId, JobId, ProviderCallId, RequestId, Seq, SessionId, TurnId,
};
pub use pre_session::{PreSessionExit, PreSessionPayload};
pub use rules::{Rule, RuleDecision, Rules, RulesError, StandingRules};
