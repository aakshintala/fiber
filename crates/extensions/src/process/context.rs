//! What one process extension runs as (`docs/extensions.md`, "Process
//! extensions"): its session, its workspace, its data directories and its
//! folded state. The package builds it once and hands it to the owner, and
//! tests inject fakes without changing the public `PackageOptions`.
//!
//! The production host services dispatch native host operations through
//! this context; package tests inject a fake implementation behind the
//! same trait.

use std::path::PathBuf;

use serde_json::{Map, Value};

/// What one process extension runs as: the first `extension_hello` line's
/// payload, and the context every native host call reads.
#[derive(Debug, Clone)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) struct ProcessContext {
    /// The session's id, as `extension_hello` carries it.
    pub session_id: contract::SessionId,
    /// Why the session started, resent after a restart.
    pub reason: contract::events::ExtensionReason,
    /// The session's workspace, resolving relative paths.
    pub workspace: PathBuf,
    /// The paths `host.data_dir` returns.
    pub data_dirs: contract::events::ExtensionDataDirs,
    /// The extension's folded state, as `state.get` reads it.
    pub state: Map<String, Value>,
    /// The extension's name, attributing its calls.
    pub extension: String,
    /// Fiber home, anchoring secrets and data directories.
    pub home: PathBuf,
}

#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
impl ProcessContext {
    /// The `extension_hello` payload: the session's identity, why it
    /// started, its workspace, the extension's data directories and its
    /// folded state, with the state as it stands now.
    pub(crate) fn hello(&self) -> contract::events::ExtensionHello {
        contract::events::ExtensionHello {
            session_id: self.session_id.clone(),
            reason: self.reason,
            workspace: self.workspace.display().to_string(),
            data_dirs: self.data_dirs.clone(),
            state: self.state.clone(),
        }
    }

    /// Refolds `state`, as sent again after a restart with the state as it
    /// stands in the session.
    pub(crate) fn refold(&mut self, state: Map<String, Value>) {
        self.state = state;
    }
}
