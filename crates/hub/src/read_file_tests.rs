//! Tests for `read_file`: the answered bytes and media type, the argument
//! shape and the lexical path rule, links at every level, the missing, too
//! large and unreadable cases, the media-type table, and the wire.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use contract::clock::Clock;
use serde_json::{Value, json};

use super::*;
use crate::connection::{Hub, serve_connection};
use crate::fake::FakeStarter;
use crate::testkit::{args, id};
use fakes::Deadline;

/// One named deadline per receive: the hub answers before it.
const DEADLINE: Duration = Duration::from_secs(10);

/// Bytes outside `artifacts/` that no answer or rejection may carry.
const MARKER: &[u8] = b"marker-via-link-should-never-leave";

struct Temp {
    dir: PathBuf,
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let (held, dir) = crate::testkit::home("hr");
        Self { dir, held }
    }

    fn hub_with(&self) -> (Arc<Hub>, Arc<FakeStarter>) {
        let timed: Arc<dyn Clock> = fakes::clock::FakeClock::new();
        let starter = Arc::new(FakeStarter::bind_and_hold(&self.dir));
        let hub = Arc::new(crate::testkit::hub(&self.dir, starter.clone(), timed));
        (hub, starter)
    }

    fn dir_of(&self, project: &str, n: u64) -> PathBuf {
        crate::recent::session_dir(&self.dir, project, &id(n))
    }

    /// Session `n` in `project`: its log, `artifacts/` and an unheld lock
    /// file. `read_file` takes no lock; the file is for the running
    /// session's case.
    fn session(&self, project: &str, n: u64) -> PathBuf {
        let dir = self.dir_of(project, n);
        fs::create_dir_all(dir.join("artifacts")).unwrap();
        let first = json!({"kind": "session_started", "seq": 0, "session_id": id(n), "payload": {"workspace": "/w"}});
        fs::write(dir.join("events.jsonl"), format!("{first}\n")).unwrap();
        fs::write(dir.join("session.lock"), b"").unwrap();
        dir
    }

    /// Session `n` in `project` with no `artifacts/` directory.
    fn session_without_artifacts(&self, project: &str, n: u64) -> PathBuf {
        let dir = self.dir_of(project, n);
        fs::create_dir_all(&dir).unwrap();
        let first = json!({"kind": "session_started", "seq": 0, "session_id": id(n), "payload": {"workspace": "/w"}});
        fs::write(dir.join("events.jsonl"), format!("{first}\n")).unwrap();
        dir
    }

    fn write(&self, dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let file = dir.join(name);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&file, bytes).unwrap();
        file
    }
}

fn refused(home: &Path, value: Value) -> Refusal {
    read_file(home, &args(value)).unwrap_err()
}

/// No rejection carries a byte of any file or a path the hub produced:
/// only the client's own text.
fn assert_no_leak(temp: &Temp, message: &str) {
    let marker = String::from_utf8_lossy(MARKER);
    assert!(!message.contains(marker.as_ref()), "{message}");
    let home = temp.dir.to_string_lossy().into_owned();
    assert!(!message.contains(&home), "{message}");
}

/// Holds session `dir`'s lock, as a running session process does.
fn hold(dir: &Path) -> File {
    let file = File::open(dir.join("session.lock")).unwrap();
    file.try_lock().unwrap();
    file
}

/// Sends one line on a served connection and returns the answer after
/// `hub_hello`.
fn over_the_wire(hub: &Arc<Hub>, line: &str) -> Value {
    let (a, b) = UnixStream::pair().unwrap();
    let serving = Arc::clone(hub);
    thread::spawn(move || serve_connection(a, serving));
    b.set_read_timeout(Some(DEADLINE)).unwrap();
    (&b).write_all(format!("{line}\n").as_bytes()).unwrap();
    let mut read = BufReader::new(b);
    let mut next = || {
        let mut text = String::new();
        read.read_line(&mut text).unwrap();
        serde_json::from_str::<Value>(&text).unwrap()
    };
    assert_eq!(next()["kind"], "hub_hello");
    next()
}

#[test]
fn an_exited_sessions_file_answers_its_bytes_and_type() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1);
    let bytes = b"\x89PNG\r\n\x1a\n\x00\xffbinary";
    temp.write(&dir.join("artifacts"), "i_3f2a.png", bytes);
    let (hub, starter) = temp.hub_with();
    let line = json!({"id": "c_1", "command": "read_file", "args": {"session": id(1), "path": "artifacts/i_3f2a.png"}});
    let answer = over_the_wire(&hub, &line.to_string());
    assert_eq!(answer["kind"], "command_accepted");
    assert_eq!(
        answer["payload"],
        json!({"command_id": "c_1", "result": {"data": STANDARD.encode(bytes), "mime_type": "image/png"}})
    );
    assert!(starter.resumed().is_empty(), "no session was resumed");
    assert!(starter.received().is_empty(), "no session was contacted");
}

