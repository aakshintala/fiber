//! The person's system prompt files (`docs/system-prompt.md`, "The
//! person's files"): `SYSTEM.md` replaces Fiber's text and
//! `APPEND_SYSTEM.md` is appended at the end. Each can sit at the top of
//! Fiber home or under `projects/<key>/`. A per-project file wins over the
//! global file of the same name; the two are not combined. An empty or
//! whitespace-only file is treated as absent.

use std::path::Path;

use config::ProjectKey;

/// `SYSTEM.md`: the project file wins; an empty file counts as absent, so
/// the global one applies.
pub(crate) fn system(home: &Path, project: &ProjectKey) -> Option<String> {
    read_one(home, project, "SYSTEM.md")
}

/// `APPEND_SYSTEM.md`: the project file wins; an empty file counts as
/// absent, so the global one applies.
pub(crate) fn append(home: &Path, project: &ProjectKey) -> Option<String> {
    read_one(home, project, "APPEND_SYSTEM.md")
}

fn read_one(home: &Path, project: &ProjectKey, name: &str) -> Option<String> {
    let project_file = home.join("projects").join(project.as_str()).join(name);
    if let Some(text) = read_lossy(&project_file).and_then(present) {
        return Some(text);
    }
    read_lossy(&home.join(name)).and_then(present)
}

fn read_lossy(file: &Path) -> Option<String> {
    match std::fs::read(file) {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(_) => None,
    }
}

fn present(text: String) -> Option<String> {
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg(test)]
#[path = "prompt_files_tests.rs"]
mod tests;
