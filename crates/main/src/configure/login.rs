//! `/login` through `cli::login` (`docs/tui.md`, "Logging in"): the
//! installed providers by name, then the secrets installed extensions
//! declare, and one store call through the same steps as `fiber login`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::clock::Clock;
use contract::shapes::Failure;

use super::{from_config, from_failure};
use tui::{
    BrowserLogin, ConfigureError, HostGap, HostSaved, LoginKind, LoginShow, LoginTarget, Stored,
};

/// The login's browser: `open` shows the URL and asks the terminal to open
/// it, `show` shows the device code, and a person ran `/login`, so one is
/// attached (`docs/tui.md`, "Logging in"). Never the system browser, so
/// nothing writes to stderr under the TUI.
struct ShownBrowser {
    shown: Arc<dyn LoginShow>,
    method: extensions::LoginMethod,
    cancel: Arc<cli::LoginCancel>,
}

impl extensions::Browser for ShownBrowser {
    fn open(&self, url: &str) {
        self.shown.open(url);
        if self.method == extensions::LoginMethod::Device {
            self.cancel.cancel();
        }
    }

    fn show(&self, url: &str, code: &str) {
        self.shown.show(url, code);
    }

    fn attended(&self) -> bool {
        true
    }
}

/// A browser login through `cli::browser_login`: what `fiber login`
/// stores, cancelled through `cli::LoginCancel`.
struct SeamLogin {
    home: PathBuf,
    name: String,
    browser: Arc<dyn extensions::Browser>,
    clock: Arc<dyn Clock>,
    cancel: Arc<cli::LoginCancel>,
    method: extensions::LoginMethod,
}

impl BrowserLogin for SeamLogin {
    fn run(&self) -> Result<Stored, ConfigureError> {
        let providers =
            cli::providers_in(&self.home).map_err(|e| super::from_failure(Failure::from(e)))?;
        cli::browser_login(
            &self.home,
            &providers,
            &self.name,
            None,
            self.method,
            Arc::clone(&self.browser),
            Arc::clone(&self.clock),
            &self.cancel,
        )
        .map(|path| Stored {
            path,
            replaced: false,
        })
        .map_err(|e| super::from_failure(Failure::from(e)))
    }

    fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// Starts the browser login for `name`, showing its URLs through `shown`.
pub(super) fn browser_login(
    home: &Path,
    name: &str,
    shown: Arc<dyn LoginShow>,
    clock: Arc<dyn Clock>,
    method: extensions::LoginMethod,
) -> Arc<dyn BrowserLogin> {
    let cancel = Arc::new(cli::LoginCancel::default());
    Arc::new(SeamLogin {
        home: home.to_path_buf(),
        name: name.to_owned(),
        browser: Arc::new(ShownBrowser {
            shown,
            method,
            cancel: cancel.clone(),
        }),
        clock,
        cancel,
        method,
    })
}

/// Reads the reviewer choice for the installed provider `name` through
/// the same steps as `fiber login`'s post-store question.
pub(super) fn reviewer_models(
    home: &Path,
    name: &str,
    clock: Arc<dyn Clock>,
) -> Result<tui::ReviewerModels, ConfigureError> {
    let providers = cli::providers_in(home).map_err(|e| super::from_failure(Failure::from(e)))?;
    cli::reviewer_models(home, &providers, name, clock)
        .map(|choices| tui::ReviewerModels {
            models: choices.models,
            selected: choices.selected,
            notices: choices.notices,
        })
        .map_err(|e| super::from_failure(Failure::from(e)))
}

/// Saves `id` as provider `name`'s reviewer: one main-owned global
/// write. Save errors name the key.
pub(super) fn set_reviewer(home: &Path, name: &str, id: &str) -> Result<(), ConfigureError> {
    cli::set_reviewer(home, name, id).map_err(|e| super::from_failure(Failure::from(e)))
}

/// The presence gaps for the installed provider `name`, in declaration
/// order: the same query as `fiber login`'s host step, without asking.
/// The hub has no terminal and no workspace argument, so the project
/// layer is read from the hub's own working directory, and the hub's
/// own environment answers the variable reads.
pub(super) fn gaps(home: &Path, name: &str) -> Result<Vec<HostGap>, ConfigureError> {
    let providers = cli::providers_in(home).map_err(|e| super::from_failure(Failure::from(e)))?;
    let workspace = std::env::current_dir()
        .map_err(|e| from_failure(Failure::from(cli::CliError::CurrentDirectory(e))))?;
    let (_, project) =
        ::cli::project_of(home, &workspace).map_err(|e| from_failure(Failure::from(e)))?;
    let config = config::Config::load(config::Sources {
        home: home.to_path_buf(),
        workspace,
        project,
        overrides: Vec::new(),
    })
    .map_err(from_config)?;
    Ok(providers
        .unset_placeholders(&config, &|var| std::env::var(var).ok(), name)
        .map(|gaps| {
            gaps.into_iter()
                .map(|(name, env)| HostGap { name, env })
                .collect()
        })
        .unwrap_or_default())
}

/// Saves `value` as `key`'s answer for provider `name`: the same global
/// write `fiber login` makes, reporting the project file hiding it.
pub(super) fn save_host(
    home: &Path,
    name: &str,
    key: &str,
    value: &str,
) -> Result<HostSaved, ConfigureError> {
    let providers = cli::providers_in(home).map_err(|e| super::from_failure(Failure::from(e)))?;
    let masked = cli::save_host(home, &providers, name, key, value)
        .map_err(|e| super::from_failure(Failure::from(e)))?;
    Ok(tui::HostSaved { masked })
}

/// The installed providers as key rows, then the declared secrets.
pub(super) fn targets(home: &Path) -> Result<Vec<LoginTarget>, ConfigureError> {
    ::cli::login_targets(home)
        .map(|names| {
            names
                .into_iter()
                .map(|name| match name {
                    ::cli::LoginName::Provider(name) => LoginTarget {
                        name,
                        kind: LoginKind::Key,
                    },
                    ::cli::LoginName::Browser(name) => LoginTarget {
                        name,
                        kind: LoginKind::Browser,
                    },
                    ::cli::LoginName::Secret(name) => LoginTarget {
                        name,
                        kind: LoginKind::Secret,
                    },
                })
                .collect()
        })
        .map_err(|e| from_failure(Failure::from(e)))
}

/// Stores `key` for provider or secret `name`, under `label` for a
/// provider, through the same steps as `fiber login`.
pub(super) fn store(
    home: &Path,
    name: &str,
    label: Option<&str>,
    key: contract::Secret,
) -> Result<Stored, ConfigureError> {
    ::cli::login_store(home, name, label, key)
        .map(|stored| Stored {
            path: stored.path,
            replaced: stored.replaced,
        })
        .map_err(|e| from_failure(Failure::from(e)))
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
