//! Binary-level tests of `fiber completion <shell>` (`docs/testing.md`,
//! "Levels"; `docs/invocation.md`, "Commands and flags"): the built
//! `fiber` prints each script, and the script loads in that shell and
//! completes exactly the visible commands and flags. The shells run the
//! checked-in drivers in `tests/completion/` through their interpreter.
//! Every process runs under a wall-clock deadline, and every shell carries
//! the test's temporary directory on its command line, so a process that
//! leaves its group is still found and killed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use crate::support;

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::clock::FakeClock;
use fakes::{Watchdog, group_empties, kill_group, kill_matching, try_matching_exits};
use support::{Deadline, Setup};

/// How long one `fiber` or shell run may take.
const RUN: Duration = Duration::from_secs(20);

/// Runs one cleanup step on its own thread and returns failures so later
/// cleanup checks still run before the test reports them. Each step's child
/// wait is half the outer wait, so the inner step expires first.
#[track_caller]
fn within_cleanup<T: Send + 'static>(
    what: &str,
    wait: Deadline,
    work: impl FnOnce(Deadline) -> T + Send + 'static,
) -> Result<T, String> {
    let bound = wait.left();
    let inner = wait.within(bound / 2);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let _sent = done.send(work(inner));
    });
    match wait.recv(&finished) {
        Ok(result) => Ok(result),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(format!("waited {bound:?} for {what}")),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(format!("{what} worker stopped without a result"))
        }
    }
}

#[test]
fn within_cleanup_hands_its_closure_half_of_what_the_wait_has_left() {
    let clock = Box::leak(Box::new(FakeClock::new()));
    let wait = Deadline::on(&**clock);
    let half = wait.left() / 2;
    let child_left = within_cleanup("a step", wait, |inner| inner.left()).unwrap();
    assert_eq!(child_left, half);
}

#[test]
fn a_matching_exits_step_gets_twice_the_child_budget_of_a_plain_wait() {
    assert_eq!(matching_exits_wait(), fakes::MUST_SUCCEED_WITHIN * 2);
}

