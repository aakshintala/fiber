//! The `write_atomic` failure stages and the lock deciding
//! `AddIfListed` (`docs/configuration.md`, "When Fiber writes"): a failure
//! before the rename leaves the file unchanged, one after it leaves the new
//! content in place, and two racing edits lose no name.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::json;

use super::*;
use fakes::Deadline;

const DEADLINE: Duration = Duration::from_secs(10);
const STEP: Duration = Duration::from_millis(10);
const WAITS: usize = 1000;

fn project_key() -> ProjectKey {
    ProjectKey::new("test-project").unwrap()
}

fn tmp_files(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn a_failure_before_the_rename_leaves_the_file_unchanged() {
    let dir = fakes::TempDir::new("fiber-edit-list-before");
    let home = dir.path().to_path_buf();
    let workspace = home.join("workspace");
    let file = home.join("config.json");
    let text = "{\"skills\": {\"disabled\": [\"a\"]}}";
    std::fs::write(&file, text).unwrap();
    fail_at(Some(Stage::BeforeRename));
    let result = edit_list(
        &home,
        &workspace,
        &project_key(),
        Layer::Global,
        &[ListEdit {
            key: "skills.disabled",
            name: "b",
            change: ListChange::Add,
            inherited: None,
        }],
    );
    fail_at(None);
    assert!(matches!(result, Err(ConfigError::Io { .. })), "{result:?}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    assert_eq!(tmp_files(&home), ["config.json", "config.json.lock"]);
}

#[test]
fn a_failure_after_the_rename_returns_err_with_the_new_content_in_place() {
    let dir = fakes::TempDir::new("fiber-edit-list-after");
    let home = dir.path().to_path_buf();
    let workspace = home.join("workspace");
    let file = home.join("config.json");
    std::fs::write(&file, "{\"skills\": {\"disabled\": [\"a\"]}}").unwrap();
    fail_at(Some(Stage::AfterRename));
    let result = edit_list(
        &home,
        &workspace,
        &project_key(),
        Layer::Global,
        &[ListEdit {
            key: "skills.disabled",
            name: "b",
            change: ListChange::Add,
            inherited: None,
        }],
    );
    fail_at(None);
    assert!(matches!(result, Err(ConfigError::Io { .. })), "{result:?}");
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(written, json!({"skills": {"disabled": ["a", "b"]}}));
    assert_eq!(tmp_files(&home), ["config.json", "config.json.lock"]);
}

#[test]
fn add_if_listed_decides_under_the_lock() {
    let dir = fakes::TempDir::new("fiber-edit-list-race");
    let home = dir.path().to_path_buf();
    let workspace = home.join("workspace");
    let file = home.join("config.json");
    std::fs::write(&file, "{\"skills\": {\"disabled\": [\"t\", \"x\"]}}").unwrap();
    let key = project_key();
    let (held, held_rx) = mpsc::channel();
    let (go, go_rx) = mpsc::channel();
    let (done_b, done_b_rx) = mpsc::channel();
    let remover = {
        let home = home.clone();
        let workspace = workspace.clone();
        let key = project_key();
        thread::spawn(move || {
            // Holds the writer between its temporary file and its rename,
            // so the adder below blocks on the lock.
            before_rename(move || {
                held.send(()).unwrap();
                Deadline::start().recv_or_fail(&go_rx, "the test releases the remover");
            });
            let wrote = edit_list(
                &home,
                &workspace,
                &key,
                Layer::Global,
                &[ListEdit {
                    key: "skills.disabled",
                    name: "t",
                    change: ListChange::Remove,
                    inherited: None,
                }],
            )
            .unwrap();
            done_b.send(wrote).unwrap();
        })
    };
    assert!(
        Deadline::after(DEADLINE).recv(&held_rx).is_ok(),
        "waited {DEADLINE:?} for the remover to hold the write"
    );
    let (done_a, done_a_rx) = mpsc::channel();
    let adder = {
        let home = home.clone();
        let workspace = workspace.clone();
        thread::spawn(move || {
            // The adder cannot have seen the remover's result before the
            // lock: the remover's write is still a temporary file.
            let wrote = edit_list(
                &home,
                &workspace,
                &key,
                Layer::Global,
                &[ListEdit {
                    key: "skills.disabled",
                    name: "t",
                    change: ListChange::AddIfListed,
                    inherited: None,
                }],
            )
            .unwrap();
            done_a.send(wrote).unwrap();
        })
    };
    // A bounded wait, never a sleep: a channel that is never sent on.
    let (_never, never) = mpsc::channel::<()>();
    for _ in 0..WAITS {
        if waiting() == 1 {
            break;
        }
        assert!(Deadline::after(STEP).recv(&never).is_err());
    }
    assert_eq!(waiting(), 1, "the adder waits on the file's lock");
    go.send(()).unwrap();
    assert!(
        Deadline::after(DEADLINE).recv(&done_b_rx).is_ok(),
        "waited {DEADLINE:?} for the remover to finish"
    );
    assert!(
        Deadline::after(DEADLINE).recv(&done_a_rx).is_ok(),
        "waited {DEADLINE:?} for the adder to finish"
    );
    remover.join().unwrap();
    adder.join().unwrap();
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(written, json!({"skills": {"disabled": ["x", "t"]}}));
}
