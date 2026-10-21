use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::Deadline;
use fakes::TempDir;
use serde_json::json;

use super::atomic::{locked, waiting};
use super::lines::{append_line, remove_line};
use super::*;
use super::{insert_entry, remove_entry};
use crate::home::ProjectKey;

/// How long the test waits for a thread before failing.
const DEADLINE: Duration = Duration::from_secs(10);

#[track_caller]
fn wait_until(what: &str, pred: impl Fn() -> bool + Send + 'static) {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        while !pred() {
            thread::yield_now();
        }
        done.send(()).unwrap();
    });
    assert!(
        Deadline::after(DEADLINE).recv(&finished).is_ok(),
        "waited {DEADLINE:?} for {what}"
    );
}

#[test]
fn a_second_update_waits_for_the_files_lock_then_keeps_both_writes() {
    let dir = TempDir::new("fiber-write-lock");
    let file = dir.path().join("fiber-acme.json");
    let setup = file.clone();
    fakes::within(
        "update",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || update(&setup, &["a".to_owned()], Value::from(1), false),
    )
    .unwrap();
    let setup = file.clone();
    let held = fakes::within(
        "locked",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || locked(&setup),
    )
    .unwrap();
    let (started, started_rx) = mpsc::channel();
    let (done, done_rx) = mpsc::channel();
    let worker_file = file.clone();
    let worker = thread::spawn(move || {
        started.send(()).unwrap();
        update(&worker_file, &["b".to_owned()], Value::from(2), false).unwrap();
        done.send(()).unwrap();
    });
    assert!(
        Deadline::after(DEADLINE).recv(&started_rx).is_ok(),
        "waited {DEADLINE:?} for the second update to reach the file's lock"
    );
    wait_until("the second update to be waiting on the file's lock", || {
        waiting() == 1
    });
    assert!(
        done_rx.try_recv().is_err(),
        "the second update finished while the file was locked"
    );
    // Another session's write lands while the second update waits:
    // the lock serializes whole-file writes, it does not hide them.
    fs::write(&file, "{\"a\": 1, \"c\": 3}\n").unwrap();
    drop(held);
    assert!(
        Deadline::after(DEADLINE).recv(&done_rx).is_ok(),
        "waited {DEADLINE:?} for the second update to finish after the release"
    );
    worker.join().unwrap();
    let written: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(written, json!({"a": 1, "b": 2, "c": 3}));
}

#[test]
fn a_remove_waits_for_the_files_lock_then_keeps_the_other_write() {
    let dir = TempDir::new("fiber-write-lock");
    let file = dir.path().join("rules");
    fs::write(&file, "{\"gone\": 1}\n").unwrap();
    let setup = file.clone();
    let held = fakes::within(
        "locked",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || locked(&setup),
    )
    .unwrap();
    let (started, started_rx) = mpsc::channel();
    let (done, done_rx) = mpsc::channel();
    let worker_file = file.clone();
    let worker = thread::spawn(move || {
        started.send(()).unwrap();
        let removed = remove_line(&worker_file, 1, "{\"gone\": 1}").unwrap();
        done.send(removed).unwrap();
    });
    assert!(
        Deadline::after(DEADLINE).recv(&started_rx).is_ok(),
        "waited {DEADLINE:?} for the remove to reach the file's lock"
    );
    wait_until("the remove to be waiting on the file's lock", || {
        waiting() == 1
    });
    assert!(
        done_rx.try_recv().is_err(),
        "the remove finished while the file was locked"
    );
    // Another session's line lands while the remove waits: the lock
    // serializes whole-file writes, it does not hide them.
    fs::write(&file, "{\"gone\": 1}\n{\"late\": 3}\n").unwrap();
    drop(held);
    let removed = match Deadline::after(DEADLINE).recv(&done_rx) {
        Ok(removed) => removed,
        Err(_) => panic!("waited {DEADLINE:?} for the remove to finish after the release"),
    };
    worker.join().unwrap();
    assert!(removed, "the held line is still there");
    assert_eq!(fs::read(&file).unwrap(), "{\"late\": 3}\n".as_bytes());
}

#[cfg(unix)]
#[test]
fn append_line_on_an_unreadable_file_fails_and_keeps_the_file() {
    use std::os::unix::fs::symlink;
    let dir = TempDir::new("fiber-append-line");
    let file = dir.path().join("rules");
    // A link to itself: reading it fails with a loop error, which is not
    // NotFound, so the file counts as present. Renaming a new file over
    // it would succeed, so a wrong guard would report Ok.
    symlink(&file, &file).unwrap();
    let result = append_line(&file, "{\"a\": 1}");
    assert!(
        matches!(result, Err(ConfigError::Io { .. })),
        "expected an I/O error, got {result:?}"
    );
    assert!(
        fs::symlink_metadata(&file)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the unreadable file was replaced"
    );
}

