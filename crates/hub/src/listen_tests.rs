//! Tests for the hub's socket (`docs/state.md`, "Sockets"): the single-hub
//! lock, stale removal, and the live-socket probe.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
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

/// Locks `run/` in `home` and binds `run/hub`, as the hub does at start.
fn listen(home: &Path) -> Result<Option<Held>, StartError> {
    let Some(lock) = lock(home)? else {
        return Ok(None);
    };
    Ok(bind(&lock, home)?.map(|bound| Held::new(lock, bound)))
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
fn a_socket_path_past_the_limit_is_home_too_long_after_the_lock() {
    let temp = Temp::new();
    let pad = SOCKET_PATH_MAX - temp.dir.to_string_lossy().len() - 1 - 8 + 1;
    let home = temp.dir.join("p".repeat(pad));
    let held = lock(&home).unwrap().expect("the lock is won");
    let Err(error) = bind(&held, &home) else {
        panic!("past the limit is refused");
    };
    assert!(
        matches!(error, StartError::HomeTooLong { max } if max == SOCKET_PATH_MAX),
        "{error:?}"
    );
    assert!(!home.join("run").join("hub").exists());
}

#[test]
fn a_run_that_is_a_regular_file_is_io_failed_naming_it() {
    let temp = Temp::new();
    fs::write(temp.dir.join("run"), b"not a directory").unwrap();
    let Err(error) = lock(&temp.dir) else {
        panic!("a file at run/ is refused");
    };
    assert_eq!(error.code(), contract::ErrorCode::IoFailed);
    assert!(
        matches!(&error, StartError::Io { path, .. } if *path == temp.dir.join("run")),
        "{error:?}"
    );
}

#[test]
fn a_second_lock_would_block_while_the_first_is_held() {
    let temp = Temp::new();
    let first = lock(&temp.dir).unwrap().expect("the first lock is won");
    assert!(lock(&temp.dir).unwrap().is_none());
    drop(first);
    assert!(lock(&temp.dir).unwrap().is_some());
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
    let Err(error) = listen(&temp.dir) else {
        panic!("a symlink at run/hub is refused");
    };
    assert_eq!(error.code(), contract::ErrorCode::IoFailed);
    assert!(
        matches!(&error, StartError::Io { path, .. } if *path == temp.socket()),
        "{error:?}"
    );
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

/// Runs [`lock_wait`] in `home` on a thread; the receiver gets its result.
fn lock_wait_in(home: &Path) -> std::sync::mpsc::Receiver<Result<Lock, StartError>> {
    let home = home.to_path_buf();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("hub-test-lock-wait".to_owned())
        .spawn(move || done_tx.send(lock_wait(&home)).unwrap_or(()))
        .unwrap();
    done_rx
}

/// How long a wait that must succeed may take, in wall time.
const WITHIN: std::time::Duration = std::time::Duration::from_secs(5);

/// How long the test watches a wait that must not finish.
const STILL: std::time::Duration = std::time::Duration::from_millis(200);

#[test]
fn lock_wait_on_a_free_run_returns_the_lock_at_once() {
    let temp = Temp::new();
    let held = lock_wait_in(&temp.dir)
        .recv_timeout(WITHIN)
        .expect("a free lock is taken before the deadline")
        .unwrap();
    assert!(lock(&temp.dir).unwrap().is_none(), "lock_wait holds run/");
    drop(held);
    assert_eq!(mode(&temp.dir.join("run")), 0o700);
}

#[test]
fn lock_wait_blocks_while_another_holds_the_lock_and_takes_it_on_release() {
    let temp = Temp::new();
    let first = lock(&temp.dir).unwrap().expect("the first lock is won");
    let waiting = lock_wait_in(&temp.dir);
    assert!(
        waiting.recv_timeout(STILL).is_err(),
        "lock_wait returned while another held the lock"
    );
    drop(first);
    let held = waiting
        .recv_timeout(WITHIN)
        .expect("lock_wait takes the lock once it is released")
        .unwrap();
    assert!(lock(&temp.dir).unwrap().is_none(), "lock_wait holds run/");
    drop(held);
}

#[test]
fn lock_wait_on_a_run_that_is_a_regular_file_is_io_failed_naming_it() {
    let temp = Temp::new();
    fs::write(temp.dir.join("run"), b"not a directory").unwrap();
    let Err(error) = lock_wait(&temp.dir) else {
        panic!("a file at run/ is refused");
    };
    assert_eq!(error.code(), contract::ErrorCode::IoFailed);
    assert!(
        matches!(&error, StartError::Io { path, .. } if *path == temp.dir.join("run")),
        "{error:?}"
    );
}
