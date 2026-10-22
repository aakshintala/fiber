//! `run`: what it shows, what it asks and what it records, with the input
//! and both output streams in memory.

use fakes::MUST_SUCCEED_WITHIN;
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use contract::ErrorCode;
use extensions::{Fetch, Origin};
use fakes::Deadline;

const APPROVE_CHILD: &str = "FIBER_CLI_APPROVE_EXIT_CHILD";

/// How long an approval run may take before the test fails instead of
/// hanging it.
const RUN_WITHIN: Duration = Duration::from_secs(10);

use super::run;
use crate::CliError;

/// A git repository declaring one hook, with Fiber home beside it.
struct Setup {
    tmp: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let tmp = fakes::TempDir::new("fiber-approve");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join(".fiber/config")).unwrap();
        fs::create_dir_all(repo.join("scripts")).unwrap();
        fs::create_dir_all(tmp.path().join("home")).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .arg(&repo)
                .status()
                .unwrap()
                .success()
        );
        fs::write(repo.join("scripts/fmt.sh"), "cargo fmt\n").unwrap();
        fs::write(
            repo.join(".fiber/config/hooks.json"),
            r#"{"hooks": {"fmt": {"point": "after_tool", "command": "scripts/fmt.sh"}}}"#,
        )
        .unwrap();
        Self { tmp }
    }

    fn repo(&self) -> PathBuf {
        self.tmp.path().join("repo")
    }

    fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }

    fn approvals(&self) -> usize {
        let dir = fs::read_dir(self.home().join("projects"))
            .ok()
            .and_then(|mut projects| projects.next())
            .map(|project| project.unwrap().path().join("approvals"));
        dir.and_then(|dir| fs::read_dir(dir).ok())
            .map_or(0, Iterator::count)
    }
}

struct Run {
    result: Result<(), CliError>,
    out: String,
    err: String,
}

fn go(setup: &Setup, yes: bool, terminal: bool, input: &str) -> Run {
    go_version(setup, yes, terminal, input, "0.1.0", missing_origin())
}

/// Nothing may fetch from here: any fetch attempt fails, so a run that
/// answers without fetching proves it never tried.
fn missing_origin() -> Origin {
    Origin::new("fiber-no-such-git-program", |name| name.to_owned())
}

fn go_version(
    setup: &Setup,
    yes: bool,
    terminal: bool,
    input: &str,
    fiber: &str,
    origin: Origin,
) -> Run {
    // `run` blocks on fetching and install steps, so it runs on a worker
    // under one deadline: a stall fails the test instead of hanging it.
    let home = setup.home();
    let repo = setup.repo();
    let fiber = fiber.to_owned();
    let input = input.as_bytes().to_vec();
    let (done, ran) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
        let fetch = Fetch::new(origin, fiber, Arc::clone(&clock));
        let result = run(
            &home,
            &repo,
            yes,
            terminal,
            super::Streams {
                input: &mut Cursor::new(input),
                out: &mut out,
                err: &mut err,
            },
            Arc::clone(&clock),
            fetch,
        );
        let _sent = done.send(Run {
            result,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        });
    });
    let bound = Deadline::after(RUN_WITHIN);
    let ran = bound.recv_or_fail(&ran, "the approval run");
    worker.join().unwrap();
    ran
}

#[test]
fn yes_approves_without_asking_and_prints_each_approval() {
    let setup = Setup::new();
    let ran = go(&setup, true, false, "");
    assert!(ran.result.is_ok());
    assert!(
        ran.out.starts_with("hook fmt (.fiber/config/"),
        "{}",
        ran.out
    );
    assert!(
        ran.out.contains("\n  runs: scripts/fmt.sh\n"),
        "{}",
        ran.out
    );
    let line = ran.err.trim_end();
    let words: Vec<&str> = line.split(' ').collect();
    assert_eq!(&words[..3], ["approved", "hook", "fmt"], "{line}");
    assert_eq!(words[3].len(), 12, "{line}");
    assert_eq!(words.len(), 4, "{line}");
    assert!(!ran.err.contains(super::PROMPT));
    assert_eq!(setup.approvals(), 1);
}

#[test]
fn a_second_run_has_nothing_to_approve() {
    let setup = Setup::new();
    go(&setup, true, false, "").result.unwrap();
    let ran = go(&setup, true, false, "");
    assert!(ran.result.is_ok());
    assert_eq!(ran.err, "nothing to approve\n");
    assert_eq!(ran.out, "");
}