#[test]
fn a_running_sessions_file_answers_while_its_lock_is_held() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1);
    temp.write(&dir.join("artifacts"), "a.txt", b"hello");
    let lock = hold(&dir);
    let got = read_file(
        &temp.dir,
        &args(json!({"session": id(1), "path": "artifacts/a.txt"})),
    )
    .unwrap()
    .unwrap();
    assert_eq!(got, json!({"data": "aGVsbG8=", "mime_type": "text/plain"}));
    let probe = File::open(dir.join("session.lock")).unwrap();
    assert!(
        probe.try_lock().is_err(),
        "the session's lock is still held"
    );
    drop(lock);
}

#[test]
fn a_home_reached_through_a_link_answers() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1);
    temp.write(&dir.join("artifacts"), "a.png", b"bytes");
    let alias = temp.held.path().join("alias");
    std::os::unix::fs::symlink(&temp.dir, &alias).unwrap();
    let got = read_file(
        &alias,
        &args(json!({"session": id(1), "path": "artifacts/a.png"})),
    )
    .unwrap()
    .unwrap();
    assert_eq!(got["mime_type"], "image/png");
}

#[test]
fn an_unknown_session_is_session_not_found() {
    let temp = Temp::new();
    temp.session("-p", 1);
    let (code, message) = refused(
        &temp.dir,
        json!({"session": id(2), "path": "artifacts/a.png"}),
    );
    assert_eq!(code, ErrorCode::SessionNotFound);
    assert_eq!(message, format!("No session `{}`.", id(2)));
}

#[test]
fn arguments_that_do_not_fit_are_invalid_arguments() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1);
    temp.write(&dir.join("artifacts"), "a.png", b"bytes");
    for bad in [
        json!({"path": "artifacts/a.png"}),
        json!({"session": id(1)}),
        json!({"session": id(1), "path": "artifacts/a.png", "extra": true}),
        json!({"session": 7, "path": "artifacts/a.png"}),
        json!({"session": id(1), "path": 7}),
        json!({"session": id(1), "path": null}),
        json!({"session": "s_XYZ", "path": "artifacts/a.png"}),
        json!({"session": "../x", "path": "artifacts/a.png"}),
        json!({}),
    ] {
        let (code, _) = refused(&temp.dir, bad.clone());
        assert_eq!(code, ErrorCode::InvalidArguments, "{bad}");
    }
}

#[test]
fn paths_outside_the_lexical_rule_are_invalid_arguments() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1);
    temp.write(&dir.join("artifacts"), "a.png", b"bytes");
    for bad in [
        "",
        "/etc/passwd",
        "a.png",
        "artifactsx/a.png",
        "artifacts",
        "artifacts/",
        "./artifacts/a.png",
        "artifacts/../events.jsonl",
        "artifacts/../missing",
        "artifacts/sub/../../events.jsonl",
        "artifacts/a\0.png",
    ] {
        let (code, message) = refused(&temp.dir, json!({"session": id(1), "path": bad}));
        assert_eq!(code, ErrorCode::InvalidArguments, "{bad:?}");
        assert_eq!(
            message,
            format!("`{bad}` is not a file under the session's `artifacts/`."),
            "{bad:?}"
        );
    }
    // The lexical rule runs before the session is looked up.
    let (code, _) = refused(
        &temp.dir,
        json!({"session": id(9), "path": "artifacts/../events.jsonl"}),
    );
    assert_eq!(code, ErrorCode::InvalidArguments);
    // The rejection quotes what the client sent, and no hub-produced path.
    let (_, message) = refused(&temp.dir, json!({"session": id(1), "path": "/etc/passwd"}));
    assert!(message.contains("/etc/passwd"), "{message}");
    assert_no_leak(&temp, &message);
}

#[test]
fn a_dotdot_that_stays_inside_answers() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1);
    temp.write(&dir.join("artifacts"), "a.png", b"bytes");
    fs::create_dir_all(dir.join("artifacts").join("sub")).unwrap();
    for path in ["artifacts/sub/../a.png", "artifacts/./a.png"] {
        let got = read_file(&temp.dir, &args(json!({"session": id(1), "path": path})))
            .unwrap()
            .unwrap();
        assert_eq!(got["data"], STANDARD.encode(b"bytes"), "{path}");
    }
}

