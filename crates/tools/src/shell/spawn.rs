//! Preparing a child command's environment, process session and controlling terminal.

use std::os::unix::process::CommandExt;
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

#[allow(
    unsafe_code,
    reason = "setsid between fork and exec, which CommandExt::pre_exec requires"
)]
pub(super) fn detach(cmd: &mut Command, controlling_tty: bool) {
    // SAFETY: the closure runs in the child between fork and exec, where only
    // async-signal-safe calls are sound. It calls only setsid and, for a
    // terminal, the TIOCSCTTY ioctl on fd 0, both system calls. Neither
    // allocates; the error path builds an `io::Error` from a raw errno, which
    // does not allocate either. The child is single-threaded.
    unsafe {
        cmd.pre_exec(move || {
            rustix::process::setsid().map_err(raw_error)?;
            if controlling_tty {
                // The session has no terminal yet; the secondary, already
                // fd 0, becomes it.
                // SAFETY: fd 0 is open, the secondary `Command` dup'd onto it.
                let stdin = rustix::fd::BorrowedFd::borrow_raw(0);
                rustix::process::ioctl_tiocsctty(stdin).map_err(raw_error)?;
            }
            Ok(())
        });
    }
}

fn raw_error(err: rustix::io::Errno) -> std::io::Error {
    std::io::Error::from_raw_os_error(err.raw_os_error())
}
