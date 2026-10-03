//! The doors crate through its public API: the prompt rules, the line a
//! process prints before any session exists, the project's identity, and a
//! session process's boundary.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use contract::events::{Event, FiberStarted, InputItem, TurnStarted};
use contract::inbox::Delivery;
use contract::shapes::{ContentPart, Failure, Origin};
use contract::{ErrorCode, SessionId, TurnId};
use doors::{
    InstallSummary, Session, exit_before_session, failure, install_approved, mint, project, prompt,
    remove_approved,
};
use log::Log;
use serde_json::Value;

/// A temporary directory, removed on drop, with a short name: a session's
/// socket path must fit in 103 bytes on macOS.
struct Temp(
    PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")] fakes::TempDir,
);

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("fd");
        let dir = held.path().to_path_buf();
        Self(dir, held)
    }
}

/// A writer the test reads back after the session is done with it.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Shared {
    fn lines(&self) -> Vec<Value> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

/// A reader that fails the test if read: stdin on a terminal is never read.
struct Untouched;

impl io::Read for Untouched {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("stdin on a terminal was read");
    }
}

fn failure_of(result: Result<String, Failure>) -> Failure {
    result.unwrap_err()
}

const NO_PROMPT: &str = "No prompt. Run `fiber ask \"<prompt>\"` or `fiber ask < <file>`.";

/// A reader whose `read` fails with an error that is not invalid UTF-8.
struct BrokenRead;

impl io::Read for BrokenRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("closed"))
    }
}

#[test]
fn the_prompt_comes_from_the_arguments_alone() {
    // P1: an argument and no `-` is the prompt, and stdin is never read.
    assert_eq!(
        prompt(Some("hi".into()), false, &mut Untouched, true).unwrap(),
        "hi"
    );
    assert_eq!(
        prompt(Some("hi".into()), false, &mut Untouched, false).unwrap(),
        "hi"
    );
    assert_eq!(
        failure_of(prompt(Some(" ".into()), false, &mut Untouched, false)).message,
        NO_PROMPT
    );

    // P2: no argument, no `-`, stdin on a terminal is the usage error, unread.
    assert_eq!(
        failure_of(prompt(None, false, &mut Untouched, true)),
        failure(ErrorCode::Usage, NO_PROMPT)
    );

    // P3: no argument, no `-`, stdin not a terminal is the prompt.
    assert_eq!(
        prompt(None, false, &mut &b"brief\n"[..], false).unwrap(),
        "brief\n"
    );

    // P4: `-` reads stdin to its end, terminal or not.
    assert_eq!(
        prompt(None, true, &mut &b"brief"[..], true).unwrap(),
        "brief"
    );
    assert_eq!(
        prompt(None, true, &mut &b"brief"[..], false).unwrap(),
        "brief"
    );

    // P5: an argument and `-` is the argument, a newline, then stdin.
    assert_eq!(
        prompt(Some("hi".into()), true, &mut &b"more"[..], false).unwrap(),
        "hi\nmore"
    );

    // P6: a whitespace-only part is dropped; nothing left is the P2 error.
    assert_eq!(
        prompt(Some("hi".into()), true, &mut &b" \n"[..], false).unwrap(),
        "hi"
    );
    assert_eq!(
        prompt(Some(" ".into()), true, &mut &b"more"[..], true).unwrap(),
        "more"
    );
    assert_eq!(
        failure_of(prompt(Some(" ".into()), true, &mut &b" \n"[..], false)).message,
        NO_PROMPT
    );
    assert_eq!(
        failure_of(prompt(None, false, &mut &b" \n"[..], false)).message,
        NO_PROMPT
    );

    // P7: a read that happens reports UTF-8 as usage and any other error as
    // io_failed; a read that does not happen never sees the bytes (P1, P2).
    assert_eq!(
        failure_of(prompt(None, false, &mut &[0xff, 0xfe][..], false)),
        failure(ErrorCode::Usage, "stdin is not UTF-8 text.")
    );
    let broken = failure_of(prompt(None, true, &mut BrokenRead, true));
    assert_eq!(broken.code, ErrorCode::IoFailed);
    assert_eq!(
        broken.message,
        format!("stdin could not be read: {}", io::Error::other("closed"))
    );
}

