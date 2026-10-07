//! Tests beside [`super::Artifact`]: what is left on disk after each way a
//! download ends.

#![allow(clippy::unwrap_used, reason = "tests unwrap")]

use std::fs::{self, File, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fakes::TempDir;

use super::{Artifact, Wrap};

/// A writer that fails every write after its first, counting its calls.
struct FailsSecond {
    file: File,
    calls: Arc<AtomicUsize>,
}

impl Write for FailsSecond {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.file.write(bytes)
        } else {
            Err(io::Error::other("the disk is full"))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn fails_second(calls: &Arc<AtomicUsize>) -> Wrap {
    let calls = Arc::clone(calls);
    Arc::new(move |file| {
        Box::new(FailsSecond {
            file,
            calls: Arc::clone(&calls),
        })
    })
}

#[test]
fn an_artifact_dropped_without_keep_is_removed() {
    let dir = TempDir::new("fiber-download");
    let artifacts = dir.path().join("artifacts");
    let path = artifacts.join("w_1.html");
    let mut artifact = Artifact::create(&artifacts, "w_1", "html", None);
    artifact.write(b"<p>x</p>");
    assert!(path.exists(), "the artifact is created");
    drop(artifact);
    assert!(!path.exists(), "the artifact is removed");
    assert!(artifacts.exists(), "the directory stays");
}

#[test]
fn a_kept_artifact_stays_with_the_bytes_written() {
    let dir = TempDir::new("fiber-download");
    let path = dir.path().join("w_1.pdf");
    let mut artifact = Artifact::create(dir.path(), "w_1", "pdf", None);
    artifact.write(b"first ");
    artifact.write(b"second");
    assert_eq!(artifact.keep(), Ok(path.display().to_string()));
    assert_eq!(fs::read(&path).unwrap(), b"first second");
}

#[test]
fn a_failed_write_is_recorded_removes_the_file_and_stops_writing() {
    let dir = TempDir::new("fiber-download");
    let path = dir.path().join("w_1.html");
    let calls = Arc::new(AtomicUsize::new(0));
    let wrap = fails_second(&calls);
    let mut artifact = Artifact::create(dir.path(), "w_1", "html", Some(&wrap));
    artifact.write(b"one");
    artifact.write(b"two");
    assert!(!path.exists(), "the partial file is removed at once");
    artifact.write(b"three");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "no write after the failure"
    );
    assert_eq!(
        artifact.keep(),
        Err(format!(
            "could not save the download to {}: the disk is full.",
            path.display()
        ))
    );
    assert!(!path.exists());
}

#[test]
fn a_directory_that_is_a_file_is_a_save_failure() {
    let dir = TempDir::new("fiber-download");
    let artifacts = dir.path().join("artifacts");
    fs::write(&artifacts, "a file where the directory goes").unwrap();
    let error = fs::create_dir_all(&artifacts).unwrap_err();
    let mut artifact = Artifact::create(&artifacts, "w_1", "html", None);
    artifact.write(b"<p>x</p>");
    assert_eq!(
        artifact.keep(),
        Err(format!(
            "could not save the download to {}: {error}.",
            artifacts.join("w_1.html").display()
        ))
    );
    assert_eq!(
        fs::read(&artifacts).unwrap(),
        b"a file where the directory goes"
    );
}

#[test]
fn a_file_already_at_the_path_is_a_save_failure_and_stays_untouched() {
    let dir = TempDir::new("fiber-download");
    let path = dir.path().join("w_1.html");
    fs::write(&path, "someone else's").unwrap();
    let error = File::create_new(&path).unwrap_err();
    let mut artifact = Artifact::create(dir.path(), "w_1", "html", None);
    artifact.write(b"<p>x</p>");
    assert_eq!(
        artifact.keep(),
        Err(format!(
            "could not save the download to {}: {error}.",
            path.display()
        ))
    );
    assert_eq!(fs::read(&path).unwrap(), b"someone else's");
    let artifact = Artifact::create(dir.path(), "w_1", "html", None);
    drop(artifact);
    assert_eq!(fs::read(&path).unwrap(), b"someone else's");
}

#[test]
fn an_artifact_that_cannot_be_removed_stays_and_the_drop_goes_on() {
    let dir = TempDir::new("fiber-download");
    let artifacts = dir.path().join("artifacts");
    let path = artifacts.join("w_1.html");
    let mut artifact = Artifact::create(&artifacts, "w_1", "html", None);
    artifact.write(b"<p>x</p>");
    fs::set_permissions(&artifacts, Permissions::from_mode(0o555)).unwrap();
    drop(artifact);
    let left = fs::read(&path);
    fs::set_permissions(&artifacts, Permissions::from_mode(0o755)).unwrap();
    assert_eq!(left.unwrap(), b"<p>x</p>");
}