/// The grammar a person completes, taken from the parser: every visible
/// command path, its subcommands, and its flags.
const GRAMMAR: &[(&str, &[&str], &[&str])] = &[
    (
        "",
        &[
            "update",
            "upgrade",
            "ask",
            "resume",
            "continue",
            "sessions",
            "models",
            "reviewer",
            "extension",
            "mcp",
            "setup",
            "approve",
            "config",
            "import",
            "login",
            "logout",
            "doctor",
            "stop",
            "feedback",
            "completion",
            "version",
            "help",
            "hub",
        ],
        &["--model", "-c", "--hub", "-h", "--help", "-v", "--version"],
    ),
    ("update", &[], &["--channel", "-h", "--help"]),
    (
        "ask",
        &[],
        &[
            "--model",
            "-c",
            "--resume",
            "--credential",
            "--worktree",
            "-h",
            "--help",
        ],
    ),
    ("models", &[], &["--json", "-h", "--help"]),
    ("reviewer", &["evidence"], &["-h", "--help"]),
    ("reviewer evidence", &[], &["--json", "-h", "--help"]),
    ("resume", &[], &["--credential", "--hub", "-h", "--help"]),
    ("continue", &[], &["--hub", "-h", "--help"]),
    (
        "setup",
        &[],
        &[
            "--provider",
            "--model",
            "--thinking",
            "--import",
            "--completions",
            "--no-completions",
            "--hub",
            "--no-hub",
            "-h",
            "--help",
        ],
    ),
    ("approve", &[], &["--yes", "-h", "--help"]),
    ("login", &[], &["--as", "--device", "-h", "--help"]),
    ("logout", &[], &["--as", "--all", "-h", "--help"]),
    ("doctor", &[], &["-h", "--help"]),
    ("stop", &[], &["--hub", "-h", "--help"]),
    ("feedback", &[], &["--session", "-h", "--help"]),
    ("completion", &[], &["-h", "--help"]),
    ("version", &[], &["-h", "--help"]),
    ("help", &[], &["-h", "--help"]),
    (
        "sessions",
        &["archive", "delete", "export", "prune", "search"],
        &["--all", "--archived", "--json", "--hub", "-h", "--help"],
    ),
    ("sessions archive", &[], &["--hub", "-h", "--help"]),
    (
        "sessions delete",
        &[],
        &["--hub", "--cascade", "--yes", "-h", "--help"],
    ),
    ("sessions export", &[], &["-h", "--help"]),
    ("sessions search", &[], &["--all", "--json", "-h", "--help"]),
    (
        "sessions prune",
        &[],
        &[
            "--hub",
            "--older-than",
            "--cascade",
            "--dry-run",
            "--yes",
            "--force",
            "-h",
            "--help",
        ],
    ),
    (
        "extension",
        &["install", "update", "remove", "reinstall", "list", "test"],
        &["-h", "--help"],
    ),
    ("extension install", &[], &["--project", "-h", "--help"]),
    ("extension update", &[], &["-h", "--help"]),
    ("extension remove", &[], &["-h", "--help"]),
    ("extension reinstall", &[], &["-h", "--help"]),
    ("extension list", &[], &["-h", "--help"]),
    ("extension test", &[], &["-h", "--help"]),
    (
        "mcp",
        &["add", "remove", "list", "login", "logout"],
        &["-h", "--help"],
    ),
    (
        "mcp add",
        &[],
        &["--project", "--repo", "-e", "--secret", "-h", "--help"],
    ),
    ("mcp remove", &[], &["--project", "--repo", "-h", "--help"]),
    ("mcp list", &[], &["-h", "--help"]),
    ("mcp login", &[], &["-h", "--help"]),
    ("mcp logout", &[], &["-h", "--help"]),
    ("config", &["get", "set"], &["-h", "--help"]),
    ("config get", &[], &["-h", "--help"]),
    ("config set", &[], &["--project", "--repo", "-h", "--help"]),
    ("import", &[], &["-h", "--help"]),
    (
        "hub",
        &[
            "add",
            "remove",
            "install",
            "uninstall",
            "status",
            "refresh",
            "pair",
            "token",
        ],
        &["-h", "--help"],
    ),
    ("hub add", &[], &["--default", "-h", "--help"]),
    ("hub remove", &[], &["-h", "--help"]),
    ("hub install", &[], &["--port", "-h", "--help"]),
    ("hub uninstall", &[], &["-h", "--help"]),
    ("hub status", &[], &["--json", "-h", "--help"]),
    ("hub refresh", &[], &["--hub", "-h", "--help"]),
    ("hub pair", &[], &["--address", "--hub", "-h", "--help"]),
    ("hub token", &["list", "revoke"], &["-h", "--help"]),
    ("hub token list", &[], &["--hub", "-h", "--help"]),
    ("hub token revoke", &[], &["--hub", "-h", "--help"]),
];

/// `fiber <path> ` with a trailing space, or `fiber ` for the top level.
fn line(path: &str, word: &str) -> String {
    if path.is_empty() {
        format!("fiber {word}")
    } else {
        format!("fiber {path} {word}")
    }
}

fn set(words: &[&str]) -> BTreeSet<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// The outer wait a `try_matching_exits` cleanup step gets: twice
/// `fakes::MUST_SUCCEED_WITHIN`, so `within_cleanup`'s halving leaves the
/// closure a full `MUST_SUCCEED_WITHIN`. That strictly exceeds the listing
/// and `pgrep` bounds inside `fakes`, which are fractions of the same const,
/// so the inner step expires first and the failure names it
/// (`docs/testing.md`, "Waits and timeouts").
fn matching_exits_wait() -> Duration {
    fakes::MUST_SUCCEED_WITHIN * 2 // ruling: #4078
}