#[test]
fn a_failure_before_any_session_prints_one_line_with_no_session_and_one_sentence() {
    for (failure, exit) in [
        (failure(ErrorCode::Usage, "No prompt."), 2),
        (failure(ErrorCode::NoModel, "No model was chosen."), 1),
    ] {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let message = failure.message.clone();
        let error = serde_json::to_value(&failure).unwrap();

        assert_eq!(exit_before_session(failure, &mut out, &mut err), exit);

        let line: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(line["kind"], "fiber_exited");
        assert_eq!(line.get("session_id"), None);
        assert_eq!(line["payload"]["exit_code"], exit);
        assert_eq!(line["payload"]["error"], error);
        assert!(out.ends_with(b"}\n"));
        assert_eq!(
            String::from_utf8(err).unwrap(),
            format!("fiber: {message}\n")
        );
    }
}

#[test]
fn the_project_is_gits_shared_directory_or_the_launch_directory() {
    let temp = Temp::new();
    let plain = temp.0.join("plain");
    fs::create_dir_all(&plain).unwrap();
    assert_eq!(project(&plain), fs::canonicalize(&plain).unwrap());

    let repo = temp.0.join("repo");
    fs::create_dir_all(repo.join("docs")).unwrap();
    let init = Command::new("git")
        .arg("init")
        .arg("-q")
        .arg(&repo)
        .status();
    assert!(init.unwrap().success());
    let common = fs::canonicalize(repo.join(".git")).unwrap();
    assert_eq!(project(&repo), common);
    assert_eq!(project(&repo.join("docs")), common);
}

/// A session's log and the door side opened on it, in `home` under `temp`.
fn open(temp: &Temp, home: &str, out: &Shared) -> (Arc<Log>, PathBuf, Result<Session, Failure>) {
    let home = temp.0.join(home);
    let sessions = home.join("projects/p/sessions");
    let id = SessionId(mint("s_"));
    let dir = sessions.join(&id.0);
    let clock = fakes::clock::FakeClock::new();
    let log = Arc::new(
        Log::create(
            &sessions,
            id,
            Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
        )
        .unwrap(),
    );
    let session = Session::open(&home, &dir, &log, clock, Vec::new(), Box::new(out.clone()));
    (log, dir, session)
}

fn socket(dir: &Path) -> PathBuf {
    let home = dir.ancestors().nth(4).unwrap();
    home.join("run").join(dir.file_name().unwrap())
}

fn started() -> Event {
    Event::FiberStarted(FiberStarted {
        version: "0.0.0".into(),
        resumed: false,
    })
}

#[test]
fn a_session_binds_its_socket_and_one_that_never_got_a_prompt_leaves_nothing() {
    let temp = Temp::new();
    let out = Shared::default();
    let (log, dir, session) = open(&temp, "h", &out);
    let session = session.unwrap();
    let path = socket(&dir);
    UnixStream::connect(&path).expect("the session's socket accepts a connection");
    let mode = fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let run = path.parent().unwrap();
    assert_eq!(
        fs::metadata(run).unwrap().permissions().mode() & 0o777,
        0o700
    );
    log.append(&started(), None, None).unwrap();

    session.close(log);

    assert!(!path.exists(), "the socket is unlinked on exit");
    assert!(
        !dir.exists(),
        "a session with no turn deletes its directory"
    );
    let lines = out.lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["kind"], "fiber_started");
}

#[test]
fn ask_runs_the_prompt_alone_and_stdout_is_the_log() {
    let temp = Temp::new();
    let out = Shared::default();
    let (log, dir, session) = open(&temp, "h", &out);
    let session = session.unwrap();
    log.append(&started(), None, None).unwrap();
    let failed = failure(ErrorCode::IoFailed, "disk full");

    let ran = session.ask("hi".into(), |inbox| {
        let Delivery::Prompt(message, _) = inbox
            .recv_timeout(Duration::from_secs(10))
            .expect("the prompt arrives")
        else {
            panic!("the prompt arrives as a prompt");
        };
        assert_eq!(message.content, [ContentPart::Text { text: "hi".into() }]);
        assert!(matches!(message.sender.origin, Origin::Driver));
        assert!(message.sender.command_id.0.starts_with("c_"));
        assert!(
            matches!(
                inbox
                    .recv_timeout(Duration::from_secs(10))
                    .expect("close follows the prompt"),
                Delivery::Close(_)
            ),
            "close follows the prompt"
        );
        // The inbox stays open for clients. `ask` itself queued nothing more.
        assert!(inbox.try_recv().is_err(), "nothing follows close");
        log.append(
            &Event::TurnStarted(TurnStarted {
                input: vec![InputItem::Message {
                    content: message.content,
                    sender: message.sender,
                    changed_by: None,
                }],
            }),
            Some(TurnId("t_1".into())),
            None,
        )
        .unwrap();
        Err(failed.clone())
    });
    assert_eq!(ran, Err(failed));
    session.close(log);

    assert!(
        dir.is_dir(),
        "a session that got a prompt keeps its directory"
    );
    assert!(!socket(&dir).exists());
    let log = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    let printed = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
    assert_eq!(printed, log);
}

