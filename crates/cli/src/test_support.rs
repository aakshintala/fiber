//! Helpers shared by this crate's unit tests.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

pub(crate) use fakes::write_record;

/// One repository package the tests declare: its dir, its manifest and
/// its files, each `(path, text)`.
pub(crate) type RepoPkg<'a> = (&'a str, Value, &'a [(&'a str, &'a str)]);

/// Inits git in `workspace` and declares these repository extension
/// packages, so the workspace reads as a project repository shipping
/// them.
pub(crate) fn declare_repo(workspace: &Path, pkgs: &[RepoPkg<'_>]) {
    fakes::git::git(workspace, &["init", "--quiet"]);
    let mut paths = Vec::new();
    for (dir, manifest, files) in pkgs {
        paths.push(json!({"path": dir}));
        fs::create_dir_all(workspace.join(dir)).unwrap();
        fs::write(
            workspace.join(dir).join("extension.json"),
            manifest.to_string(),
        )
        .unwrap();
        for (file, text) in files.iter() {
            let path = workspace.join(dir).join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
        }
    }
    let config_dir = workspace.join(".fiber");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.json"),
        json!({"repository_extensions": paths}).to_string(),
    )
    .unwrap();
}

/// Approves the declared extension `name` for the workspace's project.
pub(crate) fn approve_repo(home: &Path, workspace: &Path, name: &str) {
    let clock = fakes::clock::FakeClock::new();
    let (_, project) = crate::project_of(home, workspace).unwrap();
    let items = extensions::declared_items(workspace, &*clock).unwrap();
    let item = items.into_iter().find(|found| found.name == name).unwrap();
    let mut index = extensions::Index::load(home);
    let digest = extensions::hash(&mut index, &item).unwrap();
    extensions::Store::new(home, &project, clock)
        .approve(&item, &digest)
        .unwrap();
}

/// Records never for the declared extension `name`: a skip that holds
/// until its content changes.
pub(crate) fn never_repo(home: &Path, workspace: &Path, name: &str) {
    let clock = fakes::clock::FakeClock::new();
    let (_, project) = crate::project_of(home, workspace).unwrap();
    let items = extensions::declared_items(workspace, &*clock).unwrap();
    let item = items.into_iter().find(|found| found.name == name).unwrap();
    let mut index = extensions::Index::load(home);
    let digest = extensions::hash(&mut index, &item).unwrap();
    extensions::Store::new(home, &project, clock)
        .never(&item, &digest)
        .unwrap();
}

/// The manifest of the repository extension `name` registering
/// `providers`: each provider name to its models' distinct base URLs.
pub(crate) fn repo_manifest(name: &str, providers: &Value) -> Value {
    let mut manifest = json!({
        "name": name,
        "version": "v1.0.0",
        "fiber": "0.0.0",
        "api": extensions::API,
    });
    manifest["providers"] = providers.clone();
    manifest
}

/// The data file of the provider `name` serving these model ids at
/// `base_url`.
pub(crate) fn repo_data(name: &str, base_url: &str, ids: &[&str]) -> String {
    let models: Vec<Value> = ids
        .iter()
        .map(|id| {
            json!({"id": id, "protocol": "openai-responses",
                   "base_url": base_url, "context_window": 1000})
        })
        .collect();
    json!({"name": name, "models": models}).to_string()
}