/// Runs `command` to its exit, in its own process group, with stdin a pipe
/// the test holds open and never writes to: a run that read stdin would
/// block until `RUN` expires. After the exit, the group and every process
/// whose command line holds `tag` must be gone within the outer wait; each
/// step's child wait is half the outer, so the inner step expires first.
/// On expiry, both are killed and reaped before the test fails.
#[track_caller]
fn run_bounded(what: &str, mut command: Command, tag: &Path) -> Output {
    let tag = tag.to_str().unwrap().to_owned();
    let mut child = command
        .stdin(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let stdin = child.stdin.take();
    let group = child.id();
    let group_dog = Watchdog::group(group);
    let tag_dog = Watchdog::matching(&tag);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap_or(()));
    let Ok(output) = Deadline::after(RUN).recv(&finished) else {
        // Both bounded kills finish before either process check starts.
        let group_killed = within_cleanup(
            &format!("killing {what}'s process group"),
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move |_| kill_group(group, "KILL"),
        );
        let tag_for_kill = tag.clone();
        let tag_killed = within_cleanup(
            &format!("killing processes matching {tag}"),
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move |inner| kill_matching(&tag_for_kill, inner.left() / 2),
        );
        let tag_for_wait = tag.clone();
        let group_gone = within_cleanup(
            &format!("{what}'s process group to empty"),
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move |inner| group_empties(group, inner),
        );
        let tag_gone = within_cleanup(
            &format!("processes matching {tag} to exit"),
            Deadline::after(matching_exits_wait()), // ruling: #4078
            move |inner| try_matching_exits(&tag_for_wait, inner),
        );
        drop((group_dog, tag_dog));
        panic!(
            "waited {RUN:?} for {what} to exit; killing its group: {group_killed:?}, \
             group empty: {group_gone:?}; killing processes matching {tag}: {tag_killed:?}, \
             all gone: {tag_gone:?}"
        );
    };
    let tag_for_wait = tag.clone();
    let group_gone = within_cleanup(
        &format!("{what}'s process group to empty"),
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move |inner| group_empties(group, inner),
    );
    let tag_gone = within_cleanup(
        &format!("processes matching {tag} to exit"),
        Deadline::after(matching_exits_wait()), // ruling: #4078
        move |inner| try_matching_exits(&tag_for_wait, inner),
    );
    let stand_down = if group_gone.as_ref() == Ok(&true) && matches!(&tag_gone, Ok(Ok(()))) {
        let group_stood_down = within_cleanup(
            "the group watchdog to exit",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move |inner| {
                group_dog.stand_down(inner);
            },
        );
        let tag_stood_down = within_cleanup(
            "the matching watchdog to exit",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move |inner| {
                tag_dog.stand_down(inner);
            },
        );
        Some((group_stood_down, tag_stood_down))
    } else {
        // A watchdog that is dropped, not stood down, kills what is left.
        // Drop before the asserts so the kill starts before the failure.
        drop((group_dog, tag_dog));
        None
    };
    drop(stdin);
    assert!(
        group_gone.as_ref() == Ok(&true),
        "{what} left a process in its group behind: {group_gone:?}"
    );
    assert!(
        matches!(&tag_gone, Ok(Ok(()))),
        "{what} left a process matching its directory behind: {tag_gone:?}"
    );
    if let Some((group_stood_down, tag_stood_down)) = stand_down {
        assert!(
            group_stood_down.is_ok(),
            "{what}'s group watchdog did not exit: {group_stood_down:?}"
        );
        assert!(
            tag_stood_down.is_ok(),
            "{what}'s matching watchdog did not exit: {tag_stood_down:?}"
        );
    }
    output.unwrap()
}

/// `fiber completion <shell>` with `FIBER_HOME` a regular file: a run that
/// read home would fail on it.
#[track_caller]
fn fiber(setup: &Setup, args: &[&str]) -> Command {
    let home = setup.root.path().join("home-is-a-file");
    fs::write(&home, "not a directory").unwrap();
    let mut command = setup.fiber(args);
    command.env("FIBER_HOME", home);
    command
}

/// The script, from a run that exited 0 with nothing on stderr.
#[track_caller]
fn script(setup: &Setup, shell: &str) -> String {
    let output = run_bounded(
        &format!("fiber completion {shell}"),
        fiber(setup, &["completion", shell]),
        setup.root.path(),
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "",
        "fiber completion {shell}"
    );
    assert_eq!(output.status.code(), Some(0), "fiber completion {shell}");
    String::from_utf8(output.stdout).unwrap()
}

