//! Preparing a child command's environment.

use std::process::Command;

pub(super) fn scrub_env(cmd: &mut Command) {
    cmd.env_clear();
    for (key, value) in std::env::vars_os() {
        // Non-interactive `bash -c` reads `BASH_ENV`. Dropping it is what
        // "reads no shell startup files" means for bash. `sh -c` reads none.
        if key == "BASH_ENV" {
            continue;
        }
        // `bash -c` imports each `BASH_FUNC_<name>%%` as a function, and
        // shell functions are not carried.
        if key.as_encoded_bytes().starts_with(b"BASH_FUNC_") {
            continue;
        }
        cmd.env(key, value);
    }
}
