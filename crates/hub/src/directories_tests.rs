//! Tests for `directories`: the sorted one-level listing, the files left
//! out and the links followed, the empty answers, the `~/` expansion and
//! every `invalid_arguments` rejection.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::{Path, PathBuf};

use contract::ErrorCode;
use serde_json::{Value, json};

use super::*;
use crate::testkit::args;

struct Temp {
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
    dir: PathBuf,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("hd");
        let dir = held.path().to_path_buf();
        Self { held, dir }
    }

    fn home(&self) -> &Path {
        &self.dir
    }

    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.dir.join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file(&self, name: &str) {
        fs::write(self.dir.join(name), b"x").unwrap();
    }

    fn text(&self) -> String {
        self.dir.to_str().unwrap().to_owned()
    }
}

fn listed(home: Option<&Path>, value: Value) -> Value {
    directories(home, &args(value)).unwrap().unwrap()
}

fn refused(home: Option<&Path>, value: Value) -> Refusal {
    directories(home, &args(value)).unwrap_err()
}

#[test]
fn subdirectories_answer_sorted_with_path_unchanged() {
    let temp = Temp::new();
    for name in ["b", "a", "C"] {
        temp.dir(name);
    }
    let arg = temp.text();
    assert_eq!(
        listed(None, json!({"path": arg})),
        json!({"directories": ["C", "a", "b"], "path": arg})
    );
}

#[test]
fn files_are_left_out() {
    let temp = Temp::new();
    temp.dir("a");
    temp.file("f.txt");
    let arg = temp.text();
    assert_eq!(
        listed(None, json!({"path": arg})),
        json!({"directories": ["a"], "path": arg})
    );
}

#[test]
fn nested_directories_are_not_listed() {
    let temp = Temp::new();
    let inner = temp.dir("a").join("inner");
    fs::create_dir_all(&inner).unwrap();
    let arg = temp.text();
    assert_eq!(
        listed(None, json!({"path": arg})),
        json!({"directories": ["a"], "path": arg})
    );
}

#[test]
fn hidden_directories_are_listed() {
    let temp = Temp::new();
    temp.dir(".git");
    temp.dir("a");
    let arg = temp.text();
    assert_eq!(
        listed(None, json!({"path": arg})),
        json!({"directories": [".git", "a"], "path": arg})
    );
}

#[test]
fn links_to_directories_are_listed_but_dangling_and_file_links_are_not() {
    let temp = Temp::new();
    temp.dir("target");
    temp.file("f.txt");
    std::os::unix::fs::symlink(temp.dir.join("target"), temp.dir.join("dirlink")).unwrap();
    std::os::unix::fs::symlink(temp.dir.join("f.txt"), temp.dir.join("filelink")).unwrap();
    std::os::unix::fs::symlink(temp.dir.join("missing"), temp.dir.join("dangling")).unwrap();
    let arg = temp.text();
    assert_eq!(
        listed(None, json!({"path": arg})),
        json!({"directories": ["dirlink", "target"], "path": arg})
    );
}

#[test]
fn a_missing_path_answers_empty_with_path() {
    let temp = Temp::new();
    let arg = format!("{}/missing", temp.text());
    assert_eq!(
        listed(None, json!({"path": arg})),
        json!({"directories": [], "path": arg})
    );
}

#[test]
fn a_file_path_answers_empty() {
    let temp = Temp::new();
    temp.file("f.txt");
    let arg = format!("{}/f.txt", temp.text());
    assert_eq!(
        listed(None, json!({"path": arg})),
        json!({"directories": [], "path": arg})
    );
}

#[test]
fn an_unreadable_directory_answers_empty() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = Temp::new();
    let denied = temp.dir("sub");
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&denied).is_ok() {
        panic!("running as root: root bypasses file modes, so the mode row cannot fail here");
    }
    let arg = denied.to_str().unwrap().to_owned();
    assert_eq!(
        listed(None, json!({"path": arg})),
        json!({"directories": [], "path": arg})
    );
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn a_non_utf8_name_is_skipped() {
    use std::os::unix::ffi::OsStrExt as _;

    let temp = Temp::new();
    temp.dir("a");
    fs::create_dir_all(
        temp.dir
            .join(std::ffi::OsStr::from_bytes(b"bad\xffname")),
    )
    .unwrap();
    let arg = temp.text();
    assert_eq!(
        listed(None, json!({"path": arg})),
        json!({"directories": ["a"], "path": arg})
    );
}

#[test]
fn tilde_slash_expands_home() {
    let temp = Temp::new();
    let sub = temp.dir("sub");
    fs::create_dir_all(sub.join("n")).unwrap();
    fs::create_dir_all(sub.join("m")).unwrap();
    assert_eq!(
        listed(Some(temp.home()), json!({"path": "~/sub/"})),
        json!({"directories": ["m", "n"], "path": format!("{}/sub/", temp.text())})
    );
}

#[test]
fn tilde_slash_alone_answers_home() {
    let temp = Temp::new();
    temp.dir("a");
    assert_eq!(
        listed(Some(temp.home()), json!({"path": "~/"})),
        json!({"directories": ["a"], "path": format!("{}/", temp.text())})
    );
}

#[test]
fn a_trailing_slash_home_does_not_double() {
    let temp = Temp::new();
    temp.dir("sub");
    let home = PathBuf::from(format!("{}/", temp.text()));
    assert_eq!(
        listed(Some(&home), json!({"path": "~/sub/"})),
        json!({"directories": [], "path": format!("{}/sub/", temp.text())})
    );
}

#[test]
fn home_slash_answers_slash() {
    let got = listed(Some(Path::new("/")), json!({"path": "~/"}));
    assert_eq!(got["path"], "/");
    assert!(got["directories"].is_array(), "{got}");
}

#[test]
fn tilde_without_a_usable_home_is_invalid() {
    use std::os::unix::ffi::OsStrExt as _;

    let non_utf8 = Path::new(std::ffi::OsStr::from_bytes(b"/\xff"));
    for home in [
        None,
        Some(Path::new("rel")),
        Some(non_utf8),
    ] {
        let (code, _) = refused(home, json!({"path": "~/sub"}));
        assert_eq!(code, ErrorCode::InvalidArguments);
    }
}

#[test]
fn relative_and_misshaped_paths_are_invalid() {
    let temp = Temp::new();
    for value in [
        json!({"path": "a/"}),
        json!({"path": "~"}),
        json!({"path": "~user/"}),
        json!({"path": ""}),
        json!({"path": 5}),
        json!({}),
        json!({"path": "~/", "extra": 1}),
    ] {
        let (code, _) = refused(Some(temp.home()), value);
        assert_eq!(code, ErrorCode::InvalidArguments);
    }
}

#[test]
fn an_absolute_path_comes_back_byte_for_byte() {
    let temp = Temp::new();
    temp.dir("sub");
    for rest in ["sub", "sub/"] {
        let arg = format!("{}/{rest}", temp.text());
        let got = listed(Some(temp.home()), json!({"path": arg}));
        assert_eq!(got["path"], arg);
    }
}
