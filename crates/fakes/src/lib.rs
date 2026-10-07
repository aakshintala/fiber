//! The shared fakes that tests and jigs run against: stand-ins for everything
//! outside Fiber that a test needs (`docs/testing.md`, "Fakes"). A test-only
//! dependency of the crates that use it; no release binary contains it
//! (`docs/architecture.md`, "The call rules").

mod blocking;
mod cancel;
pub mod children;
mod client;
pub mod clock;
mod connect_proxy;
pub mod emit;
pub mod jobs;
mod oauth_server;
mod process_group;
mod provider_server;
mod rerun;
mod scripted_provider;
mod temp_dir;
mod watchdog;

use std::path::{Path, PathBuf};

pub use blocking::BlockingProvider;
pub use cancel::CancelToken;
pub use client::Client;
pub use connect_proxy::ConnectProxy;
pub use emit::Recorder;
pub use oauth_server::{OauthReply, OauthRequest, OauthServer};
pub use process_group::{
    WATCHDOG_SCRIPT, group_empties, kill_group, kill_matching, kill_pid, matching,
};
pub use provider_server::{ProviderServer, Request, Response, fingerprint};
pub use rerun::rerun;
pub use scripted_provider::{Scripted, ScriptedProvider, reply};
pub use temp_dir::TempDir;
pub use watchdog::Watchdog;

/// A stand-in script the kernel may exec: `dir/name` is a symlink to the
/// checked-in trampoline, and the freshly written `body` runs through
/// `/bin/sh` as `dir/name.sh` (`docs/testing.md`, "Waits and timeouts": a
/// test does not execute a file it wrote in the same run). The link keeps
/// the pid through `exec`, and a repeat call with the same `dir`/`name`
/// rewrites the body while keeping the link.
///
/// # Panics
///
/// When the body cannot be written or the link cannot be created.
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test helper; a failure is the test's"
)]
pub fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let sh = dir.join(format!("{name}.sh"));
    std::fs::write(&sh, body).unwrap_or_else(|err| panic!("writing {}: {err}", sh.display()));
    let link = dir.join(name);
    if std::fs::symlink_metadata(&link).is_err() {
        let trampoline =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("script-fixture/trampoline.sh");
        std::os::unix::fs::symlink(&trampoline, &link)
            .unwrap_or_else(|err| panic!("linking {}: {err}", link.display()));
    }
    link
}

/// The fixture Lua extension's directory: `extension.json`, `init.lua` and
/// the module it requires, one command per runtime behaviour a test exercises.
pub fn lua_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua-fixture")
}

/// The fake MCP stdio server's `server.sh`: it reads the directory named
/// by its first argument, holding `tools.json` and one `call-<tool>.json`
/// per tool.
pub fn mcp_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("mcp-fixture/server.sh")
}

#[cfg(test)]
mod fixture_tests;
