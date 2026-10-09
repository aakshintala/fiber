//! `scripts/cache-prune` run as a program against a small git repository,
//! with the checked-in stub under `cache_prune_fixture/bin/` standing in
//! for gh (`docs/ci.md`, "The backstop"). The fixture history is a linear
//! main of 25 commits with one pull request head on top of c12, and the
//! `origin` remote holds its `refs/pull/1/head`, so the reachable set is the
//! 10 nearest first-parent commits of c25 and of c12.

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
    reason = "the shared TestDir has helpers this test does not call"
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

/// The warning the script prints when any read fails.
const STOPPED: &str = "cache pruning stopped on a failed read";

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

/// The fixture repositories and the commits the cache keys name.
struct Fixture {
    repo: TestDir,
    state: TestDir,
    #[allow(dead_code, reason = "kept with the remote it was pushed from")]
    origin: TestDir,
    c2: String,
    c3: String,
    c12: String,
    c15: String,
    c16: String,
    c25: String,
    pr_head: String,
}

impl Fixture {
    fn new() -> Self {
        let origin = TestDir::new("cporigin");
        let repo = TestDir::new("cprepo");
        let state = TestDir::new("cpstate");
        git(&repo, &["init", "-q", "-b", "main"]);
        for n in 1..=25 {
            git(
                &repo,
                &["commit", "-q", "--allow-empty", "-m", &format!("c{n}")],
            );
        }
        let c25 = git(&repo, &["rev-parse", "HEAD"]);
        let c16 = git(&repo, &["rev-parse", "HEAD~9"]);
        let c15 = git(&repo, &["rev-parse", "HEAD~10"]);
        let c12 = git(&repo, &["rev-parse", "HEAD~13"]);
        let c3 = git(&repo, &["rev-parse", "HEAD~22"]);
        let c2 = git(&repo, &["rev-parse", "HEAD~23"]);
        git(&repo, &["checkout", "-q", &c12]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "pr1"]);
        let pr_head = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["checkout", "-q", "main"]);
        git(&origin, &["init", "-q", "--bare", "-b", "main"]);
        let origin_path = origin.path().display().to_string();
        git(&repo, &["remote", "add", "origin", &origin_path]);
        git(&repo, &["push", "-q", "origin", "main:refs/heads/main"]);
        let pushed = format!("{pr_head}:refs/tmp/pr1");
        git(&repo, &["push", "-q", "origin", &pushed]);
        git(&origin, &["update-ref", "refs/pull/1/head", &pr_head]);
        git(&origin, &["update-ref", "-d", "refs/tmp/pr1"]);
        Self {
            repo,
            state,
            origin,
            c2,
            c3,
            c12,
            c15,
            c16,
            c25,
            pr_head,
        }
    }

    fn state_file(&self, name: &str, content: &str) -> PathBuf {
        let path = self.state.path().join(name);
        self.state.write(name, content);
        path
    }

    fn prs_file(&self, name: &str, numbers: &[&str]) -> PathBuf {
        let mut content = String::new();
        for number in numbers {
            content.push_str(number);
            content.push('\n');
        }
        self.state_file(name, &content)
    }

    /// Runs `scripts/cache-prune` with the stub gh on PATH. `vars` holds the
    /// FAKE_* settings for the run.
    fn prune(&self, vars: &[(&str, &str)], dry_run: bool) -> (Option<i32>, String) {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cache_prune_fixture/bin");
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/cache-prune");
        let path = format!("{}:{}", fixture.display(), std::env::var("PATH").unwrap());
        let mut command = Command::new("bash");
        command.arg(script);
        if dry_run {
            command.arg("--dry-run");
        }
        command
            .env("PATH", path)
            .env("GITHUB_REPOSITORY", "o/r")
            .env_remove("FAKE_PRS")
            .env_remove("FAKE_CACHES")
            .env_remove("FAKE_USAGE")
            .env_remove("FAKE_DELETE_LOG")
            .env_remove("FAKE_DELETE_FAIL")
            .envs(vars.iter().copied())
            .current_dir(self.repo.path());
        run("scripts/cache-prune", command)
    }

    fn deleted(&self, log: &Path) -> Vec<String> {
        let content = fs::read_to_string(log).unwrap_or_default();
        let mut ids: Vec<String> = content
            .lines()
            .map(|line| line.trim().to_owned())
            .filter(|line| !line.is_empty())
            .collect();
        ids.sort();
        ids
    }
}

/// One cache object of the array the stub prints for the caches list.
fn cache(id: u64, key: &str, gitref: &str, created_at: &str) -> String {
    format!("{{\"id\":{id},\"key\":{key:?},\"ref\":{gitref:?},\"created_at\":{created_at:?}}}")
}

