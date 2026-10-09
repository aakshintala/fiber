//! `scripts/release-size` run as a program against a small git repository,
//! with the checked-in stubs under `release_size_fixture/bin/` standing in
//! for cargo, rustup, strip and sha256sum (`docs/ci.md`, "Release build and
//! size"). A stub cargo fails in the directory the test names and otherwise
//! writes a binary that says which commit built it, so each test reads which
//! commit the script compared against.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code may unwrap (docs/code-quality.md, \"Lints\"); a failure is the test's"
)]

#[path = "../src/child.rs"]
mod child;
#[path = "../src/test_dir.rs"]
#[allow(
    dead_code,
    reason = "the shared TestDir has a writer this test does not call"
)]
mod test_dir;

use std::fs;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use child::finished;
use test_dir::TestDir;

/// How long a test waits for one child, in real time; a passing run never
/// waits on it, it only bounds a hang.
const CHILD_WITHIN: Duration = Duration::from_secs(30);

/// The path component the base worktree has and the head checkout lacks.
const BASE_WORKTREE: &str = "release-base";
/// The path component the head checkout has and the base worktree lacks.
const HEAD_CHECKOUT: &str = "xtask-rsrepo";

/// Runs `command` to its end under a deadline and returns its exit code and
/// its output.
fn run(what: &str, mut command: Command) -> (Option<i32>, String) {
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let output = finished(what, child, &[], CHILD_WITHIN);
    let text =
        String::from_utf8(output.stdout).unwrap() + &String::from_utf8(output.stderr).unwrap();
    (output.status.code(), text)
}

fn git(repo: &TestDir, args: &[&str]) -> String {
    let mut command = Command::new("git");
    command
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(repo.path());
    let (code, out) = run(&format!("git {args:?}"), command);
    assert_eq!(code, Some(0), "git {args:?}: {out}");
    out.trim().to_owned()
}

/// A repository of three commits on one line: the ancestor, the base, the
/// head. HEAD is the head.
struct Repo {
    dir: TestDir,
    /// What the script writes under: `$RUNNER_TEMP`.
    temp: TestDir,
    ancestor: String,
    base: String,
}

impl Repo {
    fn new() -> Self {
        let dir = TestDir::new("rsrepo");
        git(&dir, &["init", "-q", "-b", "main"]);
        // The jig's source, so the paging jig finds one at every commit.
        dir.write("crates/tui/examples/paging.rs", "fn main() {}\n");
        git(&dir, &["add", "crates"]);
        for message in ["ancestor", "base", "head"] {
            git(&dir, &["commit", "-q", "--allow-empty", "-m", message]);
        }
        let ancestor = git(&dir, &["rev-parse", "HEAD~2"]);
        let base = git(&dir, &["rev-parse", "HEAD~1"]);
        Self {
            dir,
            temp: TestDir::new("rstmp"),
            ancestor,
            base,
        }
    }

    fn short(&self, rev: &str) -> String {
        git(&self.dir, &["rev-parse", "--short", rev])
    }

    /// Runs the script at HEAD with `base` as its argument, under the stubs.
    /// `fail_in` is the path component in which the stub cargo fails.
    fn release_size(
        &self,
        base: Option<&str>,
        fail_in: &str,
        env: &[(&str, &str)],
    ) -> (Option<i32>, String) {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/release_size_fixture/bin");
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/release-size");
        let path = format!("{}:{}", fixture.display(), std::env::var("PATH").unwrap());
        let mut command = Command::new("bash");
        command
            .arg(script)
            .args(base)
            .env("PATH", path)
            .env("RUNNER_TEMP", self.temp.path())
            .env("FAKE_CARGO_FAIL_IN", fail_in)
            .env_remove("BASE_CACHED")
            .env_remove("BASE_MATCHED_KEY")
            .env_remove("GITHUB_STEP_SUMMARY")
            .envs(env.iter().copied())
            .current_dir(self.dir.path());
        run("scripts/release-size", command)
    }

    /// The backstop's run at `rev`: a push, which leaves its binary and
    /// checksum in the cache directory for the workflow to store.
    fn backstop(&self, rev: &str) {
        git(&self.dir, &["checkout", "-q", rev]);
        let (code, out) = self.release_size(None, "never", &[]);
        assert_eq!(code, Some(0), "{out}");
        git(&self.dir, &["checkout", "-q", "main"]);
    }

