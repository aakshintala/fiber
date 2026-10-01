//! The shared fakes that tests and jigs run against: stand-ins for everything
//! outside Fiber that a test needs (`docs/testing.md`, "Fakes"). A test-only
//! dependency of the crates that use it; no release binary contains it
//! (`docs/architecture.md`, "The call rules").

mod provider_server;

pub use provider_server::{ProviderServer, Request, Response};

use contract::ErrorCode;

/// What can go wrong starting a fake.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No local port could be bound, or its address read back.
    #[error("cannot bind a local port: {0}")]
    Bind(std::io::Error),
    /// The thread that accepts connections could not be started.
    #[error("cannot start the accept thread: {0}")]
    Spawn(std::io::Error),
}

impl Error {
    /// The stable code a consumer switches on (`docs/errors.md`,
    /// "Registry").
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Bind(_) | Self::Spawn(_) => ErrorCode::IoFailed,
        }
    }
}