/// A directory holding one file, for the shells to run in: a completion
/// that fell back to file names would offer it.
fn cwd_with_a_file(setup: &Setup) -> PathBuf {
    let dir = setup.root.path().join("cwd");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("a-file"), "").unwrap();
    dir
}

/// A shell started with nothing from the test's environment but `PATH`.
fn shell(setup: &Setup, interpreter: &str) -> Command {
    let mut command = Command::new(interpreter);
    command
        .current_dir(cwd_with_a_file(setup))
        .env_clear()
        .envs(fakes::check_run())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", setup.root.path())
        .env("FIBER_HOME", setup.home())
        .env("FIBER_TEST_FAKE_KEY", "sk-test")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Each case's candidates, in the order the driver printed the cases.
fn cases(stdout: &str) -> Vec<BTreeSet<String>> {
    let mut found = Vec::new();
    let mut current: Option<BTreeSet<String>> = None;
    for text in stdout.lines() {
        if text.starts_with("CASE ") {
            current = Some(BTreeSet::new());
        } else if text == "CASE-END" {
            found.push(current.take().unwrap());
        } else if let Some(word) = text.strip_prefix("CAND:") {
            current.as_mut().unwrap().insert(word.to_owned());
        }
    }
    found
}

/// Checks each case's candidates against what it expects, by position.
fn assert_cases(what: &str, stdout: &str, expected: &[(String, BTreeSet<String>)]) {
    let found = cases(stdout);
    assert_eq!(found.len(), expected.len(), "{what}\n{stdout}");
    for ((case, want), got) in expected.iter().zip(&found) {
        assert_eq!(got, want, "{what}: `{case}`\n{stdout}");
    }
}

#[test]
fn the_bash_script_loads_and_completes_exactly_the_commands_and_flags() {
    let setup = Setup::new();
    install_dynamic_candidates(&setup);
    let path = setup.root.path().join("fiber.bash");
    fs::write(&path, script(&setup, "bash")).unwrap();

    let mut expected = vec![
        ("fiber ext".to_owned(), set(&["extension"])),
        ("fiber extension in".to_owned(), set(&["install"])),
    ];
    for (path, subcommands, flags) in GRAMMAR {
        let mut both = set(subcommands);
        both.extend(set(flags));
        if *path == "extension remove" {
            both.extend(set(&["example/installed", "model-extension", "fake"]));
        }
        expected.push((line(path, ""), both));
        expected.push((line(path, "-"), set(flags)));
    }
    expected.extend([
        (
            "fiber extension remove example/".to_owned(),
            set(&["example/installed"]),
        ),
        ("fiber --model model-".to_owned(), set(&["model-shell"])),
        ("fiber ask --model ".to_owned(), set(&["m", "model-shell"])),
        (
            "fiber setup --model model-".to_owned(),
            set(&["model-shell"]),
        ),
        ("fiber extension install path-prefix".to_owned(), set(&[])),
        ("fiber login provider-prefix".to_owned(), set(&[])),
        ("fiber completion b".to_owned(), set(&[])),
        ("fiber session --model model-".to_owned(), set(&[])),
    ]);

    let driver = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/completion/bash-driver.bash");
    let mut bash = shell(&setup, "bash");
    bash.arg("--norc")
        .arg("--noprofile")
        .arg(&driver)
        .arg(&path)
        .args(expected.iter().map(|(case, _)| case));
    let output = run_bounded("the bash driver", bash, setup.root.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");

    let (head, rest) = stdout.split_once("CASE 1\n").unwrap();
    let (errors, complete) = head
        .strip_prefix("SOURCE-STDERR\n")
        .unwrap()
        .split_once("COMPLETE\n")
        .unwrap();
    assert_eq!(errors, "\n", "sourcing the script wrote on stderr");
    assert!(complete.contains("-F _clap_complete_fiber"), "{complete}");
    assert!(complete.trim_end().ends_with(" fiber"), "{complete}");
    assert!(!complete.contains("default"), "{complete}");
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert_cases("bash", &format!("CASE 1\n{rest}\n{diagnostics}"), &expected);
}

#[test]
fn the_zsh_script_loads_and_completes_exactly_the_commands_and_flags() {
    let setup = Setup::new();
    install_dynamic_candidates(&setup);
    let path = setup.root.path().join("fiber.zsh");
    fs::write(&path, script(&setup, "zsh")).unwrap();

    let mut expected = vec![
        ("fiber ext".to_owned(), set(&["extension"])),
        ("fiber extension in".to_owned(), set(&["install"])),
    ];
    for (path, subcommands, flags) in GRAMMAR {
        if !subcommands.is_empty() {
            let mut candidates = set(subcommands);
            candidates.extend(set(flags));
            expected.push((line(path, ""), candidates));
        }
    }
    for (path, _, flags) in GRAMMAR {
        expected.push((line(path, "-"), set(flags)));
    }
    expected.extend([
        (
            "fiber extension remove example/".to_owned(),
            set(&["example/installed"]),
        ),
        ("fiber ask --model ".to_owned(), set(&["m", "model-shell"])),
        ("fiber completion ".to_owned(), set(&["--help", "-h"])),
        (
            "fiber extension install ".to_owned(),
            set(&["--project", "--help", "-h"]),
        ),
        (
            "fiber login ".to_owned(),
            set(&["--as", "--device", "--help", "-h"]),
        ),
        ("fiber completion b".to_owned(), set(&[])),
        ("fiber extension install path-prefix".to_owned(), set(&[])),
        ("fiber login provider-prefix".to_owned(), set(&[])),
        ("fiber session --model model-".to_owned(), set(&[])),
    ]);

    let driver = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/completion/zsh-driver.zsh");
    let mut zsh = shell(&setup, "zsh");
    zsh.arg("-f")
        .arg(&driver)
        .arg(&path)
        .args(expected.iter().map(|(case, _)| case));
    let output = run_bounded("the zsh driver", zsh, setup.root.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_cases("zsh", &stdout, &expected);
}

#[test]
fn the_fish_script_loads_and_completes_exactly_the_commands_and_flags() {
    let setup = Setup::new();
    install_dynamic_candidates(&setup);
    let path = setup.root.path().join("fiber.fish");
    fs::write(&path, script(&setup, "fish")).unwrap();

    let mut expected = vec![
        ("fiber ext".to_owned(), set(&["extension"])),
        ("fiber extension in".to_owned(), set(&["install"])),
    ];
    for (path, subcommands, flags) in GRAMMAR {
        if !subcommands.is_empty() {
            let mut candidates = set(subcommands);
            candidates.extend(set(flags));
            expected.push((line(path, ""), candidates));
        }
    }
    for (path, _, flags) in GRAMMAR {
        expected.push((line(path, "-"), set(flags)));
    }
    expected.extend([
        (
            "fiber extension remove example/".to_owned(),
            set(&["example/installed"]),
        ),
        ("fiber ask --model ".to_owned(), set(&["m", "model-shell"])),
        ("fiber completion ".to_owned(), set(&["--help", "-h"])),
        (
            "fiber extension install ".to_owned(),
            set(&["--project", "--help", "-h"]),
        ),
        (
            "fiber login ".to_owned(),
            set(&["--as", "--device", "--help", "-h"]),
        ),
        ("fiber completion b".to_owned(), set(&[])),
        ("fiber extension install path-prefix".to_owned(), set(&[])),
        ("fiber login provider-prefix".to_owned(), set(&[])),
        ("fiber session --model model-".to_owned(), set(&[])),
        (
            "fiber ask ".to_owned(),
            set(&[
                "--model",
                "-c",
                "--resume",
                "--credential",
                "--worktree",
                "--help",
                "-h",
            ]),
        ),
    ]);

    let driver = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/completion/fish-driver.fish");
    let mut fish = shell(&setup, "fish");
    fish.arg("--no-config")
        .arg(&driver)
        .arg(&path)
        .args(expected.iter().map(|(case, _)| case));
    let output = run_bounded("the fish driver", fish, setup.root.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rest = stdout
        .strip_prefix("SOURCE-STDERR\n\n")
        .unwrap_or_else(|| panic!("sourcing the script wrote on stderr\n{stdout}"));
    assert_cases("fish", rest, &expected);
}

#[test]
fn the_zsh_script_is_printed_without_reading_home_or_stdin() {
    let setup = Setup::new();
    let text = script(&setup, "zsh");
    assert!(text.starts_with("#compdef fiber\n"), "{text}");
    assert!(text.contains("FIBER_COMPLETE=\"zsh\""), "{text}");
    assert!(text.contains(env!("CARGO_BIN_EXE_fiber")), "{text}");
}

#[test]
fn the_bash_script_is_printed_without_reading_home_or_stdin() {
    let setup = Setup::new();
    let text = script(&setup, "bash");
    assert!(text.contains("FIBER_COMPLETE=\"bash\""), "{text}");
    assert!(text.contains(env!("CARGO_BIN_EXE_fiber")), "{text}");
    assert!(text.contains("complete -o nospace -o nosort -F _clap_complete_fiber"));
    assert!(!text.contains("bashdefault"), "{text}");
}

#[test]
fn the_fish_script_is_printed_without_reading_home_or_stdin() {
    let setup = Setup::new();
    let text = script(&setup, "fish");
    assert!(text.contains("FIBER_COMPLETE=fish"), "{text}");
    assert!(text.contains(env!("CARGO_BIN_EXE_fiber")), "{text}");
}

#[test]
fn an_unknown_shell_is_a_usage_error_naming_the_nearest() {
    let setup = Setup::new();
    let output = run_bounded(
        "fiber completion bsh",
        fiber(&setup, &["completion", "bsh"]),
        setup.root.path(),
    );
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "fiber: Invalid value 'bsh' for '<shell>' [possible values: bash, zsh, fish]; did you mean 'bash'? Run `fiber --help` for usage.\n"
    );
}

#[test]
fn an_unusable_probe_value_runs_ordinarily() {
    let setup = Setup::new();
    let mut command = fiber(&setup, &["--version"]);
    command.env("FIBER_COMPLETE", "1");
    let output = run_bounded(
        "FIBER_COMPLETE=1 fiber --version",
        command,
        setup.root.path(),
    );
    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("fiber "));
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}

#[test]
fn extension_remove_completes_installed_names() {
    let setup = Setup::without_provider();
    for (dir, name) in [("a", "example/alpha"), ("b", "example/beta")] {
        let extension = setup.home().join("extensions").join(dir);
        fs::create_dir_all(&extension).unwrap();
        fs::write(
            extension.join(".fiber.json"),
            serde_json::json!({
                "name": name,
                "version": "1.0.0",
                "requested": true,
                "source": { "path": "/test" }
            })
            .to_string(),
        )
        .unwrap();
    }
    let mut command = setup.fiber(&["--", "fiber", "extension", "remove", "example/"]);
    command
        .env("FIBER_COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", "3");
    let output = run_bounded(
        "FIBER_COMPLETE=bash fiber -- fiber extension remove example/<Tab>",
        command,
        setup.root.path(),
    );
    assert_eq!(output.status.code(), Some(0));
    // Completion order is not significant, so the probe compares the
    // candidate set.
    assert_eq!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| line.to_owned())
            .collect::<BTreeSet<_>>(),
        set(&["example/alpha", "example/beta"])
    );
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}

