//! The layout of Fiber home (`docs/state.md`, "What each part holds"):
//! the directories extensions live in, extension data lives in, caches
//! live in, and crash reports live in. Only `config` knows these paths
//! (`docs/architecture.md`, "The call rules"); callers ask for them here
//! instead of joining the segments themselves.

use std::path::{Path, PathBuf};

use crate::ProjectKey;

/// `<home>/extensions`: where extensions are installed.
pub fn extensions_dir(home: &Path) -> PathBuf {
    home.join("extensions")
}

/// `<layer>/data/<slug>`: one extension's data directory. `layer` is Fiber
/// home for machine-wide data or `projects/<key>/` for per-project data;
/// `slug` is already `crate::dir_name(..)`.
pub fn extension_data_dir(layer: &Path, slug: &str) -> PathBuf {
    layer.join("data").join(slug)
}

/// `<home>/projects/<key>/data/<slug>`: one extension's per-project data
/// directory.
pub fn project_data_dir(home: &Path, project: &ProjectKey, slug: &str) -> PathBuf {
    extension_data_dir(&home.join("projects").join(project.as_str()), slug)
}

/// `<home>/cache/mcp`: where MCP server output is cached.
pub fn mcp_cache_dir(home: &Path) -> PathBuf {
    home.join("cache").join("mcp")
}

/// `<home>/cache/images`: where image attachments are cached.
pub fn images_cache_dir(home: &Path) -> PathBuf {
    home.join("cache").join("images")
}

/// `<home>/crashes`: where crash reports are written.
pub fn crashes_dir(home: &Path) -> PathBuf {
    home.join("crashes")
}
