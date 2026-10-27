use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use contract::ErrorCode;
use contract::events::Outcome;
use contract::jobs::{JobRecord, Jobs as _};
use contract::shapes::Effect;
use contract::tool::Tool;
use fakes::clock::FakeClock;
use fakes::{CancelToken, Recorder};
use serde_json::{Map, Value, json};

use super::{Mode, Shell, bare_wait, exit_line, from_spawn, observed, parse, timeout_line};
use crate::error::ShellError;

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

fn run_details_call(shell: Shell, arguments: Map<String, Value>) -> contract::tool::Output {
    let (send, recv) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _sent = send.send(shell.run(&arguments, &CancelToken::new(), &Recorder::default()));
    });
    fakes::Deadline::after(Duration::from_secs(10)).recv_or_fail(&recv, "shell match details")
}

#[test]
fn nullable_optional_arguments_parse_as_absent() {
    let dir = fakes::TempDir::new("fiber-shell-nullable");
    let arguments = fakes::tool_args(json!({"command": "ls", "workdir": null, "timeout_ms": null,
        "run_in_background": null, "tty": null, "monitor": null, "deadline_ms": null}));
    let parsed = parse(&arguments, dir.path(), 600_000).unwrap();
    assert_eq!(parsed.command, "ls");
    assert_eq!(parsed.mode, Mode::Foreground);
    assert_eq!(parsed.workdir, dir.path());
    assert_eq!(parsed.timeout_ms, 600_000);
    let tool = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    assert!(tool.effects(&arguments).is_ok());
    let schema = tool.definition().input_schema;
    assert_eq!(
        schema["required"],
        json!([
            "command",
            "workdir",
            "timeout_ms",
            "run_in_background",
            "tty",
            "monitor",
            "deadline_ms"
        ])
    );
    for (key, kind) in [
        ("workdir", "string"),
        ("timeout_ms", "integer"),
        ("run_in_background", "boolean"),
        ("tty", "boolean"),
        ("monitor", "boolean"),
        ("deadline_ms", "integer"),
    ] {
        assert_eq!(schema["properties"][key]["type"], json!([kind, "null"]));
    }
    for (overrides, mode, timeout) in [
        (json!({"monitor": true}), Mode::Monitor, 300_000),
        (
            json!({"tty": true}),
            Mode::Terminal {
                run_in_background: false,
            },
            600_000,
        ),
        (
            json!({"run_in_background": true}),
            Mode::Background,
            600_000,
        ),
    ] {
        let mut args = arguments.clone();
        args.extend(overrides.as_object().unwrap().clone());
        let parsed = parse(&args, dir.path(), 600_000).unwrap();
        assert_eq!(parsed.mode, mode);
        assert_eq!(parsed.timeout_ms, timeout);
    }
    let refused = |overrides: Value| {
        let mut args = arguments.clone();
        args.extend(overrides.as_object().unwrap().clone());
        parse(&args, dir.path(), 600_000).err()
    };
    assert!(matches!(
        refused(json!({"monitor": true, "tty": true})),
        Some(ShellError::MonitorAlone)
    ));
    assert!(matches!(
        refused(json!({"monitor": true, "run_in_background": true})),
        Some(ShellError::MonitorAlone)
    ));
    assert!(matches!(
        refused(json!({"monitor": true, "timeout_ms": 0})),
        Some(ShellError::MonitorTimeout)
    ));
    assert!(matches!(
        refused(json!({"deadline_ms": 0})),
        Some(ShellError::DeadlineWithoutMonitor)
    ));
}

