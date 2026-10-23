//! Binary-level test of a session's close with a command in dispatch
//! (`docs/invocation.md`, "What the hub speaks", "Commands keep their
//! order"): every line a session reads gets exactly one answer on its
//! connection before the session shuts it. A client talks to
//! `fiber session` directly, with no hub.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{
    SessionGuard, Setup, Socket, bounded, first_line, recv_answer, spawn_watched, until,
};

const ID: &str = "s_0123456789abcdef";

/// The receipt `config::import_source` reads first. Made a FIFO, it holds
/// the reader of an `import` until the test opens its write end, and the
/// open returning is the signal that the reader is inside the command.
fn receipt(home: &Path) -> std::path::PathBuf {
    home.join("import-codex.json")
}

#[test]
fn a_command_read_before_the_session_ends_is_answered_before_end_of_file() {
    let setup = Setup::new_under(Path::new("/tmp"));
    let server = ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&server);
    let workspace = setup.workspace();
    let workspace_text = workspace.to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace_text);
    let mut command = setup.fiber(&["session", "--id", ID, "--workspace", &workspace_text]);
    command.current_dir(&workspace).stdin(Stdio::null());
    let (mut child, watchdog) = spawn_watched(&mut command);
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (lines, stdout_lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            drop(line);
        }
    });
    let started = first_line(setup.deadline, &stdout_lines);
    assert_eq!(started["kind"], "session_started");

    fakes::fifo(receipt(&setup.home()));
    let socket = setup.home().join("run").join(ID);
    // D's reader is held inside `import`: the receipt read blocks.
    let held = Socket::connect(setup.deadline, &socket);
    held.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    recv_answer(&held, "c_sub", "the held client's subscribe");
    held.send(r#"{"id":"c_import","command":"import","args":{"source":"codex"}}"#);
    let writer = {
        let path = receipt(&setup.home());
        bounded(
            setup.deadline,
            "the import to open its receipt",
            move || OpenOptions::new().write(true).open(path).unwrap(),
        )
    };

    // The lock handle is opened while the session holds the lock: a
    // blocking lock on it returns only once the session dropped its log.
    let lock = File::open(support::session_dir(&setup, ID).join("session.lock")).unwrap();
    let closer = Socket::connect(setup.deadline, &socket);
    closer.send(r#"{"id":"c_sub2","command":"subscribe","args":{"level":"full"}}"#);
    recv_answer(&closer, "c_sub2", "the closing client's subscribe");
    closer.send(r#"{"id":"c_close","command":"close"}"#);
    until(&closer, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
    bounded(setup.deadline, "the session to drop its log", move || {
        lock.lock().unwrap();
        lock.unlock().unwrap();
    });
    release(writer);

    let seen = support::until_close(&held);
    let answers: Vec<&Value> = seen
        .iter()
        .filter(|line| line["payload"]["command_id"] == "c_import")
        .collect();
    assert_eq!(
        answers.len(),
        1,
        "one answer for the command the session read, before end of file; got {seen:?}"
    );
    assert_eq!(answers[0]["kind"], "command_accepted", "{}", answers[0]);

    let (done, exited) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = setup
        .deadline
        .recv(&exited)
        .expect("waited until the deadline for the session to exit")
        .unwrap();
    assert!(status.success(), "{status:?}");
    watchdog.stand_down(setup.deadline.cleanup_phase());
    guard.wait_gone();
}

/// Completes the receipt the held `import` is reading, so the command
/// answers with the stored report and writes nothing.
fn release(mut writer: File) {
    writer
        .write_all(
            json!({"source": "codex", "imported_at": 0})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    drop(writer);
}