fn array(entries: &[String]) -> String {
    format!("[{}]", entries.join(","))
}

/// The retention case's caches: release binaries at, just below and just
/// above each edge of the reachable set. Flipping `IN($keep[])` keeps the
/// doomed ones or deletes the kept ones.
fn retention_caches(fixture: &Fixture) -> String {
    array(&[
        cache(
            11,
            &format!("release-musl-{}", fixture.c25),
            "refs/heads/main",
            "2026-01-01T00:00:00Z",
        ),
        cache(
            12,
            &format!("release-musl-{}", fixture.c16),
            "refs/heads/main",
            "2026-01-01T00:00:00Z",
        ),
        cache(
            13,
            &format!("release-musl-{}", fixture.c15),
            "refs/heads/main",
            "2026-01-01T00:00:00Z",
        ),
        cache(
            14,
            &format!("release-musl-{}", fixture.c12),
            "refs/heads/main",
            "2026-01-01T00:00:00Z",
        ),
        cache(
            15,
            &format!("release-musl-{}", fixture.c3),
            "refs/heads/main",
            "2026-01-01T00:00:00Z",
        ),
        cache(
            16,
            &format!("release-musl-{}", fixture.c2),
            "refs/heads/main",
            "2026-01-01T00:00:00Z",
        ),
        cache(
            17,
            &format!("release-musl-{}", fixture.pr_head),
            "refs/heads/main",
            "2026-01-01T00:00:00Z",
        ),
    ])
}

fn retention_env(
    fixture: &Fixture,
    log_name: &str,
) -> (PathBuf, PathBuf, PathBuf, Vec<(String, String)>) {
    let prs = fixture.prs_file("prs.txt", &["1"]);
    let caches = fixture.state_file("caches.json", &retention_caches(fixture));
    let log = fixture.state.path().join(log_name);
    let vars = vec![
        ("FAKE_PRS".to_owned(), prs.display().to_string()),
        ("FAKE_CACHES".to_owned(), caches.display().to_string()),
        ("FAKE_USAGE".to_owned(), "12345".to_owned()),
        ("FAKE_DELETE_LOG".to_owned(), log.display().to_string()),
    ];
    (prs, caches, log, vars)
}

#[test]
fn retention_keeps_the_edges_and_deletes_past_them() {
    let fixture = Fixture::new();
    let (_prs, _caches, log, vars) = retention_env(&fixture, "delete.log");
    let var_refs: Vec<(&str, &str)> = vars
        .iter()
        .map(|pair| (pair.0.as_str(), pair.1.as_str()))
        .collect();
    let (code, out) = fixture.prune(&var_refs, false);
    assert_eq!(code, Some(0), "{out}");
    assert_eq!(fixture.deleted(&log), ["13", "16", "17"], "{out}");
}

#[test]
fn build_generations_keep_only_the_newest_of_each_group() {
    // Flipping `group_by([.ref, ...])` to the key alone deletes the other
    // ref; flipping `sub("-[^-]*$"; "")` to the whole key deletes nothing;
    // dropping the `.[1:]` newest-stays slice deletes the newest too.
    let fixture = Fixture::new();
    let prs = fixture.prs_file("prs.txt", &["1"]);
    let caches = fixture.state_file(
        "caches.json",
        &array(&[
            cache(
                21,
                "v0-rust-ci-Linux-x64-aaaa-1111",
                "refs/heads/main",
                "2026-01-01T00:00:00Z",
            ),
            cache(
                22,
                "v0-rust-ci-Linux-x64-aaaa-2222",
                "refs/heads/main",
                "2026-01-01T00:00:01Z",
            ),
            cache(
                23,
                "v0-rust-ci-Linux-x64-aaaa-3333",
                "refs/heads/main",
                "2026-01-01T00:00:02Z",
            ),
            cache(
                24,
                "v0-rust-ci-Darwin-arm64-bbbb-4444",
                "refs/heads/main",
                "2026-01-01T00:00:00Z",
            ),
            cache(
                25,
                "v0-rust-ci-Linux-x64-aaaa-5555",
                "refs/heads/feature",
                "2026-01-01T00:00:00Z",
            ),
            cache(26, "other-key", "refs/heads/main", "2026-01-01T00:00:00Z"),
        ]),
    );
    let log = fixture.state.path().join("delete.log");
    let vars = [
        ("FAKE_PRS", prs.display().to_string()),
        ("FAKE_CACHES", caches.display().to_string()),
        ("FAKE_USAGE", "12345".to_owned()),
        ("FAKE_DELETE_LOG", log.display().to_string()),
    ];
    let var_refs: Vec<(&str, &str)> = vars.iter().map(|pair| (pair.0, pair.1.as_str())).collect();
    let (code, out) = fixture.prune(&var_refs, false);
    assert_eq!(code, Some(0), "{out}");
    assert_eq!(fixture.deleted(&log), ["21", "22"], "{out}");
}