#[test]
fn shell_attaches_only_strict_foreground_grep_match_records_and_cleans_up() {
    let dir = fakes::TempDir::new("fiber-shell-match-details");
    std::fs::write(dir.path().join("a.txt"), "needle\n").unwrap();
    let fiber = fakes::script(
        dir.path(),
        "search-matches",
        r#"
printf '%s' "$FIBER_GREP_MATCHES" > called
printf '%s\n' '{"path":"a.txt","line":1,"text":"needle","spans":[[0,6]],"context":false}' '{"path":"a.txt","line":2,"text":"context","spans":[],"context":true}' > "$FIBER_GREP_MATCHES"
printf 'needle\n'
"#,
    );
    for (command, attaches) in [
        ("grep needle a.txt", true),
        ("'grep' needle a.txt", true),
        ("grep needle a.txt 2>/dev/null", true),
        ("grep needle a.txt 2>&1", true),
        ("cat a.txt | grep needle", false),
        ("grep needle a.txt; true", false),
        ("grep needle a.txt > out", false),
        ("command grep needle a.txt", false),
    ] {
        let shell =
            Shell::new(dir.path().to_path_buf(), FakeClock::new()).with_search(fiber.clone());
        let output = run_details_call(shell, args(command));
        assert!(output.error.is_none(), "{command}: {:?}", output.error);
        let details = output.details.unwrap();
        assert_eq!(details["command"], command);
        assert_eq!(details["exit_code"], 0);
        let expected = attaches.then(|| {
            json!([{"path":"a.txt", "lines":[
                {"line":1,"text":"needle","spans":[[0,6]]},
                {"line":2,"text":"context","spans":[]}
            ]}])
        });
        assert_eq!(details.get("matches"), expected.as_ref(), "{command}");
        if let Ok(called) = std::fs::read_to_string(dir.path().join("called")) {
            let path = Path::new(&called).parent().unwrap();
            assert!(!path.exists());
            assert!(!path.with_extension("gone").exists());
            std::fs::remove_file(dir.path().join("called")).unwrap();
        }
    }
}

#[test]
fn shell_ignores_missing_or_malformed_side_files_and_search_off_carries_no_variable() {
    let dir = fakes::TempDir::new("fiber-shell-match-missing");
    for (name, body) in [
        ("missing", "printf needle"),
        (
            "malformed",
            "printf 'not json\\n' > \"$FIBER_GREP_MATCHES\"; printf needle",
        ),
    ] {
        let fiber = fakes::script(dir.path(), name, body);
        let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new()).with_search(fiber);
        let output = run_details_call(shell, args("grep needle a.txt"));
        assert_eq!(
            output.details,
            Some(json!({"command":"grep needle a.txt", "exit_code":0}))
        );
        assert!(text(&output).contains("needle"));
    }
    let output = run_details_call(
        Shell::new(dir.path().to_path_buf(), FakeClock::new()),
        args("printf '%s' \"${FIBER_GREP_MATCHES-unset}\""),
    );
    assert_eq!(text(&output), "unset\nExit code 0.\n");
}

#[test]
fn a_foreground_grep_moved_after_writing_matches_keeps_only_receipt_details() {
    let dir = fakes::TempDir::new("fiber-shell-moved-matches");
    let jobs = fakes::jobs::FakeJobs::new(dir.path());
    let fiber = fakes::script(
        dir.path(),
        "moved-search",
        r#"
printf '%s' "$FIBER_GREP_MATCHES" > called
printf '%s\n' '{"path":"a","line":1,"text":"x","spans":[[0,1]],"context":false}' > "$FIBER_GREP_MATCHES"
mkfifo block
exec 3<>block
/bin/bash -c 'read -r -t 120 line <&3' &
"#,
    );
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new())
        .with_search(fiber)
        .with_jobs(jobs.clone());
    let output = run_details_call(shell, args("grep x a"));
    // The receipt proves the group moved; release the descendant before assertions.
    let mut release = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.path().join("block"))
        .unwrap();
    std::io::Write::write_all(&mut release, b"done\n").unwrap();
    let ended = jobs
        .ended(fakes::Deadline::after(Duration::from_secs(10)))
        .expect("moved grep descendant ended");
    assert_eq!(ended.status, Outcome::Completed);
    assert_eq!(output.details, Some(json!({"command":"grep x a"})));
    assert!(output.process.is_none());
    let called = std::fs::read_to_string(dir.path().join("called")).unwrap();
    assert!(!Path::new(&called).parent().unwrap().exists());
}

