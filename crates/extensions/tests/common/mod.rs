//! A Fiber home and extension sources in a temporary directory, removed when
//! dropped. Tests never touch the real home.

#![allow(dead_code, reason = "each test file uses a different part")]
#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::fs;
use std::path::{Path, PathBuf};

use extensions::{Error, Origin, Request, plan};
use serde_json::{Value, json};

pub(crate) struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    pub(crate) fn new() -> Self {
        let root = fakes::TempDir::new("fiber-extensions");
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self { root }
    }

    pub(crate) fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    pub(crate) fn root(&self) -> PathBuf {
        self.root.path().to_path_buf()
    }

    pub(crate) fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    /// An extension source directory named `dir` with this manifest and
    /// these provider files.
    pub(crate) fn source(&self, dir: &str, manifest: &Value, providers: &[Value]) -> PathBuf {
        let path = self.root.path().join("src").join(dir);
        write(&path.join("extension.json"), &manifest.to_string());
        for provider in providers {
            let name = provider["name"].as_str().unwrap();
            write(
                &path.join("providers").join(format!("{name}.json")),
                &provider.to_string(),
            );
        }
        path
    }
}

pub(crate) fn write(file: &Path, text: &str) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

pub(crate) fn manifest(name: &str) -> Value {
    json!({ "name": name, "version": "v1.0.0", "fiber": "0.1.0", "api": 1 })
}

/// A provider with these model ids, each on `openai-responses`.
pub(crate) fn provider(name: &str, ids: &[&str]) -> Value {
    let models: Vec<Value> = ids
        .iter()
        .map(|id| json!({ "id": id, "protocol": "openai-responses", "base_url": "http://127.0.0.1:1/v1", "context_window": 1000 }))
        .collect();
    json!({ "name": name, "credential": { "env": "FIBER_TEST_UNSET_KEY" }, "models": models })
}

/// Installs the extension in `source` the way `fiber extension install <path>` does
/// and returns its name.
pub(crate) fn install(home: &Path, source: &Path, fiber: &str) -> Result<String, Error> {
    let names = plan(
        home,
        &Request::Path(source.into()),
        fiber,
        &Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )?
    .commit()?;
    Ok(names.into_iter().next().unwrap())
}

/// Copies the first-party package `name` from `providers/` to `dest`,
/// replacing each `(from, to)` string in every file's text, so a test copy
/// points at its fakes.
pub(crate) fn copy_package(dest: &Path, name: &str, replacements: &[(&str, &str)]) {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../providers")
        .join(name);
    copy_tree(&source, dest, replacements);
}

fn copy_tree(source: &Path, dest: &Path, replacements: &[(&str, &str)]) {
    fs::create_dir_all(dest).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = dest.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target, replacements);
        } else {
            let mut text = fs::read_to_string(entry.path()).unwrap();
            for (from, to) in replacements {
                text = text.replace(from, to);
            }
            fs::write(target, text).unwrap();
        }
    }
}
