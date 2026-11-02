//! Tests for the workspace file pair: the resolve bound including
//! symlink escapes, the missing read and create, non-UTF-8 and directory
//! refusals, the 10 MiB cap at both edges, and stale writes leaving the
//! file byte-identical with its mtime untouched.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use contract::ErrorCode;

use super::*;

struct Temp {
    dir: PathBuf,
    #[allow(dead_code, reason = "held only for its drop")]
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let (held, dir) = crate::testkit::home("hw");
        let workspace = dir.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        Self {
            dir: workspace,
            held,
        }
    }

    fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let path = self.dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, bytes).unwrap();
        path
    }

    fn read(&self, rel: &str) -> Result<(String, String), Failure> {
        super::read(&self.dir, Path::new(rel))
    }

    fn write_text(&self, rel: &str, text: &str, version: &str) -> Result<(), Failure> {
        super::write(&self.dir, Path::new(rel), text, version)
    }
}

fn code(result: &Result<(String, String), Failure>) -> ErrorCode {
    result.as_ref().unwrap_err().code.clone()
}

#[test]
fn lexical_traversal_and_absolute_outside_are_invalid() {
    let temp = Temp::new();
    temp.write("inside.txt", b"inside");
    for rel in [
        "..",
        "../inside.txt",
        "sub/../../inside.txt",
        "/etc/hostname",
    ] {
        let read = temp.read(rel);
        assert_eq!(read.unwrap_err().code, ErrorCode::InvalidArguments, "{rel}");
        let written = temp.write_text(rel, "x", "missing");
        assert_eq!(
            written.unwrap_err().code,
            ErrorCode::InvalidArguments,
            "{rel}"
        );
    }
    // A missing file outside the workspace is an escape, not a miss.
    let rel = "../nope.txt";
    let missing = temp.read(rel);
    assert_eq!(missing.unwrap_err().code, ErrorCode::InvalidArguments);
    // An absolute path inside the workspace reads like a relative one;
    // an absolute path outside is an escape.
    let absolute = temp.dir.join("inside.txt");
    let (text, _) = super::read(&temp.dir, &absolute).unwrap();
    assert_eq!(text, "inside");
    let outside = temp.dir.parent().unwrap().join("nope.txt");
    let read = super::read(&temp.dir, &outside);
    assert_eq!(read.unwrap_err().code, ErrorCode::InvalidArguments);
    // A relative workspace is a bad shape, never resolved.
    let read = super::read(Path::new("relative"), Path::new("inside.txt"));
    assert_eq!(read.unwrap_err().code, ErrorCode::InvalidArguments);
}

#[test]
fn dotdot_that_resolves_back_inside_is_allowed() {
    let temp = Temp::new();
    temp.write("sub/a.txt", b"back inside");
    let rel = "sub/../sub/a.txt";
    let (text, version) = temp.read(rel).unwrap();
    assert_eq!(text, "back inside");
    assert_eq!(version.len(), 16);
    // A missing file reached through a `..` that resolves back inside
    // reads as missing.
    let rel = "sub/../gone.txt";
    let (text, version) = temp.read(rel).unwrap();
    assert_eq!((text.as_str(), version.as_str()), ("", "missing"));
}

#[test]
fn symlink_escapes_at_every_level_are_refused() {
    let temp = Temp::new();
    let outside = temp.dir.parent().unwrap().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), b"secret").unwrap();
    temp.write("inside.txt", b"inside");
    symlink(&outside, temp.dir.join("link")).unwrap();
    symlink(outside.join("secret.txt"), temp.dir.join("file-link")).unwrap();
    for rel in [
        "link/secret.txt",
        "link/nope.txt",
        "file-link",
        "inside-link",
    ] {
        if rel == "inside-link" {
            symlink(temp.dir.join("inside.txt"), temp.dir.join("inside-link")).unwrap();
            let (text, _) = temp.read(rel).unwrap();
            assert_eq!(text, "inside", "a link inside stays inside");
            continue;
        }
        let read = temp.read(rel);
        assert_eq!(read.unwrap_err().code, ErrorCode::InvalidArguments, "{rel}");
        let written = temp.write_text(rel, "x", "missing");
        assert_eq!(
            written.unwrap_err().code,
            ErrorCode::InvalidArguments,
            "{rel}"
        );
    }
    // The workspace itself reached through a link: the bound follows
    // the link, so inside stays inside and outside stays outside.
    let alias = temp.dir.parent().unwrap().join("alias");
    symlink(&temp.dir, &alias).unwrap();
    let (text, _) = super::read(&alias, Path::new("inside.txt")).unwrap();
    assert_eq!(text, "inside");
    let escaped = super::read(&alias, Path::new("../outside/secret.txt"));
    assert_eq!(escaped.unwrap_err().code, ErrorCode::InvalidArguments);
}

