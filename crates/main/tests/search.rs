//! Binary-level tests of the hidden `grep` and `find` subcommands
//! (`docs/tools.md`, "Search"): the built `fiber` answers, and the shell
//! tool's functions reach it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, mpsc};
use std::thread;

use contract::shapes::ContentPart;
use contract::tool::Tool;
use fakes::clock::FakeClock;
use fakes::{CancelToken, Watchdog};
use serde_json::{Map, Value};
use support::Deadline;

/// A temporary workspace, removed on drop.
struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
}

impl Setup {
    fn new() -> Self {
        let deadline = Deadline::start();
        let setup = Self {
            deadline,
            root: fakes::TempDir::new("fa"),
        };
        fs::create_dir_all(setup.workspace()).unwrap();
        setup
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("w")
    }

    fn write(&self, path: &str, contents: &str) {
        let full = self.workspace().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(&full, contents).unwrap();
    }

    /// Runs `fiber` with `args` in the workspace, `stdin` piped in, waiting
    /// under the test's [`Deadline`] in its own process group with a watchdog beside
    /// it, and asserts that nothing it started is left behind.
    fn fiber(&self, args: &[&str], stdin: Option<&str>) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.workspace())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (mut child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let guard = KillGroup(group);
        // The write runs on a thread, so a child that never reads it is
        // bounded by the exit wait; the pipe closes once written.
        if let Some(mut pipe) = child.stdin.take()
            && let Some(text) = stdin
        {
            let text = text.to_owned();
            thread::spawn(move || match pipe.write_all(text.as_bytes()) {
                Ok(()) | Err(_) => {}
            });
        }
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match finished.recv_timeout(self.deadline.left()) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(
                self.deadline,
                group,
                &finished,
                &format!("`fiber {}` to exit", args.join(" ")),
            ),
        };
        assert!(
            fakes::group_empties(group, self.deadline.left()),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup());
        Run::from(output)
    }
}

/// Spawns `command` in a new process group, then a watchdog in its own
/// group.
fn spawn_watched(command: &mut Command) -> (Child, Watchdog) {
    let child = command.process_group(0).spawn().unwrap();
    let group = child.id();
    let guard = KillGroup(group);
    let watchdog = Watchdog::group(group);
    std::mem::forget(guard);
    (child, watchdog)
}

/// Kills process group `group` on drop.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        support::kill_group_detached(self.0, "KILL");
    }
}

