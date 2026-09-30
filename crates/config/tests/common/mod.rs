//! A Fiber home and a workspace in a temporary directory, removed when
//! dropped. Tests never touch the real home.

#![cfg(test)]
#![allow(dead_code, reason = "each test file uses a different part")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use config::{Config, ConfigError, ProjectKey, Sources};
use serde_json::{Map, Value};

static NEXT: AtomicUsize = AtomicUsize::new(0);

pub(crate) const PROJECT: &str = "-Users-alice-work-app-.git";

pub(crate) struct Setup {
    root: PathBuf,
}

impl Setup {
    pub(crate) fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fiber-config-{}-{n}", std::process::id()));
        fs::remove_dir_all(&root).unwrap_or(());
        fs::create_dir_all(root.join("home")).unwrap();
        fs::create_dir_all(root.join("workspace")).unwrap();
        Self { root }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    pub(crate) fn workspace(&self) -> PathBuf {
        self.root.join("workspace")
    }

    pub(crate) fn global(&self) -> PathBuf {
        self.home().join("config.json")
    }

    pub(crate) fn repository(&self) -> PathBuf {
        self.workspace().join(".fiber/config.json")
    }

    pub(crate) fn project(&self) -> PathBuf {
        self.home()
            .join("projects")
            .join(PROJECT)
            .join("config.json")
    }

    pub(crate) fn write(&self, file: &Path, text: &str) {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }

    pub(crate) fn load(&self, overrides: &[&str]) -> Result<Config, ConfigError> {
        Config::load(Sources {
            home: self.home(),
            workspace: self.workspace(),
            project: key(),
            overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
        })
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap_or(());
    }
}

/// `{"a": {"b": value}}` from `["a", "b"]`.
pub(crate) fn nest(path: &[&str], value: Value) -> Value {
    path.iter().rev().fold(value, |inner, name| {
        let mut map = Map::new();
        map.insert((*name).to_owned(), inner);
        Value::Object(map)
    })
}

/// A file's permission bits.
pub(crate) fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// The project key the tests use.
pub(crate) fn key() -> ProjectKey {
    ProjectKey::new(PROJECT).unwrap()
}