#[test]
fn missing_reads_empty_and_a_write_at_missing_creates_it() {
    let temp = Temp::new();
    let (text, version) = temp.read("gone.txt").unwrap();
    assert_eq!((text.as_str(), version.as_str()), ("", "missing"));
    // A write naming a version for a file that is not there is stale:
    // nothing is created.
    let stale = temp.write_text("gone.txt", "x", "0123456789abcdef");
    assert_eq!(stale.unwrap_err().code, ErrorCode::StaleRequest);
    assert!(!temp.dir.join("gone.txt").exists());
    let rel = "deep/gone.txt";
    temp.write_text(rel, "new", "missing").unwrap();
    assert_eq!(
        fs::read_to_string(temp.dir.join("deep/gone.txt")).unwrap(),
        "new"
    );
    let (text, version) = temp.read(rel).unwrap();
    assert_eq!(text, "new");
    assert_ne!(version, "missing");
}

#[test]
fn non_utf8_fails_io_failed_and_a_directory_is_not_regular() {
    let temp = Temp::new();
    temp.write("binary", &[0xff, 0xfe]);
    let read = temp.read("binary");
    assert_eq!(code(&read), ErrorCode::IoFailed);
    assert!(read.unwrap_err().message.contains("binary"));
    fs::create_dir_all(temp.dir.join("sub")).unwrap();
    let read = temp.read("sub");
    assert_eq!(code(&read), ErrorCode::InvalidArguments);
    assert!(read.unwrap_err().message.contains("not a regular file"));
    let written = temp.write_text("sub", "x", "missing");
    assert_eq!(written.unwrap_err().code, ErrorCode::InvalidArguments);
}

#[test]
fn exactly_10_mib_answers_and_one_byte_more_is_too_large() {
    let temp = Temp::new();
    let limit = 10 * 1024 * 1024;
    temp.write("exact.txt", &vec![b'x'; limit]);
    let (text, _) = temp.read("exact.txt").unwrap();
    assert_eq!(text.len(), limit);
    temp.write("over.txt", &vec![b'x'; limit + 1]);
    let read = temp.read("over.txt");
    assert_eq!(code(&read), ErrorCode::TooLarge);
    let written = temp.write_text("fresh.txt", &"y".repeat(limit + 1), "missing");
    assert_eq!(written.unwrap_err().code, ErrorCode::TooLarge);
    assert!(!temp.dir.join("fresh.txt").exists());
    temp.write_text("fresh.txt", &"y".repeat(limit), "missing")
        .unwrap();
    assert_eq!(fs::read(temp.dir.join("fresh.txt")).unwrap().len(), limit);
}

#[test]
fn a_stale_write_leaves_content_and_mtime_untouched() {
    let temp = Temp::new();
    temp.write_text("file.txt", "first", "missing").unwrap();
    let (_, version) = temp.read("file.txt").unwrap();
    // A hub-side edit lands first.
    fs::write(temp.dir.join("file.txt"), b"second").unwrap();
    let before = fs::read(temp.dir.join("file.txt")).unwrap();
    let mtime = fs::metadata(temp.dir.join("file.txt"))
        .unwrap()
        .modified()
        .unwrap();
    let stale = temp.write_text("file.txt", "lost", &version);
    assert_eq!(stale.unwrap_err().code, ErrorCode::StaleRequest);
    assert_eq!(fs::read(temp.dir.join("file.txt")).unwrap(), before);
    assert_eq!(
        fs::metadata(temp.dir.join("file.txt"))
            .unwrap()
            .modified()
            .unwrap(),
        mtime,
        "a stale write touches neither content nor mtime"
    );
    // The matching version lands.
    let (_, version) = temp.read("file.txt").unwrap();
    temp.write_text("file.txt", "third", &version).unwrap();
    assert_eq!(
        fs::read_to_string(temp.dir.join("file.txt")).unwrap(),
        "third"
    );
}

#[test]
fn versions_are_16_digit_hex_and_stable_across_reads() {
    let temp = Temp::new();
    temp.write("file.txt", b"stable");
    let (_, first) = temp.read("file.txt").unwrap();
    let (_, second) = temp.read("file.txt").unwrap();
    assert_eq!(first, second, "the hash compares equal across reads");
    assert_eq!(first.len(), 16);
    assert!(
        first
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
}