#[test]
fn invalid_fiber_home_values_make_dynamic_candidates_empty() {
    let setup = Setup::new();
    let home_file = setup.root.path().join("home-is-a-file");
    fs::write(&home_file, "not a directory").unwrap();
    let file_path = home_file.as_os_str();

    for (name, fiber_home, home) in [
        ("file", Some(file_path), Some(setup.root.path())),
        (
            "relative",
            Some(OsStr::new("relative-home")),
            Some(setup.root.path()),
        ),
        ("empty", Some(OsStr::new("")), Some(setup.root.path())),
    ] {
        assert!(
            probe_candidates_with_home(
                &setup,
                &["--", "fiber", "extension", "remove", "example/"],
                "3",
                fiber_home,
                home,
                name,
            )
            .is_empty(),
            "extension candidates with {name} FIBER_HOME"
        );
        assert!(
            probe_candidates_with_home(
                &setup,
                &["--", "fiber", "--model", "model-"],
                "2",
                fiber_home,
                home,
                name,
            )
            .is_empty(),
            "model candidates with {name} FIBER_HOME"
        );
    }

    for (label, args, index) in [
        (
            "extension candidates without HOME",
            &["--", "fiber", "extension", "remove", "example/"][..],
            "3",
        ),
        (
            "model candidates without HOME",
            &["--", "fiber", "--model", "model-"][..],
            "2",
        ),
    ] {
        assert!(
            probe_candidates_with_home(&setup, args, index, None, None, label).is_empty(),
            "{label}"
        );
    }
}

