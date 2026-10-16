//! Preparing a child command's environment.

use std::process::Command;
use std::ffi::OsStr;

pub(super) fn carried(key: &OsStr) -> bool {
    key != "BASH_ENV"
        && key != crate::search::matches::MATCHES_ENV
        && !key.as_encoded_bytes().starts_with(b"BASH_FUNC_")
}

pub(super) fn scrub_env(cmd: &mut Command) {
    cmd.env_clear();
    for (key, value) in std::env::vars_os() {
        // No startup files, imported functions or another call's side file.
        if carried(&key) {
            cmd.env(key, value);
        }
    }
}
