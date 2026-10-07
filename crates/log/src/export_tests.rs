use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::*;
use contract::ErrorCode;

const FIRST: &str = "{\"seq\":0,\"kind\":\"a\"}\n";
const SECOND: &str = "{\"seq\":1,\"kind\":\"b\"}\n";

/// A session directory `id` under `root` with `log` as its `events.jsonl`.
fn session(root: &Path, id: &str, log: &[u8]) -> PathBuf {
    let dir = root.join(id);
    fs::create_dir_all(dir.join("artifacts")).unwrap();
    fs::write(dir.join("events.jsonl"), log).unwrap();
    dir
}

#[test]
fn a_complete_log_and_nested_artifacts_land_byte_identical() {
    let root = fakes::TempDir::new("log-export-full");
    let log = format!("{FIRST}{SECOND}");
    let dir = session(root.path(), "s_1", log.as_bytes());
    fs::write(dir.join("artifacts/a_1.txt"), b"full").unwrap();
    fs::create_dir_all(dir.join("artifacts/sub")).unwrap();
    fs::write(dir.join("artifacts/sub/deep.bin"), [0, 1, 2, 255]).unwrap();
    fs::write(dir.join("session.lock"), b"held").unwrap();
    fs::write(dir.join("stray"), b"not a session file").unwrap();

    let target = root.path().join("out");
    export(&dir, &target).unwrap();

    assert_eq!(
        fs::read(target.join("events.jsonl")).unwrap(),
        log.as_bytes()
    );
    assert_eq!(fs::read(target.join("artifacts/a_1.txt")).unwrap(), b"full");
    assert_eq!(
        fs::read(target.join("artifacts/sub/deep.bin")).unwrap(),
        [0, 1, 2, 255]
    );
    assert!(!target.join("session.lock").exists());
    assert!(!target.join("stray").exists());
}

#[test]
fn a_torn_tail_is_left_behind() {
    let root = fakes::TempDir::new("log-export-torn");
    let dir = session(
        root.path(),
        "s_1",
        b"{\"seq\":0,\"kind\":\"a\"}\n{\"seq\":1,\"kin",
    );

    export(&dir, &root.path().join("out")).unwrap();

    assert_eq!(
        fs::read(root.path().join("out/events.jsonl")).unwrap(),
        FIRST.as_bytes()
    );
}

#[test]
fn a_log_with_no_newline_exports_an_empty_log() {
    let root = fakes::TempDir::new("log-export-nonl");
    let dir = session(root.path(), "s_1", b"{\"seq\":0,\"kind\":\"a\"");

    export(&dir, &root.path().join("out")).unwrap();

    assert_eq!(fs::read(root.path().join("out/events.jsonl")).unwrap(), b"");
    assert!(root.path().join("out/artifacts").is_dir());
}

#[test]
fn an_existing_target_is_refused_and_left_alone() {
    let root = fakes::TempDir::new("log-export-exists");
    let dir = session(root.path(), "s_1", FIRST.as_bytes());
    let taken = root.path().join("taken");
    fs::create_dir_all(&taken).unwrap();
    fs::write(taken.join("keep"), b"untouched").unwrap();
    let file = root.path().join("file");
    fs::write(&file, b"untouched").unwrap();

    for target in [&taken, &file] {
        let e = export(&dir, target).unwrap_err();
        assert_eq!(e.code(), ErrorCode::Usage);
        let Error::Exists(path) = e else {
            panic!("{} was overwritten", target.display());
        };
        assert_eq!(&path, target);
    }
    assert_eq!(fs::read(taken.join("keep")).unwrap(), b"untouched");
    assert_eq!(fs::read(&file).unwrap(), b"untouched");
    assert!(!taken.join("events.jsonl").exists());
}

#[test]
fn missing_parents_are_created() {
    let root = fakes::TempDir::new("log-export-parents");
    let dir = session(root.path(), "s_1", FIRST.as_bytes());

    let target = root.path().join("a/b/out");
    export(&dir, &target).unwrap();

    assert_eq!(
        fs::read(target.join("events.jsonl")).unwrap(),
        FIRST.as_bytes()
    );
}

#[test]
fn a_session_with_no_artifacts_gets_an_empty_one() {
    let root = fakes::TempDir::new("log-export-noart");
    let dir = root.path().join("s_1");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("events.jsonl"), FIRST).unwrap();

    export(&dir, &root.path().join("out")).unwrap();

    let artifacts = root.path().join("out/artifacts");
    assert!(artifacts.is_dir());
    assert_eq!(fs::read_dir(&artifacts).unwrap().count(), 0);
}

