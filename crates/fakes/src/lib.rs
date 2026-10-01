//! The shared fakes that tests and jigs run against: stand-ins for everything
//! outside Fiber that a test needs (`docs/testing.md`, "Fakes"). A test-only
//! dependency of the crates that use it; no release binary contains it
//! (`docs/architecture.md`, "The call rules").

mod provider_server;
mod scripted_provider;

pub use provider_server::{ProviderServer, Request, Response};
pub use scripted_provider::{Scripted, ScriptedProvider, reply};
