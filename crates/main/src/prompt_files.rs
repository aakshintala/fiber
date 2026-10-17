//! The person's home for skills (`docs/system-prompt.md`, "The
//! person's files"): where `~/.agents/skills` is read from.

use std::ffi::OsString;
use std::path::PathBuf;

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
