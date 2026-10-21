//! Tests for `directories`: the sorted one-level listing, the files left
//! out and the links followed, the empty answers, the `~/` expansion and
//! every `invalid_arguments` rejection.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::{Path, PathBuf};

use contract::ErrorCode;
use serde_json::{Value, json};

use super::*;
use crate::fake::FakeStarter;
use crate::testkit::args;
use crate::{Environments, Mode, Settings};
use fakes::Deadline;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

/// How long each wait holds, on the wall clock.
const WITHIN: Duration = Duration::from_secs(10);

/// One socket call's bound: each wait's deadline is a `Deadline` receive.
const SOCKET_BOUND: Duration = Duration::from_secs(10);

struct Temp {
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
    dir: PathBuf,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("hd");
        let dir = held.path().to_path_buf();
        Self { held, dir }
    }

    fn home(&self) -> &Path {
        &self.dir
    }

    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.dir.join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file(&self, name: &str) {
        fs::write(self.dir.join(name), b"x").unwrap();
    }

    fn text(&self) -> String {
        self.dir.to_str().unwrap().to_owned()
    }
}

fn listed(home: Option<&Path>, value: Value) -> Value {
    directories(home, &args(value)).unwrap().unwrap()
}

fn refused(home: Option<&Path>, value: Value) -> Refused {
    directories(home, &args(value)).unwrap_err()
}

#[test]
fn subdirectories_answer_sorted_with_path_unchanged() {
    let temp = Temp::new();
    for name in ["b", "a", "C"] {
        temp.dir(name);
    }
    let arg = temp.text();
    assert_eq!(
        listed(None, json!({"path": arg})),
        json!({"directories": ["C", "a", "b"], "path": arg})
    );
}

#[test]
fn files_are_left_out() {
    let temp = Temp::new();
    temp.dir("a");
    temp.file("f.txt");
    let arg = temp.text();
    assert_eq!(
        listed(None, json!({"path": arg})),
        json!({"directories": ["a"], "path": arg})
    );
}

#[test]
fn nested_directories_are_not_listed() {
    let temp = Temp::new();
    let inner = temp.dir("a").join("inner");
    fs::create_dir_all(&inner).unwrap();
    let arg = temp.text();
    assert_eq!(
        listed(None, json!({"path": arg})),
        json!({"directories": ["a"], "path": arg})
    );
}

#[test]
fn hidden_directories_are_listed() {
    let temp = Temp::new();
    temp.dir(".git");
    temp.dir("a");
    let arg = temp.text();
    assert_eq!(
        listed(None, json!({"path": arg})),
        json!({"directories": [".git", "a"], "path": arg})
    );
}

#[test]
fn links_to_directories_are_listed_but_dangling_and_file_links_are_not() {
    let temp = Temp::new();
    temp.dir("target");
    temp.file("f.txt");
    std::os::unix::fs::symlink(temp.dir.join("target"), temp.dir.join("dirlink")).unwrap();
    std::os::unix::fs::symlink(temp.dir.join("f.txt"), temp.dir.join("filelink")).unwrap();
    std::os::unix::fs::symlink(temp.dir.join("missing"), temp.dir.join("dangling")).unwrap();
    let arg = temp.text();
    assert_eq!(
        listed(None, json!({"path": arg})),
        json!({"directories": ["dirlink", "target"], "path": arg})
    );
}

#[test]
fn a_missing_path_answers_empty_with_path() {
    let temp = Temp::new();
    let arg = format!("{}/missing", temp.text());
    assert_eq!(
        listed(None, json!({"path": arg})),
        json!({"directories": [], "path": arg})
    );
}

#[test]
fn a_file_path_answers_empty() {
    let temp = Temp::new();
    temp.file("f.txt");
    let arg = format!("{}/f.txt", temp.text());
    assert_eq!(
        listed(None, json!({"path": arg})),
        json!({"directories": [], "path": arg})
    );
}

