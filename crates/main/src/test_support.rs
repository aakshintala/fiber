//! Helpers shared by this crate's unit tests.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::path::{Path, PathBuf};

/// Writes a healthy extension install record beside its manifest.
pub(crate) fn write_record(dir: &Path) {
    let text = std::fs::read_to_string(dir.join("extension.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();
    let name = manifest.get("name").and_then(|n| n.as_str()).unwrap();
    let version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("v0.0.0");
    std::fs::write(
        dir.join(".fiber.json"),
        serde_json::json!({"name": name, "version": version, "requested": true, "source": {"path": "/p"}}).to_string(),
    )
    .unwrap();
}

/// Loads the configuration for `home` and `workspace` under `project`,
/// with `overrides` as `-c key=value` reads them. The one `Config::load`
/// in this crate's unit tests: every test reaches it through here, so
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

    /// Writes the global `config.json` in the home.
    pub(crate) fn write_global(&self, value: &serde_json::Value) {
        std::fs::write(self.home.join("config.json"), value.to_string()).unwrap();
    }

    /// Loads the configuration for this rig's home and workspace under
    /// the `test` project. The caller may write files and call again, to
    /// reload.
    pub(crate) fn config(
        &self,
        overrides: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> config::Config {
        load(&self.home, &self.workspace, "test", overrides)
    }
}

/// Loads the configuration for a fresh empty rig under `prefix`, dropped
/// after the read, as the local copies this replaces did.
pub(crate) fn config(
    prefix: &str,
    overrides: impl IntoIterator<Item = impl AsRef<str>>,
) -> config::Config {
    Rig::new(prefix).config(overrides)
}

/// Installs the extension `home/dir` with `manifest` as its
/// `extension.json` and one `providers/<name>.json` per provider: the way
/// `fiber extension install <path>` leaves a data-only extension.
/// `providers/` is created only when `providers` is non-empty, so an
/// extension with no provider keeps its layout.
pub(crate) fn install_extension(
    home: &Path,
    dir: &str,
    manifest: serde_json::Value,
    providers: &[(&str, serde_json::Value)],
) {
    let dir = home.join(dir);
    if providers.is_empty() {
        std::fs::create_dir_all(&dir).unwrap();
    } else {
        std::fs::create_dir_all(dir.join("providers")).unwrap();
    }
    std::fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
    write_record(&dir);
    for (name, data) in providers {
        std::fs::write(
            dir.join("providers").join(format!("{name}.json")),
            data.to_string(),
        )
        .unwrap();
    }
}