#[test]
fn background_search_has_no_match_side_channel() {
    let dir = fakes::TempDir::new("fiber-shell-background-matches");
    let jobs = fakes::jobs::FakeJobs::new(dir.path());
    let fiber = fakes::script(
        dir.path(),
        "background-search",
        "printf '%s' \"${FIBER_GREP_MATCHES-unset}\"",
    );
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new())
        .with_search(fiber)
        .with_jobs(jobs.clone());
    let mut arguments = args("grep needle a.txt");
    arguments.insert("run_in_background".into(), json!(true));
    let output = run_details_call(shell, arguments);
    let ended = jobs
        .ended(fakes::Deadline::after(Duration::from_secs(10)))
        .expect("background grep ended");
    assert_eq!(ended.status, Outcome::Completed);
    assert_eq!(output.details, Some(json!({"command":"grep needle a.txt"})));
    let [JobRecord::Started(started)] = output.jobs.as_slice() else {
        panic!("one job");
    };
    assert!(text(&output).contains("Started in the background."));
    let file = dir.path().join(&started.output_path);
    assert_eq!(std::fs::read_to_string(file).unwrap(), "unset");
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
    let git = effects(&shell, "git status");
    assert_eq!(git.declared.effects, vec![Effect::Reads]);
    assert!(git.declared.reversible);
    assert_eq!(git.subject.as_deref(), Some("git status"));
    assert_eq!(git.prefix.as_deref(), Some("git status"));
    assert!(git.declared.paths.is_none());
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
fn a_redirect_stays_in_the_subject_but_never_in_the_prefix() {
    let dir = fakes::TempDir::new("fiber-shell-redirect");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let root = dir.path().to_str().unwrap().to_owned();
    let npm = effects(&shell, "npm test 2>&1");
    assert_closed(&npm);
    assert_eq!(npm.subject.as_deref(), Some("npm test 2>&1"));
    assert_eq!(npm.prefix.as_deref(), Some("npm test"));
    let bare = effects(&shell, "npm 2>&1");
    assert_closed(&bare);
    assert_eq!(bare.subject.as_deref(), Some("npm 2>&1"));
    assert_eq!(bare.prefix.as_deref(), Some("npm"));
    let listed = effects(&shell, "ls 2>/dev/null");
    assert_eq!(listed.declared.effects, vec![Effect::Reads]);
    assert_eq!(listed.subject.as_deref(), Some("ls 2>/dev/null"));
    assert_eq!(listed.prefix.as_deref(), Some("ls"));
    assert_eq!(listed.declared.paths, Some(vec![root]));
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
        Some(vec![src.to_str().unwrap().to_owned()])
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
    // A zero timeout, so a command that wrongly starts stops at once and the
    // assertions below fail, where it would otherwise run for 30 seconds.
    let mut arguments = args(&command);
    arguments.insert("timeout_ms".into(), json!(0));
    let output = shell.run(&arguments, &CancelToken::new(), &Recorder::default());
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
    let mut not_string = Map::new();
    not_string.insert("command".into(), json!(1));
    for (arguments, needle) in [
        (missing_dir, "not a directory"),
        (file_dir, "not a directory"),
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
    // Wiring: a signalled status reads its name through the shared helper.
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "kill -TERM $$"]).process_group(0);
    let mut child = cmd.spawn().unwrap();
    let watchdog = fakes::Watchdog::group(child.id());
    let (done, waited) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _sent = done.send(child.wait());
    });
    let killed = fakes::Deadline::after(Duration::from_secs(10))
        .recv_or_fail(&waited, "the signalled shell to exit")
        .unwrap();
    watchdog.stand_down(fakes::Deadline::after(Duration::from_secs(10)));
    let (_, signal, line) = observed(killed);
    assert_eq!(signal.as_deref(), Some("SIGTERM"));
    assert_eq!(line, "Killed by SIGTERM.");
}