#[test]
fn an_answer_of_y_or_yes_in_any_case_approves_everything_shown() {
    for answer in ["y\n", "Y\n", "yes\n", "YES\n", "Yes\n", "  y  \n", "y"] {
        let setup = Setup::new();
        let ran = go(&setup, false, true, answer);
        assert!(ran.result.is_ok(), "{answer:?}");
        assert!(
            ran.err.starts_with("approve all? [y/N] "),
            "{answer:?}: {}",
            ran.err
        );
        assert!(
            ran.err.contains("approved hook fmt "),
            "{answer:?}: {}",
            ran.err
        );
        assert_eq!(setup.approvals(), 1, "{answer:?}");
    }
}

#[test]
fn any_other_answer_records_nothing() {
    for answer in [
        "n\n", "no\n", "\n", "yy\n", "ye\n", "yeah\n", "y es\n", "sure\n",
    ] {
        let setup = Setup::new();
        let ran = go(&setup, false, true, answer);
        assert!(ran.result.is_ok(), "{answer:?}");
        assert!(
            ran.err.ends_with("nothing approved\n"),
            "{answer:?}: {}",
            ran.err
        );
        assert!(!ran.err.contains("approved hook"), "{answer:?}");
        assert_eq!(setup.approvals(), 0, "{answer:?}");
    }
}

#[test]
fn a_piped_answer_counts_when_there_is_no_terminal() {
    let setup = Setup::new();
    assert!(go(&setup, false, false, "n\n").result.is_ok());
    assert_eq!(setup.approvals(), 0);
    assert!(go(&setup, false, false, "y\n").result.is_ok());
    assert_eq!(setup.approvals(), 1);
}

#[test]
fn no_terminal_and_no_input_refuses_naming_yes_and_records_nothing() {
    let setup = Setup::new();
    let ran = go(&setup, false, false, "");
    let failure = ran.result.unwrap_err();
    assert_eq!(failure.code(), ErrorCode::Usage);
    let CliError::NobodyToAsk { message } = &failure else {
        panic!("expected NobodyToAsk, got {failure:?}");
    };
    assert!(message.contains("--yes"), "{message}");
    assert_eq!(setup.approvals(), 0);
    assert!(!setup.home().join("pinned").exists());
}

#[test]
fn a_terminal_that_gives_end_of_input_approves_nothing() {
    let setup = Setup::new();
    let ran = go(&setup, false, true, "");
    assert!(ran.result.is_ok());
    assert!(ran.err.ends_with("nothing approved\n"), "{}", ran.err);
    assert_eq!(setup.approvals(), 0);
}

#[test]
fn a_failure_names_the_item_and_leaves_the_earlier_ones_recorded() {
    let setup = Setup::new();
    for (dir, name, install) in [
        (
            "good",
            "fiber.test/good",
            r#", "install": ["sh", "-c", "true"]"#,
        ),
        (
            "bad",
            "fiber.test/bad",
            r#", "install": ["sh", "-c", "exit 1"]"#,
        ),
    ] {
        let pkg = setup.repo().join(dir);
        fs::create_dir_all(&pkg).unwrap();
        fs::write(
            pkg.join("extension.json"),
            format!(
                r#"{{"name": "{name}", "version": "v1.0.0", "fiber": "0.1.0", "api": 1{install}}}"#
            ),
        )
        .unwrap();
    }
    fs::write(
        setup.repo().join(".fiber/config.json"),
        r#"{"repository_extensions": [{"path": "good"}, {"path": "bad"}]}"#,
    )
    .unwrap();
    let ran = go(&setup, true, false, "");
    let failure = ran.result.unwrap_err();
    let CliError::ApproveFailed { kind, name, .. } = &failure else {
        panic!("expected ApproveFailed, got {failure:?}");
    };
    assert_eq!(*kind, "extension");
    assert_eq!(name, "fiber.test/bad");
    assert_eq!(failure.code(), ErrorCode::NonzeroExit);
    // The extension before it is recorded; the hook after it is not.
    assert_eq!(setup.approvals(), 1);
    assert!(
        ran.err.contains("approved extension fiber.test/good "),
        "{}",
        ran.err
    );
    assert!(!ran.err.contains("approved hook"), "{}", ran.err);
    // Run again without the failing extension: the hook approves.
    fs::write(
        setup.repo().join(".fiber/config.json"),
        r#"{"repository_extensions": [{"path": "good"}]}"#,
    )
    .unwrap();
    go(&setup, true, false, "").result.unwrap();
    assert_eq!(setup.approvals(), 2);
}