#[test]
fn a_fiber_home_too_long_for_a_socket_is_a_usage_error_and_leaves_no_session() {
    let temp = Temp::new();
    let (_log, dir, session) = open(&temp, &"h".repeat(110), &Shared::default());

    let error = session.err().unwrap();

    assert_eq!(error.code, ErrorCode::Usage);
    assert!(error.message.contains("FIBER_HOME"));
    assert!(!dir.exists());
}

#[test]
fn a_socket_path_at_the_platforms_limit_binds() {
    let max = if cfg!(target_os = "macos") { 103 } else { 107 };
    let temp = Temp::new();
    // `<home>/run/` and an 18-byte session id.
    let pad = max - "/run/".len() - 18 - temp.0.as_os_str().len() - 1;
    let (log, dir, session) = open(&temp, &"h".repeat(pad), &Shared::default());

    let session = session.unwrap();

    assert_eq!(socket(&dir).as_os_str().len(), max);
    session.close(log);
}

fn summary() -> InstallSummary {
    InstallSummary {
        name: "github.com/aakshintala/fiber/providers/opencode".into(),
        source: "/src/opencode".into(),
        version: "v1.2.0".into(),
        changes: None,
        process: None,
        install_step: None,
        carries: Vec::new(),
        staged: PathBuf::from("/nonexistent-staged"),
        providers: vec![(
            "opencode".into(),
            vec![
                "https://opencode.ai/zen/go/v1".into(),
                "https://opencode.ai/zen/v1".into(),
            ],
        )],
    }
}

#[test]
fn an_install_in_a_terminal_shows_its_summary_and_goes_ahead_only_on_yes() {
    for (answer, approved) in [
        ("y\n", true),
        ("yes\n", true),
        ("n\n", false),
        ("\n", false),
        ("", false),
    ] {
        let mut out = Vec::new();
        let ok = install_approved(&[summary()], true, &mut answer.as_bytes(), &mut out).unwrap();
        assert_eq!(ok, approved, "answer {answer:?}");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Install github.com/aakshintala/fiber/providers/opencode from /src/opencode\n\
             Version v1.2.0\n\
             Provider opencode: https://opencode.ai/zen/go/v1, https://opencode.ai/zen/v1\n\
             Go ahead? [y/N/s to show the full source] "
        );
    }
    let none = InstallSummary {
        providers: Vec::new(),
        ..summary()
    };
    let mut out = Vec::new();
    install_approved(&[none], true, &mut "y\n".as_bytes(), &mut out).unwrap();
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("It registers no provider.\n")
    );
}

#[test]
fn an_install_without_a_terminal_goes_ahead_without_asking() {
    let mut out = Vec::new();
    assert!(install_approved(&[summary()], false, &mut "n\n".as_bytes(), &mut out).unwrap());
    assert!(out.is_empty());
}

#[test]
fn a_summary_of_several_extensions_asks_once_and_an_update_shows_its_changes() {
    let update = InstallSummary {
        changes: Some(" b.lua | 1 +\n".into()),
        ..summary()
    };
    let dep = InstallSummary {
        name: "github.com/acme/dep".into(),
        version: "v1.4.0".into(),
        providers: Vec::new(),
        ..summary()
    };
    let mut out = Vec::new();
    install_approved(&[update, dep], true, &mut "y\n".as_bytes(), &mut out).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "Update github.com/aakshintala/fiber/providers/opencode from /src/opencode\n\
         Version v1.2.0\n\
         Changes since the installed commit:\n b.lua | 1 +\n\
         Provider opencode: https://opencode.ai/zen/go/v1, https://opencode.ai/zen/v1\n\
         Install github.com/acme/dep from /src/opencode\n\
         Version v1.4.0\n\
         It registers no provider.\n\
         Go ahead? [y/N/s to show the full source] "
    );
}