#[test]
fn a_link_out_of_artifacts_is_invalid_arguments() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1);
    let artifacts = dir.join("artifacts");
    temp.write(&artifacts, "a.png", b"bytes");
    let outside = temp.held.path().join("outside");
    fs::write(&outside, MARKER).unwrap();
    std::os::unix::fs::symlink(dir.join("events.jsonl"), artifacts.join("out.png")).unwrap();
    // A relative path from `artifacts/` to the marker outside Fiber home.
    let mut rel = PathBuf::new();
    let mut at = artifacts.as_path();
    while at != temp.held.path() {
        rel.push("..");
        at = at.parent().unwrap();
    }
    rel.push("outside");
    std::os::unix::fs::symlink(&rel, artifacts.join("far.png")).unwrap();
    std::os::unix::fs::symlink(&outside, artifacts.join("abs.png")).unwrap();
    let outer = temp.held.path().join("outer");
    fs::create_dir_all(&outer).unwrap();
    fs::write(outer.join("x"), MARKER).unwrap();
    std::os::unix::fs::symlink(&outer, artifacts.join("sub")).unwrap();
    let holder = temp.held.path().join("holder");
    fs::create_dir_all(&holder).unwrap();
    fs::write(temp.held.path().join("x"), MARKER).unwrap();
    std::os::unix::fs::symlink(&holder, artifacts.join("inner")).unwrap();
    for path in [
        "artifacts/out.png",
        "artifacts/far.png",
        "artifacts/abs.png",
        "artifacts/sub/x",
        "artifacts/inner/../x",
    ] {
        let (code, message) = refused(&temp.dir, json!({"session": id(1), "path": path}));
        assert_eq!(code, ErrorCode::InvalidArguments, "{path}");
        assert_no_leak(&temp, &message);
    }
}

#[test]
fn a_link_inside_artifacts_answers_its_target() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1);
    let artifacts = dir.join("artifacts");
    temp.write(&artifacts, "sub/a.txt", b"hello");
    temp.write(&artifacts, "a.png", b"bytes");
    std::os::unix::fs::symlink("sub/a.txt", artifacts.join("view.html")).unwrap();
    let got = read_file(
        &temp.dir,
        &args(json!({"session": id(1), "path": "artifacts/view.html"})),
    )
    .unwrap()
    .unwrap();
    assert_eq!(got, json!({"data": "aGVsbG8=", "mime_type": "text/html"}));
    std::os::unix::fs::symlink(artifacts.join("a.png"), artifacts.join("abs.png")).unwrap();
    let got = read_file(
        &temp.dir,
        &args(json!({"session": id(1), "path": "artifacts/abs.png"})),
    )
    .unwrap()
    .unwrap();
    assert_eq!(got["mime_type"], "image/png");
}

#[test]
fn a_link_above_or_at_artifacts_is_invalid_arguments() {
    let temp = Temp::new();
    for n in [1, 2, 3, 4] {
        let dir = temp.session("-p", n);
        temp.write(&dir.join("artifacts"), "a.png", MARKER);
    }
    // A linked `sessions` directory.
    let sessions = temp.dir.join("projects/-p/sessions");
    let moved = temp.held.path().join("sessions-out");
    fs::rename(&sessions, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &sessions).unwrap();
    let (code, message) = refused(
        &temp.dir,
        json!({"session": id(1), "path": "artifacts/a.png"}),
    );
    assert_eq!(code, ErrorCode::InvalidArguments, "linked sessions");
    assert_no_leak(&temp, &message);
    fs::remove_file(&sessions).unwrap();
    fs::rename(&moved, &sessions).unwrap();
    // A linked session directory.
    let link = temp.dir_of("-p", 2);
    let target = temp.held.path().join("session-out");
    fs::rename(&link, &target).unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let (code, message) = refused(
        &temp.dir,
        json!({"session": id(2), "path": "artifacts/a.png"}),
    );
    assert_eq!(code, ErrorCode::InvalidArguments, "linked session");
    assert_no_leak(&temp, &message);
    // `artifacts` itself a link to an outside directory.
    let dir = temp.dir_of("-p", 3);
    let outer = temp.held.path().join("artifacts-out");
    fs::create_dir_all(&outer).unwrap();
    fs::write(outer.join("a.png"), MARKER).unwrap();
    fs::remove_dir_all(dir.join("artifacts")).unwrap();
    std::os::unix::fs::symlink(&outer, dir.join("artifacts")).unwrap();
    let (code, message) = refused(
        &temp.dir,
        json!({"session": id(3), "path": "artifacts/a.png"}),
    );
    assert_eq!(code, ErrorCode::InvalidArguments, "linked artifacts");
    assert_no_leak(&temp, &message);
    // `artifacts` a regular file.
    let dir = temp.dir_of("-p", 4);
    fs::remove_dir_all(dir.join("artifacts")).unwrap();
    fs::write(dir.join("artifacts"), b"not a directory").unwrap();
    let (code, message) = refused(
        &temp.dir,
        json!({"session": id(4), "path": "artifacts/a.png"}),
    );
    assert_eq!(code, ErrorCode::InvalidArguments, "file artifacts");
    assert_no_leak(&temp, &message);
}