#[test]
fn shell_details_describe_the_command_and_reaped_status() {
    use super::command::StopKind;
    use std::os::unix::process::ExitStatusExt;
    let cases = [
        (
            Some(0),
            None,
            false,
            Some("c"),
            Some(json!({"command":"c", "exit_code":0})),
        ),
        (
            Some(256),
            None,
            false,
            Some("c"),
            Some(json!({"command":"c", "exit_code":1})),
        ),
        (
            Some(9),
            None,
            false,
            Some("c"),
            Some(json!({"command":"c", "signal":"SIGKILL"})),
        ),
        (
            Some(9),
            Some(StopKind::Timeout),
            false,
            Some("c"),
            Some(json!({"command":"c", "signal":"SIGKILL"})),
        ),
        (None, None, true, Some("c"), Some(json!({"command":"c"}))),
        (None, None, false, Some("c"), Some(json!({"command":"c"}))),
        (Some(0), None, false, None, None),
    ];
    for (raw, stop, indeterminate, command, expected) in cases {
        let output = super::assemble(
            super::Limit {
                ms: 1,
                monitor: false,
            },
            Finished {
                output: Kept::Whole(Vec::new()),
                status: raw.map(std::process::ExitStatus::from_raw),
                stop,
                indeterminate,
                held_open: false,
                sent_signal: false,
                capped: false,
                flooded: false,
            },
            None,
            command,
        );
        assert_eq!(output.details, expected, "raw={raw:?}, stop={stop:?}");
    }
    let output = from_spawn(
        Err(std::io::Error::other("missing")),
        super::Limit {
            ms: 1,
            monitor: false,
        },
        "c",
    );
    assert_eq!(output.details, Some(json!({"command":"c"})));
}

#[test]
fn shell_details_keep_the_argument_not_the_search_prelude() {
    let (send, recv) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let dir = fakes::TempDir::new("fiber-shell-command-details");
        let fiber = fakes::script(dir.path(), "search", "exit 0");
        let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new()).with_search(fiber);
        send.send(shell.run(&args("exit 3"), &CancelToken::new(), &Recorder::default()))
            .unwrap();
    });
    let output =
        fakes::Deadline::after(Duration::from_secs(10)).recv_or_fail(&recv, "shell details");
    assert_eq!(
        output.details,
        Some(json!({"command":"exit 3", "exit_code":3}))
    );
}

#[test]
fn a_spawn_failure_is_tool_error_with_no_process() {
    let output = from_spawn(
        Err(std::io::Error::new(std::io::ErrorKind::NotFound, "missing")),
        super::Limit {
            ms: 1,
            monitor: false,
        },
        "c",
    );
    assert_eq!(code(&output), Some(ErrorCode::ToolError));
    assert!(output.process.is_none());
    assert!(text(&output).contains("could not be started"));
}

#[test]
fn execute_fails_when_the_program_does_not_exist() {
    let dir = fakes::TempDir::new("fiber-shell-missing");
    let sandbox = super::sandbox::Settings {
        enabled: false,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: std::path::PathBuf::new(),
    };
    let workspace = dir.path().to_path_buf();
    let err = super::command::execute(
        Path::new("/no/such/fiber-shell"),
        "true",
        dir.path(),
        Duration::from_secs(1),
        FakeClock::new().as_ref(),
        &CancelToken::new(),
        &Recorder::default(),
        super::command::MovePolicy::Stay,
        None,
        None,
        None,
        &sandbox,
        &workspace,
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
    let receipt_details = output.details.clone();
    assert_eq!(started.description, "sleep 30");
    assert_eq!(started.tool.as_deref(), Some("shell"));
    jobs.stop(&started.job_id);
    let ended = jobs
        .ended(fakes::Deadline::after(Duration::from_secs(10)))
        .expect("the stopped sleep to end");
    assert_eq!(ended.status, Outcome::Cancelled);
    assert!(ended.output_tail.is_none());
    assert_eq!(receipt_details, Some(json!({"command": "sleep 30"})));
}

fn monitor_args(command: &str) -> Map<String, Value> {
    let mut arguments = args(command);
    arguments.insert("monitor".into(), json!(true));
    arguments
}

/// Each refusal, with what its message names. A marker the command would
/// touch shows it never started.
#[test]
fn a_monitor_call_that_breaks_a_rule_never_starts() {
    let dir = fakes::TempDir::new("fiber-shell-monitor-args");
    let marker = dir.path().join("marker");
    let touch = format!("touch {}", marker.display());
    let jobs = fakes::jobs::FakeJobs::new(dir.path());
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new()).with_jobs(jobs.clone());
    let with = |pairs: &[(&str, Value)]| {
        let mut arguments = args(&touch);
        for (key, value) in pairs {
            arguments.insert((*key).into(), value.clone());
        }
        arguments
    };
    let cases = [
        (
            with(&[("deadline_ms", json!(1_000))]),
            "set `monitor: true`",
        ),
        (
            with(&[("monitor", json!(true)), ("timeout_ms", json!(1_000))]),
            "A monitor's limit is `deadline_ms`.",
        ),
        (
            with(&[("monitor", json!(true)), ("tty", json!(true))]),
            "`monitor` cannot be combined",
        ),
        (
            with(&[("monitor", json!(true)), ("run_in_background", json!(true))]),
            "`monitor` cannot be combined",
        ),
        (
            with(&[("monitor", json!(true)), ("deadline_ms", json!(1_800_001))]),
            "more than 1800000",
        ),
    ];
    for (arguments, needle) in cases {
        let output = shell.run(&arguments, &CancelToken::new(), &Recorder::default());
        assert_eq!(code(&output), Some(ErrorCode::InvalidArguments), "{needle}");
        assert!(
            text(&output).contains(needle),
            "{needle}: {}",
            text(&output)
        );
        assert!(output.process.is_none());
    }
    let without = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let output = without.run(
        &monitor_args(&touch),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    assert!(
        text(&output).contains("Background jobs are not available in this session."),
        "{}",
        text(&output)
    );
    assert!(!marker.exists(), "a refused call started its command");
    assert!(jobs.started().is_empty());
}

#[test]
fn a_non_interactive_shell_allows_a_deadline_of_ten_minutes_at_most() {
    let dir = fakes::TempDir::new("fiber-shell-monitor-limit");
    let marker = dir.path().join("marker");
    let jobs = fakes::jobs::FakeJobs::new(dir.path());
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new())
        .with_jobs(jobs.clone())
        .non_interactive();
    let mut arguments = monitor_args(&format!("touch {}", marker.display()));
    arguments.insert("deadline_ms".into(), json!(600_001));
    let output = shell.run(&arguments, &CancelToken::new(), &Recorder::default());
    assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    assert!(
        text(&output).contains("more than 600000"),
        "{}",
        text(&output)
    );
    assert!(!marker.exists());
}