#[test]
fn approve_returns_zero_on_success_and_two_on_refusal() {
    if let Some(case) = std::env::var_os(APPROVE_CHILD) {
        std::process::exit(super::approve(
            case == "yes",
            env!("CARGO_PKG_VERSION"),
            fakes::clock::FakeClock::new(),
        ));
    }

    let setup = Setup::new();
    let name = module_path!().split_once("::").unwrap().1;
    for (case, expected) in [("refuse", 2), ("yes", 0)] {
        let output = fakes::rerun_prepared(
            &format!("{name}::approve_returns_zero_on_success_and_two_on_refusal"),
            &[(APPROVE_CHILD, case)],
            Deadline::after(MUST_SUCCEED_WITHIN),
            |command| {
                command
                    .env("FIBER_HOME", setup.home())
                    .current_dir(setup.repo());
            },
        );
        assert_eq!(output.status.code(), Some(expected), "{case}");
    }
}

/// A git repository holding one tagged extension, fetched by name.
struct Deps {
    tmp: fakes::TempDir,
}

impl Deps {
    fn new() -> Self {
        Self {
            tmp: fakes::TempDir::new("fiber-approve-deps"),
        }
    }

    /// Commit `manifest` as the package of `repo` (empty `dir` for its
    /// root), tagged `tag`.
    fn tag(&self, repo: &str, manifest: &str, tag: &str) {
        let path = self.tmp.path().join(repo.replace('/', "-"));
        if !path.exists() {
            fs::create_dir_all(&path).unwrap();
            fakes::git::git(&path, &["init", "--quiet"]);
        }
        fs::create_dir_all(path.join("providers")).unwrap();
        fs::write(path.join("extension.json"), manifest).unwrap();
        fs::write(
            path.join("providers/acme.json"),
            r#"{"name": "acme", "credential": {"env": "FIBER_TEST_UNSET_KEY"}, "models": [{"id": "m1", "protocol": "openai-responses", "base_url": "http://127.0.0.1:1/v1", "context_window": 1000}]}"#,
        )
        .unwrap();
        fakes::git::git(&path, &["add", "."]);
        fakes::git::git(
            &path,
            &["commit", "--quiet", "--allow-empty", "-m", "release"],
        );
        if !tag.is_empty() {
            fakes::git::git(&path, &["tag", tag]);
        }
    }

    fn origin(&self) -> Origin {
        let root = self.tmp.path().to_path_buf();
        Origin::new("git", move |repo| {
            let path = root.join(repo.replace('/', "-"));
            if path.is_dir() {
                format!("file://{}", path.display())
            } else {
                "file:///nonexistent-repository".to_owned()
            }
        })
    }
}

