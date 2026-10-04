//! The shell's built-in `grep` and `find` through [`tools::Shell`]
//! (`docs/tools.md`, "Search"): `grep` and `find` reach the search binary,
//! everything else keeps the system tools.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use contract::shapes::ContentPart;
use contract::tool::Tool;
use fakes::CancelToken;
use fakes::clock::FakeClock;
use serde_json::{Map, Value};
use tools::{Shell, find_main, grep_main};

/// Serializes the tests that move the process's working directory, for
/// runners that share one process across tests.
static CWD: Mutex<()> = Mutex::new(());

fn text(output: &contract::tool::Output) -> String {
    match output.content.first() {
        Some(ContentPart::Text { text }) => text.clone(),
        _ => String::new(),
    }
}

/// A stand-in search binary: appends its arguments to `calls.log` beside
/// itself, prints them, and exits `code`.
fn stand_in(dir: &Path, name: &str, code: i32) -> PathBuf {
    let script = dir.join(name);
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"{}.log\"\nprintf 'STANDIN:%s\\n' \"$@\"\nexit {code}\n",
            script.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// The arguments the stand-in recorded, or nothing when it never ran.
fn calls(script: &Path) -> Vec<String> {
    let log = PathBuf::from(format!("{}.log", script.display()));
    if !log.is_file() {
        return Vec::new();
    }
    fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn shell(dir: &Path, fiber: Option<&Path>) -> Shell {
    let shell = Shell::new(dir.to_path_buf(), FakeClock::new());
    match fiber {
        Some(exe) => shell.with_search(exe.to_path_buf()),
        None => shell,
    }
}

fn run(shell: &Shell, command: &str) -> contract::tool::Output {
    let mut arguments = Map::new();
    arguments.insert("command".into(), Value::String(command.into()));
    shell.run(&arguments, &CancelToken::new())
}

#[test]
fn grep_and_find_reach_the_search_binary() {
    let dir = fakes::TempDir::new("fiber-search-shell");
    fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let exe = stand_in(dir.path(), "fiber", 0);
    let shell = shell(dir.path(), Some(&exe));
    let grep = run(&shell, "grep x a.txt");
    assert!(
        text(&grep).contains("STANDIN:grep\nSTANDIN:x\nSTANDIN:a.txt\n"),
        "{}",
        text(&grep)
    );
    assert_eq!(calls(&exe), ["grep", "x", "a.txt"]);
    let find = run(&shell, "find . -maxdepth 0");
    assert!(
        text(&find).contains("STANDIN:find\nSTANDIN:.\nSTANDIN:-maxdepth\nSTANDIN:0\n"),
        "{}",
        text(&find)
    );
    assert_eq!(calls(&exe).last().unwrap(), "0");
}

#[test]
fn command_a_path_children_and_scripts_keep_the_system_tools() {
    let dir = fakes::TempDir::new("fiber-search-system");
    fs::write(dir.path().join("a.txt"), "needle\nhay\n").unwrap();
    fs::write(dir.path().join("runner.sh"), "grep needle a.txt\n").unwrap();
    let exe = stand_in(dir.path(), "fiber", 0);
    let shell = shell(dir.path(), Some(&exe));
    for command in [
        "command grep needle a.txt",
        "/usr/bin/grep needle a.txt",
        "sh -c 'grep needle a.txt'",
        "sh runner.sh",
        "env grep needle a.txt",
    ] {
        let output = run(&shell, command);
        assert_eq!(text(&output), "needle\nExit code 0.\n", "{command}");
    }
    assert!(calls(&exe).is_empty());
}

#[test]
fn a_quoted_binary_path_still_runs() {
    let dir = fakes::TempDir::new("fiber-search-quote's");
    let quoted = dir.path().join("odd ' dir");
    fs::create_dir_all(&quoted).unwrap();
    let exe = stand_in(&quoted, "fiber", 0);
    let shell = shell(dir.path(), Some(&exe));
    let output = run(&shell, "grep x");
    assert!(
        text(&output).contains("STANDIN:grep\nSTANDIN:x\n"),
        "{}",
        text(&output)
    );
}

#[test]
fn a_missing_executable_falls_back_to_the_system_tool() {
    let dir = fakes::TempDir::new("fiber-search-missing");
    fs::write(dir.path().join("a.txt"), "needle\n").unwrap();
    let shell = shell(
        dir.path(),
        Some(Path::new("/nonexistent/fiber-search-missing")),
    );
    let output = run(&shell, "grep needle a.txt");
    assert_eq!(text(&output), "needle\nExit code 0.\n");
}

#[test]
fn the_function_returns_the_subcommand_status() {
    let dir = fakes::TempDir::new("fiber-search-status");
    let exe = stand_in(dir.path(), "fiber", 3);
    let shell = shell(dir.path(), Some(&exe));
    let output = run(&shell, "grep x");
    assert_eq!(
        output
            .process
            .as_ref()
            .and_then(|process| process.exit_code),
        Some(3)
    );
    assert!(text(&output).contains("Exit code 3."), "{}", text(&output));
}

#[test]
fn the_prelude_leaves_the_dollar_question_zero() {
    let dir = fakes::TempDir::new("fiber-search-status-zero");
    let exe = stand_in(dir.path(), "fiber", 0);
    let shell = shell(dir.path(), Some(&exe));
    let output = run(&shell, "echo \"status=$?\"");
    assert!(text(&output).contains("status=0\n"), "{}", text(&output));
}

#[test]
fn the_classifier_sees_the_original_command() {
    let dir = fakes::TempDir::new("fiber-search-effects");
    let plain = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let exe = stand_in(dir.path(), "fiber", 0);
    let searched = shell(dir.path(), Some(&exe));
    let command = |shell: &Shell, text: &str| {
        let mut arguments = Map::new();
        arguments.insert("command".into(), Value::String(text.into()));
        shell.effects(&arguments).unwrap()
    };
    assert_eq!(command(&plain, "grep x"), command(&searched, "grep x"));
    assert_eq!(
        command(&plain, "find . -exec true ;"),
        command(&searched, "find . -exec true ;")
    );
}

#[test]
fn without_search_nothing_is_defined() {
    let dir = fakes::TempDir::new("fiber-search-plain");
    let shell = shell(dir.path(), None);
    let direct = run(&shell, "grep --version");
    let bypass = run(&shell, "command grep --version");
    assert_eq!(text(&direct), text(&bypass));
    assert!(text(&direct).contains("grep"), "{}", text(&direct));
}

#[test]
fn functions_do_not_leak_to_grandchildren() {
    let dir = fakes::TempDir::new("fiber-search-leak");
    let exe = stand_in(dir.path(), "fiber", 0);
    let shell = shell(dir.path(), Some(&exe));
    let output = run(&shell, "bash -c 'type grep'");
    assert!(
        text(&output).contains("is /usr/bin/grep"),
        "{}",
        text(&output)
    );
    assert!(!text(&output).contains("function"), "{}", text(&output));
}

#[test]
fn find_main_lists_and_grep_main_matches() {
    let _guard = CWD.lock().unwrap();
    let here = std::env::current_dir().unwrap();
    let dir = fakes::TempDir::new("fiber-search-mains");
    fs::write(dir.path().join("a.txt"), "needle\n").unwrap();
    std::env::set_current_dir(dir.path()).unwrap();
    assert_eq!(find_main(vec![".".into()]), 0);
    assert_eq!(find_main(vec!["no_such_root".into()]), 2);
    assert_eq!(grep_main(vec!["needle".into(), "a.txt".into()]), 0);
    assert_eq!(grep_main(vec!["absent".into(), "a.txt".into()]), 1);
    std::env::set_current_dir(here).unwrap();
}
