//! The session's extensions, loaded once its configuration is read
//! (`docs/extensions.md`, "Registering"): `extensions_loaded` after
//! `fiber_started`, and the hooks on the loop when any registered one.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// The extension set installed in a running session.
pub(crate) struct Current(Mutex<Arc<SessionExtensions>>);

impl Current {
    pub(crate) fn new(extensions: Arc<SessionExtensions>) -> Self {
        Self(Mutex::new(extensions))
    }
    pub(crate) fn get(&self) -> Arc<SessionExtensions> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    pub(crate) fn swap(&self, extensions: Arc<SessionExtensions>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = extensions;
    }
}

use contract::hook::Hooks;
use extensions::SessionExtensions;
use log::Log;
use r#loop::Loop;

/// Where the session's repository extensions are read: the workspace
/// whose declarations count (`None`: the launch directory) and the
/// content the session skipped.
#[derive(Debug, Clone, Default)]
pub(crate) struct RepositoryScope {
    pub(crate) workspace: Option<PathBuf>,
    pub(crate) skipped: Vec<r#loop::OfferKey>,
}

/// Loads the repository set the session starts or reloads from: the
/// scope's workspace, or the launch directory when it names none, minus
/// the content the session skipped.
pub(crate) fn repository_set(
    home: &Path,
    project: &config::ProjectKey,
    launch: &Path,
    scope: &RepositoryScope,
    clock: &Arc<dyn contract::clock::Clock>,
) -> extensions::RepositorySet {
    let workspace = scope
        .workspace
        .clone()
        .unwrap_or_else(|| launch.to_path_buf());
    let skipped: BTreeSet<(String, String)> = scope
        .skipped
        .iter()
        .map(|key| (key.name.clone(), key.hash.clone()))
        .collect();
    extensions::RepositorySet::load(home, project, &workspace, &skipped, fetch(clock))
}

/// `extension_required_failed` naming the first missing required
/// extension and its notice's text.
pub(crate) fn required(
    extensions: &extensions::SessionExtensions,
) -> Result<(), contract::shapes::Failure> {
    let Some(name) = extensions.missing_required().into_iter().next() else {
        return Ok(());
    };
    let short = config::short_name(&name);
    let detail = extensions
        .notices()
        .iter()
        .find(|notice| notice.extension.as_deref() == Some(name.as_str()))
        .map(|notice| notice.message.clone())
        .unwrap_or_else(|| "it did not start".to_owned());
    Err(crate::failed(
        contract::ErrorCode::ExtensionRequiredFailed,
        format!("The repository requires the extension `{short}`, which did not load: {detail}"),
    ))
}

/// Writes `extensions_loaded` and the notices loading raised.
pub(crate) fn written(log: &Log, extensions: &SessionExtensions) -> Result<(), r#loop::Error> {
    r#loop::extensions_loaded(log, extensions.loaded(), extensions.notices())
}

/// Every notice the startup already wrote: the session's extension
/// notices, plus the startup notices `mcp_servers_started` writes
/// after the late preparer reads them. The late preparer drops every
/// notice equal to one of these, so `config_key_ignored` and the like
/// reach the late turn once.
pub(crate) fn startup_written(
    extensions: &SessionExtensions,
    startup: &[contract::events::Notice],
) -> Vec<contract::events::Notice> {
    let mut written = extensions.notices();
    written.extend(startup.iter().cloned());
    written
}

/// The startup notices `mcp_servers_started` still has to write: the
/// providers load and the session extensions load share `candidates`,
/// so a shadow report lands in both lists, and `written` already
/// wrote the session's half.
pub(crate) fn unwritten(
    notices: Vec<contract::events::Notice>,
    extensions: &SessionExtensions,
) -> Vec<contract::events::Notice> {
    let written = extensions.notices();
    notices
        .into_iter()
        .filter(|notice| !written.contains(notice))
        .collect()
}

/// How the session's repository approvals fetch dependencies: the github
/// `Origin`, this binary's version, on `clock`. No repository set is
/// loaded here; that is Part 2.
pub(crate) fn fetch(clock: &Arc<dyn contract::clock::Clock>) -> extensions::Fetch {
    extensions::Fetch::new(
        extensions::Origin::github(),
        env!("CARGO_PKG_VERSION"),
        Arc::clone(clock),
    )
}

/// Attaches loaded extensions, including empty hook chains whose state
/// still needs to reach the loop's drains.
pub(crate) fn hooked(looped: Loop, current: &Current) -> Loop {
    let extensions = current.get();
    if !extensions.has_extensions() {
        return looped;
    }
    let hooks: Arc<dyn Hooks> = extensions as Arc<dyn Hooks>;
    looped.hooks(hooks)
}
