//! One child's run to its end under a deadline, and one git run asserting
//! success, shared by the integration tests. Each caller configures its
//! command's program, arguments, environment and working directory, and its
//! standard input; the helpers own the wait, so a hung child fails the test
//! instead of hanging it.

use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::time::Duration;

use fakes::{Deadline, TempDir, finished};

/// Runs `command` to its end under `within` and returns its exit code and
/// its combined output. `stdin` holds the bytes for piped standard input,
/// if any; every other stream the helper pipes.
pub(crate) fn run(
    what: &str,
    mut command: Command,
    stdin: &[u8],
    within: Duration,
) -> (Option<i32>, String) {
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let output = finished(what, child, stdin, Deadline::after(within));
    let text =
        String::from_utf8(output.stdout).unwrap() + &String::from_utf8(output.stderr).unwrap();
    (output.status.code(), text)
}

/// Runs `git args` in `dir` and returns its trimmed standard output,
/// failing the test when git exits nonzero. The wait is bounded at 30 s,
/// the bound both callers use today; a passing run never waits on it.
/// (`cli` keeps its own `git`, under the same shared bound, so this function
/// is unused in that test target.)
#[allow(dead_code, reason = "shared module; cli keeps its own git")]
pub(crate) fn git(dir: &TempDir, args: &[&str]) -> String {
    let mut command = Command::new("git");
    command
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir.path())
        .stdin(Stdio::null());
    let (code, out) = run(
        &format!("git {args:?}"),
        command,
        &[],
        Duration::from_secs(30),
    );
    assert_eq!(code, Some(0), "git {args:?}: {out}");
    out.trim().to_owned()
}