/// A home, a workspace and a project key for the entry tests: the home
/// and the workspace are different directories, so each layer's file is
/// told apart.
struct Layers {
    tmp: TempDir,
    project: ProjectKey,
}

impl Layers {
    fn new() -> Self {
        Self {
            tmp: TempDir::new("fiber-write-entries"),
            project: ProjectKey::new("test-proj").unwrap(),
        }
    }

    fn home(&self) -> std::path::PathBuf {
        self.tmp.path().join("home")
    }

    fn workspace(&self) -> std::path::PathBuf {
        self.tmp.path().join("ws")
    }

    fn file(&self, layer: super::Layer) -> std::path::PathBuf {
        match layer {
            super::Layer::Global => self.home().join("config.json"),
            super::Layer::Project => self
                .home()
                .join("projects")
                .join("test-proj")
                .join("config.json"),
            super::Layer::Repository => self.workspace().join(".fiber/config.json"),
        }
    }
}

fn remote(url: &str) -> serde_json::Value {
    json!({"url": url})
}

#[test]
fn insert_into_an_absent_file_creates_the_file_and_the_servers_objects() {
    for layer in [
        super::Layer::Global,
        super::Layer::Project,
        super::Layer::Repository,
    ] {
        let dirs = Layers::new();
        let wrote = insert_entry(
            &dirs.home(),
            &dirs.workspace(),
            &dirs.project,
            layer,
            "mcp.servers",
            "docs",
            remote("https://example.test/mcp"),
        )
        .unwrap();
        assert!(wrote);
        let text = fs::read_to_string(dirs.file(layer)).unwrap();
        let root: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            root,
            json!({"mcp": {"servers": {"docs": {"url": "https://example.test/mcp"}}}})
        );
    }
}

#[test]
fn insert_of_a_present_name_returns_false_and_leaves_the_bytes() {
    let dirs = Layers::new();
    insert_entry(
        &dirs.home(),
        &dirs.workspace(),
        &dirs.project,
        super::Layer::Global,
        "mcp.servers",
        "docs",
        remote("https://example.test/mcp"),
    )
    .unwrap();
    let before = fs::read(dirs.file(super::Layer::Global)).unwrap();
    let wrote = insert_entry(
        &dirs.home(),
        &dirs.workspace(),
        &dirs.project,
        super::Layer::Global,
        "mcp.servers",
        "docs",
        remote("https://other.test/mcp"),
    )
    .unwrap();
    assert!(!wrote);
    assert_eq!(fs::read(dirs.file(super::Layer::Global)).unwrap(), before);
}

#[test]
fn insert_type_checks_the_value_and_writes_nothing_on_a_mismatch() {
    let dirs = Layers::new();
    let e = insert_entry(
        &dirs.home(),
        &dirs.workspace(),
        &dirs.project,
        super::Layer::Global,
        "mcp.servers",
        "docs",
        json!({"url": 1}),
    )
    .unwrap_err();
    assert!(
        matches!(e, crate::ConfigError::WrongType { .. }),
        "expected a wrong-type error, got {e:?}"
    );
    assert!(!dirs.file(super::Layer::Global).exists());
}

#[test]
fn remove_of_a_present_name_returns_true_and_keeps_the_rest() {
    let dirs = Layers::new();
    for name in ["a", "b"] {
        insert_entry(
            &dirs.home(),
            &dirs.workspace(),
            &dirs.project,
            super::Layer::Global,
            "mcp.servers",
            name,
            remote("https://example.test/mcp"),
        )
        .unwrap();
    }
    // Another key beside `mcp` stays where it is.
    super::set(
        &dirs.home(),
        &dirs.workspace(),
        &dirs.project,
        super::Layer::Global,
        "model",
        serde_json::Value::from("acme/m"),
    )
    .unwrap();
    assert!(
        remove_entry(
            &dirs.home(),
            &dirs.workspace(),
            &dirs.project,
            super::Layer::Global,
            "mcp.servers",
            "a",
        )
        .unwrap()
    );
    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dirs.file(super::Layer::Global)).unwrap())
            .unwrap();
    assert_eq!(
        root,
        json!({"mcp": {"servers": {"b": {"url": "https://example.test/mcp"}}}, "model": "acme/m"})
    );
}