#[test]
fn extension_remove_with_no_installed_extensions_completes_nothing() {
    let setup = Setup::without_provider();
    fs::write(setup.root.path().join("a-file"), "").unwrap();
    let candidates = probe_candidates(
        &setup,
        &["--", "fiber", "extension", "remove", ""],
        "3",
        "extension remove with no installed extensions",
    );
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.starts_with('-')),
        "expected no extension names or file names, got {candidates:?}"
    );
}

#[test]
fn model_values_complete_only_unique_switched_on_ids_on_visible_args() {
    let setup = Setup::without_provider();
    install_cached_provider(
        &setup.home(),
        "alpha-extension",
        "alpha",
        &["model-unique", "model-shared"],
        true,
    );
    install_cached_provider(
        &setup.home(),
        "beta-extension",
        "beta",
        &["model-second", "model-shared"],
        true,
    );
    install_cached_provider(
        &setup.home(),
        "offline-extension",
        "offline",
        &["model-offline"],
        false,
    );

    for (path, args, index) in [
        ("root", &["--", "fiber", "--model", "model-"][..], "2"),
        ("ask", &["--", "fiber", "ask", "--model", "model-"][..], "3"),
        (
            "setup",
            &["--", "fiber", "setup", "--model", "model-"][..],
            "3",
        ),
    ] {
        // Completion order is not significant, so each probe compares the
        // candidate set.
        assert_eq!(
            probe_candidates(&setup, args, index, path)
                .into_iter()
                .collect::<BTreeSet<_>>(),
            set(&["model-second", "model-unique"]),
            "{path} --model"
        );
    }
    assert!(
        probe_candidates(
            &setup,
            &["--", "fiber", "--model", "model-shared"],
            "2",
            "shared model id"
        )
        .is_empty()
    );
    assert!(
        probe_candidates(
            &setup,
            &["--", "fiber", "ask", "--model", "model-offline"],
            "3",
            "logged-out model id"
        )
        .is_empty()
    );
    assert!(
        probe_candidates(
            &setup,
            &["--", "fiber", "session", "--model", "model-"],
            "3",
            "hidden session --model"
        )
        .is_empty()
    );
}

