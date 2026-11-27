//! Which kernel argument vectors host a session in their process
//! (`docs/permissions.md`, "Sessions on the sockets"). A session process is
//! a `fiber` process running a command that hosts a session in that process:
//! the internal session command, `fiber ask`, or the case runner's child.
//! Which session it is, Fiber reads from the process tree, never from the
//! arguments, so this predicate takes argv only and returns whether it
//! matches, with no session id.

/// Whether `argv` is a session-hosting command: the `fiber` executable
/// with `session`, `ask`, `extension-case` or `keeper` as its first argument.
/// The executable matches by basename, and the subcommand matches exactly; a
/// fake with the same words only adds ancestors, never removes a refusal.
pub fn is_session_process(argv: &[Vec<u8>]) -> bool {
    is_keeper_process(argv) || is_session_command(argv)
}

/// Whether `argv` names the keeper process, distinct from other session
/// commands that do not own the session-control connection.
pub fn is_keeper_process(argv: &[Vec<u8>]) -> bool {
    let [exe, sub, ..] = argv else {
        return false;
    };
    let basename = exe.rsplit(|byte| *byte == b'/').next().unwrap_or(exe);
    basename == b"fiber" && sub == b"keeper"
}

fn is_session_command(argv: &[Vec<u8>]) -> bool {
    let [exe, sub, ..] = argv else {
        return false;
    };
    let basename = exe.rsplit(|byte| *byte == b'/').next().unwrap_or(exe);
    basename == b"fiber" && (sub == b"session" || sub == b"ask" || sub == b"extension-case")
}

#[cfg(test)]
#[path = "session_process_tests.rs"]
mod tests;