#[test]
fn remove_of_an_absent_name_or_file_returns_false_and_creates_no_file() {
    let dirs = Layers::new();
    assert!(
        !remove_entry(
            &dirs.home(),
            &dirs.workspace(),
            &dirs.project,
            super::Layer::Global,
            "mcp.servers",
            "missing",
        )
        .unwrap()
    );
    assert!(!dirs.file(super::Layer::Global).exists());
    insert_entry(
        &dirs.home(),
        &dirs.workspace(),
        &dirs.project,
        super::Layer::Global,
        "mcp.servers",
        "a",
        remote("https://example.test/mcp"),
    )
    .unwrap();
    assert!(
        !remove_entry(
            &dirs.home(),
            &dirs.workspace(),
            &dirs.project,
            super::Layer::Global,
            "mcp.servers",
            "missing",
        )
        .unwrap()
    );
}

#[test]
fn removing_the_last_entry_removes_the_emptied_objects() {
    let dirs = Layers::new();
    insert_entry(
        &dirs.home(),
        &dirs.workspace(),
        &dirs.project,
        super::Layer::Global,
        "mcp.servers",
        "only",
        remote("https://example.test/mcp"),
    )
    .unwrap();
    assert!(
        remove_entry(
            &dirs.home(),
            &dirs.workspace(),
            &dirs.project,
            super::Layer::Global,
            "mcp.servers",
            "only",
        )
        .unwrap()
    );
    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dirs.file(super::Layer::Global)).unwrap())
            .unwrap();
    assert_eq!(root, json!({}));
}

#[test]
fn a_repository_file_that_is_a_link_is_refused() {
    let dirs = Layers::new();
    fs::create_dir_all(dirs.workspace().join(".fiber")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        dirs.home().join("elsewhere.json"),
        dirs.workspace().join(".fiber/config.json"),
    )
    .unwrap();
    let e = insert_entry(
        &dirs.home(),
        &dirs.workspace(),
        &dirs.project,
        super::Layer::Repository,
        "mcp.servers",
        "docs",
        remote("https://example.test/mcp"),
    )
    .unwrap_err();
    assert!(
        matches!(e, crate::ConfigError::NotPlain { .. }),
        "expected a not-plain error, got {e:?}"
    );
    let e = remove_entry(
        &dirs.home(),
        &dirs.workspace(),
        &dirs.project,
        super::Layer::Repository,
        "mcp.servers",
        "docs",
    )
    .unwrap_err();
    assert!(
        matches!(e, crate::ConfigError::NotPlain { .. }),
        "expected a not-plain error, got {e:?}"
    );
}

#[test]
fn a_name_holding_a_dot_is_stored_under_that_exact_key() {
    let dirs = Layers::new();
    assert!(
        insert_entry(
            &dirs.home(),
            &dirs.workspace(),
            &dirs.project,
            super::Layer::Global,
            "mcp.servers",
            "a.b",
            remote("https://example.test/mcp"),
        )
        .unwrap()
    );
    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dirs.file(super::Layer::Global)).unwrap())
            .unwrap();
    assert_eq!(
        root,
        json!({"mcp": {"servers": {"a.b": {"url": "https://example.test/mcp"}}}})
    );
    assert!(
        remove_entry(
            &dirs.home(),
            &dirs.workspace(),
            &dirs.project,
            super::Layer::Global,
            "mcp.servers",
            "a.b",
        )
        .unwrap()
    );
}

#[test]
fn two_threads_inserting_different_names_both_land() {
    let dirs = Layers::new();
    let (done, finished) = mpsc::channel();
    for name in ["a", "b"] {
        let (home, workspace, project) = (dirs.home(), dirs.workspace(), dirs.project.clone());
        let done = done.clone();
        thread::spawn(move || {
            let wrote = insert_entry(
                &home,
                &workspace,
                &project,
                super::Layer::Global,
                "mcp.servers",
                name,
                remote("https://example.test/mcp"),
            );
            done.send((name, wrote)).unwrap();
        });
    }
    drop(done);
    for _ in ["a", "b"] {
        let (name, wrote) = match Deadline::after(DEADLINE).recv(&finished) {
            Ok(done) => done,
            Err(_) => panic!("waited {DEADLINE:?} for the insert to finish"),
        };
        assert!(wrote.unwrap(), "the insert of {name} did not write");
    }
    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dirs.file(super::Layer::Global)).unwrap())
            .unwrap();
    assert_eq!(
        root,
        json!({"mcp": {"servers": {
            "a": {"url": "https://example.test/mcp"},
            "b": {"url": "https://example.test/mcp"},
        }}})
    );
}

