//! Running the system tool when the built-in search declines
//! (`docs/tools.md`, "Search", "Flags").

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// Replaces the process with the system `program`, handing it `args` and
/// the untouched standard streams. Success never returns; when the
/// replacement fails, the reason goes to `stderr` and 2 is returned.
pub(crate) fn exec(program: &str, args: &[OsString], stderr: &mut dyn Write) -> i32 {
    let error = Command::new(program).args(args).exec();
    stderr
        .write_all(program.as_bytes())
        .and_then(|()| stderr.write_all(b": cannot run the system tool: "))
        .and_then(|()| stderr.write_all(error.to_string().as_bytes()))
        .and_then(|()| stderr.write_all(b"\n"))
        .unwrap_or(());
    2
}

#[cfg(test)]
#[path = "fallback_tests.rs"]
mod tests;