/// Cancels its token on drop, without waiting.
struct CancelOnDrop(CancelToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// What one `fiber` run wrote.
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl From<Output> for Run {
    fn from(output: Output) -> Self {
        Self {
            code: output.status.code(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

#[test]
fn help_hides_the_search_subcommands() {
    let setup = Setup::new();
    for args in [&["--help"][..], &["help"]] {
        let run = setup.fiber(args, None);
        assert_eq!(run.code, Some(0), "{args:?}");
        assert!(!run.stdout.contains("grep"), "{args:?}:\n{}", run.stdout);
        assert!(!run.stdout.contains("find"), "{args:?}:\n{}", run.stdout);
    }
}

#[test]
fn grep_searches_a_file() {
    let setup = Setup::new();
    setup.write("a.txt", "needle\nhay\n");
    let run = setup.fiber(&["grep", "needle", "a.txt"], None);
    assert_eq!(run.code, Some(0));
    assert_eq!(run.stdout, "needle\n");
    assert_eq!(run.stderr, "");
    let missing = setup.fiber(&["grep", "absent", "a.txt"], None);
    assert_eq!(missing.code, Some(1));
    assert_eq!(missing.stdout, "");
}

#[test]
fn grep_filters_standard_input() {
    let setup = Setup::new();
    let run = setup.fiber(&["grep", "b"], Some("a\nb\nc\n"));
    assert_eq!(run.code, Some(0));
    assert_eq!(run.stdout, "b\n");
}

#[test]
fn grep_keeps_the_argv_delimiter() {
    let setup = Setup::new();
    setup.write("dash.txt", "-needle\nplain\n");
    // `--` ends flags: the pattern is `-needle`, not `-n` with `eedle`.
    let run = setup.fiber(&["grep", "--", "-needle", "dash.txt"], None);
    assert_eq!(run.code, Some(0));
    assert_eq!(run.stdout, "-needle\n");
    assert_eq!(run.stderr, "");
}

#[test]
fn grep_prints_only_the_match() {
    let setup = Setup::new();
    setup.write("a.txt", "xneedle yneedle\nnone\n");
    // Built-in `-o`: each non-empty match on its own line.
    let run = setup.fiber(&["grep", "-o", "needle", "a.txt"], None);
    assert_eq!(run.code, Some(0));
    assert_eq!(run.stdout, "needle\nneedle\n");
    assert_eq!(run.stderr, "");
}

#[test]
fn grep_only_matching_skips_ignored_directories() {
    let setup = Setup::new();
    setup.write(".gitignore", "ignored/\n");
    setup.write("ignored/needle.txt", "needle\n");
    setup.write("kept.txt", "needle\n");
    // Built-in `-o` walks like the search, not the system grep: the
    // ignored directory stays skipped.
    let run = setup.fiber(&["grep", "-ro", "needle", "."], None);
    assert_eq!(run.code, Some(0));
    assert_eq!(run.stdout, "./kept.txt:needle\n");
    assert_eq!(run.stderr, "");
}

#[test]
fn find_lists_the_tree() {
    let setup = Setup::new();
    setup.write("a.txt", "a\n");
    setup.write("sub/b.txt", "b\n");
    let run = setup.fiber(&["find", "."], None);
    assert_eq!(run.code, Some(0));
    assert_eq!(run.stdout, ".\n./a.txt\n./sub\n./sub/b.txt\n");
    assert_eq!(run.stderr, "");
    let filtered = setup.fiber(&["find", ".", "-name", "*.txt"], None);
    assert_eq!(filtered.code, Some(0));
    assert_eq!(filtered.stdout, "./a.txt\n./sub/b.txt\n");
}

#[test]
fn shell_functions_reach_the_built_binary() {
    let setup = Setup::new();
    setup.write(".gitignore", "target/\n");
    setup.write("target/needle.txt", "needle\n");
    setup.write("kept_needle.txt", "needle\n");
    let fiber = PathBuf::from(env!("CARGO_BIN_EXE_fiber"));
    let shell = Arc::new(tools::Shell::new(setup.workspace(), FakeClock::new()).with_search(fiber));
    let token = CancelToken::new();
    // The fake clock never advances, so the shell's own timeout never
    // fires: each run is bounded by the test's deadline instead, and a
    // timeout's unwind cancels the command, which stops its group.
    let _cancel = CancelOnDrop(token.clone());
    let run = |command: &str| {
        let shell = Arc::clone(&shell);
        let token = token.clone();
        let command = command.to_owned();
        support::bounded(
            setup.deadline,
            &format!("the shell run of {command}"),
            move || {
                let mut arguments = Map::new();
                arguments.insert("command".into(), Value::String(command));
                shell.run(&arguments, &token, &fakes::emit::Recorder::default())
            },
        )
    };
    let text = |output: &contract::tool::Output| match output.content.first() {
        Some(ContentPart::Text { text }) => text.clone(),
        _ => String::new(),
    };
    // A pipe filter, as `cargo test | fiber grep FAILED` runs one.
    let filtered = run("printf 'a\\nb\\n' | grep b");
    assert!(text(&filtered).contains("b\n"), "{}", text(&filtered));
    // A recursive search skips the ignored `target/`.
    let recursive = run("grep -r needle .");
    assert!(
        text(&recursive).contains("./kept_needle.txt:needle\n"),
        "{}",
        text(&recursive)
    );
    assert!(!text(&recursive).contains("target"), "{}", text(&recursive));
}
