//! Helpers shared by this crate's unit tests.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::path::{Path, PathBuf};

/// Loads the configuration for `home` and `workspace` under `project`,
/// with `overrides` as `-c key=value` reads them. The one configuration
/// read in this crate's unit tests: every test reaches it through here, so
/// caller-prepared files (credentials, global, repository and project
/// configuration) are read as the caller left them, and `home` and
/// `workspace` may be one directory.
pub(crate) fn load(
    home: &Path,
    workspace: &Path,
    project: &str,
    overrides: impl IntoIterator<Item = impl AsRef<str>>,
) -> config::Config {
    config::Config::load(config::Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project: config::ProjectKey::new(project).unwrap(),
        overrides: overrides
            .into_iter()
            .map(|o| o.as_ref().to_owned())
            .collect(),
    })
    .unwrap()
}

/// An empty Fiber home and workspace in a temporary directory, removed on
/// drop: the shape most tests load their configuration from.
pub(crate) struct Rig {
    /// The temporary directory both paths live under.
    pub(crate) root: fakes::TempDir,
    /// The empty Fiber home, `root/home`.
    pub(crate) home: PathBuf,
    /// The empty workspace, `root/workspace`.
    pub(crate) workspace: PathBuf,
}

impl Rig {
    /// Creates `root/home` and `root/workspace` under `prefix`.
    pub(crate) fn new(prefix: &str) -> Self {
        let root = fakes::TempDir::new(prefix);
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        Self {
            root,
            home,
            workspace,
        }
    }
}

/// Installs the extension `home/dir` with `manifest` as its
/// `extension.json` and one `providers/<name>.json` per provider: the way
/// `fiber extension install <path>` leaves a data-only extension.
/// `providers/` is created only when `providers` is non-empty, so an
/// extension with no provider keeps its layout. When the manifest has no
/// `providers` key, the matching declaration is derived from the data's
/// provider names and their models' distinct `base_url`s.
pub(crate) fn install_extension(
    home: &Path,
    dir: &str,
    manifest: serde_json::Value,
    providers: &[(&str, serde_json::Value)],
) {
    let mut manifest = manifest;
    if manifest.get("providers").is_none() {
        let mut map = serde_json::Map::new();
        for (name, data) in providers {
            let mut urls = Vec::new();
            for model in data
                .get("models")
                .and_then(|m| m.as_array())
                .into_iter()
                .flatten()
            {
                if let Some(url) = model.get("base_url").and_then(|u| u.as_str())
                    && !urls.contains(&url)
                {
                    urls.push(url);
                }
            }
            map.insert(
                (*name).to_owned(),
                serde_json::Value::Array(urls.into_iter().map(serde_json::Value::from).collect()),
            );
        }
        manifest["providers"] = serde_json::Value::Object(map);
    }
    let dir = home.join(dir);
    if providers.is_empty() {
        std::fs::create_dir_all(&dir).unwrap();
    } else {
        std::fs::create_dir_all(dir.join("providers")).unwrap();
    }
    std::fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
    fakes::write_record(&dir);
    for (name, data) in providers {
        std::fs::write(
            dir.join("providers").join(format!("{name}.json")),
            data.to_string(),
        )
        .unwrap();
    }
}

/// A diagnostic writer that is or is not at debug and writes nothing.
pub(crate) struct Level {
    /// Whether the writer is at debug.
    pub(crate) debug: bool,
}

impl contract::diag::DebugLog for Level {
    fn debugging(&self) -> bool {
        self.debug
    }

    fn provider_request(&self, _request: &contract::diag::ProviderRequest) {}
}
