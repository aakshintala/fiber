//! Re-running a test in a child process with a scrubbed proxy environment:
//! the child re-runs one test of the current binary, so a test can prove a
//! client reads the proxy from the environment (`docs/dependencies.md`,
//! "Proxies") without inheriting the developer's shell variables.
//!
//! Every child runs in its own process group under a [`Watchdog`]: the
//! group is killed and reaped on every path, including failure and timeout,
//! because the fixtures ignore `SIGTERM` on purpose (`docs/testing.md`,
//! "Fakes").

use std::os::unix::process::CommandExt;
use std::panic::AssertUnwindSafe;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::deadline::Deadline;

/// How long [`rerun`] waits for the child to exit, and how long [`rerun`]
/// and [`rerun_prepared`] wait to clean up after it. Half of it is the wait
/// for an exited child's group to empty.
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
    rerun_bounded(test, env, CHILD_WITHIN, CHILD_WITHIN, |_| {})
}

/// [`rerun`] with the caller's deadline on the child: a child that runs
/// slowly on a loaded host gets a longer one. What remains of the deadline
/// applies twice, once to the child's exit and once to the whole cleanup
/// after it, so a caller keeps `2 * within` inside half of nextest's
/// per-test timeout (`docs/testing.md`, "Waits and timeouts").
///
/// # Panics
///
/// When the test binary cannot be re-run, or the child is still running
/// after `deadline`: the child's group is killed first, so a hung child never
/// keeps running. When the child exits but leaves processes in its group:
/// the group is killed and the rerun fails, so a leaked background process
/// never keeps running either.
#[track_caller]
pub fn rerun_within(test: &str, env: &[(&str, &str)], deadline: Deadline) -> Output {
    let within = deadline.left();
    rerun_bounded(test, env, within, within, |_| {})
}

/// [`rerun_within`] with `prepare` run on the child's command before the
/// platform setup, so a test can arrange the child's inherited state.
/// `prepare` sees the fixed arguments (`--exact`, the test path,
/// `--nocapture`, `--test-threads=1`), the scrubbed environment with `env`
/// set, null standard input and piped standard output and error: the
/// working directory, path-valued environment, removed variables and a
/// standard input with bytes to read are set there. The child always leads
/// its own process group: a `process_group` call in `prepare` cannot move
/// it out of the group the kill and the watchdog name. Cleanup after the
/// exit wait is bounded by 10 s.
///
/// # Panics
///
/// As [`rerun_within`].
#[allow(
    clippy::panic,
    reason = "a child that cannot run means the test cannot proceed"
)]
#[track_caller]
pub fn rerun_prepared(
    test: &str,
    env: &[(&str, &str)],
    deadline: Deadline,
    prepare: impl FnOnce(&mut Command),
) -> Output {
    rerun_bounded(test, env, deadline.left(), CHILD_WITHIN, prepare)
}

/// [`rerun_prepared`] with the exit bound and the cleanup bound set
/// separately: a rerun waits at most `within + reap` on every path.
#[allow(
    clippy::panic,
    reason = "a child that cannot run means the test cannot proceed"
)]
#[track_caller]
fn rerun_bounded(
    test: &str,
    env: &[(&str, &str)],
    within: Duration,
    reap: Duration,
    prepare: impl FnOnce(&mut Command),
) -> Output {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => panic!("the test binary's path: {err}"),
    };
    let mut command = Command::new(exe);
    command.args(["--exact", test, "--nocapture", "--test-threads=1"]);
    for var in SCRUBBED {
        command.env_remove(var);
    }
    command.envs(env.iter().copied());
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    prepare(&mut command);
    // After `prepare`, so no caller arrangement can move the child out of
    // the group the kill and the watchdog below name.
    command.process_group(0);
    #[cfg(target_os = "macos")]
    crate::crash_ports::silence(&mut command);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => panic!("`{test}` failed to start: {err}"),
    };
    let group = child.id();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let output = child.wait_with_output();
        match done.send(output) {
            Ok(()) | Err(_) => {}
        }
    });
    // The child is already spawned, so a watchdog that cannot start must
    // still leave nothing running: kill the group and wait for the waiter
    // before the panic continues.
    let watchdog =
        match std::panic::catch_unwind(AssertUnwindSafe(|| crate::Watchdog::group(group))) {
            Ok(watchdog) => watchdog,
            Err(payload) => {
                match crate::kill_group(group, "KILL") {
                    Ok(_) | Err(_) => {}
                }
                match Deadline::after(reap).recv(&finished) {
                    Ok(_) | Err(_) => {}
                }
                std::panic::resume_unwind(payload);
            }
        };
    match Deadline::after(within).recv(&finished) {
        Ok(Ok(output)) => {
            let cleanup = Deadline::after(reap);
            if crate::group_empties(group, Deadline::after(half(reap))) {
                watchdog.stand_down(cleanup);
                return output;
            }
            match crate::kill_group(group, "KILL") {
                Ok(_) | Err(_) => {}
            }
            let emptied = crate::group_empties(group, cleanup);
            // Dropping the watchdog closes its standard input, a second net
            // around the group.
            panic!(
                "`{test}` exited and left processes in its group (killed; group empty: {emptied})"
            );
        }
        Ok(Err(err)) => {
            let cleanup = Deadline::after(reap);
            match crate::kill_group(group, "KILL") {
                Ok(_) | Err(_) => {}
            }
            match crate::group_empties(group, cleanup) {
                true | false => {}
            }
            panic!("`{test}` has no output: {err}");
        }
        Err(_) => {
            let cleanup = Deadline::after(reap);
            match crate::kill_group(group, "KILL") {
                Ok(_) | Err(_) => {}
            }
            let reaped = cleanup.recv(&finished).is_ok();
            let emptied = crate::group_empties(group, cleanup);
            panic!(
                "waited {within:?} for `{test}` to exit (reaped: {reaped}, group empty: {emptied})"
            );
        }
    }
}

/// Half of `reap`: the share of the cleanup bound a normal exit spends waiting
/// for its group to empty before the group is killed.
fn half(reap: Duration) -> Duration {
    reap / 2
}

#[cfg(test)]
#[path = "rerun_tests.rs"]
mod tests;