#[test]
fn other_name_values_do_not_complete_installed_extensions() {
    let setup = Setup::without_provider();
    let extension = setup.home().join("extensions/installed");
    fs::create_dir_all(&extension).unwrap();
    fs::write(
        extension.join(".fiber.json"),
        serde_json::json!({
            "name": "example/installed",
            "version": "1.0.0",
            "requested": true,
            "source": { "path": "/test" }
        })
        .to_string(),
    )
    .unwrap();
    // Only `extension remove` completes installed names: the `update` and
    // `reinstall` name values stay empty even with an install present.
    for (label, args) in [
        (
            "update",
            &["--", "fiber", "extension", "update", "example/"][..],
        ),
        (
            "reinstall",
            &["--", "fiber", "extension", "reinstall", "example/"][..],
        ),
    ] {
        assert!(
            probe_candidates(&setup, args, "3", label).is_empty(),
            "{label} must not complete installed extensions"
        );
    }
}

#[test]
fn login_values_complete_nothing() {
    let setup = Setup::without_provider();
    // A login value completes no provider names. File-name fallback is a
    // shell-side behaviour, pinned by the bash load test running in a
    // directory that holds a file.
    assert!(
        probe_candidates(
            &setup,
            &["--", "fiber", "login", "provider-prefix"],
            "2",
            "login value"
        )
        .is_empty(),
        "login offered a candidate instead of nothing"
    );
}