#[test]
fn a_self_looped_artifacts_is_io_failed() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1);
    temp.write(&dir.join("artifacts"), "a.png", b"bytes");
    fs::remove_dir_all(dir.join("artifacts")).unwrap();
    std::os::unix::fs::symlink("artifacts", dir.join("artifacts")).unwrap();
    let (code, message) = refused(
        &temp.dir,
        json!({"session": id(1), "path": "artifacts/a.png"}),
    );
    assert_eq!(code, ErrorCode::IoFailed);
    assert!(message.contains("artifacts/a.png"), "{message}");
    assert_no_leak(&temp, &message);
}

#[test]
fn a_missing_file_is_not_found() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1);
    temp.write(&dir.join("artifacts"), "a.png", b"bytes");
    std::os::unix::fs::symlink("gone", dir.join("artifacts").join("dangling.png")).unwrap();
    temp.session_without_artifacts("-p", 2);
    for (n, path) in [
        (1, "artifacts/none.png"),
        (1, "artifacts/a.png/x"),
        (1, "artifacts/a.png/"),
        (1, "artifacts/a.png/."),
        (1, "artifacts/dangling.png"),
        (2, "artifacts/a.png"),
    ] {
        let (code, message) = refused(&temp.dir, json!({"session": id(n), "path": path}));
        assert_eq!(code, ErrorCode::NotFound, "{path}");
        assert_eq!(
            message,
            format!("No file `{path}` in session `{}`.", id(n)),
            "{path}"
        );
    }
}

#[test]
fn a_directory_fifo_or_socket_is_invalid_arguments() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1);
    let artifacts = dir.join("artifacts");
    fs::create_dir_all(artifacts.join("sub")).unwrap();
    std::os::unix::fs::symlink("sub", artifacts.join("dir.png")).unwrap();
    std::os::unix::fs::symlink(".", artifacts.join("up")).unwrap();
    // Bound under a relative path: the session's own path is past
    // `SUN_LEN`, and only the bind reads it. The node still lives in
    // `artifacts/`, so the type check refuses it there.
    let cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&artifacts).unwrap();
    let _held = std::os::unix::net::UnixListener::bind("s.sock").unwrap();
    std::env::set_current_dir(&cwd).unwrap();
    let fifo = artifacts.join("f.png");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    for path in [
        "artifacts/sub",
        "artifacts/dir.png",
        "artifacts/up",
        "artifacts/s.sock",
    ] {
        let (code, message) = refused(&temp.dir, json!({"session": id(1), "path": path}));
        assert_eq!(code, ErrorCode::InvalidArguments, "{path}");
        assert_eq!(
            message,
            format!("`{path}` is not a regular file."),
            "{path}"
        );
    }
    // The FIFO is never opened: the answer arrives without a writer.
    let home = temp.dir.clone();
    let (done, answered) = mpsc::channel();
    thread::spawn(move || {
        done.send(read_file(
            &home,
            &args(json!({"session": id(1), "path": "artifacts/f.png"})),
        ))
        .unwrap();
    });
    match Deadline::after(DEADLINE).recv(&answered) {
        Ok(got) => {
            let (code, _) = got.unwrap_err();
            assert_eq!(code, ErrorCode::InvalidArguments);
        }
        Err(_) => {
            // Release the thread's open, then fail: it must never block.
            drop(File::create(&fifo).unwrap());
            panic!("waited {DEADLINE:?} for the FIFO refusal");
        }
    }
}