#[test]
fn a_summary_shows_the_program_the_install_step_and_what_the_package_carries() {
    let full = InstallSummary {
        process: Some("node dist/main.js".into()),
        install_step: Some("npm ci".into()),
        carries: vec!["skills: plan, review".into(), "themes: dark.json".into()],
        ..summary()
    };
    let mut out = Vec::new();
    install_approved(&[full], true, &mut "n\n".as_bytes(), &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    for line in [
        "Runs the program: node dist/main.js\n",
        "Install step, run now and at every update: npm ci\n",
        "Its dependencies' own install scripts run too.\n",
        "Carries skills: plan, review\n",
        "Carries themes: dark.json\n",
    ] {
        assert!(text.contains(line), "{line:?} in {text}");
    }
    let plain = String::from_utf8({
        let mut out = Vec::new();
        install_approved(&[summary()], true, &mut "n\n".as_bytes(), &mut out).unwrap();
        out
    })
    .unwrap();
    assert!(
        !plain.contains("Install step") && !plain.contains("Carries"),
        "{plain}"
    );
}

#[test]
fn the_s_key_shows_every_staged_file_and_asks_again() {
    let dir = fakes::TempDir::new("fiber-doors-source");
    fs::create_dir_all(dir.path().join("lib")).unwrap();
    fs::write(dir.path().join("extension.json"), "{}").unwrap();
    fs::write(dir.path().join("lib/a.lua"), "return 1\n").unwrap();
    fs::write(dir.path().join("blob"), [0xff, 0xfe, 0xfd]).unwrap();
    let shown = InstallSummary {
        staged: dir.path().to_path_buf(),
        ..summary()
    };
    let mut out = Vec::new();
    let ok = install_approved(&[shown], true, &mut "s\ny\n".as_bytes(), &mut out).unwrap();
    assert!(ok);
    let text = String::from_utf8(out).unwrap();
    let prompt = "Go ahead? [y/N/s to show the full source] ";
    assert_eq!(text.matches(prompt).count(), 2, "{text}");
    let shown_at = text
        .find("=== github.com/aakshintala/fiber/providers/opencode")
        .unwrap();
    let after = &text[shown_at..];
    assert!(after.contains("--- extension.json\n{}\n"), "{after}");
    assert!(after.contains("--- lib/a.lua\nreturn 1\n"), "{after}");
    assert!(after.contains("--- blob (3 bytes, not text)\n"), "{after}");
    assert!(after.find("--- blob").unwrap() < after.find("--- extension.json").unwrap());
    // `s` then no is still no, and nothing else asks again.
    let mut out = Vec::new();
    let shown = InstallSummary {
        staged: dir.path().to_path_buf(),
        ..summary()
    };
    assert!(!install_approved(&[shown], true, &mut "s\nn\n".as_bytes(), &mut out).unwrap());
}

#[test]
fn a_remove_in_a_terminal_lists_what_it_deletes_and_goes_ahead_only_on_yes() {
    let names = vec![
        "github.com/acme/x".to_owned(),
        "github.com/acme/dep".to_owned(),
    ];
    let data = vec![PathBuf::from("/h/data/github.com-acme-x")];
    for (answer, approved) in [("y\n", true), ("yes\n", true), ("n\n", false), ("", false)] {
        let mut out = Vec::new();
        let ok = remove_approved(&names, &data, true, &mut answer.as_bytes(), &mut out).unwrap();
        assert_eq!(ok, approved, "{answer:?}");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Remove github.com/acme/x\nRemove github.com/acme/dep\n\
             Delete /h/data/github.com-acme-x\nGo ahead? [y/N] "
        );
    }
    let mut out = Vec::new();
    assert!(remove_approved(&names, &data, false, &mut "n\n".as_bytes(), &mut out).unwrap());
    assert!(out.is_empty());
}
