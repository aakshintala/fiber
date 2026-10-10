//! Tests beside the fixture paths and [`super::script`].

use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::{mcp_fixture, script};
use crate::TempDir;
use crate::deadline::Deadline;

/// How long a script child may take. A wait that reaches it fails the test.
const DEADLINE: Duration = Duration::from_secs(10);

/// Waits for `child` on a thread, so a hang fails at [`DEADLINE`].
#[track_caller]
fn waited(child: Child) -> Output {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    match Deadline::after(DEADLINE).recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(err) => panic!("waited {DEADLINE:?} for the script: {err}"),
    }
}

fn spawn(path: &Path, args: &[&str]) -> Child {
    Command::new(path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

#[track_caller]
fn run(path: &Path, args: &[&str]) -> Output {
    waited(spawn(path, args))
}

#[test]
fn the_script_receives_its_arguments() {
    let dir = TempDir::new("fiber-script-args");
    let path = script(dir.path(), "tool", "printf '%s\\n' \"$@\"");
    let output = run(&path, &["a", "b c"]);
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "a\nb c\n");
}

#[test]
fn the_script_keeps_the_spawned_pid() {
    let dir = TempDir::new("fiber-script-pid");
    let path = script(dir.path(), "tool", "echo $$");
    let child = spawn(&path, &[]);
    let spawned = child.id();
    let output = waited(child);
    assert!(output.status.success());
    let printed: u32 = String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(printed, spawned);
}

#[test]
fn the_script_exits_with_the_bodys_status() {
    let dir = TempDir::new("fiber-script-status");
    let path = script(dir.path(), "tool", "exit 3");
    assert_eq!(run(&path, &[]).status.code(), Some(3));
}

#[test]
fn a_second_call_runs_the_new_body() {
    let dir = TempDir::new("fiber-script-rewrite");
    let first = script(dir.path(), "tool", "echo one");
    assert_eq!(first, dir.path().join("tool"));
    assert_eq!(String::from_utf8(run(&first, &[]).stdout).unwrap(), "one\n");
    let second = script(dir.path(), "tool", "echo two");
    assert_eq!(second, first);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("tool.sh")).unwrap(),
        "echo two"
    );
    let target = std::fs::read_link(&second).unwrap();
    assert!(
        target.ends_with("script-fixture/trampoline.sh"),
        "unexpected link target: {}",
        target.display()
    );
    assert_eq!(
        String::from_utf8(run(&second, &[]).stdout).unwrap(),
        "two\n"
    );
}

#[test]
fn the_mcp_fixture_points_at_server_sh() {
    let path = mcp_fixture();
    assert!(
        path.ends_with("mcp-fixture/server.sh"),
        "unexpected fixture path: {}",
        path.display()
    );
    assert!(path.is_file(), "missing fixture: {}", path.display());
}

/// Runs the MCP fixture over `dir`, feeding it `lines` and closing stdin.
#[track_caller]
fn fixture(dir: &Path, lines: &[&str]) -> Output {
    use std::io::Write;
    let mut child = Command::new(mcp_fixture())
        .arg(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for line in lines {
        // A server that already exited refuses the write; the output shows it.
        match writeln!(stdin, "{line}") {
            Ok(()) | Err(_) => {}
        }
    }
    drop(stdin);
    waited(child)
}

const CALL: &str =
    r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"arguments":{},"name":"t"}}"#;
const PING: &str = r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#;

#[test]
fn the_mcp_fixture_exits_unanswered_on_an_exit_result() {
    let dir = TempDir::new("fiber-fixture-exit");
    std::fs::write(dir.path().join("call-t.json"), "exit").unwrap();
    let output = fixture(dir.path(), &[CALL, PING]);
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
    let log = std::fs::read_to_string(dir.path().join("requests.log")).unwrap();
    assert_eq!(log, format!("{CALL}\n"), "nothing is read after the exit");
}

#[test]
fn the_mcp_fixture_notifies_a_list_change_before_answering() {
    let dir = TempDir::new("fiber-fixture-notify");
    std::fs::write(dir.path().join("call-t.json"), r#"{"content":[]}"#).unwrap();
    std::fs::write(dir.path().join("notify-t"), "").unwrap();
    let output = fixture(dir.path(), &[CALL]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n\
         {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[]}}\n",
    );
}

#[test]
fn the_mcp_fixture_fails_its_start_on_a_fail_start_file() {
    let dir = TempDir::new("fiber-fixture-fail-start");
    std::fs::write(dir.path().join("fail-start"), "").unwrap();
    let output = fixture(dir.path(), &[PING]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
    assert!(dir.path().join("pid.txt").exists(), "the server did spawn");
    assert!(
        !dir.path().join("requests.log").exists(),
        "nothing is read before the exit",
    );
}
