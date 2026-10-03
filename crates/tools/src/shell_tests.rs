use std::path::Path;
use std::time::Duration;

use contract::ErrorCode;
use contract::shapes::Effect;
use contract::tool::Tool;
use fakes::CancelToken;
use fakes::clock::FakeClock;
use rustix::process::Signal;
use serde_json::{Map, Value, json};

use super::{Shell, bare_wait, exit_line, from_spawn, signal_name, timeout_line};

fn shell() -> Shell {
    Shell::new(std::env::temp_dir(), FakeClock::new())
}

fn args(command: &str) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("command".into(), Value::String(command.into()));
    map
}

fn code(output: &contract::tool::Output) -> Option<ErrorCode> {
    output.error.as_ref().map(|error| error.code.clone())
}

fn text(output: &contract::tool::Output) -> String {
    match output.content.first() {
        Some(contract::shapes::ContentPart::Text { text }) => text.clone(),
        _ => String::new(),
    }
}

#[test]
fn the_schema_is_command_workdir_and_timeout() {
    let schema = shell().definition().input_schema;
    assert_eq!(shell().definition().name, "shell");
    assert!(shell().definition().description.contains("timeout_ms"));
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["required"], json!(["command"]));
    assert_eq!(schema["additionalProperties"], false);
    let properties = schema["properties"].as_object().unwrap();
    assert_eq!(
        properties.keys().cloned().collect::<Vec<_>>(),
        ["command", "timeout_ms", "workdir"]
    );
    assert_eq!(properties["command"]["type"], "string");
    assert_eq!(properties["workdir"]["type"], "string");
    assert_eq!(properties["timeout_ms"]["type"], "integer");
    assert_eq!(properties["timeout_ms"]["minimum"], 0);
}

#[test]
fn effects_are_executes_and_not_reversible() {
    let effects = shell().effects(&args("echo hi")).unwrap();
    assert_eq!(effects.declared.effects, vec![Effect::Executes]);
    assert!(!effects.declared.reversible);
    assert!(effects.declared.paths.is_none());
    assert!(effects.subject.is_none());
    assert!(effects.prefix.is_none());
    let again = shell().effects(&Map::new()).unwrap();
    assert_eq!(again, effects);
}

#[test]
fn the_bound_keeps_both_ends() {
    let bound = shell().bound();
    assert_eq!(bound.start, 8192);
    assert_eq!(bound.end, 8192);
}

#[test]
fn a_long_sleep_at_the_start_is_a_bare_wait() {
    for command in [
        "sleep 25",
        "sleep 25s",
        "sleep 25.",
        "sleep 25.0",
        "sleep 1m",
        "sleep 1h",
        "sleep 1d",
        "sleep 0.5m",
        "  sleep\t25",
        "sleep  25",
        "sleep 25; ls",
        "sleep 1m; ls",
        "sleep 30 && make",
        "sleep 30 || true",
        "sleep 25&echo",
        "sleep 25 | cat",
        "sleep 25\nls",
    ] {
        assert!(bare_wait(command), "{command}");
    }
}

#[test]
fn anything_else_is_not_a_bare_wait() {
    for command in [
        "sleep 24",
        "sleep 24s",
        "sleep 24.9",
        "sleep 0.5",
        "sleep 0",
        "sleep 0.4m",
        "while true; do sleep 30; done",
        "echo; sleep 30",
        "echo && sleep 30",
        "sleep $N",
        "sleep",
        "sleep 25 30",
        "sleep 25x",
        "sleep -1",
        "SLEEP 25",
        "; sleep 30",
    ] {
        assert!(!bare_wait(command), "{command}");
    }
}

#[test]
fn a_bare_wait_never_starts() {
    let dir = fakes::TempDir::new("fiber-bare-wait");
    let marker = dir.path().join("marker");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let command = format!("sleep 30; touch {}", marker.display());
    let output = shell.run(&args(&command), &CancelToken::new());
    assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    assert!(text(&output).contains("run_in_background"));
    assert!(text(&output).contains("jobs wait"));
    assert!(text(&output).contains("until"));
    assert!(!marker.exists());
    assert!(output.process.is_none());
}

