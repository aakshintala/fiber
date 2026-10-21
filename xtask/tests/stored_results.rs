//! `scripts/stored-results` run as a program against a small git repository,
//! with the checked-in stub under `stored_results_fixture/bin/` standing in
//! for gh (`docs/ci.md`, "On every pull request that changes code"). An
//! artifact listing for a commit is the test data in `$FAKE_GH_DIR`, so each
//! test reads which commit the script settled on.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code may unwrap (docs/code-quality.md, \"Lints\"); a failure is the test's"
)]

use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use fakes::{Deadline, TempDir, finished};

/// How long a test waits for one child, in real time; a passing run never
/// waits on it, it only bounds a hang.
const CHILD_WITHIN: Duration = Duration::from_secs(30);

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
    let output = finished(what, child, &[], Deadline::after(CHILD_WITHIN));
    let text =
        String::from_utf8(output.stdout).unwrap() + &String::from_utf8(output.stderr).unwrap();
    (output.status.code(), text)
}

fn git(repo: &TempDir, args: &[&str]) -> String {
    let mut command = Command::new("git");
    command
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(repo.path());
    let (code, out) = run(&format!("git {args:?}"), command);
    assert_eq!(code, Some(0), "git {args:?}: {out}");
    out.trim().to_owned()
}

/// A repository of twelve commits on one line on `main`. HEAD is the head.
struct Repo {
    dir: TempDir,
    /// What the script writes under: `$RUNNER_TEMP`.
    temp: TempDir,
    /// The stub's data and log: `$FAKE_GH_DIR`, `$FAKE_GH_LOG`.
    fake: TempDir,
    log: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let dir = TempDir::new("xtask-srrepo");
        git(&dir, &["init", "-q", "-b", "main"]);
        dir.write("file", "contents\n");
        git(&dir, &["add", "file"]);
        for n in 0..12 {
            git(
                &dir,
                &[
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    &format!("commit {n}"),
                ],
            );
        }
        let temp = TempDir::new("xtask-srtmp");
        let fake = TempDir::new("xtask-srfake");
        let log = fake.path().join("calls");
        std::fs::write(&log, "").unwrap();
        Self {
            dir,
            temp,
            fake,
            log,
        }
    }

    fn rev(&self, rev: &str) -> String {
        git(&self.dir, &["rev-parse", rev])
    }

    fn short(&self, rev: &str) -> String {
        git(&self.dir, &["rev-parse", "--short", rev])
    }

    /// Writes the artifact listing the stub serves for `rev`.
    fn listing(&self, rev: &str, text: &str) {
        let sha = self.rev(rev);
        std::fs::write(self.fake.path().join(format!("{sha}.json")), text).unwrap();
    }

    /// Runs the script with `base` as its argument, under the stub.
    fn stored_results(&self, base: Option<&str>, env: &[(&str, &str)]) -> (Option<i32>, String) {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/stored_results_fixture/bin");
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/stored-results");
        let path = format!("{}:{}", fixture.display(), std::env::var("PATH").unwrap());
        let mut command = Command::new("bash");
        command
            .arg(script)
            .args(base)
            .env("PATH", path)
            .env("RUNNER_TEMP", self.temp.path())
            .env("GITHUB_REPOSITORY", "o/r")
            .env("FAKE_GH_DIR", self.fake.path())
            .env("FAKE_GH_LOG", &self.log)
            .env_remove("FAKE_GH_FAIL")
            .env_remove("GITHUB_STEP_SUMMARY")
            .envs(env.iter().copied())
            .current_dir(self.dir.path());
        run("scripts/stored-results", command)
    }

    /// The stub's calls, one per line.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .filter(|line| !line.is_empty())
            .collect()
    }

    /// The commit in each `api` call's query, nearest first.
    fn looked_up(&self) -> Vec<String> {
        self.calls()
            .iter()
            .filter_map(|call| {
                call.strip_prefix("api ")
                    .and_then(|query| query.split("name=benchmarks-").nth(1))
                    .and_then(|rest| rest.split('&').next())
                    .map(str::to_owned)
            })
            .collect()
    }

    fn stored_commit(&self) -> Option<String> {
        std::fs::read_to_string(self.temp.path().join("stored/commit"))
            .ok()
            .map(|text| text.trim().to_owned())
    }

    fn stored_head(&self) -> Option<String> {
        std::fs::read_to_string(self.temp.path().join("stored/head.json")).ok()
    }
}

/// An artifact listing holding `artifacts`.
fn listing(artifacts: &str) -> String {
    format!("{{\"total_count\":1,\"artifacts\":[{artifacts}]}}")
}

/// One artifact: `expired`, from branch `branch`, in repository `repo` with
/// head repository `head_repo`.
fn good(sha: &str) -> String {
    artifact(1, 700, false, "main", 1, 1, sha)
}

fn artifact(
    id: u64,
    run: u64,
    expired: bool,
    branch: &str,
    repo: u64,
    head_repo: u64,
    sha: &str,
) -> String {
    format!(
        "{{\"id\":{id},\"name\":\"x\",\"expired\":{expired},\
         \"workflow_run\":{{\"id\":{run},\"repository_id\":{repo},\
         \"head_repository_id\":{head_repo},\"head_branch\":\"{branch}\",\
         \"head_sha\":\"{sha}\"}}}}"
    )
}

#[test]
fn the_bases_own_stored_results_are_used() {
    let repo = Repo::new();
    let base = repo.rev("HEAD");
    repo.listing("HEAD", &listing(&good(&base)));
    let (code, out) = repo.stored_results(Some(&base), &[]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains(&repo.short("HEAD")), "{out}");
    assert_eq!(repo.stored_commit(), Some(base));
    assert!(repo.stored_head().is_some());
    assert_eq!(repo.looked_up().len(), 1);
}