/// Declare the package at `dir` with this manifest in the approved
/// repository.
fn declare(setup: &Setup, dir: &str, manifest: &str) {
    let pkg = setup.repo().join(dir);
    fs::create_dir_all(&pkg).unwrap();
    fs::write(pkg.join("extension.json"), manifest).unwrap();
    fs::write(pkg.join("init.lua"), "-- entry\n").unwrap();
    fs::write(
        setup.repo().join(".fiber/config.json"),
        format!(r#"{{"repository_extensions": [{{"path": "{dir}"}}]}}"#),
    )
    .unwrap();
}

fn manifest(name: &str, depends: &str, fiber: &str) -> String {
    format!(r#"{{"name": "{name}", "version": "v1.0.0", "fiber": "{fiber}", "api": 1{depends}}}"#)
}

fn depends_on(dep: &str, minimum: &str) -> String {
    format!(r#", "depends": {{"{dep}": "{minimum}"}}"#)
}

#[test]
fn yes_installs_the_dependencies_inside_each_pinned_copy() {
    let setup = Setup::new();
    let deps = Deps::new();
    let helper = "example.com/acme/helper";
    for tag in ["v1.1.0", "v1.2.0"] {
        deps.tag(helper, &manifest(helper, "", "0.1.0"), tag);
    }
    let root = "fiber.test/root";
    declare(
        &setup,
        "pkg",
        &manifest(root, &depends_on(helper, "1.2"), "0.1.0"),
    );
    let ran = go_version(&setup, true, false, "", "0.1.0", deps.origin());
    assert!(ran.result.is_ok(), "{:?}", ran.result);
    // What the person saw: the root, with its version and path.
    assert!(
        ran.out.starts_with("extension fiber.test/root (pkg)"),
        "{}",
        ran.out
    );
    assert!(ran.out.contains("\n  version: v1.0.0\n"), "{}", ran.out);
    let line = ran.err.trim_end();
    assert!(
        line.starts_with("approved extension fiber.test/root "),
        "{line}"
    );
    // The lowest tag meeting the minimum installed inside the root's copy
    // (the setup's hook has its own copy with no dependencies), with an
    // unrequested record and no floor.
    let root = fs::read_dir(setup.home().join("pinned"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.') && !n.ends_with(".ready"))
        .find(|n| {
            setup
                .home()
                .join("pinned")
                .join(n)
                .join(".fiber-depends")
                .is_dir()
        })
        .unwrap();
    let record = fs::read_to_string(
        setup
            .home()
            .join("pinned")
            .join(root)
            .join(".fiber-depends/extensions/example.com-acme-helper/.fiber.json"),
    )
    .unwrap();
    assert!(record.contains(r#""version":"v1.2.0""#), "{record}");
    assert!(record.contains(r#""requested":false"#), "{record}");
    assert!(!record.contains("floor"), "{record}");
    // The root and the setup's hook.
    assert_eq!(setup.approvals(), 2);
}

#[test]
fn a_declined_offer_fetches_nothing() {
    let setup = Setup::new();
    let root = "fiber.test/root";
    declare(
        &setup,
        "pkg",
        &manifest(
            root,
            &depends_on("example.com/acme/missing", "1.0"),
            "0.1.0",
        ),
    );
    // A missing git program: any fetch attempt fails, so declining must
    // succeed without fetching.
    let ran = go(&setup, false, false, "n\n");
    assert!(ran.result.is_ok(), "{:?}", ran.result);
    assert!(ran.err.ends_with("nothing approved\n"), "{}", ran.err);
    assert_eq!(setup.approvals(), 0);
    assert!(!setup.home().join("pinned").exists());
}

#[test]
fn a_dependency_failure_keeps_earlier_approvals_and_skips_the_rest() {
    let setup = Setup::new();
    let deps = Deps::new();
    let good = "fiber.test/good";
    declare(&setup, "good", &manifest(good, "", "0.1.0"));
    let bad = "fiber.test/bad";
    declare(
        &setup,
        "bad",
        &manifest(bad, &depends_on("example.com/acme/missing", "1.0"), "0.1.0"),
    );
    fs::write(
        setup.repo().join(".fiber/config.json"),
        r#"{"repository_extensions": [{"path": "good"}, {"path": "bad"}]}"#,
    )
    .unwrap();
    let ran = go_version(&setup, true, false, "", "0.1.0", deps.origin());
    let failure = ran.result.unwrap_err();
    // Pinned stdout, stderr and code: both extensions were shown, only the
    // first approved, and the hook after the failure never ran.
    assert!(
        ran.out.contains("extension fiber.test/good "),
        "{}",
        ran.out
    );
    assert!(ran.out.contains("extension fiber.test/bad "), "{}", ran.out);
    assert!(ran.out.contains("hook fmt "), "{}", ran.out);
    assert!(
        ran.err.contains("approved extension fiber.test/good "),
        "{}",
        ran.err
    );
    assert!(
        !ran.err.contains("approved extension fiber.test/bad"),
        "{}",
        ran.err
    );
    assert!(!ran.err.contains("approved hook"), "{}", ran.err);
    let CliError::ApproveFailed { kind, name, .. } = &failure else {
        panic!("expected ApproveFailed, got {failure:?}");
    };
    assert_eq!(*kind, "extension");
    assert_eq!(name, "fiber.test/bad");
    assert_eq!(failure.code(), ErrorCode::ExtensionNotFound);
    assert_eq!(setup.approvals(), 1);
    // Without the failing extension the hook approves.
    fs::write(
        setup.repo().join(".fiber/config.json"),
        r#"{"repository_extensions": [{"path": "good"}]}"#,
    )
    .unwrap();
    go(&setup, true, false, "").result.unwrap();
    assert_eq!(setup.approvals(), 2);
}

#[test]
fn the_passed_fiber_version_decides_dependency_compatibility() {
    let setup = Setup::new();
    let deps = Deps::new();
    let helper = "example.com/acme/helper";
    deps.tag(helper, &manifest(helper, "", "0.2.0"), "v1.0.0");
    let root = "fiber.test/root";
    declare(
        &setup,
        "pkg",
        &manifest(root, &depends_on(helper, "1.0"), "0.1.0"),
    );
    // The dependency needs Fiber 0.2.0: the passed 0.1.0 refuses it, while
    // the passed 0.2.0 installs it, so the check runs on the argument.
    let refused = go_version(&setup, true, false, "", "0.1.0", deps.origin());
    let failure = refused.result.unwrap_err();
    let CliError::ApproveFailed { kind, name, .. } = &failure else {
        panic!("expected ApproveFailed, got {failure:?}");
    };
    assert_eq!(*kind, "extension");
    assert_eq!(name, "fiber.test/root");
    assert!(
        failure.to_string().contains("needs Fiber 0.2.0"),
        "{failure}"
    );
    assert_eq!(failure.code(), ErrorCode::ExtensionIncompatible);
    assert_eq!(setup.approvals(), 0);
    let ran = go_version(&setup, true, false, "", "0.2.0", deps.origin());
    assert!(ran.result.is_ok(), "{:?}", ran.result);
    // The root and the setup's hook.
    assert_eq!(setup.approvals(), 2);
}
