use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process;

use fakes::TempDir;

use super::{Ending, ending_of, land, shape_replacement, temporary_name};

fn fiber_temps(dir: &std::path::Path) -> Vec<String> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        if name.contains(".fiber-") {
            names.push(name);
        }
    }
    names
}

#[test]
fn a_successful_write_leaves_no_temp_file() {
    let dir = TempDir::new("fiber-land-ok");
    let target = dir.path().join("a.txt");
    land(&target, b"hi\n").unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"hi\n");
    assert!(fiber_temps(dir.path()).is_empty());
}

#[test]
fn a_failed_rename_leaves_no_temp_file() {
    let dir = TempDir::new("fiber-land-dir");
    let target = dir.path().join("sub");
    fs::create_dir(&target).unwrap();
    let err = land(&target, b"nope").unwrap_err();
    assert!(!err.to_string().is_empty());
    assert!(fiber_temps(dir.path()).is_empty());
    assert!(target.is_dir());
}

#[test]
fn permission_bits_are_kept() {
    let dir = TempDir::new("fiber-land-mode");
    let target = dir.path().join("a.txt");
    fs::write(&target, b"old").unwrap();
    let mut perms = fs::metadata(&target).unwrap().permissions();
    perms.set_mode(0o640);
    fs::set_permissions(&target, perms).unwrap();
    land(&target, b"new").unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"new");
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn a_read_only_file_is_replaced_and_stays_read_only() {
    let dir = TempDir::new("fiber-land-ro");
    let target = dir.path().join("a.txt");
    fs::write(&target, b"old").unwrap();
    let mut perms = fs::metadata(&target).unwrap().permissions();
    perms.set_mode(0o444);
    fs::set_permissions(&target, perms).unwrap();
    land(&target, b"new").unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"new");
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o444
    );
}

#[test]
fn a_hard_link_is_written_in_place() {
    let dir = TempDir::new("fiber-land-link");
    let outside = TempDir::new("fiber-land-link-out");
    let primary = dir.path().join("a.txt");
    let other = outside.path().join("b.txt");
    fs::write(&primary, b"old").unwrap();
    fs::hard_link(&primary, &other).unwrap();
    let inode = fs::metadata(&primary).unwrap().ino();
    land(&primary, b"new").unwrap();
    assert_eq!(fs::read(&primary).unwrap(), b"new");
    assert_eq!(fs::read(&other).unwrap(), b"new");
    assert_eq!(fs::metadata(&primary).unwrap().ino(), inode);
    assert!(fiber_temps(dir.path()).is_empty());
}

#[test]
fn missing_parents_are_created() {
    let dir = TempDir::new("fiber-land-parents");
    let target = dir.path().join("a").join("b").join("c.txt");
    land(&target, b"hi\n").unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"hi\n");
    assert!(fiber_temps(&dir.path().join("a").join("b")).is_empty());
}

#[test]
fn the_temp_name_sits_beside_the_target() {
    let target = std::path::Path::new("/ws/dir/foo.txt");
    let got = temporary_name(target, process::id(), 7);
    assert_eq!(
        got,
        std::path::PathBuf::from(format!("/ws/dir/.foo.txt.fiber-{}-7.tmp", process::id()))
    );
}

#[test]
fn line_endings_and_the_byte_order_mark_follow_the_existing_file() {
    assert_eq!(ending_of(b"a\r\nb\n"), Ending::Crlf);
    assert_eq!(ending_of(b"a\nb\r\n"), Ending::Lf);
    assert_eq!(ending_of(b"no newline"), Ending::Lf);
    assert_eq!(ending_of(b"\n"), Ending::Lf);

    let existing = b"\xEF\xBB\xBFa\r\nb\r\n";
    assert_eq!(
        shape_replacement(existing, "a\nc\n"),
        b"\xEF\xBB\xBFa\r\nc\r\n"
    );
    assert_eq!(
        shape_replacement(existing, "\u{feff}a\r\nc"),
        b"\xEF\xBB\xBFa\r\nc"
    );
    assert_eq!(shape_replacement(b"a\nb\n", "\u{feff}a\nc\n"), b"a\nc\n");
    assert_eq!(shape_replacement(b"a\n", "b\r\nc\n"), b"b\nc\n");
}

#[test]
fn a_lone_carriage_return_is_kept() {
    assert_eq!(shape_replacement(b"a\nb\n", "x\ry\n"), b"x\ry\n");
    assert_eq!(shape_replacement(b"a\r\nb\r\n", "x\ry\n"), b"x\ry\r\n");
}

#[test]
fn a_parent_that_is_a_file_creates_nothing() {
    let dir = TempDir::new("fiber-land-parent-file");
    let parent = dir.path().join("f");
    fs::write(&parent, b"x").unwrap();
    assert!(land(&parent.join("child"), b"nope").is_err());
    assert_eq!(fs::read(&parent).unwrap(), b"x");
    assert!(fiber_temps(dir.path()).is_empty());
}