fn install_dynamic_candidates(setup: &Setup) {
    let home = setup.home();
    let extension = home.join("extensions/installed");
    fs::create_dir_all(&extension).unwrap();
    fs::write(
        extension.join(".fiber.json"),
        serde_json::json!({
            "name": "example/installed",
            "version": "1.0.0",
            "requested": true,
            "source": { "path": "/test" }
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        extension.join("extension.json"),
        serde_json::json!({
            "name": "example/installed",
            "version": "1.0.0",
            "fiber": "0.0.0",
            "api": 1,
            "providers": {}
        })
        .to_string(),
    )
    .unwrap();
    install_cached_provider(&home, "model-extension", "models", &["model-shell"], true);
}

fn install_cached_provider(
    home: &Path,
    extension: &str,
    provider: &str,
    model_ids: &[&str],
    logged_in: bool,
) {
    let url = "https://example.test/v1";
    let directory = home.join("extensions").join(extension);
    fs::create_dir_all(directory.join("providers")).unwrap();
    fs::write(
        directory.join(".fiber.json"),
        serde_json::json!({
            "name": extension,
            "version": "1.0.0",
            "requested": true,
            "source": { "path": "/test" }
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        directory.join("extension.json"),
        serde_json::json!({
            "name": extension,
            "version": "1.0.0",
            "fiber": "0.0.0",
            "api": 1,
            "providers": { provider: [url] }
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        directory.join("providers").join(format!("{provider}.json")),
        serde_json::json!({"name": provider, "models": []}).to_string(),
    )
    .unwrap();
    config::write_model_cache(
        home,
        provider,
        &serde_json::json!(
            model_ids
                .iter()
                .map(|id| {
                    serde_json::json!({
                        "id": id,
                        "protocol": "openai-responses",
                        "base_url": url,
                        "context_window": 100000
                    })
                })
                .collect::<Vec<_>>()
        ),
    )
    .unwrap();
    if logged_in {
        let credential = home.join("credentials").join(provider).join("default");
        fs::create_dir_all(credential.parent().unwrap()).unwrap();
        fs::write(credential, "stored credential").unwrap();
    }
}

fn probe_candidates(setup: &Setup, args: &[&str], index: &str, label: &str) -> Vec<String> {
    probe_candidates_with_home(
        setup,
        args,
        index,
        Some(setup.home().as_os_str()),
        Some(setup.root.path()),
        label,
    )
}

fn probe_candidates_with_home(
    setup: &Setup,
    args: &[&str],
    index: &str,
    fiber_home: Option<&OsStr>,
    home: Option<&Path>,
    label: &str,
) -> Vec<String> {
    let mut command = setup.fiber(args);
    command
        .env("FIBER_COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", index);
    if let Some(fiber_home) = fiber_home {
        command.env("FIBER_HOME", fiber_home);
    } else {
        command.env_remove("FIBER_HOME");
    }
    if let Some(home) = home {
        command.env("HOME", home);
    } else {
        command.env_remove("HOME");
    }
    let output = run_bounded(label, command, setup.root.path());
    assert_eq!(output.status.code(), Some(0), "{label}");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "", "{label}");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_closed_stdout_still_exits_0() {
    let setup = Setup::new();
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let mut command = fiber(&setup, &["completion", "zsh"]);
    command.stdout(writer);
    let output = run_bounded("fiber completion zsh", command, setup.root.path());
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}