#[test]
fn replace_global_rejects_a_non_object_document_and_keeps_its_bytes() {
    for existing in ["[]", "5", "null"] {
        for remove in [false, true] {
            let dir = TempDir::new("fiber-write-guard");
            let home = dir.path().to_path_buf();
            fs::write(home.join("config.json"), existing).unwrap();
            let before = fs::read(home.join("config.json")).unwrap();
            let result = if remove {
                replace_global(&home, "providers.acme.reviewer_model", None)
            } else {
                replace_global(
                    &home,
                    "providers.acme.reviewer_model",
                    Some(Value::from("m")),
                )
            };
            let e = result.unwrap_err();
            assert_eq!(e.code(), contract::ErrorCode::ConfigInvalid, "{existing}");
            assert!(
                e.to_string().contains("(the whole file)"),
                "{existing}: {e}"
            );
            assert_eq!(
                fs::read(home.join("config.json")).unwrap(),
                before,
                "{existing}: the rejected write changed the file"
            );
        }
    }
}

#[test]
fn replace_global_rejects_a_mistyped_known_subtree_and_keeps_its_bytes() {
    for existing in [
        r#"{"providers": {"acme": {"reviewer_model": 5}}}"#,
        r#"{"providers": {"acme": 5}}"#,
        r#"{"hub": {"port": "fast"}}"#,
    ] {
        for remove in [false, true] {
            let dir = TempDir::new("fiber-write-guard");
            let home = dir.path().to_path_buf();
            fs::write(home.join("config.json"), existing).unwrap();
            let before = fs::read(home.join("config.json")).unwrap();
            let result = if remove {
                replace_global(&home, "providers.acme.reviewer_model", None)
            } else {
                replace_global(
                    &home,
                    "providers.acme.reviewer_model",
                    Some(Value::from("m")),
                )
            };
            let e = result.unwrap_err();
            assert_eq!(e.code(), contract::ErrorCode::ConfigInvalid, "{existing}");
            assert_eq!(
                fs::read(home.join("config.json")).unwrap(),
                before,
                "{existing}: the rejected write changed the file"
            );
        }
    }
}

#[test]
fn replace_global_waits_for_the_lock_then_rejects_a_concurrent_non_object() {
    let dir = TempDir::new("fiber-write-guard");
    let home = dir.path().to_path_buf();
    let file = home.join("config.json");
    fs::write(&file, "{\"reviewer\": {\"model\": \"all/a\"}}\n").unwrap();
    let setup = file.clone();
    let held = fakes::within(
        "locked",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || locked(&setup),
    )
    .unwrap();
    let (started, started_rx) = mpsc::channel();
    let (done, done_rx) = mpsc::channel();
    let worker_home = home.clone();
    let worker = thread::spawn(move || {
        started.send(()).unwrap();
        let result = replace_global(
            &worker_home,
            "providers.acme.reviewer_model",
            Some(Value::from("m")),
        );
        done.send(result).unwrap();
    });
    assert!(
        Deadline::after(DEADLINE).recv(&started_rx).is_ok(),
        "waited {DEADLINE:?} for the replace to reach the file's lock"
    );
    wait_until("the replace to be waiting on the file's lock", || {
        waiting() == 1
    });
    assert!(
        done_rx.try_recv().is_err(),
        "the replace finished while the file was locked"
    );
    // Another session replaces the whole document with `[]` while the
    // replace waits: the locked read must fail instead of setting a key
    // on the array and silently destroying it.
    fs::write(&file, "[]").unwrap();
    drop(held);
    let result = match Deadline::after(DEADLINE).recv(&done_rx) {
        Ok(result) => result,
        Err(_) => panic!("waited {DEADLINE:?} for the replace to finish after the release"),
    };
    worker.join().unwrap();
    let e = result.unwrap_err();
    assert_eq!(e.code(), contract::ErrorCode::ConfigInvalid);
    assert!(e.to_string().contains("(the whole file)"), "{e}");
    assert_eq!(fs::read(&file).unwrap(), b"[]");
}

#[test]
fn replace_global_on_a_valid_document_keeps_unknown_and_sibling_keys() {
    let dir = TempDir::new("fiber-write-guard");
    let home = dir.path().to_path_buf();
    fs::write(
        home.join("config.json"),
        r#"{"providers": {"acme": {"credential": "default"}}, "tomorrow": {"known": false}}"#,
    )
    .unwrap();
    replace_global(
        &home,
        "providers.acme.reviewer_model",
        Some(Value::from("m")),
    )
    .unwrap();
    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(home.join("config.json")).unwrap()).unwrap();
    assert_eq!(
        root,
        json!({"providers": {"acme": {"credential": "default", "reviewer_model": "m"}}, "tomorrow": {"known": false}}),
        "unknown and sibling keys survive the guarded write"
    );
    // Removing an absent reviewer key writes nothing.
    let removed = replace_global(&home, "providers.other.reviewer_model", None).unwrap();
    assert_eq!(removed, None);
}