#[test]
fn a_failing_pr_list_deletes_nothing() {
    let fixture = Fixture::new();
    let caches = fixture.state_file(
        "caches.json",
        &array(&[cache(
            31,
            &format!("release-musl-{}", fixture.c2),
            "refs/heads/main",
            "2026-01-01T00:00:00Z",
        )]),
    );
    let log = fixture.state.path().join("delete.log");
    let vars = [
        ("FAKE_CACHES", caches.display().to_string()),
        ("FAKE_USAGE", "12345".to_owned()),
        ("FAKE_DELETE_LOG", log.display().to_string()),
    ];
    let var_refs: Vec<(&str, &str)> = vars.iter().map(|pair| (pair.0, pair.1.as_str())).collect();
    let (code, out) = fixture.prune(&var_refs, false);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains(STOPPED), "{out}");
    assert!(fixture.deleted(&log).is_empty(), "{out}");
}

#[test]
fn a_failing_fetch_deletes_nothing() {
    let fixture = Fixture::new();
    let prs = fixture.prs_file("prs.txt", &["999"]);
    let caches = fixture.state_file(
        "caches.json",
        &array(&[cache(
            31,
            &format!("release-musl-{}", fixture.c2),
            "refs/heads/main",
            "2026-01-01T00:00:00Z",
        )]),
    );
    let log = fixture.state.path().join("delete.log");
    let vars = [
        ("FAKE_PRS", prs.display().to_string()),
        ("FAKE_CACHES", caches.display().to_string()),
        ("FAKE_USAGE", "12345".to_owned()),
        ("FAKE_DELETE_LOG", log.display().to_string()),
    ];
    let var_refs: Vec<(&str, &str)> = vars.iter().map(|pair| (pair.0, pair.1.as_str())).collect();
    let (code, out) = fixture.prune(&var_refs, false);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains(STOPPED), "{out}");
    assert!(fixture.deleted(&log).is_empty(), "{out}");
}

#[test]
fn more_than_a_thousand_prs_deletes_nothing() {
    // The script aborts on the count before fetching: PR 1 has a ref, so a
    // fetch step would leave FETCH_HEAD behind. Flipping `-gt 1000` to
    // `-gt 1001` fetches PR 1 and the file appears, failing this test.
    // The `more than 1000` warning itself never reaches the output: it is
    // printed to stdout inside `reachable`, whose caller captures stdout
    // into $keep (possible script bug, reported, script not touched).
    let fixture = Fixture::new();
    let numbers: Vec<String> = (1..=1001).map(|n| n.to_string()).collect();
    let number_refs: Vec<&str> = numbers.iter().map(String::as_str).collect();
    let prs = fixture.prs_file("prs1001.txt", &number_refs);
    let caches = fixture.state_file(
        "caches.json",
        &array(&[cache(
            31,
            &format!("release-musl-{}", fixture.c2),
            "refs/heads/main",
            "2026-01-01T00:00:00Z",
        )]),
    );
    let log = fixture.state.path().join("delete.log");
    let vars = [
        ("FAKE_PRS", prs.display().to_string()),
        ("FAKE_CACHES", caches.display().to_string()),
        ("FAKE_USAGE", "12345".to_owned()),
        ("FAKE_DELETE_LOG", log.display().to_string()),
    ];
    let var_refs: Vec<(&str, &str)> = vars.iter().map(|pair| (pair.0, pair.1.as_str())).collect();
    let (code, out) = fixture.prune(&var_refs, false);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("more than 1000 open pull requests"), "{out}");
    assert!(out.contains(STOPPED), "{out}");
    assert!(fixture.deleted(&log).is_empty(), "{out}");
    assert!(
        !fixture.repo.path().join(".git/FETCH_HEAD").exists(),
        "no fetch ran: {out}"
    );
}

