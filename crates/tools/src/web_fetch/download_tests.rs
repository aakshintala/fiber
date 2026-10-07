//! Tests beside [`super::Artifact`]: what is left on disk after each way a
//! download ends.

#![allow(clippy::unwrap_used, reason = "tests unwrap")]

use std::cell::Cell;
use std::collections::VecDeque;
use std::fs::{self, File, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fakes::TempDir;

use super::{Artifact, Html, Sink, Wrap, copy};

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

/// A body that yields its pieces one read at a time, as much of each as the
/// buffer takes, then the end, or an error where one is given.
struct Pieces(VecDeque<io::Result<&'static [u8]>>);

impl Read for Pieces {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.0.pop_front() {
            None => Ok(0),
            Some(Err(error)) => Err(error),
            Some(Ok(piece)) => {
                let (now, later) = piece.split_at(piece.len().min(buffer.len()));
                if !later.is_empty() {
                    self.0.push_front(Ok(later));
                }
                let (to, _) = buffer.split_at_mut(now.len());
                to.copy_from_slice(now);
                Ok(now.len())
            }
        }
    }
}

fn pieces(list: impl IntoIterator<Item = io::Result<&'static [u8]>>) -> Pieces {
    Pieces(list.into_iter().collect())
}

/// An HTML download's sink saving into `dir` through `wrap`.
fn html_sink<'a>(
    dir: &std::path::Path,
    wrap: Option<&Wrap>,
    stopped: &'a dyn Fn() -> bool,
) -> Sink<'a> {
    Sink {
        artifact: Some(Artifact::create(dir, "w_1", "html", wrap)),
        html: Some(Html::new(Some("text/html"))),
        stopped,
    }
}

#[test]
fn a_save_failure_stops_saving_and_converting_but_the_body_is_still_counted() {
    let dir = TempDir::new("fiber-download");
    let path = dir.path().join("w_1.html");
    let calls = Arc::new(AtomicUsize::new(0));
    let wrap = fails_second(&calls);
    let mut sink = html_sink(dir.path(), Some(&wrap), &|| false);
    let mut body = pieces([
        Ok(b"<p>one</p>".as_slice()),
        Ok(b"<p>two</p>"),
        Ok(b"<p>three</p>"),
    ]);
    let count = copy(&mut body, 100, &mut sink).unwrap();
    assert_eq!(count, 32, "the whole body is counted");
    assert!(!path.exists(), "the partial file is removed");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "no write after the failure"
    );
    assert_eq!(
        sink.html.unwrap().finish(),
        "one\n",
        "only the first piece converted"
    );
    assert_eq!(
        sink.artifact.unwrap().keep(),
        Err(format!(
            "could not save the download to {}: the disk is full.",
            path.display()
        ))
    );
}

#[test]
fn a_body_past_the_limit_is_read_to_the_limit() {
    let dir = TempDir::new("fiber-download");
    let path = dir.path().join("w_1.html");
    let mut sink = html_sink(dir.path(), None, &|| false);
    let mut body = pieces([Ok(b"<p>one</p>".as_slice()), Ok(b"<p>two</p>")]);
    assert_eq!(copy(&mut body, 12, &mut sink).unwrap(), 12);
    assert_eq!(fs::read(&path).unwrap(), b"<p>one</p><p");
    drop(sink);
    assert!(!path.exists(), "dropped unkept, the artifact is removed");
}

#[test]
fn a_read_error_is_the_copys_error() {
    let dir = TempDir::new("fiber-download");
    let path = dir.path().join("w_1.html");
    let mut sink = html_sink(dir.path(), None, &|| false);
    let mut body = pieces([
        Ok(b"<p>on".as_slice()),
        Err(io::Error::other("the peer went away")),
    ]);
    let error = copy(&mut body, 100, &mut sink).unwrap_err();
    assert_eq!(error.to_string(), "the peer went away");
    drop(sink);
    assert!(!path.exists(), "dropped unkept, the artifact is removed");
}

#[test]
fn a_stop_takes_no_further_piece() {
    let dir = TempDir::new("fiber-download");
    let path = dir.path().join("w_1.html");
    let checks = Cell::new(0);
    let stopped = || {
        checks.set(checks.get() + 1);
        checks.get() > 1
    };
    let mut sink = html_sink(dir.path(), None, &stopped);
    let mut body = pieces([Ok(b"<p>one</p>".as_slice()), Ok(b"<p>two</p>")]);
    let error = copy(&mut body, 100, &mut sink).unwrap_err();
    assert_eq!(error.to_string(), "the fetch was stopped");
    assert_eq!(
        fs::read(&path).unwrap(),
        b"<p>one</p>",
        "the second piece is not saved"
    );
    assert_eq!(sink.html.unwrap().finish(), "one\n", "nor converted");
}

#[test]
fn a_body_neither_saved_nor_converted_is_counted() {
    let mut sink = Sink {
        artifact: None,
        html: None,
        stopped: &|| false,
    };
    let mut body = pieces([Ok(b"12345".as_slice()), Ok(b"678")]);
    assert_eq!(copy(&mut body, 100, &mut sink).unwrap(), 8);
}
