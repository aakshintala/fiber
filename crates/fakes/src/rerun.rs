//! Re-running a test in a child process with a scrubbed proxy environment:
//! the child re-runs one test of the current binary, so a test can prove a
//! client reads the proxy from the environment (`docs/dependencies.md`,
//! "Proxies") without inheriting the developer's shell variables.

use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::deadline::Deadline;

/// How long [`rerun`] waits for the child to exit, and for it to be reaped
/// after a kill.
const CHILD_WITHIN: Duration = Duration::from_secs(10);

/// Proxy variables a re-run child never inherits: ureq reads `ALL_PROXY`
/// ahead of `HTTPS_PROXY`, and an inherited `NO_PROXY` skips the fake proxy,
/// so either fails a proxy test on a machine behind a proxy. [`rerun`]
/// removes these before setting the ones the test needs.
const SCRUBBED: [&str; 8] = [
    "ALL_PROXY",
    "all_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "NO_PROXY",
    "no_proxy",
];

/// Re-runs the current test binary's `test` in a child process and returns
/// the child's output. The child's environment holds `env` on top of a
/// scrubbed one: every one of `SCRUBBED` is removed first, so only the
/// pairs the test passes reach the child.
///
/// # Panics
///
/// As [`rerun_within`], with a 10 s bound.
#[track_caller]
pub fn rerun(test: &str, env: &[(&str, &str)]) -> Output {
    rerun_within(test, env, CHILD_WITHIN)
}

/// [`rerun`] with the caller's bound on the child: a child that runs slowly
/// on a loaded host gets a longer one. The bound applies twice, once to the
/// child's exit and once to its reaping after a kill, so a caller keeps
/// `2 * within` inside half of nextest's per-test timeout
/// (`docs/testing.md`, "Waits and timeouts").
///
/// # Panics
///
/// When the test binary cannot be re-run, or the child is still running
/// after `within`: the child is killed first, so a hung child never keeps
/// running.
#[track_caller]
pub fn rerun_within(test: &str, env: &[(&str, &str)], within: Duration) -> Output {
    rerun_prepared(test, env, within, |_| {})
}

/// [`rerun_within`] with `prepare` run on the child's command before the
/// platform setup, so a test can arrange the child's inherited state.
///
/// # Panics
///
/// As [`rerun_within`].
#[allow(
    clippy::panic,
    reason = "a child that cannot run means the test cannot proceed"
)]
#[track_caller]
fn rerun_prepared(
    test: &str,
    env: &[(&str, &str)],
    within: Duration,
    prepare: impl FnOnce(&mut Command),
) -> Output {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => panic!("the test binary's path: {err}"),
    };
    let mut command = Command::new(exe);
    command.args(["--exact", test, "--nocapture"]);
    for var in SCRUBBED {
        command.env_remove(var);
    }
    command.envs(env.iter().copied());
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    prepare(&mut command);
    #[cfg(target_os = "macos")]
    crate::crash_ports::silence(&mut command);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => panic!("`{test}` failed to start: {err}"),
    };
    let pid = child.id();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let output = child.wait_with_output();
        match done.send(output) {
            Ok(()) | Err(_) => {}
        }
    });
    match Deadline::after(within).recv(&finished) {
        Ok(output) => match output {
            Ok(output) => output,
            Err(err) => panic!("`{test}` has no output: {err}"),
        },
        Err(_) => {
            match crate::kill_pid(pid, "KILL") {
                Ok(_) | Err(_) => {}
            }
            let reaped = Deadline::after(within).recv(&finished).is_ok();
            panic!("waited {within:?} for `{test}` to exit (reaped: {reaped})");
        }
    }
}

#[cfg(test)]
#[path = "rerun_tests.rs"]
mod tests;
