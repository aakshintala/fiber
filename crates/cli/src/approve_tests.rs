//! `run`: what it shows, what it asks and what it records, with the input
//! and both output streams in memory.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::ErrorCode;

const APPROVE_CHILD: &str = "FIBER_CLI_APPROVE_EXIT_CHILD";
const CHILD_DEADLINE: Duration = Duration::from_secs(5);
const REAP_DEADLINE: Duration = Duration::from_secs(5);

use super::{run, says_yes};
use fakes::Deadline;

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
    result: Result<(), contract::shapes::Failure>,
    out: String,
    err: String,
}

fn go(setup: &Setup, yes: bool, terminal: bool, input: &str) -> Run {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let result = run(
        &setup.home(),
        &setup.repo(),
        yes,
        terminal,
        super::Streams {
            input: &mut Cursor::new(input.as_bytes().to_vec()),
            out: &mut out,
            err: &mut err,
        },
        fakes::clock::FakeClock::new(),
    );
    Run {
        result,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
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
    assert_eq!(failure.code, ErrorCode::Usage);
    assert!(failure.message.contains("--yes"), "{}", failure.message);
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
    assert!(
        failure.message.contains("extension fiber.test/bad"),
        "{}",
        failure.message
    );
    assert_eq!(failure.code, ErrorCode::NonzeroExit);
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
            fakes::clock::FakeClock::new(),
        ));
    }

    let setup = Setup::new();
    let name = module_path!().split_once("::").unwrap().1;
    for (case, expected) in [("refuse", 2), ("yes", 0)] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("{name}::approve_returns_zero_on_success_and_two_on_refusal"),
                "--nocapture",
                "--test-threads=1",
            ])
            .env(APPROVE_CHILD, case)
            .env("FIBER_HOME", setup.home())
            .current_dir(setup.repo())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let (status_tx, status_rx) = mpsc::channel();
        thread::spawn(move || {
            let _sent = status_tx.send(child.wait().unwrap());
        });
        let status = match Deadline::after(CHILD_DEADLINE).recv(&status_rx) {
            Ok(status) => status,
            Err(_) => {
                let _killed = fakes::kill_pid(pid, "KILL").unwrap();
                Deadline::after(REAP_DEADLINE)
                    .recv(&status_rx)
                    .expect("the killed approve child is reaped");
                panic!("waited {CHILD_DEADLINE:?} for `fiber approve` ({case}) to exit");
            }
        };
        assert_eq!(status.code(), Some(expected), "{case}");
    }
}

#[test]
fn says_yes_matches_exactly_y_and_yes() {
    for yes in ["y", "Y", "yes", "YES", "yEs", " yes\n"] {
        assert!(says_yes(yes), "{yes:?}");
    }
    for no in ["", "n", "ye", "yess", "yy", "no", "y y"] {
        assert!(!says_yes(no), "{no:?}");
    }
}
