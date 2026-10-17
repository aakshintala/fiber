use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::TempDir;
use crate::deadline::Deadline;

/// How long the killed-run test waits for the child to print, and to exit.
const DEADLINE: Duration = Duration::from_secs(10);

/// The child test [`a_killed_run_leaves_a_directory_a_later_one_does_not_reuse`]
/// re-executes. Unset, the test does nothing.
const CHILD: &str = "temp_dir::tests::child_leaves_a_read_only_directory";
const CHILD_PREFIX: &str = "FAKES_TEMP_PREFIX";

fn set_mode(path: &Path, mode: u32) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(mode);
    fs::set_permissions(path, perms).unwrap();
}

fn name_is(path: &Path, prefix: &str) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let Some(suffix) = name.strip_prefix(&format!("{prefix}-")) else {
        return false;
    };
    suffix.len() == 8
        && suffix
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[test]
fn under_places_the_directory_under_its_parent_and_removes_it_on_drop() {
    let parent = TempDir::new("under-parent");
    let child = TempDir::under(parent.path(), "kid");
    assert_eq!(child.path().parent(), Some(parent.path()));
    assert!(name_is(child.path(), "kid"), "{}", child.path().display());
    assert!(child.path().is_dir());
    assert_eq!(fs::read_dir(child.path()).unwrap().count(), 0);
    let path = child.path().to_path_buf();
    drop(child);
    assert!(!path.exists());
}

#[test]
fn write_creates_nested_parents_and_content() {
    let dir = TempDir::new("w");
    dir.write("a/b/c.txt", "hello");
    assert_eq!(
        fs::read_to_string(dir.path().join("a/b/c.txt")).unwrap(),
        "hello"
    );
    dir.write("top.txt", "top");
    assert_eq!(
        fs::read_to_string(dir.path().join("top.txt")).unwrap(),
        "top"
    );
    dir.write("a/b/c.txt", "again");
    assert_eq!(
        fs::read_to_string(dir.path().join("a/b/c.txt")).unwrap(),
        "again"
    );
}

#[test]
fn two_directories_differ_and_leave_nothing() {
    let parent = std::env::temp_dir();
    let a = TempDir::new("t");
    let b = TempDir::new("t");
    assert_ne!(a.path(), b.path());
    assert_eq!(a.path().parent(), Some(parent.as_path()));
    assert_eq!(b.path().parent(), Some(parent.as_path()));
    assert!(a.path().is_dir());
    assert!(b.path().is_dir());
    assert_eq!(fs::read_dir(a.path()).unwrap().count(), 0);
    assert_eq!(fs::read_dir(b.path()).unwrap().count(), 0);
    assert!(name_is(a.path(), "t"), "{}", a.path().display());
    assert!(name_is(b.path(), "t"), "{}", b.path().display());
    let path_a = a.path().to_path_buf();
    let path_b = b.path().to_path_buf();
    drop(a);
    drop(b);
    assert!(!path_a.exists());
    assert!(!path_b.exists());
}

#[test]
fn an_existing_candidate_is_left_and_the_next_is_created() {
    let parent = TempDir::new("skip");
    let stale = parent.path().join("pref-aaaa");
    fs::create_dir(&stale).unwrap();
    fs::write(stale.join("keep"), "x").unwrap();
    let got = super::create(
        parent.path(),
        "pref",
        ["aaaa".to_owned(), "bbbb".to_owned()].into_iter(),
    );
    assert_eq!(got, parent.path().join("pref-bbbb"));
    assert!(got.is_dir());
    assert_eq!(fs::read_dir(&got).unwrap().count(), 0);
    assert!(stale.is_dir());
    assert_eq!(fs::read_to_string(stale.join("keep")).unwrap(), "x");
}

#[test]
fn every_taken_candidate_panics_naming_the_prefix() {
    let parent = TempDir::new("full");
    for suffix in ["aaaa", "bbbb"] {
        fs::create_dir(parent.path().join(format!("pref-{suffix}"))).unwrap();
    }
    let path = parent.path().to_path_buf();
    let caught = std::panic::catch_unwind(|| {
        super::create(
            &path,
            "pref",
            ["aaaa".to_owned(), "bbbb".to_owned()].into_iter(),
        )
    });
    let message = panic_message(caught.unwrap_err());
    assert!(message.contains("pref"), "{message}");
}

#[test]
fn an_error_other_than_a_taken_name_panics_at_once_naming_the_path() {
    let parent = TempDir::new("gone");
    let missing = parent.path().join("missing");
    let caught = std::panic::catch_unwind(|| {
        super::create(&missing, "pref", ["aaaa".to_owned()].into_iter())
    });
    let message = panic_message(caught.unwrap_err());
    assert!(message.contains("creating"), "{message}");
    assert!(
        message.contains(&missing.display().to_string()),
        "{message}"
    );
}

#[test]
fn a_read_only_tree_is_removed_on_drop() {
    let dir = TempDir::new("ro");
    let path = dir.path().to_path_buf();
    let locked = path.join("locked");
    fs::create_dir(&locked).unwrap();
    set_mode(&locked, 0o000);
    let readonly = path.join("readonly");
    fs::create_dir(&readonly).unwrap();
    fs::write(readonly.join("file"), "x").unwrap();
    set_mode(&readonly, 0o555);
    drop(dir);
    assert!(!path.exists());
}