/// At each limit the call is accepted: a command that exits at once
/// completes in the foreground with no job.
#[test]
fn a_deadline_at_the_limit_is_accepted() {
    let dir = fakes::TempDir::new("fiber-shell-monitor-at");
    let jobs = fakes::jobs::FakeJobs::new(dir.path());
    let interactive =
        Shell::new(dir.path().to_path_buf(), FakeClock::new()).with_jobs(jobs.clone());
    let batch = Shell::new(dir.path().to_path_buf(), FakeClock::new())
        .with_jobs(jobs.clone())
        .non_interactive();
    for (shell, deadline) in [(&interactive, 1_800_000), (&batch, 600_000)] {
        let mut arguments = monitor_args("exit 0");
        arguments.insert("deadline_ms".into(), json!(deadline));
        let output = shell.run(&arguments, &CancelToken::new(), &Recorder::default());
        assert_ne!(
            code(&output),
            Some(ErrorCode::InvalidArguments),
            "{deadline}"
        );
    }
}

#[test]
fn a_bare_wait_is_refused_for_a_monitor() {
    let dir = fakes::TempDir::new("fiber-shell-monitor-sleep");
    let marker = dir.path().join("marker");
    let jobs = fakes::jobs::FakeJobs::new(dir.path());
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new()).with_jobs(jobs.clone());
    // A zero deadline, so a command that wrongly starts stops at once.
    let mut arguments = monitor_args(&format!("sleep 30; touch {}", marker.display()));
    arguments.insert("deadline_ms".into(), json!(0));
    let output = shell.run(&arguments, &CancelToken::new(), &Recorder::default());
    assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    assert!(text(&output).contains("jobs wait"), "{}", text(&output));
    assert!(!marker.exists());
    assert!(jobs.started().is_empty());
}

#[test]
fn a_monitor_is_classified_as_its_command() {
    let shell = shell();
    let plain = shell.effects(&args("ls")).unwrap();
    let monitored = shell.effects(&monitor_args("ls")).unwrap();
    assert_eq!(plain, monitored);
}

