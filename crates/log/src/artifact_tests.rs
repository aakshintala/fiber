//! Tests for the shared artifact path check: the answered file, the
//! lexical rule, links at every level, and the missing, irregular and
//! unreadable cases.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use super::*;
use fakes::Deadline;

/// One named deadline per wait: opening never blocks on a FIFO.
const DEADLINE: Duration = Duration::from_secs(10);

/// Every path the lexical rule refuses, as `read_file` pins them.
const LEXICAL: [&str; 11] = [
    "",
    "/etc/passwd",
    "a.png",
    "artifactsx/a.png",
    "artifacts",
    "artifacts/",
    "./artifacts/a.png",
    "artifacts/../events.jsonl",
    "artifacts/../missing",
    "artifacts/sub/../../events.jsonl",
    "artifacts/a\0.png",
];

struct Temp {
    dir: PathBuf,
    _held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("log-artifact");
        let dir = held.path().join("session");
        fs::create_dir_all(dir.join("artifacts")).unwrap();
        fs::write(dir.join("events.jsonl"), "{}\n").unwrap();
        Self { dir, _held: held }
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let file = self.dir.join(name);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&file, bytes).unwrap();
        file
    }
}

fn read(mut file: File) -> Vec<u8> {
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn an_artifact_and_a_dotdot_that_stays_inside_answer() {
    let temp = Temp::new();
    temp.write("artifacts/a.txt", b"hello");
    fs::create_dir_all(temp.dir.join("artifacts").join("sub")).unwrap();
    for path in ["artifacts/a.txt", "artifacts/sub/../a.txt"] {
        let got = open_artifact(&temp.dir, path).unwrap();
        assert_eq!(read(got), b"hello", "{path}");
    }
}

#[test]
fn paths_outside_the_lexical_rule_are_outside() {
    let temp = Temp::new();
    temp.write("artifacts/a.png", b"bytes");
    for bad in LEXICAL {
        assert!(
            matches!(open_artifact(&temp.dir, bad), Err(Refusal::Outside)),
            "{bad:?}"
        );
    }
}

#[test]
fn the_lexical_check_alone_refuses_without_touching_the_disk() {
    for bad in LEXICAL {
        assert!(
            matches!(check_artifact_path(bad), Err(Refusal::Outside)),
            "{bad:?}"
        );
    }
    // A missing directory is never consulted: the rule is lexical.
    assert!(matches!(
        check_artifact_path("artifacts/../events.jsonl"),
        Err(Refusal::Outside)
    ));
    assert!(check_artifact_path("artifacts/a.png").is_ok());
    assert!(check_artifact_path("artifacts/sub/../a.png").is_ok());
}

#[test]
fn a_link_out_of_artifacts_is_outside() {
    let temp = Temp::new();
    let artifacts = temp.dir.join("artifacts");
    temp.write("artifacts/a.png", b"bytes");
    let outside = temp._held.path().join("outside");
    fs::write(&outside, b"marker-via-link-should-never-leave").unwrap();
    std::os::unix::fs::symlink(temp.dir.join("events.jsonl"), artifacts.join("out.png")).unwrap();
    let mut rel = PathBuf::new();
    let mut at = artifacts.as_path();
    while at != temp._held.path() {
        rel.push("..");
        at = at.parent().unwrap();
    }
    rel.push("outside");
    std::os::unix::fs::symlink(&rel, artifacts.join("far.png")).unwrap();
    std::os::unix::fs::symlink(&outside, artifacts.join("abs.png")).unwrap();
    let outer = temp._held.path().join("outer");
    fs::create_dir_all(&outer).unwrap();
    fs::write(outer.join("x"), b"marker-via-link-should-never-leave").unwrap();
    std::os::unix::fs::symlink(&outer, artifacts.join("sub")).unwrap();
    for path in [
        "artifacts/out.png",
        "artifacts/far.png",
        "artifacts/abs.png",
        "artifacts/sub/x",
    ] {
        assert!(
            matches!(open_artifact(&temp.dir, path), Err(Refusal::Outside)),
            "{path}"
        );
    }
}

#[test]
fn a_link_inside_artifacts_answers_its_target() {
    let temp = Temp::new();
    let artifacts = temp.dir.join("artifacts");
    temp.write("artifacts/sub/a.txt", b"hello");
    temp.write("artifacts/a.png", b"bytes");
    std::os::unix::fs::symlink("sub/a.txt", artifacts.join("view.html")).unwrap();
    let got = open_artifact(&temp.dir, "artifacts/view.html").unwrap();
    assert_eq!(read(got), b"hello");
    std::os::unix::fs::symlink(artifacts.join("a.png"), artifacts.join("abs.png")).unwrap();
    let got = open_artifact(&temp.dir, "artifacts/abs.png").unwrap();
    assert_eq!(read(got), b"bytes");
}

#[test]
fn a_linked_artifacts_or_a_file_artifacts_is_outside() {
    let temp = Temp::new();
    temp.write("artifacts/a.png", b"bytes");
    let outer = temp._held.path().join("artifacts-out");
    fs::create_dir_all(&outer).unwrap();
    fs::write(outer.join("a.png"), b"bytes").unwrap();
    fs::remove_dir_all(temp.dir.join("artifacts")).unwrap();
    std::os::unix::fs::symlink(&outer, temp.dir.join("artifacts")).unwrap();
    assert!(matches!(
        open_artifact(&temp.dir, "artifacts/a.png"),
        Err(Refusal::Outside)
    ));
    fs::remove_file(temp.dir.join("artifacts")).unwrap();
    fs::write(temp.dir.join("artifacts"), b"not a directory").unwrap();
    assert!(matches!(
        open_artifact(&temp.dir, "artifacts/a.png"),
        Err(Refusal::Outside)
    ));
}

#[test]
fn a_self_looped_artifacts_is_root_unreadable() {
    let temp = Temp::new();
    temp.write("artifacts/a.png", b"bytes");
    fs::remove_dir_all(temp.dir.join("artifacts")).unwrap();
    std::os::unix::fs::symlink("artifacts", temp.dir.join("artifacts")).unwrap();
    assert!(matches!(
        open_artifact(&temp.dir, "artifacts/a.png"),
        Err(Refusal::RootUnreadable(_))
    ));
}

#[test]
fn a_dangling_artifacts_link_is_no_artifacts() {
    let temp = Temp::new();
    temp.write("artifacts/a.png", b"bytes");
    fs::remove_dir_all(temp.dir.join("artifacts")).unwrap();
    std::os::unix::fs::symlink("gone", temp.dir.join("artifacts")).unwrap();
    assert!(matches!(
        open_artifact(&temp.dir, "artifacts/a.png"),
        Err(Refusal::NoArtifacts)
    ));
}

#[test]
fn a_missing_artifacts_is_no_artifacts() {
    let held = fakes::TempDir::new("log-artifact");
    let dir = held.path().join("session");
    fs::create_dir_all(&dir).unwrap();
    assert!(matches!(
        open_artifact(&dir, "artifacts/a.png"),
        Err(Refusal::NoArtifacts)
    ));
}

#[test]
fn a_missing_file_is_missing() {
    let temp = Temp::new();
    temp.write("artifacts/a.png", b"bytes");
    std::os::unix::fs::symlink("gone", temp.dir.join("artifacts").join("dangling.png")).unwrap();
    for path in [
        "artifacts/none.png",
        "artifacts/a.png/x",
        "artifacts/a.png/",
        "artifacts/a.png/.",
        "artifacts/dangling.png",
    ] {
        assert!(
            matches!(open_artifact(&temp.dir, path), Err(Refusal::Missing)),
            "{path}"
        );
    }
}

#[test]
fn a_directory_a_link_to_one_a_dot_link_or_a_fifo_is_not_regular() {
    let temp = Temp::new();
    let artifacts = temp.dir.join("artifacts");
    fs::create_dir_all(artifacts.join("sub")).unwrap();
    std::os::unix::fs::symlink("sub", artifacts.join("dir.png")).unwrap();
    std::os::unix::fs::symlink(".", artifacts.join("up")).unwrap();
    for path in ["artifacts/sub", "artifacts/dir.png", "artifacts/up"] {
        assert!(
            matches!(open_artifact(&temp.dir, path), Err(Refusal::NotRegular)),
            "{path}"
        );
    }
    // The FIFO is never opened: the answer arrives without a writer.
    let fifo = artifacts.join("f.png");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    let dir = temp.dir.clone();
    let (done, answered) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        done.send(open_artifact(&dir, "artifacts/f.png")).unwrap();
    });
    match Deadline::after(DEADLINE).recv(&answered) {
        Ok(got) => {
            assert!(matches!(got, Err(Refusal::NotRegular)));
        }
        Err(_) => {
            panic!("waited {DEADLINE:?} for the FIFO refusal");
        }
    }
}

