use std::path::Path;
use std::time::Duration;

use contract::ErrorCode;
use contract::events::Outcome;
use contract::jobs::JobRecord;
use contract::shapes::Effect;
use contract::tool::Tool;
use fakes::clock::FakeClock;
use fakes::{CancelToken, Recorder};
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
        ["command", "run_in_background", "timeout_ms", "workdir"]
    );
    assert_eq!(properties["run_in_background"]["type"], "boolean");
    assert_eq!(properties["command"]["type"], "string");
    assert_eq!(properties["workdir"]["type"], "string");
    assert_eq!(properties["timeout_ms"]["type"], "integer");
    assert_eq!(properties["timeout_ms"]["minimum"], 0);
}

fn effects(shell: &Shell, command: &str) -> contract::tool::Effects {
    shell.effects(&args(command)).unwrap()
}

fn assert_closed(effects: &contract::tool::Effects) {
    assert_eq!(effects.declared.effects, vec![Effect::Executes]);
    assert!(!effects.declared.reversible);
    assert!(effects.declared.paths.is_none());
}

#[test]
fn a_listed_command_reads_and_names_its_subject() {
    let dir = fakes::TempDir::new("fiber-shell-effects");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let root = dir.path().to_str().unwrap().to_owned();
    let git = effects(&shell, "git status");
    assert_eq!(git.declared.effects, vec![Effect::Reads]);
    assert!(git.declared.reversible);
    assert_eq!(git.subject.as_deref(), Some("git status"));
    assert_eq!(git.prefix.as_deref(), Some("git status"));
    assert_eq!(git.declared.paths, Some(vec![root]));
}

#[test]
fn a_plain_command_offers_its_prefix() {
    let dir = fakes::TempDir::new("fiber-shell-effects");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let npm = effects(&shell, "npm test -- --watch");
    assert_closed(&npm);
    assert_eq!(npm.subject.as_deref(), Some("npm test -- --watch"));
    assert_eq!(npm.prefix.as_deref(), Some("npm test"));
    let spaced = effects(&shell, "npm  test -- --watch");
    assert_eq!(spaced.subject.as_deref(), Some("npm test -- --watch"));
    assert_eq!(spaced.prefix.as_deref(), Some("npm test"));
    let remove = effects(&shell, "rm -rf build");
    assert_closed(&remove);
    assert_eq!(remove.subject.as_deref(), Some("rm -rf build"));
    assert_eq!(remove.prefix.as_deref(), Some("rm"));
    let quoted = effects(&shell, "'npm' test");
    assert_eq!(quoted.subject.as_deref(), Some("'npm' test"));
    assert!(quoted.prefix.is_none());
    let dotted = effects(&shell, "npm test.js");
    assert_eq!(dotted.prefix.as_deref(), Some("npm"));
    let path = effects(&shell, "cat foo/bar");
    assert_eq!(path.prefix.as_deref(), Some("cat"));
}

#[test]
fn paths_are_absolute_operands_and_the_workdir_when_there_are_none() {
    let dir = fakes::TempDir::new("fiber-shell-effects");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let root = dir.path().to_str().unwrap().to_owned();
    let src = dir.path().join("src");
    assert_eq!(
        effects(&shell, "ls").declared.paths,
        Some(vec![root.clone()])
    );
    assert_eq!(
        effects(&shell, "cat src").declared.paths,
        Some(vec![src.to_str().unwrap().to_owned()])
    );
    assert_eq!(
        effects(&shell, "cat /etc/hosts").declared.paths,
        Some(vec!["/etc/hosts".to_owned()])
    );
    assert_eq!(
        effects(&shell, "cat /etc//hosts").declared.paths,
        Some(vec!["/etc//hosts".to_owned()])
    );
    assert_eq!(
        effects(&shell, "cat src /etc/hosts").declared.paths,
        Some(vec![
            src.to_str().unwrap().to_owned(),
            "/etc/hosts".to_owned()
        ])
    );
    assert_eq!(
        effects(&shell, "ls ..").declared.paths,
        Some(vec![dir.path().join("..").to_str().unwrap().to_owned()])
    );
    let listed = effects(&shell, "ls 'src'");
    assert_eq!(listed.subject.as_deref(), Some("ls 'src'"));
    assert_eq!(listed.prefix.as_deref(), Some("ls"));
    assert_eq!(
        listed.declared.paths,
        Some(vec![src.to_str().unwrap().to_owned()])
    );
    let both = effects(&shell, "ls src && git status");
    assert_eq!(both.declared.effects, vec![Effect::Reads]);
    assert!(both.declared.reversible);
    assert!(both.subject.is_none());
    assert!(both.prefix.is_none());
    assert_eq!(
        both.declared.paths,
        Some(vec![src.to_str().unwrap().to_owned(), root])
    );
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let mut arguments = args("ls file");
    arguments.insert("workdir".into(), json!("sub"));
    let nested = shell.effects(&arguments).unwrap();
    assert_eq!(
        nested.declared.paths,
        Some(vec![sub.join("file").to_str().unwrap().to_owned()])
    );
}

