//! The person's system prompt files (`docs/system-prompt.md`, "The
//! person's files"): the system prompt file replaces Fiber's text and
//! the append prompt file is appended at the end. Only `config` reads
//! them (`docs/architecture.md`, "The call rules"); these pass on what it
//! returns.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use config::ProjectKey;

/// The system prompt file: the project file wins, read through `config`.
pub(crate) fn system(home: &Path, project: &ProjectKey) -> Option<String> {
    config::system_prompt(home, project)
}

/// The append prompt file: the project file wins, read through `config`.
pub(crate) fn append(home: &Path, project: &ProjectKey) -> Option<String> {
    config::append_prompt(home, project)
}

/// The person's home, where `~/.agents/skills` is read from: the `HOME`
/// value, when it is set to an absolute path. Unset, empty or relative
/// leaves that place unread.
pub(crate) fn agents_home(home_env: Option<OsString>) -> Option<PathBuf> {
    home_env
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
}

#[cfg(test)]
#[path = "prompt_files_tests.rs"]
mod tests;