#[test]
fn the_flags_parse_to_one_mode_and_only_run_in_background_skips_the_bare_wait() {
    let cases = [
        (false, false, false, Some(Mode::Foreground)),
        (true, false, false, Some(Mode::Background)),
        (
            false,
            true,
            false,
            Some(Mode::Terminal {
                run_in_background: false,
            }),
        ),
        (
            true,
            true,
            false,
            Some(Mode::Terminal {
                run_in_background: true,
            }),
        ),
        (false, false, true, Some(Mode::Monitor)),
        (true, false, true, None),
        (false, true, true, None),
        (true, true, true, None),
    ];
    for (background, tty, monitor, expected) in cases {
        let mut arguments = args("ls");
        arguments.insert("run_in_background".into(), json!(background));
        arguments.insert("tty".into(), json!(tty));
        arguments.insert("monitor".into(), json!(monitor));
        let mode = parse(&arguments, Path::new("/"), 1_800_000)
            .ok()
            .map(|parsed| parsed.mode);
        assert_eq!(mode, expected, "{background} {tty} {monitor}");
        if let Some(mode) = mode {
            assert_eq!(mode.in_background(), background, "{mode:?}");
        }
    }
}

use super::command::{Finished, Kept};
use super::spill::{Config as SpillConfig, Spill};
use contract::tool::Bound;

fn artifacts(dir: &fakes::TempDir) -> SpillConfig {
    SpillConfig {
        dir: dir.path().join("artifacts"),
        bound: Bound {
            start: 8192,
            end: 8192,
        },
        wrap: None,
        name: None,
    }
}

#[test]
fn with_cap_splits_its_share_and_leaves_the_shell_otherwise() {
    let capped = shell().with_cap(100).unwrap();
    assert_eq!(capped.bound(), Bound { start: 50, end: 50 });
    let capped = shell().with_cap(101).unwrap();
    assert_eq!(capped.bound(), Bound { start: 51, end: 50 });
    assert_eq!(capped.definition().name, "shell");
}

#[test]
fn with_artifacts_stores_a_relative_directory_absolute() {
    let dir = fakes::TempDir::new("fiber-shell-artifacts");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new()).with_artifacts("rel".into());
    assert!(shell.artifacts.as_ref().unwrap().is_absolute());
}

#[test]
fn assemble_of_a_spilled_output_has_no_text_and_names_the_file() {
    let dir = fakes::TempDir::new("fiber-shell-assemble");
    std::fs::create_dir(dir.path().join("artifacts")).unwrap();
    let spill = Spill::start(artifacts(&dir), vec![b'x'; 20_000]);
    let status = Command::new("true").status().unwrap();
    let output = super::assemble(
        super::Limit {
            ms: 1,
            monitor: false,
        },
        Finished {
            output: Kept::Spilled(Box::new(spill)),
            status: Some(status),
            stop: None,
            indeterminate: false,
            held_open: false,
            sent_signal: false,
            capped: false,
            flooded: false,
        },
        None,
        None,
    );
    assert!(output.content.is_empty());
    assert!(output.error.is_none());
    let spilled = output.spilled.expect("spilled is set");
    let kept = spilled.file.as_ref().expect("the file is kept");
    let file = std::fs::read(&kept.path).unwrap();
    assert_eq!(file.len(), 20_000 + 14);
    assert_eq!(&file[..100], &[b'x'; 100][..]);
    assert_eq!(&file[file.len() - 14..], b"\nExit code 0.\n");
    assert!(spilled.tail.ends_with("Exit code 0.\n"));
    assert_eq!(spilled.tail.len(), 8192);
    assert_eq!(spilled.head.len(), 8192);
    assert_eq!(&spilled.head.as_bytes()[..100], &[b'x'; 100][..]);
    assert_eq!(spilled.removed, 20_000 + 14 - 16_384);
}

#[test]
fn assemble_of_a_failed_spilled_output_keeps_its_error_and_process() {
    let dir = fakes::TempDir::new("fiber-shell-assemble-failed");
    std::fs::create_dir(dir.path().join("artifacts")).unwrap();
    let spill = Spill::start(artifacts(&dir), b"partial".to_vec());
    let status = Command::new("sh").args(["-c", "exit 3"]).status().unwrap();
    let output = super::assemble(
        super::Limit {
            ms: 1,
            monitor: false,
        },
        Finished {
            output: Kept::Spilled(Box::new(spill)),
            status: Some(status),
            stop: None,
            indeterminate: false,
            held_open: false,
            sent_signal: false,
            capped: false,
            flooded: false,
        },
        None,
        None,
    );
    assert!(output.content.is_empty());
    assert_eq!(code(&output), Some(ErrorCode::NonzeroExit));
    let process = output.process.expect("a spilled output names its process");
    assert_eq!(process.exit_code, Some(3));
    assert_eq!(process.signal, None);
    assert!(!process.timed_out);
    let spilled = output.spilled.expect("spilled is set");
    assert!(spilled.head.ends_with("Exit code 3.\n"));
}

