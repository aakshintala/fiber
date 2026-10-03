//! A Fiber home and a workspace in a temporary directory, removed when
//! dropped. Tests never touch the real home.

#![cfg(test)]
#![allow(dead_code, reason = "each test file uses a different part")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use config::{Config, ConfigError, ProjectKey, Sources};
use serde_json::{Map, Value};

pub(crate) const PROJECT: &str = "-Users-alice-work-app-.git";

pub(crate) struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    pub(crate) fn new() -> Self {
        let root = fakes::TempDir::new("fiber-config");
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self { root }
    }

    pub(crate) fn root(&self) -> &Path {
        self.root.path()
    }

    pub(crate) fn home(&self) -> PathBuf {
        self.root().join("home")
    }

    pub(crate) fn workspace(&self) -> PathBuf {
        self.root().join("workspace")
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
