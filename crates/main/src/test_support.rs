//! Helpers shared by this crate's unit tests.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub(crate) use fakes::write_record;

/// Fixture binaries warmed past their first-exec stall in this test
/// process, keyed by binary path: nextest runs one test per process, so
/// the first start of each binary warms and later starts skip it. The
/// registry mutex is released before any warm runs, so concurrent starts
/// share one cell without holding a lock across the bounded child wait.
static WARMED: OnceLock<Mutex<HashMap<PathBuf, Arc<OnceLock<()>>>>> = OnceLock::new();

/// Warms `path` past its first-exec stall once per test process: a
/// `fail-start` directory makes the fixture exit 1 before reading
/// anything, so the warm-up never blocks on input. The warm runs under
/// `fakes::START_BOUND` before the test's own deadline starts, so the
/// stall never eats the success-wait margin.
fn warm_once(path: &Path) {
    let cell = WARMED
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(OnceLock::new()))
        .clone();
    cell.get_or_init(|| {
        let warm_dir = fakes::TempDir::new("fiber-main-warm");
        std::fs::write(warm_dir.path().join("fail-start"), "").unwrap();
        let warm_arg = warm_dir.path().display().to_string();
        fakes::warm_up(path, &[warm_arg.as_str()], 1);
    });
}

/// Warms the shared MCP fixture past its first-exec stall once per test
/// process, before the test's own deadline starts.
pub(crate) fn warm_mcp_fixture_once() {
    warm_once(&fakes::mcp_fixture());
}

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
    write_record(&dir);
    for (name, data) in providers {
        std::fs::write(
            dir.join("providers").join(format!("{name}.json")),
            data.to_string(),
        )
        .unwrap();
    }
}

/// Merges `providers` into the installed `extension.json` at `dir`.
pub(crate) fn declare(dir: &Path, providers: serde_json::Value) {
    let file = dir.join("extension.json");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    manifest["providers"] = providers;
    std::fs::write(&file, manifest.to_string()).unwrap();
}