#[test]
fn assemble_hands_the_open_failure_line_to_both_kinds_of_output() {
    let dir = fakes::TempDir::new("fiber-shell-open-line");
    std::fs::create_dir(dir.path().join("artifacts")).unwrap();
    let status = Command::new("true").status().unwrap();
    let finished = |output| Finished {
        output,
        status: Some(status),
        stop: None,
        indeterminate: false,
        held_open: false,
        sent_signal: false,
        capped: false,
        flooded: false,
    };
    let limit = super::Limit {
        ms: 1,
        monitor: false,
    };
    let line = "It could not move to the background: the jobs are full.";
    let spilled = super::assemble(
        limit,
        finished(Kept::Spilled(Box::new(Spill::start(
            artifacts(&dir),
            vec![b'x'; 20_000],
        )))),
        Some(line),
        None,
    );
    let spilled = spilled.spilled.expect("spilled is set");
    let kept = spilled.file.expect("kept");
    let file = std::fs::read_to_string(&kept.path).unwrap();
    assert!(file.ends_with(&format!("{line}\nExit code 0.\n")), "{file}");
    assert!(
        spilled.tail.ends_with(&format!("{line}\nExit code 0.\n")),
        "{}",
        spilled.tail
    );
    let whole = super::assemble(
        limit,
        finished(Kept::Whole(b"hi".to_vec())),
        Some(line),
        None,
    );
    assert_eq!(
        text(&whole),
        format!("hi\n{line}\nExit code 0.\n"),
        "the line ends the text of a whole output too"
    );
}

