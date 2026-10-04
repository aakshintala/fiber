//! The `grep` and `find` shell functions (`docs/tools.md`, "Search", "How it
//! runs"): before each command, two functions route those names to the Fiber
//! binary's hidden search subcommands.

use std::path::Path;

/// Builds the function definitions for `fiber`, or nothing when no search
/// binary is set. The functions exist only in the model's own command line:
/// shell functions never reach child processes, so `command grep`, a full
/// path, `xargs`, `find -exec` and any script keep the system tools. A
/// missing executable falls back to the system tool, and a command's own
/// `$?` before its first statement stays 0.
pub(super) fn define(fiber: Option<&Path>) -> String {
    let Some(fiber) = fiber else {
        return String::new();
    };
    let exe = quoted(fiber);
    // The unexport runs only under bash: `export -nf` is a fatal error in
    // dash, which aborts the whole command line (probed with dash 0.5 on
    // macOS), and dash has no exported functions to unexport. Under bash
    // the functions exist, so the unexport succeeds and `$?` stays 0.
    format!(
        "grep() {{ if [ -x {exe} ]; then {exe} grep \"$@\"; else command grep \"$@\"; fi; }}; \
         find() {{ if [ -x {exe} ]; then {exe} find \"$@\"; else command find \"$@\"; fi; }}; \
         if [ -n \"$BASH_VERSION\" ]; then export -nf grep find 2>/dev/null; fi"
    )
}

/// Single-quote escapes `path` for the shell: `'` becomes `'\''`.
fn quoted(path: &Path) -> String {
    format!(
        "'{}'",
        path.as_os_str().to_string_lossy().replace('\'', "'\\''")
    )
}

#[cfg(test)]
#[path = "prelude_tests.rs"]
mod tests;