#[test]
fn an_unreadable_directory_answers_empty() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = Temp::new();
    let denied = temp.dir("sub");
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&denied).is_ok() {
        panic!("running as root: root bypasses file modes, so the mode row cannot fail here");
    }
    let arg = denied.to_str().unwrap().to_owned();
    assert_eq!(
        listed(None, json!({"path": arg})),
        json!({"directories": [], "path": arg})
    );
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn a_non_utf8_name_is_skipped() {
    use std::os::unix::ffi::OsStrExt as _;

    let temp = Temp::new();
    temp.dir("a");
    fs::create_dir_all(temp.dir.join(std::ffi::OsStr::from_bytes(b"bad\xffname"))).unwrap();
    let arg = temp.text();
    assert_eq!(
        listed(None, json!({"path": arg})),
        json!({"directories": ["a"], "path": arg})
    );
}

#[test]
fn tilde_slash_expands_home() {
    let temp = Temp::new();
    let sub = temp.dir("sub");
    fs::create_dir_all(sub.join("n")).unwrap();
    fs::create_dir_all(sub.join("m")).unwrap();
    assert_eq!(
        listed(Some(temp.home()), json!({"path": "~/sub/"})),
        json!({"directories": ["m", "n"], "path": format!("{}/sub/", temp.text())})
    );
}

#[test]
fn tilde_slash_alone_answers_home() {
    let temp = Temp::new();
    temp.dir("a");
    assert_eq!(
        listed(Some(temp.home()), json!({"path": "~/"})),
        json!({"directories": ["a"], "path": format!("{}/", temp.text())})
    );
}

#[test]
fn a_trailing_slash_home_does_not_double() {
    let temp = Temp::new();
    temp.dir("sub");
    let home = PathBuf::from(format!("{}/", temp.text()));
    assert_eq!(
        listed(Some(&home), json!({"path": "~/sub/"})),
        json!({"directories": [], "path": format!("{}/sub/", temp.text())})
    );
}

#[test]
fn home_slash_answers_slash() {
    let got = listed(Some(Path::new("/")), json!({"path": "~/"}));
    assert_eq!(got["path"], "/");
    assert!(got["directories"].is_array(), "{got}");
}

#[test]
fn tilde_without_a_usable_home_is_invalid() {
    use std::os::unix::ffi::OsStrExt as _;

    let non_utf8 = Path::new(std::ffi::OsStr::from_bytes(b"/\xff"));
    for home in [None, Some(Path::new("rel")), Some(non_utf8)] {
        let refused = refused(home, json!({"path": "~/sub"}));
        assert_eq!(refused.code, ErrorCode::InvalidArguments);
    }
}

#[test]
fn relative_and_misshaped_paths_are_invalid() {
    let temp = Temp::new();
    for value in [
        json!({"path": "a/"}),
        json!({"path": "~"}),
        json!({"path": "~user/"}),
        json!({"path": ""}),
        json!({"path": 5}),
        json!({}),
        json!({"path": "~/", "extra": 1}),
    ] {
        let refused = refused(Some(temp.home()), value);
        assert_eq!(refused.code, ErrorCode::InvalidArguments);
    }
}

#[test]
fn an_absolute_path_comes_back_byte_for_byte() {
    let temp = Temp::new();
    temp.dir("sub");
    for rest in ["sub", "sub/"] {
        let arg = format!("{}/{rest}", temp.text());
        let got = listed(Some(temp.home()), json!({"path": arg}));
        assert_eq!(got["path"], arg);
    }
}

