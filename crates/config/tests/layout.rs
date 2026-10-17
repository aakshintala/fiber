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
    crashes_dir, extension_data_dir, extensions_dir, images_cache_dir, mcp_cache_dir,
    project_data_dir,
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
