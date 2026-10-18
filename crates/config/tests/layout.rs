//! The layout of Fiber home: each function names the path its directory
//! lives at under a root named `h`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::path::{Path, PathBuf};

use config::{
    crashes_dir, docs_dir, extension_data_dir, extensions_dir, global_config_file,
    images_cache_dir, logs_dir, mcp_cache_dir, project_config_file, project_data_dir,
    repository_config_file, run_dir,
};

fn root() -> PathBuf {
    PathBuf::from("/h")
}

#[test]
fn extensions_dir_is_extensions_under_home() {
    assert_eq!(extensions_dir(&root()), Path::new("/h/extensions"));
}

#[test]
fn extension_data_dir_is_data_slug_under_the_layer() {
    assert_eq!(
        extension_data_dir(Path::new("/h"), "acme"),
        PathBuf::from("/h/data/acme")
    );
}

#[test]
fn extension_data_dir_under_a_project_layer_stays_there() {
    assert_eq!(
        extension_data_dir(Path::new("/h/projects/p"), "acme"),
        PathBuf::from("/h/projects/p/data/acme")
    );
}

#[test]
fn project_data_dir_is_project_layer_data_slug() {
    let project = config::ProjectKey::new("proj").unwrap();
    assert_eq!(
        project_data_dir(&root(), &project, "acme"),
        PathBuf::from("/h/projects/proj/data/acme")
    );
}

#[test]
fn mcp_cache_dir_is_cache_mcp() {
    assert_eq!(mcp_cache_dir(&root()), Path::new("/h/cache/mcp"));
}

#[test]
fn images_cache_dir_is_cache_images() {
    assert_eq!(images_cache_dir(&root()), Path::new("/h/cache/images"));
}

#[test]
fn crashes_dir_is_crashes_under_home() {
    assert_eq!(crashes_dir(&root()), Path::new("/h/crashes"));
}

#[test]
fn config_files_live_at_their_layout_paths() {
    let project = config::ProjectKey::new("proj").unwrap();
    assert_eq!(global_config_file(&root()), Path::new("/h/config.json"));
    assert_eq!(
        project_config_file(&root(), &project),
        PathBuf::from("/h/projects/proj/config.json")
    );
    assert_eq!(
        repository_config_file(Path::new("/w")),
        PathBuf::from("/w/.fiber/config.json")
    );
}

#[test]
fn logs_dir_is_logs_under_home() {
    assert_eq!(logs_dir(&root()), Path::new("/h/logs"));
}

#[test]
fn docs_dir_is_docs_under_home() {
    assert_eq!(docs_dir(&root()), Path::new("/h/docs"));
}

#[test]
fn run_dir_is_run_under_home() {
    assert_eq!(run_dir(Path::new("/h")), Path::new("/h/run"));
}

#[test]
fn layout_accessors_create_nothing() {
    let missing = Path::new("/h/no-such-home");
    assert_eq!(logs_dir(missing), Path::new("/h/no-such-home/logs"));
    assert_eq!(docs_dir(missing), Path::new("/h/no-such-home/docs"));
    assert_eq!(crashes_dir(missing), Path::new("/h/no-such-home/crashes"));
    assert!(!missing.exists());
}
