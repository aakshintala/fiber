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

mod support;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::{Watchdog, group_empties, kill_group, kill_matching, matching_exits};
use support::Setup;

/// How long one `fiber` or shell run may take.
const RUN: Duration = Duration::from_secs(20);

/// How long the checks and cleanup after one run may take in all.
const REAP: Duration = Duration::from_secs(5);

/// Each cleanup path has four steps, each with a quarter of the `REAP` budget.
const GROUP_KILL: Duration = Duration::from_millis(1_250);
const MATCHING_KILL: Duration = Duration::from_millis(1_250);
const GROUP_EMPTY: Duration = Duration::from_millis(1_250);
const MATCHING_EXIT: Duration = Duration::from_millis(1_250);
const GROUP_WATCHDOG: Duration = Duration::from_millis(1_250);
const MATCHING_WATCHDOG: Duration = Duration::from_millis(1_250);

const _: () = assert!(
    REAP.as_millis()
        == GROUP_KILL.as_millis()
            + MATCHING_KILL.as_millis()
            + GROUP_EMPTY.as_millis()
            + MATCHING_EXIT.as_millis()
);
const _: () = assert!(
    REAP.as_millis()
        == GROUP_EMPTY.as_millis()
            + MATCHING_EXIT.as_millis()
            + GROUP_WATCHDOG.as_millis()
            + MATCHING_WATCHDOG.as_millis()
);

/// Runs one cleanup step on its own thread and returns failures so later
/// cleanup checks still run before the test reports them.
fn within_cleanup<T: Send + 'static>(
    what: &str,
    deadline: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let _sent = done.send(work());
    });
    match finished.recv_timeout(deadline) {
        Ok(result) => Ok(result),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(format!("waited {deadline:?} for {what}")),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(format!("{what} worker stopped without a result"))
        }
    }
}