#[test]
fn an_unknown_or_flagged_command_declares_no_paths() {
    let dir = fakes::TempDir::new("fiber-shell-effects");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let python = effects(&shell, "python -c 'open(\"/tmp/x\")'");
    assert_closed(&python);
    assert_eq!(
        python.subject.as_deref(),
        Some("python -c 'open(\"/tmp/x\")'")
    );
    assert_eq!(python.prefix.as_deref(), Some("python"));
    let diff = effects(&shell, "git diff --output=out.txt");
    assert_closed(&diff);
    assert_eq!(diff.subject.as_deref(), Some("git diff --output=out.txt"));
    assert_eq!(diff.prefix.as_deref(), Some("git diff"));
    let sort = effects(&shell, "sort -o x y");
    assert_closed(&sort);
    assert_eq!(sort.subject.as_deref(), Some("sort -o x y"));
    assert_eq!(sort.prefix.as_deref(), Some("sort"));
    let substituted = effects(&shell, "cat $(echo x)");
    assert_closed(&substituted);
    assert!(substituted.subject.is_none());
    assert!(substituted.prefix.is_none());
    let assigned = effects(&shell, "FOO=bar ls");
    assert_closed(&assigned);
    assert!(assigned.subject.is_none());
    assert!(assigned.prefix.is_none());
}

#[test]
fn an_invalid_call_is_executes_with_no_subject() {
    let dir = fakes::TempDir::new("fiber-shell-effects");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let mut bad_timeout = args("git status");
    bad_timeout.insert("timeout_ms".into(), json!(-1));
    let mut bad_dir = args("git status");
    bad_dir.insert("workdir".into(), json!("no/such/directory"));
    let mut not_string = Map::new();
    not_string.insert("command".into(), json!(1));
    let mut bad_background = args("git status");
    bad_background.insert("run_in_background".into(), json!("yes"));
    for arguments in [Map::new(), bad_timeout, bad_dir, not_string, bad_background] {
        let effects = shell.effects(&arguments).unwrap();
        assert_closed(&effects);
        assert!(effects.subject.is_none());
        assert!(effects.prefix.is_none());
    }
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
        "sleep inf",
        "sleep infinity",
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
    let output = shell.run(&args(&command), &CancelToken::new(), &Recorder::default());
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
        let output = shell.run(&arguments, &CancelToken::new(), &Recorder::default());
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
    let output = shell.run(&arguments, &CancelToken::new(), &Recorder::default());
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
        &Recorder::default(),
        super::command::MovePolicy::Stay,
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
    let output = shell.run(
        &args(&format!("touch {}", marker.display())),
        &cancel,
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    assert!(output.process.is_none());
    assert_eq!(text(&output), "Cancelled before it started.\n");
    assert!(!marker.exists());
}

#[test]
fn guidelines_are_the_shell_section() {
    let shell = Shell::new(std::env::temp_dir(), FakeClock::new());
    let text = shell.guidelines().unwrap();
    assert_eq!(text, crate::guidelines::of("shell").unwrap());
    assert!(!text.is_empty(), "{text}");
    let md = include_str!("../prompt/guidelines.md");
    let rest = &md[md.find("## shell\n").unwrap() + "## shell\n".len()..];
    let end = rest.find("\n## ").map(|i| i + 1).unwrap_or(rest.len());
    assert_eq!(text, rest[..end].trim(), "{text}");
    assert!(text.contains("Commands run with no terminal"), "{text}");
}

#[test]
fn a_non_boolean_run_in_background_never_starts() {
    let dir = fakes::TempDir::new("fiber-shell-bg-arg");
    let marker = dir.path().join("marker");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    for value in [json!("yes"), json!(1)] {
        let mut arguments = args(&format!("touch {}", marker.display()));
        arguments.insert("run_in_background".into(), value);
        let output = shell.run(&arguments, &CancelToken::new(), &Recorder::default());
        assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
        assert!(text(&output).contains("boolean"), "{}", text(&output));
        assert!(output.process.is_none());
        assert!(!marker.exists());
    }
}

#[test]
fn run_in_background_without_jobs_never_starts() {
    let dir = fakes::TempDir::new("fiber-shell-no-jobs");
    let marker = dir.path().join("marker");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let mut arguments = args(&format!("sleep 30; touch {}", marker.display()));
    arguments.insert("run_in_background".into(), json!(true));
    let output = shell.run(&arguments, &CancelToken::new(), &Recorder::default());
    assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    assert!(
        text(&output).contains("Background jobs are not available in this session."),
        "{}",
        text(&output)
    );
    assert!(!text(&output).contains("for 25 seconds"));
    assert!(output.process.is_none());
    assert!(!marker.exists());
}

#[test]
fn run_in_background_skips_the_bare_wait_and_returns_a_receipt() {
    let dir = fakes::TempDir::new("fiber-shell-bg-sleep");
    let jobs = fakes::jobs::FakeJobs::new(dir.path());
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new()).with_jobs(jobs.clone());
    let mut arguments = args("sleep 30");
    arguments.insert("run_in_background".into(), json!(true));
    let output = shell.run(&arguments, &CancelToken::new(), &Recorder::default());
    assert!(output.error.is_none(), "{}", text(&output));
    assert!(output.process.is_none());
    assert!(
        text(&output).starts_with("Started in the background.\n"),
        "{}",
        text(&output)
    );
    let started = match output.jobs.as_slice() {
        [JobRecord::Started(started)] => started.clone(),
        other => panic!("expected one started job, got {other:?}"),
    };
    assert_eq!(started.description, "sleep 30");
    assert_eq!(started.tool.as_deref(), Some("shell"));
    jobs.stop(&started.job_id);
    let ended = jobs
        .ended(Duration::from_secs(10))
        .expect("the stopped sleep to end");
    assert_eq!(ended.status, Outcome::Cancelled);
    assert!(ended.output_tail.is_none());
}