#[test]
fn an_unreadable_artifact_fails_naming_it_and_removes_the_target() {
    let root = fakes::TempDir::new("log-export-unread");
    let dir = session(root.path(), "s_1", FIRST.as_bytes());
    let hidden = dir.join("artifacts/hidden.txt");
    fs::write(&hidden, b"secret").unwrap();
    fs::set_permissions(&hidden, fs::Permissions::from_mode(0o000)).unwrap();

    let target = root.path().join("out");
    let Err(Error::Io { path, .. }) = export(&dir, &target) else {
        panic!("the unreadable artifact was copied");
    };
    assert_eq!(path, hidden);
    assert!(!target.exists());
}

#[test]
fn a_target_inside_artifacts_exports_without_itself() {
    // Kills the mutant that replaces the excluded-directory skip with
    // `false`: without the skip the export copies `out` into itself
    // until the filesystem errors and the target is removed again.
    let root = fakes::TempDir::new("log-export-nested-target");
    let dir = session(root.path(), "s_1", FIRST.as_bytes());
    fs::write(dir.join("artifacts/a_1.txt"), b"kept").unwrap();

    let target = dir.join("artifacts/out");
    export(&dir, &target).unwrap();

    assert_eq!(
        fs::read(target.join("events.jsonl")).unwrap(),
        FIRST.as_bytes()
    );
    assert_eq!(fs::read(target.join("artifacts/a_1.txt")).unwrap(), b"kept");
    assert!(
        !target.join("artifacts/out").exists(),
        "the export directory must not appear inside its own artifacts"
    );
}

#[test]
fn a_missing_log_is_not_found() {
    let root = fakes::TempDir::new("log-export-nolog");
    let dir = root.path().join("s_1");
    fs::create_dir_all(&dir).unwrap();

    let Err(Error::NotFound(path)) = export(&dir, &root.path().join("out")) else {
        panic!("a missing log exported");
    };
    assert_eq!(path, dir);
    assert!(!root.path().join("out").exists());
}

#[test]
fn an_unreadable_artifacts_directory_fails_naming_it_and_removes_the_target() {
    // Kills the mutant that replaces the `NotFound` guard in
    // `copy_artifacts` with `true`: without the guard an unreadable
    // `artifacts/` is mistaken for a missing one.
    let root = fakes::TempDir::new("log-export-noart-read");
    let dir = session(root.path(), "s_1", FIRST.as_bytes());
    let artifacts = dir.join("artifacts");
    fs::set_permissions(&artifacts, fs::Permissions::from_mode(0o000)).unwrap();

    let target = root.path().join("out");
    let Err(Error::Io { path, .. }) = export(&dir, &target) else {
        fs::set_permissions(&artifacts, fs::Permissions::from_mode(0o755)).unwrap();
        panic!("the unreadable artifacts directory was copied");
    };
    assert_eq!(path, artifacts);
    assert!(!target.exists());
    fs::set_permissions(&artifacts, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn symlinks_are_copied_as_links_without_reading_through() {
    let root = fakes::TempDir::new("log-export-symlink");
    let dir = session(root.path(), "s_1", FIRST.as_bytes());
    let secret = root.path().join("outside-secret.txt");
    fs::write(&secret, b"super-secret").unwrap();
    let outside_dir = root.path().join("outside-dir");
    fs::create_dir_all(outside_dir.join("sub")).unwrap();
    fs::write(outside_dir.join("sub/inner.txt"), b"inner-secret").unwrap();

    std::os::unix::fs::symlink(&secret, dir.join("artifacts/x")).unwrap();
    std::os::unix::fs::symlink(&outside_dir, dir.join("artifacts/d")).unwrap();

    let target = root.path().join("out");
    export(&dir, &target).unwrap();

    for (name, original) in [("x", secret.as_path()), ("d", outside_dir.as_path())] {
        let exported = target.join("artifacts").join(name);
        let meta = fs::symlink_metadata(&exported).unwrap();
        assert!(meta.file_type().is_symlink(), "{name} must stay a symlink");
        assert_eq!(fs::read_link(&exported).unwrap(), original);
        assert!(
            !meta.file_type().is_file(),
            "{name} must not be a regular file"
        );
    }
    let mut regular = Vec::new();
    for entry in fs::read_dir(target.join("artifacts")).unwrap() {
        let entry = entry.unwrap();
        if fs::symlink_metadata(entry.path())
            .unwrap()
            .file_type()
            .is_file()
        {
            regular.push(entry.path());
        }
    }
    assert!(
        regular.is_empty(),
        "secret bytes were written as files: {regular:?}"
    );
}

#[test]
fn the_top_directory_is_private_and_file_modes_are_kept() {
    let root = fakes::TempDir::new("log-export-modes");
    let dir = session(root.path(), "s_1", FIRST.as_bytes());
    let artifact = dir.join("artifacts/a_1.txt");
    fs::write(&artifact, b"kept").unwrap();
    fs::set_permissions(&artifact, fs::Permissions::from_mode(0o640)).unwrap();

    let target = root.path().join("out");
    export(&dir, &target).unwrap();

    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(target.join("artifacts/a_1.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
}
