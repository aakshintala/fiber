//! Hooks support queries and global installs, without starting a session.
use std::path::Path;
use std::sync::Arc;

use config::{Config, Sources};
use contract::clock::Clock;
use tui::ConfigureError;

use super::{Seam, from_config, from_failure};
use crate::session_extensions::{RepositoryScope, repository_set};

fn from_extension(error: extensions::Error) -> ConfigureError {
    ConfigureError {
        code: error.code(),
        message: error.to_string(),
    }
}

pub(super) fn blocking(
    seam: &Seam,
    workspace: &Path,
    overrides: &[String],
) -> Result<Vec<String>, ConfigureError> {
    let (_, project) =
        ::cli::project_of(&seam.home, workspace).map_err(|error| from_failure(error.into()))?;
    let config = Config::load(Sources {
        home: seam.home.clone(),
        workspace: workspace.to_path_buf(),
        project: project.clone(),
        overrides: overrides.to_vec(),
    })
    .map_err(from_config)?;
    let clock: Arc<dyn Clock> = Arc::new(crate::clock::System);
    let listing = extensions::list(&seam.home, clock.as_ref()).map_err(from_extension)?;
    if listing
        .installed
        .iter()
        .any(|item| item.name == config::full_name("hooks"))
    {
        return Ok(Vec::new());
    }
    // The set holds approved copies through the filtered gate read. Gathering
    // may prepare copies, but no extension VM is loaded to ask for names.
    let set = repository_set(
        &seam.home,
        &project,
        workspace,
        &RepositoryScope::default(),
        &clock,
    );
    let filtered = set.hooks_config(&config);
    match extensions::hooks_state(&filtered, &[]).map_err(from_extension)? {
        extensions::HooksState::MissingBlocking { names } => Ok(names),
        extensions::HooksState::Ready | extensions::HooksState::MissingNotice { .. } => {
            Ok(Vec::new())
        }
    }
}

pub(super) fn install(seam: &Seam, clock: Arc<dyn Clock>) -> Result<(), ConfigureError> {
    let request = extensions::Request::Install("hooks".to_owned());
    let plan = extensions::plan(
        &seam.home,
        &request,
        env!("CARGO_PKG_VERSION"),
        &seam.origin,
        clock.as_ref(),
    )
    .map_err(from_extension)?;
    plan.commit().map_err(from_extension)?;
    Ok(())
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