    /// Runs `scripts/paging-jig` at HEAD, after `release-size` left its base.
    fn paging_jig(&self) -> String {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/release_size_fixture/bin");
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/paging-jig");
        let path = format!("{}:{}", fixture.display(), std::env::var("PATH").unwrap());
        let mut command = Command::new("bash");
        command
            .arg(script)
            .env("PATH", path)
            .env("RUNNER_TEMP", self.temp.path())
            .env("FAKE_CARGO_FAIL_IN", "never")
            .current_dir(self.dir.path());
        let (code, out) = run("scripts/paging-jig", command);
        assert_eq!(code, Some(0), "{out}");
        out
    }

    fn base_jig(&self) -> Option<String> {
        fs::read_to_string(self.temp.path().join("release/base/paging")).ok()
    }

    fn base_commit(&self) -> Option<String> {
        fs::read_to_string(self.temp.path().join("release/base/commit")).ok()
    }

    fn base_binary(&self) -> PathBuf {
        self.temp.path().join("release/base/fiber")
    }

    fn summary(&self) -> String {
        fs::read_to_string(self.temp.path().join("summary")).unwrap_or_default()
    }

    fn summary_env(&self) -> String {
        self.temp.path().join("summary").display().to_string()
    }
}

#[test]
fn a_base_that_builds_is_built_and_compared() {
    let repo = Repo::new();
    let (code, out) = repo.release_size(Some(&repo.base), NEVER, &[]);
    assert_eq!(code, Some(0), "{out}");
    let base = repo.short(&repo.base);
    assert!(
        out.contains(&format!("release-size: base {base} fiber is")),
        "{out}"
    );
    assert!(out.contains("release-size: head - base = "), "{out}");
    assert!(!out.contains("does not build"), "{out}");
    assert_eq!(
        fs::read_to_string(repo.base_binary()).unwrap(),
        format!("built at {base}\n")
    );
    assert_eq!(repo.base_commit().unwrap().trim(), repo.base);
    repo.paging_jig();
    assert_eq!(repo.base_jig().unwrap(), format!("built at {base}\n"));
}

/// A path component no directory in a run has, so the stub cargo never fails.
const NEVER: &str = "no-such-directory";

#[test]
fn a_base_that_does_not_build_is_compared_against_its_stored_ancestor() {
    let repo = Repo::new();
    repo.backstop(&repo.ancestor);
    let summary = repo.summary_env();
    let key = format!("release-musl-{}", repo.ancestor);
    let (code, out) = repo.release_size(
        Some(&repo.base),
        BASE_WORKTREE,
        &[
            ("BASE_CACHED", "false"),
            ("BASE_MATCHED_KEY", &key),
            ("GITHUB_STEP_SUMMARY", &summary),
        ],
    );
    assert_eq!(code, Some(0), "{out}");
    let (base, ancestor) = (repo.short(&repo.base), repo.short(&repo.ancestor));
    assert_eq!(
        fs::read_to_string(repo.base_binary()).unwrap(),
        format!("built at {ancestor}\n"),
        "the ancestor's stored binary stands in for the base"
    );
    assert!(out.contains("release-size: head - base = "), "{out}");
    for text in [out, repo.summary()] {
        assert!(
            text.contains(&format!("The base {base} does not build")),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "nearest first-parent ancestor with a stored binary, {ancestor}"
            )),
            "{text}"
        );
    }
}

#[test]
fn a_base_that_does_not_build_with_no_stored_ancestor_skips_the_comparison() {
    let repo = Repo::new();
    let summary = repo.summary_env();
    let (code, out) = repo.release_size(
        Some(&repo.base),
        BASE_WORKTREE,
        &[
            ("BASE_CACHED", "false"),
            ("BASE_MATCHED_KEY", ""),
            ("GITHUB_STEP_SUMMARY", &summary),
        ],
    );
    assert_eq!(code, Some(0), "{out}");
    let base = repo.short(&repo.base);
    assert!(!repo.base_binary().exists(), "no base binary to compare");
    assert!(repo.base_commit().is_none());
    let jig = repo.paging_jig();
    assert!(repo.base_jig().is_none(), "no base jig: {jig}");
    assert!(!out.contains("head - base"), "{out}");
    // The head was still measured and checked against the limit.
    assert!(out.contains("release-size: head "), "{out}");
    for text in [out, repo.summary()] {
        assert!(
            text.contains(&format!(
                "The base {base} does not build and no first-parent ancestor has a stored binary"
            )),
            "{text}"
        );
        assert!(text.contains("skipped"), "{text}");
    }
}

#[test]
fn a_head_that_does_not_build_fails_the_script() {
    let repo = Repo::new();
    let (code, out) = repo.release_size(Some(&repo.base), HEAD_CHECKOUT, &[]);
    assert_ne!(code, Some(0), "{out}");
    assert!(out.contains("could not compile fiber"), "{out}");
    assert!(
        !out.contains("release-size: base"),
        "the base is never reached: {out}"
    );
}