#[test]
fn exactly_10_mib_answers_and_one_byte_more_is_too_large() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1);
    let exact = temp.write(&dir.join("artifacts"), "exact.bin", b"");
    File::options()
        .write(true)
        .open(&exact)
        .unwrap()
        .set_len(10 * 1024 * 1024)
        .unwrap();
    let got = read_file(
        &temp.dir,
        &args(json!({"session": id(1), "path": "artifacts/exact.bin"})),
    )
    .unwrap()
    .unwrap();
    assert_eq!(got["mime_type"], "application/octet-stream");
    let decoded = STANDARD.decode(got["data"].as_str().unwrap()).unwrap();
    assert_eq!(decoded.len(), 10 * 1024 * 1024);
    let over = temp.write(&dir.join("artifacts"), "over.bin", b"");
    File::options()
        .write(true)
        .open(&over)
        .unwrap()
        .set_len(10 * 1024 * 1024 + 1)
        .unwrap();
    let (code, message) = refused(
        &temp.dir,
        json!({"session": id(1), "path": "artifacts/over.bin"}),
    );
    assert_eq!(code, ErrorCode::TooLarge);
    assert_eq!(message, "`artifacts/over.bin` is larger than 10 MiB.");
}

#[test]
fn an_unreadable_file_or_a_link_loop_is_io_failed() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = Temp::new();
    let dir = temp.session("-p", 1);
    let denied = temp.write(&dir.join("artifacts"), "a.png", b"bytes");
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).unwrap();
    if File::open(&denied).is_ok() {
        panic!("running as root: root bypasses file modes, so the mode row cannot fail here");
    }
    let (code, message) = refused(
        &temp.dir,
        json!({"session": id(1), "path": "artifacts/a.png"}),
    );
    assert_eq!(code, ErrorCode::IoFailed);
    assert!(message.contains("artifacts/a.png"), "{message}");
    assert_no_leak(&temp, &message);
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o644)).unwrap();
    std::os::unix::fs::symlink("loop", dir.join("artifacts").join("loop")).unwrap();
    let (code, message) = refused(
        &temp.dir,
        json!({"session": id(1), "path": "artifacts/loop"}),
    );
    assert_eq!(code, ErrorCode::IoFailed);
    assert!(message.contains("artifacts/loop"), "{message}");
    assert_no_leak(&temp, &message);
}

#[test]
fn media_type_by_extension() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1);
    for (name, mime) in [
        ("a.png", "image/png"),
        ("a.jpg", "image/jpeg"),
        ("a.jpeg", "image/jpeg"),
        ("a.gif", "image/gif"),
        ("a.webp", "image/webp"),
        ("a.svg", "image/svg+xml"),
        ("a.pdf", "application/pdf"),
        ("a.html", "text/html"),
        ("a.htm", "text/html"),
        ("a.txt", "text/plain"),
        ("a.log", "text/plain"),
        ("a.md", "text/markdown"),
        ("a.json", "application/json"),
        ("a.csv", "text/csv"),
        ("A.PNG", "image/png"),
        ("a.JpEg", "image/jpeg"),
        ("noext", "application/octet-stream"),
        (".png", "application/octet-stream"),
        ("a.xyz", "application/octet-stream"),
        ("a.tar.gz", "application/octet-stream"),
    ] {
        temp.write(&dir.join("artifacts"), name, b"x");
        let path = format!("artifacts/{name}");
        let got = read_file(&temp.dir, &args(json!({"session": id(1), "path": path})))
            .unwrap()
            .unwrap();
        assert_eq!(got["mime_type"], mime, "{name}");
    }
}

#[test]
fn read_file_over_the_wire_accepts_and_rejects() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1);
    temp.write(&dir.join("artifacts"), "a.txt", b"hello");
    let hub = temp.hub_with().0;
    let (a, b) = UnixStream::pair().unwrap();
    let serving = Arc::clone(&hub);
    thread::spawn(move || serve_connection(a, serving));
    b.set_read_timeout(Some(DEADLINE)).unwrap();
    let mut write = b.try_clone().unwrap();
    let mut read = BufReader::new(b);
    let mut next = || {
        let mut text = String::new();
        read.read_line(&mut text).unwrap();
        serde_json::from_str::<Value>(&text).unwrap()
    };
    assert_eq!(next()["kind"], "hub_hello");
    write
        .write_all(
            format!(
                "{}\n",
                json!({"id": "c_1", "command": "read_file", "args": {"session": id(1), "path": "artifacts/a.txt"}})
            )
            .as_bytes(),
        )
        .unwrap();
    let accepted = next();
    assert_eq!(accepted["kind"], "command_accepted");
    assert_eq!(
        accepted["payload"],
        json!({"command_id": "c_1", "result": {"data": "aGVsbG8=", "mime_type": "text/plain"}})
    );
    write
        .write_all(
            format!(
                "{}\n",
                json!({"id": "c_2", "command": "read_file", "args": {"session": id(1), "path": "artifacts/../events.jsonl"}})
            )
            .as_bytes(),
        )
        .unwrap();
    let rejected = next();
    assert_eq!(rejected["kind"], "command_rejected");
    assert_eq!(rejected["payload"]["command_id"], "c_2");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");
}
