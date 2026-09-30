//! The vocabulary every other Fiber crate speaks (`docs/architecture.md`).
//!
//! It holds the event envelope (`docs/events.md`, "The envelope"). It
//! contains no behaviour beyond what the envelope itself defines.

mod envelope;

pub use envelope::{ActionId, Envelope, Seq, SessionId, TurnId};

pub fn  demo_fmt( )->u8{ 1 }
