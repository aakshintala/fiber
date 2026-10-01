//! A Fiber home and extension sources in a temporary directory, removed when
//! dropped. Tests never touch the real home.

#![allow(dead_code, reason = "each test file uses a different part")]
#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

static NEXT: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct Setup {
    root: PathBuf,
}

impl Setup {
    pub(crate) fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("fiber-extensions-{}-{n}", std::process::id()));
        fs::remove_dir_all(&root).unwrap_or(());
        fs::create_dir_all(root.join("home")).unwrap();
        fs::create_dir_all(root.join("workspace")).unwrap();
        Self { root }
    }

    pub(crate) fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    pub(crate) fn workspace(&self) -> PathBuf {
        self.root.join("workspace")
    }

    /// An extension source directory named `dir` with this manifest and
    /// these provider files.
    pub(crate) fn source(&self, dir: &str, manifest: &Value, providers: &[Value]) -> PathBuf {
        let path = self.root.join("src").join(dir);
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

impl Drop for Setup {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap_or(());
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
        .map(|id| json!({ "id": id, "protocol": "openai-responses", "base_url": "http://127.0.0.1:1/v1" }))
        .collect();
    json!({ "name": name, "credential": { "env": "FIBER_TEST_UNSET_KEY" }, "models": models })
}
