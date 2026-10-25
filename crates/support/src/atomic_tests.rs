//! The `write_atomic` failure phases and the temporary-name retry: a
//! failure before the rename leaves the old file with a plain error, one
//! after it leaves the new content in place with an [`Unsynced`] payload, a
//! taken temporary name retries without touching its owner, and exhaustion
//! reports `AlreadyExists` with the destination intact.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::Ordering;

use super::*;

fn temps(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    names.sort();
    names
}

fn candidate_name(file: &Path, n: u64) -> std::path::PathBuf {
    let pid = std::process::id();
    file.with_file_name(format!(
        "{}.{}-{n}.tmp",
        file.file_name().unwrap().to_string_lossy(),
        pid
    ))
}

#[test]
fn injected_failure_before_rename_keeps_the_old_file() {
    let dir = fakes::TempDir::new("fiber-support-atomic-before");
    let file = dir.path().join("value");
    fs::write(&file, b"old").unwrap();
    fail_at(Some(Stage::BeforeRename));
    let error = write_atomic(&file, b"new", 0o600).unwrap_err();
    fail_at(None);
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert!(
        error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<Unsynced>())
            .is_none(),
        "a pre-rename failure carries no Unsynced marker: {error}"
    );
    assert_eq!(fs::read(&file).unwrap(), b"old");
    assert!(temps(dir.path()).is_empty());
}

#[test]
fn injected_failure_after_rename_reports_unsynced_with_new_file_in_place() {
    let dir = fakes::TempDir::new("fiber-support-atomic-after");
    let file = dir.path().join("value");
    fs::write(&file, b"old").unwrap();
    fail_at(Some(Stage::AfterRename));
    let error = write_atomic(&file, b"new", 0o600).unwrap_err();
    fail_at(None);
    let marker = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<Unsynced>())
        .expect("a post-rename failure carries the Unsynced marker");
    assert_eq!(marker.0.to_string(), "injected");
    assert_eq!(fs::read(&file).unwrap(), b"new");
    assert!(temps(dir.path()).is_empty());
}

#[test]
fn real_rename_failure_keeps_the_old_file() {
    let dir = fakes::TempDir::new("fiber-support-atomic-rename");
    let file = dir.path().join("value");
    fs::write(&file, b"old").unwrap();
    // The name this call will draw for its temporary, read before the call
    // so the retry counter is deterministic within the test.
    let owned = candidate_name(&file, NEXT.load(Ordering::SeqCst));
    before_rename(move || {
        fs::remove_file(&owned).unwrap();
    });
    let error = write_atomic(&file, b"new", 0o600).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(
        error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<Unsynced>())
            .is_none(),
        "a failed rename carries no Unsynced marker: {error}"
    );
    assert_eq!(fs::read(&file).unwrap(), b"old");
    assert!(temps(dir.path()).is_empty());
}

#[test]
fn success_writes_bytes_with_the_requested_mode() {
    let dir = fakes::TempDir::new("fiber-support-atomic-success");
    let file = dir.path().join("value");
    write_atomic(&file, b"hello", 0o600).unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"hello");
    let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert!(temps(dir.path()).is_empty());
}

#[test]
fn missing_parent_fails_not_found_without_a_temp() {
    let dir = fakes::TempDir::new("fiber-support-atomic-parent");
    let file = dir.path().join("no-such-dir").join("value");
    let error = write_atomic(&file, b"new", 0o600).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(!dir.path().join("no-such-dir").exists());
}

#[test]
fn file_as_parent_fails_not_a_directory_without_a_temp() {
    let dir = fakes::TempDir::new("fiber-support-atomic-file-parent");
    let blocker = dir.path().join("file");
    fs::write(&blocker, b"x").unwrap();
    let file = blocker.join("value");
    let error = write_atomic(&file, b"new", 0o600).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotADirectory);
    assert_eq!(fs::read(&blocker).unwrap(), b"x");
    assert!(temps(dir.path()).is_empty());
}

#[test]
fn taken_temporary_names_retry_without_touching_their_owners() {
    let dir = fakes::TempDir::new("fiber-support-atomic-retry");
    let file = dir.path().join("value");
    fs::write(&file, b"old").unwrap();
    let n = NEXT.load(Ordering::SeqCst);
    let first = candidate_name(&file, n);
    let second = candidate_name(&file, n + 1);
    fs::write(&first, b"sentinel-one").unwrap();
    fs::write(&second, b"sentinel-two").unwrap();
    write_atomic(&file, b"new", 0o600).unwrap();
    assert_eq!(fs::read(&first).unwrap(), b"sentinel-one");
    assert_eq!(fs::read(&second).unwrap(), b"sentinel-two");
    assert_eq!(fs::read(&file).unwrap(), b"new");
    assert_eq!(
        temps(dir.path()),
        vec![
            first.file_name().unwrap().to_string_lossy().into_owned(),
            second.file_name().unwrap().to_string_lossy().into_owned(),
        ]
    );
}