/// A spill writer that fails every write.
fn failing_writer() -> super::spill::Wrap {
    use std::io::{self, Write};
    use std::sync::Arc;
    struct Fails;
    impl Write for Fails {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("boom"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    Arc::new(|_file| {
        let fails: Box<dyn Write + Send> = Box::new(Fails);
        fails
    })
}

#[test]
fn a_spill_that_cannot_be_written_keeps_only_its_ends() {
    let dir = fakes::TempDir::new("fiber-shell-spill-fail");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new())
        .with_artifacts(dir.path().join("artifacts"))
        .with_spill_writer(failing_writer());
    std::fs::create_dir(dir.path().join("artifacts")).unwrap();
    let output = shell.run(
        &args("head -c 1048577 /dev/zero | tr '\\0' a"),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    assert!(output.content.is_empty());
    let spilled = output.spilled.expect("spilled is set");
    let Err(failed) = spilled.file else {
        panic!("a failed spill is kept");
    };
    assert!(failed.message.contains("boom"), "{}", failed.message);
    assert_eq!(failed.left, None);
    assert!(
        std::fs::read_dir(dir.path().join("artifacts"))
            .unwrap()
            .next()
            .is_none(),
        "the partial file was removed"
    );
}

fn noted(shell: &Shell, command: &str) -> Option<String> {
    shell.review_note(&args(command))
}

#[test]
fn review_note_names_the_stages_not_shown_read_only() {
    let dir = fakes::TempDir::new("fiber-shell-review-note");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    assert_eq!(
        noted(&shell, "ls && npm test"),
        Some("Not shown read-only: `npm test`".to_owned())
    );
    assert_eq!(noted(&shell, "ls && pwd"), None);
    assert_eq!(noted(&shell, "npm test"), None);
}

#[test]
fn review_note_sees_a_reloaded_read_only_entry() {
    use std::sync::{Arc, Mutex};

    let dir = fakes::TempDir::new("fiber-shell-review-note-reload");
    let handle = Arc::new(Mutex::new(Vec::new()));
    let shell =
        Shell::new(dir.path().to_path_buf(), FakeClock::new()).with_read_only(Arc::clone(&handle));
    assert_eq!(
        noted(&shell, "ls && npm test"),
        Some("Not shown read-only: `npm test`".to_owned())
    );
    *handle.lock().unwrap() = vec![("npm".to_owned(), Vec::new())];
    assert_eq!(noted(&shell, "ls && npm test"), None);
}

fn sandbox_handle_for_test(
    name: &str,
) -> std::sync::Arc<std::sync::Mutex<crate::shell::sandbox::Settings>> {
    let home = std::env::var_os("HOME").map_or_else(
        || std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from,
    );
    let fiber_home = home.join(format!("fiber-shell-sandbox-{name}-{}", std::process::id()));
    std::sync::Arc::new(std::sync::Mutex::new(crate::shell::sandbox::Settings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home,
    }))
}

#[test]
fn effects_sets_confined_for_an_enabled_linux_sandbox() {
    let dir = fakes::TempDir::new("fiber-shell-effects-confined");
    let workspace = dir.path().to_path_buf();
    let shell = Shell::new(workspace.clone(), FakeClock::new());
    let plain = effects(&shell, "echo hi");
    assert!(!plain.declared.confined);
    let sandbox = sandbox_handle_for_test("effects");
    let shell = Shell::new(workspace, FakeClock::new()).with_sandbox(sandbox);
    let confined = effects(&shell, "echo hi");
    #[cfg(target_os = "linux")]
    assert!(confined.declared.confined);
    #[cfg(not(target_os = "linux"))]
    assert!(!confined.declared.confined);
}

#[test]
fn effects_does_not_declare_confined_when_sandbox_setup_is_unavailable() {
    // An unresolvable write grant leaves the call on the reviewed path.
    let dir = fakes::TempDir::new("fiber-shell-effects-unappliable");
    let workspace = dir.path().to_path_buf();
    let home = std::env::var_os("HOME").map_or_else(
        || std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from,
    );
    let sandbox = std::sync::Arc::new(std::sync::Mutex::new(crate::shell::sandbox::Settings {
        enabled: true,
        allow_write: vec![dir.path().join("missing-write")],
        deny_read: Vec::new(),
        fiber_home: home.join(format!("fiber-shell-effects-fail-{}", std::process::id())),
    }));
    let shell = Shell::new(workspace, FakeClock::new()).with_sandbox(sandbox);
    let declared = effects(&shell, "echo hi");
    assert!(!declared.declared.confined);
}

#[test]
fn a_configured_direct_run_shares_the_sandbox_selection() {
    let dir = fakes::TempDir::new("fiber-shell-run-confined");
    let workspace = dir.path().to_path_buf();
    let shell = Shell::new(workspace.clone(), FakeClock::new())
        .with_sandbox(sandbox_handle_for_test("run"));
    // A direct `run` derives its decision at spawn, like `effects` does: the
    // same selection confines this call, and the driver's own shell stays
    // unconfigured everywhere else.
    let confined = effects(&shell, "echo hi");
    #[cfg(target_os = "linux")]
    assert!(confined.declared.confined);
    #[cfg(not(target_os = "linux"))]
    assert!(!confined.declared.confined);
    let output = shell.run(&args("echo hi"), &CancelToken::new(), &Recorder::default());
    assert_eq!(code(&output), None);
    assert!(text(&output).starts_with("hi"), "a direct echo succeeds");
}

#[test]
fn a_failed_confined_setup_never_runs_unconfined() {
    let dir = fakes::TempDir::new("fiber-shell-run-confined-fail");
    let workspace = dir.path().to_path_buf();
    let marker = workspace.join("marker");
    let home = std::env::var_os("HOME").map_or_else(
        || std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from,
    );
    let fiber_home = home.join(format!("fiber-shell-sandbox-fail-{}", std::process::id()));
    let sandbox = std::sync::Arc::new(std::sync::Mutex::new(crate::shell::sandbox::Settings {
        enabled: true,
        allow_write: vec![std::path::PathBuf::from("/nonexistent-fiber-sandbox-allow")],
        deny_read: Vec::new(),
        fiber_home,
    }));
    let shell = Shell::new(workspace.clone(), FakeClock::new()).with_sandbox(sandbox);
    let command = format!("touch {} && echo hi", marker.display());
    let output = shell.run(&args(&command), &CancelToken::new(), &Recorder::default());
    assert_eq!(code(&output), Some(contract::ErrorCode::ToolError));
    assert!(!marker.is_file(), "no command runs after an apply failure");
}