#[test]
fn exactly_a_thousand_prs_reaches_the_fetch_step() {
    // PR 1 has a ref, so the fetch step leaves FETCH_HEAD behind before
    // PR 2 (no ref) fails the run. Flipping `-gt 1000` to `-ge 1000`
    // aborts on the count instead and the file never appears.
    let fixture = Fixture::new();
    let numbers: Vec<String> = (1..=1000).map(|n| n.to_string()).collect();
    let number_refs: Vec<&str> = numbers.iter().map(String::as_str).collect();
    let prs = fixture.prs_file("prs1000.txt", &number_refs);
    let caches = fixture.state_file(
        "caches.json",
        &array(&[cache(
            31,
            &format!("release-musl-{}", fixture.c2),
            "refs/heads/main",
            "2026-01-01T00:00:00Z",
        )]),
    );
    let log = fixture.state.path().join("delete.log");
    let vars = [
        ("FAKE_PRS", prs.display().to_string()),
        ("FAKE_CACHES", caches.display().to_string()),
        ("FAKE_USAGE", "12345".to_owned()),
        ("FAKE_DELETE_LOG", log.display().to_string()),
    ];
    let var_refs: Vec<(&str, &str)> = vars.iter().map(|pair| (pair.0, pair.1.as_str())).collect();
    let (code, out) = fixture.prune(&var_refs, false);
    assert_eq!(code, Some(0), "{out}");
    assert!(!out.contains("more than 1000 open pull requests"), "{out}");
    assert!(out.contains(STOPPED), "{out}");
    assert!(fixture.deleted(&log).is_empty(), "{out}");
    assert!(
        fixture.repo.path().join(".git/FETCH_HEAD").exists(),
        "the fetch step ran: {out}"
    );
}

#[test]
fn a_failing_usage_read_deletes_nothing() {
    let fixture = Fixture::new();
    let prs = fixture.prs_file("prs.txt", &["1"]);
    let caches = fixture.state_file(
        "caches.json",
        &array(&[cache(
            31,
            &format!("release-musl-{}", fixture.c2),
            "refs/heads/main",
            "2026-01-01T00:00:00Z",
        )]),
    );
    let log = fixture.state.path().join("delete.log");
    let vars = [
        ("FAKE_PRS", prs.display().to_string()),
        ("FAKE_CACHES", caches.display().to_string()),
        ("FAKE_USAGE", "fail".to_owned()),
        ("FAKE_DELETE_LOG", log.display().to_string()),
    ];
    let var_refs: Vec<(&str, &str)> = vars.iter().map(|pair| (pair.0, pair.1.as_str())).collect();
    let (code, out) = fixture.prune(&var_refs, false);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains(STOPPED), "{out}");
    assert!(fixture.deleted(&log).is_empty(), "{out}");
}

#[test]
fn a_failing_caches_read_deletes_nothing() {
    // Covers the `caches=$(...) || return 1` read: without it the script
    // would delete from an empty list instead of stopping.
    let fixture = Fixture::new();
    let prs = fixture.prs_file("prs.txt", &["1"]);
    let missing = fixture.state.path().join("no-caches.json");
    let log = fixture.state.path().join("delete.log");
    let vars = [
        ("FAKE_PRS", prs.display().to_string()),
        ("FAKE_CACHES", missing.display().to_string()),
        ("FAKE_USAGE", "12345".to_owned()),
        ("FAKE_DELETE_LOG", log.display().to_string()),
    ];
    let var_refs: Vec<(&str, &str)> = vars.iter().map(|pair| (pair.0, pair.1.as_str())).collect();
    let (code, out) = fixture.prune(&var_refs, false);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains(STOPPED), "{out}");
    assert!(fixture.deleted(&log).is_empty(), "{out}");
}

#[test]
fn a_failing_delete_warns_and_keeps_deleting() {
    let fixture = Fixture::new();
    let (_prs, _caches, log, vars) = retention_env(&fixture, "delete.log");
    let mut with_fail: Vec<(String, String)> = vars;
    with_fail.push(("FAKE_DELETE_FAIL".to_owned(), "16".to_owned()));
    let var_refs: Vec<(&str, &str)> = with_fail
        .iter()
        .map(|pair| (pair.0.as_str(), pair.1.as_str()))
        .collect();
    let (code, out) = fixture.prune(&var_refs, false);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("could not delete cache 16"), "{out}");
    assert_eq!(fixture.deleted(&log), ["13", "17"], "{out}");
}

#[test]
fn dry_run_prints_without_deleting() {
    let fixture = Fixture::new();
    let (_prs, _caches, log, vars) = retention_env(&fixture, "delete.log");
    let var_refs: Vec<(&str, &str)> = vars
        .iter()
        .map(|pair| (pair.0.as_str(), pair.1.as_str()))
        .collect();
    let (code, out) = fixture.prune(&var_refs, true);
    assert_eq!(code, Some(0), "{out}");
    for id in ["13", "16", "17"] {
        assert!(out.contains(&format!("would delete cache {id}")), "{out}");
    }
    assert!(fixture.deleted(&log).is_empty(), "{out}");
}