#[test]
fn exhausted_temporary_names_report_already_exists_with_destination_intact() {
    let dir = fakes::TempDir::new("fiber-support-atomic-exhausted");
    let file = dir.path().join("value");
    fs::write(&file, b"old").unwrap();
    let n = NEXT.load(Ordering::SeqCst);
    for i in 0..64 {
        fs::write(candidate_name(&file, n + i), b"sentinel").unwrap();
    }
    let error = write_atomic(&file, b"new", 0o600).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&file).unwrap(), b"old");
    for i in 0..64 {
        assert_eq!(fs::read(candidate_name(&file, n + i)).unwrap(), b"sentinel");
    }
}

#[test]
fn unsynced_display_shows_the_inner_message() {
    let marker = Unsynced(io::Error::other("sync broke"));
    assert_eq!(format!("{marker}"), "sync broke");
    assert_eq!(marker.to_string(), "sync broke");
}

#[test]
fn unsynced_source_returns_the_inner_error() {
    use std::error::Error as _;
    let marker = Unsynced(io::Error::other("sync broke"));
    let source = marker.source().expect("Unsynced carries its sync failure");
    assert_eq!(source.to_string(), "sync broke");
    assert!(source.downcast_ref::<Unsynced>().is_none());
}

#[test]
fn fail_nth_before_rename_fails_only_the_second_call() {
    let dir = fakes::TempDir::new("fiber-support-atomic-fail-nth-before");
    let file = dir.path().join("value");
    fs::write(&file, b"old").unwrap();
    fail_nth(2, Stage::BeforeRename);
    write_atomic(&file, b"new-one", 0o600).unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"new-one");
    let error = write_atomic(&file, b"new-two", 0o600).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert!(
        error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<Unsynced>())
            .is_none(),
        "a pre-rename failure carries no Unsynced marker: {error}"
    );
    assert_eq!(fs::read(&file).unwrap(), b"new-one");
    assert!(temps(dir.path()).is_empty());
    write_atomic(&file, b"new-three", 0o600).unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"new-three");
    assert!(temps(dir.path()).is_empty());
}

#[test]
fn fail_nth_after_rename_fails_only_the_second_call() {
    let dir = fakes::TempDir::new("fiber-support-atomic-fail-nth-after");
    let file = dir.path().join("value");
    fs::write(&file, b"old").unwrap();
    fail_nth(2, Stage::AfterRename);
    write_atomic(&file, b"new-one", 0o600).unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"new-one");
    let error = write_atomic(&file, b"new-two", 0o600).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    let marker = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<Unsynced>())
        .expect("a post-rename failure carries the Unsynced marker");
    assert_eq!(marker.0.to_string(), "injected");
    assert_eq!(fs::read(&file).unwrap(), b"new-two");
    assert!(temps(dir.path()).is_empty());
    write_atomic(&file, b"new-three", 0o600).unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"new-three");
    assert!(temps(dir.path()).is_empty());
}

#[test]
fn failure_leaves_no_temp_beside_the_target() {
    let dir = fakes::TempDir::new("fiber-support-atomic-cleanup");
    let file = dir.path().join("value");
    fail_at(Some(Stage::BeforeRename));
    let error = write_atomic(&file, b"new", 0o600).unwrap_err();
    fail_at(None);
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert!(!file.exists());
    assert!(temps(dir.path()).is_empty());
}

#[test]
fn write_count_counts_one_successful_write() {
    let dir = fakes::TempDir::new("fiber-support-atomic-count-one");
    let file = dir.path().join("value");
    reset_write_count();
    write_atomic(&file, b"hello", 0o600).unwrap();
    assert_eq!(write_count(), 1);
    assert_eq!(fs::read(&file).unwrap(), b"hello");
}

#[test]
fn write_count_counts_two_successful_writes() {
    let dir = fakes::TempDir::new("fiber-support-atomic-count-two");
    let file = dir.path().join("value");
    reset_write_count();
    write_atomic(&file, b"one", 0o600).unwrap();
    write_atomic(&file, b"two", 0o600).unwrap();
    assert_eq!(write_count(), 2);
    assert_eq!(fs::read(&file).unwrap(), b"two");
}

#[test]
fn reset_write_count_zeroes_after_writes() {
    let dir = fakes::TempDir::new("fiber-support-atomic-count-reset");
    let file = dir.path().join("value");
    reset_write_count();
    write_atomic(&file, b"one", 0o600).unwrap();
    assert_ne!(write_count(), 0);
    reset_write_count();
    assert_eq!(write_count(), 0);
}
