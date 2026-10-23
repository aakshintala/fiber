//! The scrubbed `git` command builder carries its `-c` pairs, its pinned
//! variables and its removed variables, passes another program through,
//! starts with null stdin, and is not redirected by a `GIT_DIR` the caller
//! exported.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::{command, hooked_command};

/// The scrubbed pairs, pins and removals each builder carries.
#[test]
fn builders_carry_the_scrubbed_environment() {
    check(
        command,
        &[
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
        ],
    );
    check(hooked_command, &["-c", "core.fsmonitor=false"]);
}

/// One builder's program, args, pinned variables and removed variables.
fn check(build: fn(&str) -> Command, args: &[&str]) {
    let built = build("git");
    assert_eq!(built.get_program(), OsStr::new("git"));
    let got: Vec<OsString> = built.get_args().map(|arg| arg.to_owned()).collect();
    let wanted: Vec<OsString> = args.iter().map(OsString::from).collect();
    assert_eq!(got, wanted);
    let envs: BTreeMap<&OsStr, Option<&OsStr>> = built.get_envs().collect();
    assert_eq!(
        envs.get(OsStr::new("GIT_OPTIONAL_LOCKS")),
        Some(&Some(OsStr::new("0")))
    );
    assert_eq!(
        envs.get(OsStr::new("GIT_TERMINAL_PROMPT")),
        Some(&Some(OsStr::new("0")))
    );
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
        assert_eq!(envs.get(OsStr::new(name)), Some(&None), "{name} is removed");
    }
    // A program other than `git` is passed through: the builder scrubs
    // the environment, not the program.
    assert_eq!(build("other-git").get_program(), OsStr::new("other-git"));
}

/// The child re-run marker: set, the test hashes stdin through the
/// builder; unset, the test re-runs itself with a byte on stdin.
const STDIN_CHILD: &str = "FIBER_SUPPORT_GIT_STDIN_CHILD";

/// This test binary's path of `test`: `module_path!` carries the crate
/// name, which is not part of a test's path.
fn this(test: &str) -> String {
    let module = module_path!();
    let module = module.split_once("::").map_or(module, |(_, rest)| rest);
    format!("{module}::{test}")
}

/// The builder starts with null stdin: the child hashes its stdin and
/// must read the empty blob, not the byte the re-run gave it.
#[test]
fn stdin_is_null() {
    if std::env::var(STDIN_CHILD).is_ok() {
        let child = command("git")
            .args(["hash-object", "--stdin"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        return;
    }
    let temp = fakes::TempDir::new("support-git-stdin");
    let byte = temp.path().join("byte");
    fs::write(&byte, b"x").unwrap();
    let out = fakes::rerun_prepared(
        &this("stdin_is_null"),
        &[(STDIN_CHILD, "1")],
        fakes::Deadline::after(Duration::from_secs(60)),
        |child| {
            let input = fs::File::open(&byte).unwrap();
            child.stdin(Stdio::from(input));
        },
    );
    assert!(
        out.status.success(),
        "stdin must hash as empty:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The child re-run markers for the redirect test.
const REDIRECT_CHILD: &str = "FIBER_SUPPORT_GIT_REDIRECT_CHILD";
const REDIRECT_REPO: &str = "FIBER_SUPPORT_GIT_REDIRECT_REPO";

/// A `GIT_DIR` pointing at a decoy repository does not redirect the
/// builder: `-C <repo> rev-parse --absolute-git-dir` still prints the
/// repository's own directory, resolved as git prints it.
#[test]
fn a_redirected_git_dir_does_not_reach_the_command() {
    if std::env::var(REDIRECT_CHILD).is_ok() {
        let repo = PathBuf::from(std::env::var(REDIRECT_REPO).unwrap());
        let out = command("git")
            .arg("-C")
            .arg(&repo)
            .args(["rev-parse", "--absolute-git-dir"])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(
            PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_owned()),
            fs::canonicalize(repo.join(".git")).unwrap()
        );
        return;
    }
    let temp = fakes::TempDir::new("support-git-redirect");
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    fakes::git::git(&repo, &["init", "--quiet"]);
    let decoy = temp.path().join("decoy");
    fs::create_dir_all(&decoy).unwrap();
    fakes::git::git(&decoy, &["init", "--quiet"]);
    let out = fakes::rerun_prepared(
        &this("a_redirected_git_dir_does_not_reach_the_command"),
        &[
            (REDIRECT_CHILD, "1"),
            (REDIRECT_REPO, repo.to_str().unwrap()),
            ("GIT_DIR", decoy.join(".git").to_str().unwrap()),
        ],
        fakes::Deadline::after(Duration::from_secs(60)),
        |_| {},
    );
    assert!(
        out.status.success(),
        "the builder must see past GIT_DIR:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