/// The grammar a person completes, taken from the parser: every visible
/// command path, its subcommands, and its flags.
const GRAMMAR: &[(&str, &[&str], &[&str])] = &[
    (
        "",
        &[
            "ask",
            "sessions",
            "models",
            "extension",
            "approve",
            "config",
            "login",
            "logout",
            "completion",
            "version",
            "help",
            "hub",
        ],
        &["-h", "--help", "-v", "--version"],
    ),
    ("ask", &[], &["--model", "--resume", "-h", "--help"]),
    ("models", &[], &["--json", "-h", "--help"]),
    ("approve", &[], &["--yes", "-h", "--help"]),
    ("login", &[], &["--as", "-h", "--help"]),
    ("logout", &[], &["--as", "--all", "-h", "--help"]),
    ("completion", &[], &["-h", "--help"]),
    ("version", &[], &["-h", "--help"]),
    ("help", &[], &["-h", "--help"]),
    (
        "sessions",
        &["delete", "export", "prune"],
        &["--all", "--json", "-h", "--help"],
    ),
    (
        "sessions delete",
        &[],
        &["--cascade", "--yes", "-h", "--help"],
    ),
    ("sessions export", &[], &["-h", "--help"]),
    (
        "sessions prune",
        &[],
        &[
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
        &["install", "update", "remove", "list"],
        &["-h", "--help"],
    ),
    ("extension install", &[], &["-h", "--help"]),
    ("extension update", &[], &["-h", "--help"]),
    ("extension remove", &[], &["-h", "--help"]),
    ("extension list", &[], &["-h", "--help"]),
    ("config", &["get", "set"], &["-h", "--help"]),
    ("config get", &[], &["-h", "--help"]),
    ("config set", &[], &["--project", "--repo", "-h", "--help"]),
    (
        "hub",
        &["install", "uninstall", "status"],
        &["-h", "--help"],
    ),
    ("hub install", &[], &["--port", "-h", "--help"]),
    ("hub uninstall", &[], &["-h", "--help"]),
    ("hub status", &[], &["--json", "-h", "--help"]),
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

/// Runs `command` to its exit, in its own process group, with stdin a pipe
/// the test holds open and never writes to: a run that read stdin would
/// block until `RUN` expires. After the exit, the group and every process
/// whose command line holds `tag` must be gone within `REAP`. On expiry,
/// both are killed and reaped before the test fails.
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
    let Ok(output) = finished.recv_timeout(RUN) else {
        // Both bounded kills finish before either process check starts.
        let group_killed = within_cleanup(
            &format!("killing {what}'s process group"),
            GROUP_KILL,
            move || kill_group(group, "KILL"),
        );
        let tag_for_kill = tag.clone();
        let tag_killed = within_cleanup(
            &format!("killing processes matching {tag}"),
            MATCHING_KILL,
            move || kill_matching(&tag_for_kill),
        );
        let tag_for_wait = tag.clone();
        let group_gone = within_cleanup(
            &format!("{what}'s process group to empty"),
            GROUP_EMPTY,
            move || group_empties(group, GROUP_EMPTY),
        );
        let tag_gone = within_cleanup(
            &format!("processes matching {tag} to exit"),
            MATCHING_EXIT,
            move || matching_exits(&tag_for_wait, MATCHING_EXIT),
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
        GROUP_EMPTY,
        move || group_empties(group, GROUP_EMPTY),
    );
    let tag_gone = within_cleanup(
        &format!("processes matching {tag} to exit"),
        MATCHING_EXIT,
        move || matching_exits(&tag_for_wait, MATCHING_EXIT),
    );
    let stand_down = if group_gone.as_ref() == Ok(&true) && tag_gone.as_ref() == Ok(&true) {
        let group_stood_down =
            within_cleanup("the group watchdog to exit", GROUP_WATCHDOG, move || {
                group_dog.stand_down(GROUP_WATCHDOG);
            });
        let tag_stood_down = within_cleanup(
            "the matching watchdog to exit",
            MATCHING_WATCHDOG,
            move || {
                tag_dog.stand_down(MATCHING_WATCHDOG);
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
        tag_gone.as_ref() == Ok(&true),
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
fn fiber(setup: &Setup, args: &[&str]) -> Command {
    let home = setup.root.path().join("home-is-a-file");
    fs::write(&home, "not a directory").unwrap();
    let mut command = setup.fiber(args);
    command.env("FIBER_HOME", home);
    command
}

/// The script, from a run that exited 0 with nothing on stderr.
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
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", setup.root.path())
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
        assert_eq!(got, want, "{what}: `{case}`");
    }
}

#[test]
fn the_bash_script_loads_and_completes_exactly_the_commands_and_flags() {
    let setup = Setup::new();
    let path = setup.root.path().join("fiber.bash");
    fs::write(&path, script(&setup, "bash")).unwrap();

    let mut expected = vec![
        ("fiber ext".to_owned(), set(&["extension"])),
        ("fiber extension in".to_owned(), set(&["install"])),
    ];
    for (path, subcommands, flags) in GRAMMAR {
        let mut both = set(subcommands);
        both.extend(set(flags));
        expected.push((line(path, ""), both));
        expected.push((line(path, "-"), set(flags)));
    }
    // A value is not completed, not even as a file name in this directory.
    expected.push(("fiber ask --model ".to_owned(), set(&[""])));

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
    assert!(complete.contains("-F _fiber"), "{complete}");
    assert!(complete.trim_end().ends_with(" fiber"), "{complete}");
    assert!(!complete.contains("default"), "{complete}");
    assert_cases("bash", &format!("CASE 1\n{rest}"), &expected);
}

#[test]
fn the_zsh_script_loads_and_completes_exactly_the_commands_and_flags() {
    let setup = Setup::new();
    let path = setup.root.path().join("fiber.zsh");
    fs::write(&path, script(&setup, "zsh")).unwrap();

    let mut expected = vec![
        ("fiber ext".to_owned(), set(&["extension"])),
        ("fiber extension in".to_owned(), set(&["install"])),
    ];
    for (path, subcommands, _) in GRAMMAR {
        if !subcommands.is_empty() {
            expected.push((line(path, ""), set(subcommands)));
        }
    }
    for (path, _, flags) in GRAMMAR {
        expected.push((line(path, "-"), set(flags)));
    }
    // A value is not completed, not even as a file name in this directory.
    expected.push(("fiber ask --model ".to_owned(), set(&[])));

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
    let path = setup.root.path().join("fiber.fish");
    fs::write(&path, script(&setup, "fish")).unwrap();

    let mut expected = vec![
        ("fiber ext".to_owned(), set(&["extension"])),
        ("fiber extension in".to_owned(), set(&["install"])),
    ];
    for (path, subcommands, _) in GRAMMAR {
        if !subcommands.is_empty() {
            expected.push((line(path, ""), set(subcommands)));
        }
    }
    for (path, _, flags) in GRAMMAR {
        expected.push((line(path, "-"), set(flags)));
    }
    // A value is not completed, not even as a file name in this directory.
    expected.push(("fiber ask --model ".to_owned(), set(&[])));
    expected.push(("fiber ask ".to_owned(), set(&[])));

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
}

#[test]
fn the_bash_script_is_printed_without_reading_home_or_stdin() {
    let setup = Setup::new();
    let text = script(&setup, "bash");
    let completes: Vec<&str> = text
        .lines()
        .filter(|line| line.trim_start().starts_with("complete "))
        .collect();
    assert_eq!(
        completes,
        [
            "    complete -F _fiber -o nosort fiber",
            "    complete -F _fiber fiber"
        ]
    );
}

#[test]
fn the_fish_script_is_printed_without_reading_home_or_stdin() {
    let setup = Setup::new();
    let text = script(&setup, "fish");
    assert!(
        text.contains("complete -c fiber -n \"__fish_fiber_needs_command\" -f -a \"extension\""),
        "{text}"
    );
    assert!(text.ends_with("\ncomplete -c fiber -f\n"), "{text}");
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
