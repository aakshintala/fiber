//! Tests for device records: the SHA-256 hex, matching, every skipped
//! shape, symlinks and directories, and the stat-to-open swap race.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::{Path, PathBuf};

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
    assert!(holds(&temp.dir, "t0k3n"));
    assert!(!holds(&temp.dir, "t0k3m"), "one character off holds nothing");
}

#[test]
fn no_devices_directory_holds_nothing() {
    let temp = Temp::new();
    assert!(!holds(&temp.dir, "t0k3n"));
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
    assert!(holds(&temp.dir, "t0k3n"));
}

#[test]
fn each_skipped_shape_alone_holds_nothing() {
    for (name, contents) in [
        ("bad", "not json".to_owned()),
        ("no-hash", r#"{"name":"phone"}"#.to_owned()),
        (
            "upper",
            format!("{{\"token_sha256\":\"{}\"}}", sha256_hex(b"t0k3n").to_uppercase()),
        ),
        ("wrong", format!("{{\"token_sha256\":\"{}\"}}", sha256_hex(b"other"))),
    ] {
        let temp = Temp::new();
        fs::create_dir_all(temp.devices()).unwrap();
        fs::write(temp.devices().join(name), contents).unwrap();
        assert!(!holds(&temp.dir, "t0k3n"), "{name} holds nothing");
    }
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
    assert!(!holds(&temp.dir, "t0k3n"));
}

#[test]
fn a_record_swapped_for_a_symlink_between_stat_and_open_is_refused() {
    let temp = Temp::new();
    write_record(&temp.dir, "phone", "t0k3n");
    // A valid record elsewhere, for the same token: without the dev/ino
    // comparison the swapped read would match.
    let elsewhere = temp.dir.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(
        elsewhere.join("real"),
        format!("{{\"token_sha256\":\"{}\"}}", sha256_hex(b"t0k3n")),
    )
    .unwrap();
    let record = temp.devices().join("phone");
    let target = elsewhere.join("real");
    between_stat_and_open(move || {
        fs::remove_file(&record).unwrap();
        std::os::unix::fs::symlink(&target, &record).unwrap();
    });
    assert!(!holds(&temp.dir, "t0k3n"));
}