#[test]
fn a_missing_or_bad_argument_never_starts() {
    let dir = fakes::TempDir::new("fiber-shell-args");
    let marker = dir.path().join("marker");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let touch = format!("touch {}", marker.display());
    let mut missing_dir = args(&touch);
    missing_dir.insert("workdir".into(), json!("no/such/directory"));
    let mut file_dir = args(&touch);
    let file = dir.path().join("file");
    std::fs::write(&file, "x").unwrap();
    file_dir.insert("workdir".into(), json!(file.display().to_string()));
    let mut negative = args(&touch);
    negative.insert("timeout_ms".into(), json!(-1));
    let mut fraction = args(&touch);
    fraction.insert("timeout_ms".into(), json!(1.5));
    let mut not_string = Map::new();
    not_string.insert("command".into(), json!(1));
    for (arguments, needle) in [
        (missing_dir, "not a directory"),
        (file_dir, "not a directory"),
        (negative, "negative"),
        (fraction, "integer"),
        (Map::new(), "command"),
        (not_string, "string"),
    ] {
        let output = shell.run(&arguments, &CancelToken::new());
        assert_eq!(code(&output), Some(ErrorCode::InvalidArguments), "{needle}");
        assert!(
            text(&output).contains(needle),
            "{needle}: {}",
            text(&output)
        );
        assert!(output.process.is_none());
        assert!(!marker.exists(), "{needle}");
    }
}

#[test]
fn an_absolute_workdir_that_is_a_directory_is_accepted_by_parsing() {
    let dir = fakes::TempDir::new("fiber-shell-abs");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let mut arguments = args("echo hi");
    arguments.insert("workdir".into(), json!(dir.path().display().to_string()));
    // Parsing accepts it. The command itself is covered by the integration tests.
    let output = shell.run(&arguments, &CancelToken::new());
    assert!(output.error.is_none(), "{}", text(&output));
}

#[test]
fn result_lines_name_what_was_observed() {
    assert_eq!(exit_line(0), "Exit code 0.");
    assert_eq!(exit_line(3), "Exit code 3.");
    assert_eq!(timeout_line(1000), "Timed out after 1000 ms and stopped.");
    assert_eq!(signal_name(Signal::SEGV.as_raw()), "SIGSEGV");
    assert_eq!(signal_name(Signal::KILL.as_raw()), "SIGKILL");
    assert_eq!(signal_name(Signal::TERM.as_raw()), "SIGTERM");
    assert_eq!(signal_name(0), "SIG0");
}

#[test]
fn a_spawn_failure_is_tool_error_with_no_process() {
    let output = from_spawn(
        Err(std::io::Error::new(std::io::ErrorKind::NotFound, "missing")),
        1,
    );
    assert_eq!(code(&output), Some(ErrorCode::ToolError));
    assert!(output.process.is_none());
    assert!(text(&output).contains("could not be started"));
}

#[test]
fn execute_fails_when_the_program_does_not_exist() {
    let dir = fakes::TempDir::new("fiber-shell-missing");
    let err = super::command::execute(
        Path::new("/no/such/fiber-shell"),
        "true",
        dir.path(),
        Duration::from_secs(1),
        FakeClock::new().as_ref(),
        &CancelToken::new(),
    );
    assert!(err.is_err());
}

#[test]
fn already_cancelled_starts_nothing() {
    let dir = fakes::TempDir::new("fiber-shell-pre-cancel");
    let marker = dir.path().join("marker");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let cancel = CancelToken::new();
    cancel.cancel();
    let output = shell.run(&args(&format!("touch {}", marker.display())), &cancel);
    assert!(output.error.is_none());
    assert!(output.process.is_none());
    assert_eq!(text(&output), "Cancelled before it started.\n");
    assert!(!marker.exists());
}