#[test]
fn a_symlink_is_not_followed_when_modes_are_restored() {
    let outside = TempDir::new("out");
    fs::write(outside.path().join("marker"), "keep").unwrap();
    let mode = fs::metadata(outside.path()).unwrap().permissions().mode();
    let dir = TempDir::new("in");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
    let locked = dir.path().join("locked");
    fs::create_dir(&locked).unwrap();
    set_mode(&locked, 0o000);
    let path = dir.path().to_path_buf();
    drop(dir);
    assert!(!path.exists());
    assert_eq!(
        fs::read_to_string(outside.path().join("marker")).unwrap(),
        "keep"
    );
    assert_eq!(
        fs::metadata(outside.path()).unwrap().permissions().mode(),
        mode
    );
}

/// Run by [`a_killed_run_leaves_a_directory_a_later_one_does_not_reuse`]:
/// creates a directory, locks a subdirectory, prints the path and waits.
/// Run on its own it does nothing.
#[test]
#[allow(clippy::print_stdout, reason = "the parent test reads this line")]
#[allow(
    clippy::print_stderr,
    reason = "the parent test quotes this line in its failure message"
)]
fn child_leaves_a_read_only_directory() {
    let Ok(prefix) = std::env::var(CHILD_PREFIX) else {
        return;
    };
    let dir = TempDir::new(&prefix);
    let locked = dir.path().join("locked");
    fs::create_dir(&locked).unwrap();
    set_mode(&locked, 0o000);
    println!("{}", dir.path().display());
    std::io::Write::flush(&mut std::io::stdout()).unwrap();
    let mut rest = String::new();
    let read = std::io::stdin().read_line(&mut rest);
    eprintln!("read_line: {read:?}");
}

#[test]
fn a_killed_run_leaves_a_directory_a_later_one_does_not_reuse() {
    let mut child = spawn_child();
    let leftover = printed_dir(&mut child);

    if let Some(running) = child.0.as_mut() {
        match running.kill() {
            Ok(()) | Err(_) => {}
        }
    }
    let mut killed = child.0.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = killed.wait().and_then(|status| {
            let mut stderr = String::new();
            match killed.stderr.take() {
                Some(mut pipe) => {
                    use std::io::Read;
                    pipe.read_to_string(&mut stderr).map(|_| (status, stderr))
                }
                None => Ok((status, stderr)),
            }
        });
        match tx.send(result) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    let (status, stderr) = Deadline::after(DEADLINE)
        .recv(&rx)
        .unwrap_or_else(|_| panic!("waited {DEADLINE:?} for the killed child to exit"))
        .unwrap();
    assert!(
        status.signal() == Some(9),
        "the child did not die by SIGKILL: {status}; its stderr: {stderr:?}"
    );
    assert!(
        leftover.exists(),
        "the kill left {}; the child's stderr: {stderr:?}",
        leftover.display()
    );

    let next = TempDir::new("fk");
    assert_ne!(next.path(), leftover);
    assert!(next.path().is_dir());
    assert_eq!(fs::read_dir(next.path()).unwrap().count(), 0);
    fs::write(next.path().join("probe"), "ok").unwrap();

    super::restore_modes(&leftover);
    fs::remove_dir_all(&leftover).unwrap();
    assert!(!leftover.exists());
}

/// Kills the child unless it was already reaped, so a failed wait does not
/// leave it running.
struct ChildGuard(Option<Child>);

fn spawn_child() -> ChildGuard {
    ChildGuard(Some(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD, "--nocapture"])
            .env(CHILD_PREFIX, "fk")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ))
}

fn printed_dir(child: &mut ChildGuard) -> PathBuf {
    let stdout = child.0.as_mut().unwrap().stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut lines = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match lines.read_line(&mut line) {
                Ok(0) => {
                    send_line(&tx, None);
                    break;
                }
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.starts_with('/') {
                        send_line(&tx, Some(trimmed.to_owned()));
                        // Drain to EOF: the child's harness prints to stdout after
                        // the test body, and a closed pipe would make it panic.
                        loop {
                            line.clear();
                            match lines.read_line(&mut line) {
                                Ok(0) | Err(_) => break,
                                Ok(_) => {}
                            }
                        }
                        break;
                    }
                }
                Err(_) => {
                    send_line(&tx, None);
                    break;
                }
            }
        }
    });
    let printed = Deadline::after(DEADLINE)
        .recv(&rx)
        .unwrap_or_else(|_| panic!("waited {DEADLINE:?} for the child to print its directory"));
    let Some(printed) = printed else {
        panic!("the child exited before printing its directory");
    };
    let leftover = PathBuf::from(printed);
    assert!(
        leftover.is_dir(),
        "the child printed {}",
        leftover.display()
    );
    leftover
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            match child.kill() {
                Ok(()) | Err(_) => {}
            }
            match child.wait() {
                Ok(_) | Err(_) => {}
            }
        }
    }
}

fn send_line(tx: &mpsc::Sender<Option<String>>, line: Option<String>) {
    match tx.send(line) {
        Ok(()) | Err(mpsc::SendError(_)) => {}
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "panic".to_owned())
}
