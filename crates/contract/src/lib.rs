//! The vocabulary every other Fiber crate speaks (`docs/architecture.md`).
//!
//! It holds the event envelope and every event kind (`docs/events.md`), every
//! driver command (`docs/invocation.md`) and every error code
//! (`docs/errors.md`). It contains no behaviour beyond serialisation.

mod codes;
mod envelope;
pub mod events;
mod ids;
pub mod shapes;

pub use codes::ErrorCode;
pub use envelope::{Envelope, SCHEMA_VERSION};
pub use ids::{
    ActionId, CommandId, GenerationId, JobId, ProviderCallId, RequestId, Seq, SessionId, TurnId,
};
