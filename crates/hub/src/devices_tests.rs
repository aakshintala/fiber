//! Tests for device records: the SHA-256 hex, matching, every skipped
//! shape, symlinks and directories, and the stat-to-open swap race.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use fakes::Deadline;

use super::*;

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let (held, dir) = crate::testkit::home("hd");
        Self { dir, held }
    }

    fn devices(&self) -> PathBuf {
        self.dir.join("hub").join("devices")
    }
}

/// Writes `hub/devices/name` holding `token`'s hash.
fn write_record(home: &Path, name: &str, token: &str) {
    let dir = home.join("hub").join("devices");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(name),
        format!("{{\"token_sha256\":\"{}\"}}", sha256_hex(token.as_bytes())),
    )
    .unwrap();
}

#[test]
fn sha256_of_abc_is_the_standard_digest() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn a_matching_record_holds_its_token() {
    let temp = Temp::new();
    write_record(&temp.dir, "phone", "t0k3n");
    assert_eq!(holder(&temp.dir, "t0k3n").as_deref(), Some("phone"));
    assert!(
        holder(&temp.dir, "t0k3m").is_none(),
        "one character off holds nothing"
    );
}

#[test]
fn no_devices_directory_holds_nothing() {
    let temp = Temp::new();
    assert!(holder(&temp.dir, "t0k3n").is_none());
}

