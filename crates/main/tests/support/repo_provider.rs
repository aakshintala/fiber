//! Declares a repository extension package registering a data provider,
//! shared by the binary-level CLI tests: the workspace reads as a project
//! repository shipping the package, for `approve --yes` to approve.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

/// Inits git in `workspace`, so it reads as a project repository.
#[track_caller]
pub(crate) fn git_init(workspace: &Path) {
    fakes::git::git(workspace, &["init", "--quiet"]);
}

/// Declares the repository extension package `repo` registering the data
/// provider `provider` serving `models`, and lists it under
/// `repository_extensions`.
#[track_caller]
pub(crate) fn declare_repo_provider(workspace: &Path, name: &str, provider: &str, models: &Value) {
    let dir = workspace.join("repo");
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1,
               "providers": {provider: ["http://127.0.0.1:9/v1"]}})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("providers").join(format!("{provider}.json")),
        json!({"name": provider, "models": models}).to_string(),
    )
    .unwrap();
    let file = workspace.join(".fiber/config.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(
        file,
        json!({"repository_extensions": [{"path": "repo"}]}).to_string(),
    )
    .unwrap();
}
