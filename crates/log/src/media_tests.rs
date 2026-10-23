use std::fs;
use std::path::{Path, PathBuf};

use super::read_media as read;

/// `<root>/sessions/s_child`, the session under test, with `<root>/sessions`
/// holding its siblings.
struct Sessions {
    root: fakes::TempDir,
    dir: PathBuf,
}

impl Sessions {
    fn new() -> Self {
        let root = fakes::TempDir::new("log-media");
        let dir = root.path().join("sessions").join("s_child");
        fs::create_dir_all(&dir).unwrap();
        Self { root, dir }
    }

    /// Writes `bytes` to `rel` under the sessions directory and returns the
    /// file's absolute path.
    fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let file = self.root.path().join("sessions").join(rel);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, bytes).unwrap();
        file
    }
}

#[test]
fn a_relative_artifact_is_read_under_the_session_directory() {
    let sessions = Sessions::new();
    sessions.write("s_child/artifacts/i.png", b"abcd");

    assert_eq!(
        read(&sessions.dir, "artifacts/i.png").as_deref(),
        Some(&b"abcd"[..])
    );
}

#[test]
fn a_relative_path_that_leaves_the_session_directory_is_not_read() {
    let sessions = Sessions::new();
    sessions.write("s_child/artifacts/i.png", b"abcd");
    sessions.write("x", b"outside");

    for path in [
        "../x",
        "artifacts/../../x",
        "./artifacts/i.png",
        "artifacts/../artifacts/i.png",
    ] {
        assert_eq!(read(&sessions.dir, path), None, "{path}");
    }
}

#[test]
fn an_interior_dot_or_doubled_slash_names_the_same_file() {
    // `Path::components` drops both, so the rule accepts them as
    // `artifacts/i.png`; only a leading `.` or a `..` is refused.
    let sessions = Sessions::new();
    sessions.write("s_child/artifacts/i.png", b"abcd");

    for path in ["artifacts/./i.png", "artifacts//i.png"] {
        assert_eq!(
            read(&sessions.dir, path).as_deref(),
            Some(&b"abcd"[..]),
            "{path}"
        );
    }
}

#[test]
fn a_relative_path_is_not_limited_to_artifacts() {
    // The rule never limited a relative path to `artifacts/`.
    let sessions = Sessions::new();
    sessions.write("s_child/events.jsonl", b"{}\n");

    assert_eq!(
        read(&sessions.dir, "events.jsonl").as_deref(),
        Some(&b"{}\n"[..])
    );
}

#[test]
fn an_absolute_path_outside_the_sessions_directory_is_not_read() {
    let sessions = Sessions::new();

    assert!(Path::new("/etc/passwd").is_file());
    assert_eq!(read(&sessions.dir, "/etc/passwd"), None);

    let elsewhere = sessions.root.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("i.png"), b"abcd").unwrap();
    assert_eq!(
        read(&sessions.dir, elsewhere.join("i.png").to_str().unwrap()),
        None
    );
}

#[test]
fn a_sibling_sessions_artifact_is_read_by_its_absolute_path() {
    let sessions = Sessions::new();
    let parent_file = sessions.write("s_parent/artifacts/i.png", b"abcd");

    assert_eq!(
        read(&sessions.dir, parent_file.to_str().unwrap()).as_deref(),
        Some(&b"abcd"[..])
    );
}

#[test]
fn an_absolute_path_in_a_sibling_session_is_read_only_under_artifacts() {
    let sessions = Sessions::new();
    let events = sessions.write("s_parent/events.jsonl", b"{}\n");
    sessions.write("s_parent/artifacts/i.png", b"abcd");
    let artifacts = sessions.root.path().join("sessions/s_parent/artifacts");

    assert_eq!(read(&sessions.dir, events.to_str().unwrap()), None);
    // `artifacts` with no file under it names the directory, not a file.
    assert_eq!(read(&sessions.dir, artifacts.to_str().unwrap()), None);
    // `..` in an absolute path is refused even when it lands in `artifacts/`.
    let dotdot = format!("{}/../artifacts/i.png", artifacts.display());
    assert_eq!(read(&sessions.dir, &dotdot), None);
}

#[test]
fn a_missing_file_is_not_read() {
    let sessions = Sessions::new();
    fs::create_dir_all(sessions.dir.join("artifacts")).unwrap();
    let sibling = sessions
        .root
        .path()
        .join("sessions/s_parent/artifacts/gone.png");

    assert_eq!(read(&sessions.dir, "artifacts/gone.png"), None);
    assert_eq!(read(&sessions.dir, sibling.to_str().unwrap()), None);
}