#[test]
fn directories_over_serve_accepts_home_and_rejects_relative() {
    let held = fakes::TempDir::new("hw");
    let home = held.path().join("h");
    fs::create_dir_all(&home).unwrap();
    let work = held.path().join("work");
    for name in ["fiber", "fiber-worktrees"] {
        fs::create_dir_all(work.join(name)).unwrap();
    }
    let environments = Environments::new(
        vec![(OsString::from("HOME"), held.path().as_os_str().to_owned())],
        &home,
    );
    let clock = fakes::clock::FakeClock::new();
    let origin = clock.origin();
    let idle_exit = Duration::from_secs(30);
    let serve_clock = Arc::clone(&clock);
    let (done_tx, done_rx) = mpsc::channel();
    let handle = thread::spawn({
        let home = home.clone();
        move || {
            let starter = Arc::new(FakeStarter::hang(&home));
            let result = crate::serve(
                &home,
                Mode::OnDemand,
                || {
                    Ok(Settings {
                        idle_exit,
                        level: log::diag::Level::Info,
                        port: None,
                    })
                },
                "0.0.0",
                starter,
                serve_clock,
                environments,
                Arc::new(crate::fake::FakeMachine::default()),
            );
            done_tx.send(result).unwrap_or(());
        }
    });
    let socket = home.join("run").join("hub");
    let connect_by = Deadline::after(WITHIN);
    let stream = loop {
        if let Ok(stream) = UnixStream::connect(&socket) {
            break stream;
        }
        if connect_by.left().is_zero() {
            panic!("waited for the hub to bind run/hub");
        }
        thread::yield_now();
    };
    stream.set_read_timeout(Some(SOCKET_BOUND)).unwrap();
    let mut writer = stream.try_clone().unwrap();
    let closer = stream.try_clone().unwrap();
    let (line_tx, line_rx) = mpsc::channel();
    let (reader_done_tx, reader_done_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut read = BufReader::new(stream);
        loop {
            let mut text = String::new();
            match read.read_line(&mut text) {
                Ok(0) => break,
                Ok(_) => {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    if line_tx.send(value).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        reader_done_tx.send(()).unwrap_or(());
    });
    let hello = Deadline::after(WITHIN).recv_or_fail(&line_rx, "hub_hello");
    assert_eq!(hello["kind"], "hub_hello");
    // The start-up park at `origin + idle_exit` still stands while this
    // client is counted, so that instant alone proves nothing about the
    // departure below: only a park with no deadline proves the hub saw a
    // client open, which its idle wait takes only after the count left 0.
    assert!(
        clock.await_parked_unbounded(fakes::Deadline::after(WITHIN)),
        "the hub parks with no deadline while the client is open"
    );
    writer
        .write_all(
            format!(
                "{}\n",
                json!({"id": "c_1", "command": "directories", "args": {"path": "~/work/"}})
            )
            .as_bytes(),
        )
        .unwrap();
    let accepted =
        Deadline::after(WITHIN).recv_or_fail(&line_rx, "the directories acknowledgement");
    assert_eq!(accepted["kind"], "command_accepted");
    assert_eq!(accepted["payload"]["command_id"], "c_1");
    assert_eq!(
        accepted["payload"]["result"]["directories"],
        json!(["fiber", "fiber-worktrees"])
    );
    assert_eq!(
        accepted["payload"]["result"]["path"],
        format!("{}/work/", held.path().to_str().unwrap())
    );
    writer
        .write_all(
            format!(
                "{}\n",
                json!({"id": "c_2", "command": "directories", "args": {"path": "work/"}})
            )
            .as_bytes(),
        )
        .unwrap();
    let rejected = Deadline::after(WITHIN).recv_or_fail(&line_rx, "the directories rejection");
    assert_eq!(rejected["kind"], "command_rejected");
    assert_eq!(rejected["payload"]["command_id"], "c_2");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");
    drop(writer);
    closer.shutdown(Shutdown::Both).unwrap();
    Deadline::after(WITHIN).recv_or_fail(&reader_done_rx, "the reader thread to end");
    let serve_id = handle.thread().id();
    assert!(
        clock.await_parked(origin + idle_exit, fakes::Deadline::after(WITHIN)),
        "the hub parks for its idle exit"
    );
    clock.advance_when_parked(
        Some(origin + idle_exit),
        Some(serve_id),
        idle_exit,
        &Deadline::after(WITHIN),
    );
    let code = Deadline::after(WITHIN)
        .recv_or_fail(&done_rx, "the hub to exit for idleness")
        .expect("serve returns");
    assert_eq!(code, 0);
}
