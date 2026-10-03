//! The shared fakes that tests and jigs run against: stand-ins for everything
//! outside Fiber that a test needs (`docs/testing.md`, "Fakes"). A test-only
//! dependency of the crates that use it; no release binary contains it
//! (`docs/architecture.md`, "The call rules").

pub mod clock;
mod process_group;
mod provider_server;
mod scripted_provider;
mod temp_dir;

use std::path::PathBuf;

pub use process_group::{WATCHDOG_SCRIPT, kill_group};
pub use provider_server::{ProviderServer, Request, Response, fingerprint};
pub use scripted_provider::{Scripted, ScriptedProvider, reply};
pub use temp_dir::TempDir;

/// The fixture Lua extension's directory: `extension.json`, `init.lua` and
/// the module it requires, one command per runtime behaviour a test exercises.
pub fn lua_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua-fixture")
}
