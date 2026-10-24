//! The stored media a request names (`docs/model-routing.md`, "Image
//! limits"): the one place that decides which file an image or PDF path
//! names and reads it, so the provider does no file I/O.

use std::path::{Component, Path, PathBuf};

/// The file `path` names on disk, or `None` when it must not be read. A
/// relative path stays under `session_dir`; an absolute path is read only
/// when it names a session's stored artifacts, `<sessions>/<one>/artifacts/`
/// plus the file, which is how a rewound child names its parent's files:
/// the parent lives beside the child in the same `sessions/` directory.
fn media_path(path: &str, session_dir: &Path) -> Option<PathBuf> {
    let candidate = Path::new(path);
    if !candidate.is_absolute() {
        if !candidate
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        {
            return None;
        }
        return Some(session_dir.join(candidate));
    }
    if !candidate
        .components()
        .all(|part| matches!(part, Component::Normal(_) | Component::RootDir))
    {
        return None;
    }
    let rest = candidate.strip_prefix(session_dir.parent()?).ok()?;
    let mut parts = rest.components();
    if !matches!(parts.next(), Some(Component::Normal(_))) {
        return None;
    }
    if !matches!(parts.next(), Some(Component::Normal(name)) if name == "artifacts") {
        return None;
    }
    parts.next()?;
    Some(candidate.to_path_buf())
}

/// The bytes of the stored file `path` names for the session in
/// `session_dir`, or `None` when the path must not be read or the file
/// cannot be read.
pub fn read_media(session_dir: &Path, path: &str) -> Option<Vec<u8>> {
    std::fs::read(media_path(path, session_dir)?).ok()
}

#[cfg(test)]
#[path = "media_tests.rs"]
mod tests;