#[test]
fn an_unreadable_file_is_unreadable() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = Temp::new();
    let denied = temp.write("artifacts/a.png", b"bytes");
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&denied).is_ok() {
        panic!("running as root: root bypasses file modes, so the mode row cannot fail here");
    }
    assert!(matches!(
        open_artifact(&temp.dir, "artifacts/a.png"),
        Err(Refusal::Unreadable(_))
    ));
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn each_refusal_maps_to_its_stable_code() {
    use std::io::Error;
    let unreadable = || Refusal::Unreadable(Error::other("denied"));
    let root = || Refusal::RootUnreadable(Error::other("looped"));
    assert_eq!(Refusal::Outside.code(), ErrorCode::InvalidArguments);
    assert_eq!(Refusal::NotRegular.code(), ErrorCode::InvalidArguments);
    assert_eq!(Refusal::NoArtifacts.code(), ErrorCode::NotFound);
    assert_eq!(Refusal::Missing.code(), ErrorCode::NotFound);
    assert_eq!(root().code(), ErrorCode::IoFailed);
    assert_eq!(unreadable().code(), ErrorCode::IoFailed);
    // The messages carry no path the client did not send.
    for refusal in [
        Refusal::NoArtifacts,
        root(),
        Refusal::Outside,
        Refusal::NotRegular,
        Refusal::Missing,
        unreadable(),
    ] {
        let text = refusal.to_string();
        assert!(!text.contains('/'), "{text}");
    }
}
