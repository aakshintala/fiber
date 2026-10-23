//! A `git` command with the scrubbed environment: `core.fsmonitor` off
//! (`-c core.fsmonitor=false`), `GIT_OPTIONAL_LOCKS` and
//! `GIT_TERMINAL_PROMPT` pinned to `0`, and `GIT_DIR`, `GIT_WORK_TREE`,
//! `GIT_COMMON_DIR` and `GIT_INDEX_FILE` removed, so the caller's
//! environment cannot redirect the invocation. Hooks are off
//! (`-c core.hooksPath=/dev/null`); only the command [`hooked_command`]
//! builds runs them. Standard input is null; a caller may override stdin,
//! stdout or stderr, and appends its own args after the two `-c` pairs.

use std::process::{Command, Stdio};

/// A `git` command that runs no repository hook. Every product `git`
/// spawn is built by this or [`hooked_command`].
pub fn command(program: &str) -> Command {
    let mut command = hooked_command(program);
    command.args(["-c", "core.hooksPath=/dev/null"]);
    command
}

/// A `git` command that runs the repository's hooks: only `git worktree
/// add` is built this way (`post-checkout`, which Git LFS needs).
pub fn hooked_command(program: &str) -> Command {
    let mut command = Command::new(program);
    command
        .args(["-c", "core.fsmonitor=false"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null());
    command
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;