#[test]
fn the_nearest_stored_ancestor_wins() {
    let repo = Repo::new();
    let base = repo.rev("HEAD");
    let second = repo.rev("HEAD~2");
    repo.listing("HEAD~2", &listing(&good(&second)));
    repo.listing("HEAD~5", &listing(&good(&repo.rev("HEAD~5"))));
    let (code, out) = repo.stored_results(Some(&base), &[]);
    assert_eq!(code, Some(0), "{out}");
    assert_eq!(repo.stored_commit(), Some(second));
    assert_eq!(
        repo.looked_up(),
        [repo.rev("HEAD"), repo.rev("HEAD~1"), repo.rev("HEAD~2")]
    );
}

#[test]
fn none_among_the_base_and_its_nine_ancestors_writes_nothing() {
    let repo = Repo::new();
    let base = repo.rev("HEAD");
    // The 10th ancestor back stands outside the window.
    repo.listing("HEAD~10", &listing(&good(&repo.rev("HEAD~10"))));
    let summary = TempDir::new("xtask-srsum");
    let summary_file = summary.path().join("summary.md");
    let summary = summary_file.to_str().unwrap().to_owned();
    let (code, out) =
        repo.stored_results(Some(&base), &[("GITHUB_STEP_SUMMARY", summary.as_str())]);
    assert_eq!(code, Some(0), "{out}");
    assert!(repo.stored_commit().is_none());
    assert!(repo.stored_head().is_none());
    let said = format!(
        "No stored results for the base {} or its 9 nearest first-parent ancestors",
        repo.short("HEAD")
    );
    assert!(out.contains(&said), "{out}");
    assert!(
        std::fs::read_to_string(&summary_file)
            .unwrap()
            .contains(&said),
        "{out}"
    );
    assert_eq!(repo.looked_up().len(), 10);
}

#[test]
fn an_expired_branch_or_fork_artifact_is_skipped() {
    let repo = Repo::new();
    let base = repo.rev("HEAD");
    let first = repo.rev("HEAD~1");
    let stale = [
        artifact(4, 703, true, "main", 1, 1, &base),
        artifact(2, 701, false, "feature", 1, 1, &base),
        artifact(3, 702, false, "main", 1, 2, &base),
    ]
    .join(",");
    repo.listing("HEAD", &listing(&stale));
    repo.listing("HEAD~1", &listing(&good(&first)));
    let (code, out) = repo.stored_results(Some(&base), &[]);
    assert_eq!(code, Some(0), "{out}");
    assert_eq!(repo.stored_commit(), Some(first));
}

#[test]
fn an_api_failure_stops_the_search_and_fails_nothing() {
    let repo = Repo::new();
    let base = repo.rev("HEAD");
    let (code, out) = repo.stored_results(Some(&base), &[("FAKE_GH_FAIL", "api")]);
    assert_eq!(code, Some(0), "{out}");
    assert!(repo.stored_commit().is_none());
    assert!(repo.stored_head().is_none());
    assert!(out.contains("looking up"), "{out}");
    assert_eq!(repo.looked_up().len(), 1);
}

#[test]
fn a_download_failure_writes_nothing() {
    let repo = Repo::new();
    let base = repo.rev("HEAD");
    repo.listing("HEAD", &listing(&good(&base)));
    let (code, out) = repo.stored_results(Some(&base), &[("FAKE_GH_FAIL", "download")]);
    assert_eq!(code, Some(0), "{out}");
    assert!(repo.stored_commit().is_none());
    assert!(repo.stored_head().is_none());
    assert!(out.contains("downloading"), "{out}");
}

#[test]
fn no_base_commit_is_a_usage_error() {
    let repo = Repo::new();
    let (code, out) = repo.stored_results(None, &[]);
    assert_eq!(code, Some(2), "{out}");
    assert!(
        out.contains("usage: scripts/stored-results BASE_COMMIT"),
        "{out}"
    );
}

#[test]
fn an_unreadable_listing_fails_nothing() {
    let repo = Repo::new();
    let base = repo.rev("HEAD");
    let first = repo.rev("HEAD~1");
    // A listing with no artifacts reads as none: the search moves on.
    repo.listing("HEAD", "{\"artifacts\":null}");
    repo.listing("HEAD~1", &listing(&good(&first)));
    let (code, out) = repo.stored_results(Some(&base), &[]);
    assert_eq!(code, Some(0), "{out}");
    assert_eq!(repo.stored_commit(), Some(first));

    // A listing no parser can read stops the search with no files.
    let repo = Repo::new();
    let base = repo.rev("HEAD");
    repo.listing("HEAD", "not json");
    let (code, out) = repo.stored_results(Some(&base), &[]);
    assert_eq!(code, Some(0), "{out}");
    assert!(repo.stored_commit().is_none());
    assert!(repo.stored_head().is_none());
    assert!(out.contains("artifact listing"), "{out}");
    assert_eq!(repo.looked_up().len(), 1);
}

#[test]
fn a_base_that_is_not_a_commit_fails_nothing() {
    let repo = Repo::new();
    let missing = "0000000000000000000000000000000000000000";
    let (code, out) = repo.stored_results(Some(missing), &[]);
    assert_eq!(code, Some(0), "{out}");
    assert!(repo.stored_commit().is_none());
    assert!(repo.stored_head().is_none());
    assert!(out.contains(missing), "{out}");
    assert!(repo.looked_up().is_empty());
}
