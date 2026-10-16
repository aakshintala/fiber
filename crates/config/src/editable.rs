//! The files configuration views may open for editing.
use std::path::Path;

use crate::{Layer, ProjectKey, Source};

/// Whether `path` is a configuration view's editable file for this workspace.
/// Checks lexical paths only, without reading files or following symbolic links.
pub fn is_editable_config_path(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    path: &Path,
) -> bool {
    if !workspace.is_absolute() || !path.is_absolute() {
        return false;
    }
    if path.file_name() == Some(std::ffi::OsStr::new("SKILL.md")) {
        return true;
    }
    let configuration = [Layer::Global, Layer::Project, Layer::Repository]
        .into_iter()
        .any(|layer| {
            matches!(layer.source(home, workspace, project),
            Source::Global(file) | Source::Project(file) | Source::Repository(file) if file == path)
        });
    let (global, project_file) = crate::rules::files(home, project);
    configuration || global == path || project_file == path
}

#[cfg(test)]
#[path = "editable_tests.rs"]
mod tests;
