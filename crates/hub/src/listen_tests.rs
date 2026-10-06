//! Tests for the hub's socket (`docs/state.md`, "Sockets"): the single-hub
//! lock, stale removal, and the live-socket probe.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use super::*;

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("hl");
        let dir = held.path().join("h");
        fs::create_dir_all(&dir).unwrap();
        Self { dir, held }
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("run").join("hub")
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_second_holder_gets_none_while_the_first_holds_the_lock() {
    let temp = Temp::new();
    let first = listen(&temp.dir).unwrap().expect("the first hub binds");
    assert!(UnixStream::connect(temp.socket()).is_ok());
    assert!(listen(&temp.dir).unwrap().is_none());
    first.stop();
}

#[test]
fn after_the_holder_stops_the_next_hub_binds() {
    let temp = Temp::new();
    listen(&temp.dir)
        .unwrap()
        .expect("the first hub binds")
        .stop();
    assert!(!temp.socket().exists());
    let held = listen(&temp.dir).unwrap().expect("the next hub binds");
    assert!(UnixStream::connect(temp.socket()).is_ok());
    held.stop();
}

#[test]
fn a_stale_regular_file_is_removed_and_the_socket_is_bound() {
    let temp = Temp::new();
    fs::create_dir_all(temp.dir.join("run")).unwrap();
    fs::write(temp.socket(), b"leftovers").unwrap();
    let held = listen(&temp.dir).unwrap().expect("the hub binds");
    assert!(UnixStream::connect(temp.socket()).is_ok());
    assert_eq!(mode(&temp.socket()), 0o600);
    held.stop();
}

#[test]
fn a_refused_socket_file_is_removed_and_the_socket_is_bound() {
    let temp = Temp::new();
    fs::create_dir_all(temp.dir.join("run")).unwrap();
    let dead = UnixListener::bind(temp.socket()).unwrap();
    drop(dead);
    assert!(UnixStream::connect(temp.socket()).is_err());
    let held = listen(&temp.dir).unwrap().expect("the hub binds");
    assert!(UnixStream::connect(temp.socket()).is_ok());
    held.stop();
}

#[test]
fn a_live_socket_means_no_bind() {
    let temp = Temp::new();
    fs::create_dir_all(temp.dir.join("run")).unwrap();
    let live = UnixListener::bind(temp.socket()).unwrap();
    assert!(listen(&temp.dir).unwrap().is_none());
    assert!(UnixStream::connect(temp.socket()).is_ok());
    drop(live);
    fs::remove_file(temp.socket()).unwrap();
}

#[test]
fn a_socket_path_at_the_limit_binds() {
    let temp = Temp::new();
    // `home/run/hub` is 8 bytes past `home`: pad `home` to the limit.
    let pad = SOCKET_PATH_MAX - temp.dir.to_string_lossy().len() - 1 - 8;
    let home = temp.dir.join("p".repeat(pad));
    assert_eq!(
        home.join("run").join("hub").as_os_str().len(),
        SOCKET_PATH_MAX
    );
    let held = listen(&home).unwrap().expect("at the limit binds");
    assert!(UnixStream::connect(home.join("run").join("hub")).is_ok());
    held.stop();
}

#[test]
fn a_socket_path_past_the_limit_is_invalid_input() {
    let temp = Temp::new();
    let pad = SOCKET_PATH_MAX - temp.dir.to_string_lossy().len() - 1 - 8 + 1;
    let home = temp.dir.join("p".repeat(pad));
    let Err(error) = listen(&home) else {
        panic!("past the limit is refused");
    };
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
}

#[test]
fn a_symlink_at_the_socket_path_is_not_replaced() {
    let temp = Temp::new();
    fs::create_dir_all(temp.dir.join("run")).unwrap();
    // A symlink loop fails FilesystemLoop (ELOOP) on every platform: on
    // Linux a symlink to a plain file would fail ConnectionRefused and look
    // replaceable, while macOS fails Uncategorized (ENOTSOCK), so no plain
    // file here. A symlink may lead to a live session, so the hub binds
    // nothing.
    std::os::unix::fs::symlink(temp.socket(), temp.socket()).unwrap();
    assert!(listen(&temp.dir).is_err());
    assert!(
        fs::symlink_metadata(temp.socket())
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn run_is_created_0700_when_missing() {
    let temp = Temp::new();
    let held = listen(&temp.dir).unwrap().expect("the hub binds");
    assert_eq!(mode(&temp.dir.join("run")), 0o700);
    assert_eq!(mode(&temp.socket()), 0o600);
    held.stop();
}