#[test]
fn skipped_shapes_do_not_stop_a_match() {
    let temp = Temp::new();
    fs::create_dir_all(temp.devices()).unwrap();
    // Each bad shape sorts before the valid record, so a reader that
    // stopped at the first bad file would miss it.
    fs::write(temp.devices().join("aaa-non-json"), "not json").unwrap();
    fs::write(temp.devices().join("aab-no-hash"), r#"{"name":"phone"}"#).unwrap();
    fs::write(
        temp.devices().join("aac-upper"),
        format!(
            "{{\"token_sha256\":\"{}\"}}",
            sha256_hex(b"t0k3n").to_uppercase()
        ),
    )
    .unwrap();
    write_record(&temp.dir, "zzz-phone", "t0k3n");
    assert_eq!(holder(&temp.dir, "t0k3n").as_deref(), Some("zzz-phone"));
}

#[test]
fn each_skipped_shape_alone_holds_nothing() {
    for (name, contents) in [
        ("bad", "not json".to_owned()),
        ("no-hash", r#"{"name":"phone"}"#.to_owned()),
        (
            "upper",
            format!(
                "{{\"token_sha256\":\"{}\"}}",
                sha256_hex(b"t0k3n").to_uppercase()
            ),
        ),
        (
            "wrong",
            format!("{{\"token_sha256\":\"{}\"}}", sha256_hex(b"other")),
        ),
    ] {
        let temp = Temp::new();
        fs::create_dir_all(temp.devices()).unwrap();
        fs::write(temp.devices().join(name), contents).unwrap();
        assert!(holder(&temp.dir, "t0k3n").is_none(), "{name} holds nothing");
    }
}

#[test]
fn a_fifo_does_not_hold_authentication_open() {
    let temp = Temp::new();
    fs::create_dir_all(temp.devices()).unwrap();
    let path = temp.devices().join("pipe");
    fakes::fifo(&path);

    let home = temp.dir.clone();
    let (done, returned) = mpsc::channel();
    thread::spawn(move || {
        done.send(holder(&home, "t0k3n")).unwrap_or(());
    });
    let held = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&returned)
        .expect("authentication lookup returns while a FIFO is present");
    assert!(held.is_none(), "a FIFO does not hold a device token");
}

#[test]
fn a_directory_and_a_symlink_named_like_devices_hold_nothing() {
    let temp = Temp::new();
    fs::create_dir_all(temp.devices()).unwrap();
    fs::create_dir(temp.devices().join("phone")).unwrap();
    let elsewhere = temp.dir.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(
        elsewhere.join("real"),
        format!("{{\"token_sha256\":\"{}\"}}", sha256_hex(b"t0k3n")),
    )
    .unwrap();
    std::os::unix::fs::symlink(elsewhere.join("real"), temp.devices().join("tablet")).unwrap();
    assert!(holder(&temp.dir, "t0k3n").is_none());
}

#[test]
fn a_record_swapped_for_a_symlink_before_open_is_refused() {
    let temp = Temp::new();
    write_record(&temp.dir, "phone", "t0k3n");
    let record = temp.devices().join("phone");
    let moved = temp.dir.join("moved-record");
    before_open(move || {
        // The original inode leaves hub/devices/; its path becomes a
        // symlink to that same inode. The open follows it to the same
        // dev/ino, so only refusing symlinks at open refuses it.
        fs::rename(&record, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &record).unwrap();
    });
    assert!(holder(&temp.dir, "t0k3n").is_none());
}

#[test]
fn write_leaves_only_the_record_mode_0600() {
    use std::os::unix::fs::PermissionsExt as _;
    let temp = Temp::new();
    let record = Record {
        name: "phone".to_owned(),
        paired_at: 10,
        last_connected_at: 10,
        token_sha256: sha256_hex(b"token"),
    };
    write(&temp.dir, &record).unwrap();
    assert_eq!(
        fs::metadata(temp.devices().join("phone"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(temp.devices()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(!temp.dir.join("hub/.phone.tmp").exists());
    assert_eq!(holder(&temp.dir, "token").as_deref(), Some("phone"));
}

#[test]
fn touch_changes_only_last_connected_at() {
    let temp = Temp::new();
    write_record(&temp.dir, "phone", "token");
    let path = temp.devices().join("phone");
    let before: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    touch(&temp.dir, "phone", 123).unwrap();
    let mut after: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        after.as_object_mut().unwrap().remove("last_connected_at"),
        Some(Value::from(123))
    );
    assert_eq!(after, before);
}

#[test]
fn remove_of_absent_or_non_regular_is_false() {
    let temp = Temp::new();
    assert!(!remove(&temp.dir, "phone").unwrap());
    fs::create_dir_all(temp.devices().join("phone")).unwrap();
    assert!(!remove(&temp.dir, "phone").unwrap());
    std::os::unix::fs::symlink("phone", temp.devices().join("link")).unwrap();
    assert!(!remove(&temp.dir, "link").unwrap());
    write_record(&temp.dir, "tablet", "token");
    assert!(remove(&temp.dir, "tablet").unwrap());
    assert!(!exists(&temp.dir, "tablet").unwrap());
    assert!(exists(&temp.dir, "link").unwrap());
}

#[test]
fn list_is_sorted_and_skips_non_regular() {
    let temp = Temp::new();
    write_record(&temp.dir, "z", "token");
    write_record(&temp.dir, "a", "other");
    fs::create_dir(temp.devices().join("dir")).unwrap();
    std::os::unix::fs::symlink("a", temp.devices().join("link")).unwrap();
    let listed = list(&temp.dir).unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|record| record.name.as_str())
            .collect::<Vec<_>>(),
        ["a", "z"]
    );
    assert!(
        listed
            .iter()
            .all(|record| record.paired_at.is_none() && record.last_connected_at.is_none())
    );
}

#[test]
fn sync_dir_failure_is_reported_apart_from_the_write() {
    let temp = Temp::new();
    write(
        &temp.dir,
        &Record {
            name: "phone".to_owned(),
            paired_at: 10,
            last_connected_at: 10,
            token_sha256: sha256_hex(b"token"),
        },
    )
    .unwrap();
    fault(Fault::Sync);
    assert!(sync_dir(&temp.dir).is_err());
    assert_eq!(holder(&temp.dir, "token").as_deref(), Some("phone"));
    sync_dir(&temp.dir).unwrap();
}

#[test]
fn a_hash_only_record_outside_the_name_rule_is_returned_by_name() {
    let temp = Temp::new();
    write_record(&temp.dir, ".phone", "t0k3n");
    assert_eq!(holder(&temp.dir, "t0k3n").as_deref(), Some(".phone"));
}

#[test]
fn a_temporary_that_cannot_be_removed_fails_the_write_with_that_error() {
    let temp = Temp::new();
    fs::create_dir_all(temp.dir.join("hub/.phone.tmp")).unwrap();
    let record = Record {
        name: "phone".to_owned(),
        paired_at: 10,
        last_connected_at: 10,
        token_sha256: sha256_hex(b"token"),
    };
    let error = write(&temp.dir, &record).unwrap_err();
    assert_ne!(
        error.kind(),
        io::ErrorKind::AlreadyExists,
        "the removal's error is returned, not the later create's"
    );
}

#[test]
fn a_devices_path_that_is_not_a_directory_is_an_error_not_an_absence() {
    let temp = Temp::new();
    fs::create_dir_all(temp.dir.join("hub")).unwrap();
    fs::write(temp.devices(), "not a directory").unwrap();
    assert!(remove(&temp.dir, "phone").is_err());
    assert!(exists(&temp.dir, "phone").is_err());
    assert!(list(&temp.dir).is_err());
}

#[test]
fn list_with_no_devices_directory_is_empty() {
    let temp = Temp::new();
    assert!(list(&temp.dir).unwrap().is_empty());
}
