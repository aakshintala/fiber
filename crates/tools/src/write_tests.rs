use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::ErrorCode;
use contract::events::FileChange;
use contract::shapes::{ContentPart, Effect};
use contract::tool::Tool;
use fakes::Deadline;
use fakes::{CancelToken, Recorder, TempDir};
use serde_json::{Map, Value, json};

use super::{existing, line_changes, line_count, read_present, reversible};
use crate::Files;
use crate::files::path_text;

const DEADLINE: Duration = Duration::from_secs(10);

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            panic!("arguments are an object")
        }
    }
}

fn text(output: &contract::tool::Output) -> String {
    match output.content.first() {
        Some(ContentPart::Text { text }) => text.clone(),
        Some(ContentPart::Image { .. } | ContentPart::Pdf(_) | ContentPart::Unknown) | None => {
            String::new()
        }
    }
}

fn code(output: &contract::tool::Output) -> Option<ErrorCode> {
    output.error.as_ref().map(|error| error.code.clone())
}

fn canonical(path: &Path) -> std::path::PathBuf {
    fs::canonicalize(path).unwrap()
}

fn resolved(dir: &Path, name: &str) -> std::path::PathBuf {
    canonical(dir).join(name)
}

fn set_mode(path: &Path, mode: u32) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(mode);
    fs::set_permissions(path, perms).unwrap();
}

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
fn creating_a_file_reports_its_size_and_adds_every_line() {
    let dir = TempDir::new("fiber-write-create");
    let files = Files::new(dir.path().to_path_buf());
    let output = files.write().run(
        &args(json!({"path": "sub/a.txt", "content": "x\ny\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    let path = resolved(dir.path(), "sub").join("a.txt");
    assert_eq!(fs::read(&path).unwrap(), b"x\ny\n");
    assert_eq!(
        text(&output),
        format!("Created {}: 4 bytes, 2 lines.", path_text(&path))
    );
    assert_eq!(
        output.changes,
        Some(vec![FileChange {
            path: path_text(&path),
            added: 2,
            removed: 0,
        }])
    );
}

#[test]
fn a_new_file_is_stored_as_given() {
    let dir = TempDir::new("fiber-write-as-given");
    let files = Files::new(dir.path().to_path_buf());
    let output = files.write().run(
        &args(json!({"path": "a.txt", "content": "a\r\nb"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"a\r\nb");
    assert!(
        text(&output).contains("4 bytes, 2 lines."),
        "{}",
        text(&output)
    );
}

#[test]
fn replacing_after_a_read_keeps_crlf_and_the_byte_order_mark() {
    let dir = TempDir::new("fiber-write-crlf");
    let path = dir.path().join("a.txt");
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(b"a\r\nb\r\n");
    fs::write(&path, &bytes).unwrap();
    let files = Files::new(dir.path().to_path_buf());
    files.read().run(
        &args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    let output = files.write().run(
        &args(json!({"path": "a.txt", "content": "a\nc\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    assert_eq!(fs::read(&path).unwrap(), b"\xEF\xBB\xBFa\r\nc\r\n");
    assert!(text(&output).starts_with("Replaced "), "{}", text(&output));
}

#[test]
fn replacing_an_unread_file_was_not_read() {
    let dir = TempDir::new("fiber-write-unread");
    fs::write(dir.path().join("a.txt"), "old\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let output = files.write().run(
        &args(json!({"path": "a.txt", "content": "new\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(code(&output), Some(ErrorCode::StaleFile));
    assert!(
        text(&output).contains("was not read in this context"),
        "{}",
        text(&output)
    );
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old\n");
}

#[test]
fn replacing_after_an_external_change_says_it_changed() {
    let dir = TempDir::new("fiber-write-changed");
    fs::write(dir.path().join("a.txt"), "old\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    files.read().run(
        &args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    fs::write(dir.path().join("a.txt"), "edited\n").unwrap();
    let output = files.write().run(
        &args(json!({"path": "a.txt", "content": "new\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(code(&output), Some(ErrorCode::StaleFile));
    assert!(
        text(&output).contains("changed since it was read"),
        "{}",
        text(&output)
    );
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"edited\n");
}

#[test]
fn replacing_after_the_sessions_own_write_needs_no_read() {
    let dir = TempDir::new("fiber-write-own");
    let files = Files::new(dir.path().to_path_buf());
    files.write().run(
        &args(json!({"path": "a.txt", "content": "a\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    let output = files.write().run(
        &args(json!({"path": "a.txt", "content": "b\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"b\n");
}

#[test]
fn a_reread_after_an_external_edit_lets_the_replace_land() {
    let dir = TempDir::new("fiber-write-reread");
    fs::write(dir.path().join("a.txt"), "old\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    files.read().run(
        &args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    fs::write(dir.path().join("a.txt"), "formatted\n").unwrap();
    files.read().run(
        &args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    let output = files.write().run(
        &args(json!({"path": "a.txt", "content": "done\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"done\n");
}

#[test]
fn two_replacing_writes_after_one_read_do_not_mix() {
    let dir = TempDir::new("fiber-write-race");
    fs::write(dir.path().join("a.txt"), "old\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    files.read().run(
        &args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    let path = resolved(dir.path(), "a.txt");
    let held_locks = files.locks();
    let hold = held_locks.lock(&path);
    let first = files.write();
    let second = files.write();
    let (left_tx, left_rx) = mpsc::channel();
    let (right_tx, right_rx) = mpsc::channel();
    thread::spawn(move || {
        left_tx
            .send(first.run(
                &args(json!({"path": "a.txt", "content": "AAA\n"})),
                &CancelToken::new(),
                &Recorder::default(),
            ))
            .unwrap();
    });
    thread::spawn(move || {
        right_tx
            .send(second.run(
                &args(json!({"path": "a.txt", "content": "BBB\n"})),
                &CancelToken::new(),
                &Recorder::default(),
            ))
            .unwrap();
    });
    let locks = files.locks();
    wait_until("both writes to be waiting on the path", move || {
        locks.waiting() == 2
    });
    drop(hold);
    let left = Deadline::after(DEADLINE)
        .recv(&left_rx)
        .expect("waited 10s for the first write");
    let right = Deadline::after(DEADLINE)
        .recv(&right_rx)
        .expect("waited 10s for the second write");
    let outputs = [left, right];
    assert_eq!(
        outputs
            .iter()
            .filter(|output| output.error.is_none())
            .count(),
        1
    );
    assert_eq!(
        outputs
            .iter()
            .filter(|output| code(output) == Some(ErrorCode::StaleFile))
            .count(),
        1
    );
    let body = fs::read(&path).unwrap();
    assert!(body == b"AAA\n" || body == b"BBB\n", "{body:?}");
}

#[test]
fn a_held_lock_blocks_write_until_it_is_released() {
    let dir = TempDir::new("fiber-write-lock");
    let files = Files::new(dir.path().to_path_buf());
    let path = resolved(dir.path(), "a.txt");
    let locks = files.locks();
    let hold = locks.lock(&path);
    let write = files.write();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        done_tx
            .send(write.run(
                &args(json!({"path": "a.txt", "content": "hi\n"})),
                &CancelToken::new(),
                &Recorder::default(),
            ))
            .unwrap();
    });
    let locks = files.locks();
    wait_until("write to be waiting on the path", move || {
        locks.waiting() == 1
    });
    assert!(done_rx.try_recv().is_err());
    drop(hold);
    let output = Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("waited 10s for write to finish");
    assert!(output.error.is_none(), "{}", text(&output));
    assert_eq!(fs::read(&path).unwrap(), b"hi\n");
}

#[test]
fn a_retargeted_symlink_writes_nothing() {
    let dir = TempDir::new("fiber-write-retarget");
    fs::write(dir.path().join("a.txt"), "A\n").unwrap();
    fs::write(dir.path().join("b.txt"), "B\n").unwrap();
    symlink("a.txt", dir.path().join("link")).unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let arguments = args(json!({"path": "link", "content": "new\n"}));
    files.write().effects(&arguments).unwrap();
    fs::remove_file(dir.path().join("link")).unwrap();
    symlink("b.txt", dir.path().join("link")).unwrap();
    let output = files
        .write()
        .run(&arguments, &CancelToken::new(), &Recorder::default());
    assert_eq!(code(&output), Some(ErrorCode::PathChanged));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"A\n");
    assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"B\n");
}

#[test]
fn a_symlink_retargeted_while_the_lock_is_held_writes_nothing() {
    let dir = TempDir::new("fiber-write-window");
    fs::write(dir.path().join("a.txt"), "A\n").unwrap();
    fs::write(dir.path().join("b.txt"), "B\n").unwrap();
    symlink("a.txt", dir.path().join("link")).unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let target = resolved(dir.path(), "a.txt");
    let locks = files.locks();
    let hold = locks.lock(&target);
    let write = files.write();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        done_tx
            .send(write.run(
                &args(json!({"path": "link", "content": "new\n"})),
                &CancelToken::new(),
                &Recorder::default(),
            ))
            .unwrap();
    });
    let locks = files.locks();
    wait_until("write to be waiting on the old target", move || {
        locks.waiting() == 1
    });
    fs::remove_file(dir.path().join("link")).unwrap();
    symlink("b.txt", dir.path().join("link")).unwrap();
    drop(hold);
    let output = Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("waited 10s for write to finish");
    assert_eq!(code(&output), Some(ErrorCode::PathChanged));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"A\n");
    assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"B\n");
}

#[test]
fn writing_a_directory_is_unsupported() {
    let dir = TempDir::new("fiber-write-dir");
    fs::create_dir(dir.path().join("sub")).unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let output = files.write().run(
        &args(json!({"path": "sub", "content": "x"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(code(&output), Some(ErrorCode::UnsupportedFile));
    assert!(text(&output).contains("directory"), "{}", text(&output));
}

#[test]
fn forget_makes_the_next_replace_stale() {
    let dir = TempDir::new("fiber-write-forget");
    fs::write(dir.path().join("a.txt"), "old\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    files.read().run(
        &args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    files.forget();
    let output = files.write().run(
        &args(json!({"path": "a.txt", "content": "new\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(code(&output), Some(ErrorCode::StaleFile));
    assert!(text(&output).contains("was not read"), "{}", text(&output));
}

#[test]
fn creating_is_reversible_and_replacing_is_not() {
    let dir = TempDir::new("fiber-write-effects");
    let files = Files::new(dir.path().to_path_buf());
    let create = files
        .write()
        .effects(&args(json!({"path": "a.txt", "content": "x\n"})))
        .unwrap();
    assert_eq!(create.declared.effects, vec![Effect::Writes]);
    assert!(create.declared.reversible);
    let path = resolved(dir.path(), "a.txt");
    assert_eq!(create.subject, Some(path_text(&path)));
    assert_eq!(
        create.prefix,
        Some(format!("{}/", path_text(&canonical(dir.path()))))
    );
    files.write().run(
        &args(json!({"path": "a.txt", "content": "x\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    let replace = files
        .write()
        .effects(&args(json!({"path": "a.txt", "content": "y\n"})))
        .unwrap();
    assert!(!replace.declared.reversible);
    assert_eq!(replace.declared.effects, vec![Effect::Writes]);
}

#[test]
fn a_replaced_files_changes_are_the_line_diff() {
    let dir = TempDir::new("fiber-write-diff");
    fs::write(dir.path().join("a.txt"), "a\nb\nc\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    files.read().run(
        &args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    let output = files.write().run(
        &args(json!({"path": "a.txt", "content": "a\nx\nc\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    let path = path_text(&resolved(dir.path(), "a.txt"));
    assert_eq!(
        output.changes,
        Some(vec![FileChange {
            path,
            added: 1,
            removed: 1,
        }])
    );
}

#[test]
fn a_non_utf8_file_counts_every_line_added_and_removed() {
    let old = b"\xff\n\xfe";
    let new = b"a\nb\n";
    assert_eq!(line_changes(old, new), (line_count(new), line_count(old)));
    assert_eq!(line_count(old), 2);
    assert_eq!(line_count(new), 2);
}

#[test]
fn a_symlink_to_a_missing_target_creates_the_target() {
    let dir = TempDir::new("fiber-write-symlink");
    symlink("missing.txt", dir.path().join("link")).unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let output = files.write().run(
        &args(json!({"path": "link", "content": "hi\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    assert_eq!(fs::read(dir.path().join("missing.txt")).unwrap(), b"hi\n");
}

#[test]
fn a_parent_that_is_a_file_writes_nothing() {
    let dir = TempDir::new("fiber-write-parent");
    fs::write(dir.path().join("f"), "x").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let output = files.write().run(
        &args(json!({"path": "f/child", "content": "nope"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(code(&output), Some(ErrorCode::ToolError));
    assert_eq!(fs::read(dir.path().join("f")).unwrap(), b"x");
    assert!(!dir.path().join("f").join("child").exists());
}

#[test]
fn a_diff_that_is_not_one_line_each_way_is_counted() {
    let dir = TempDir::new("fiber-write-diff-two");
    fs::write(dir.path().join("a.txt"), "a\nb\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    files.read().run(
        &args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    let output = files.write().run(
        &args(json!({"path": "a.txt", "content": "z\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    let path = path_text(&resolved(dir.path(), "a.txt"));
    assert_eq!(
        output.changes,
        Some(vec![FileChange {
            path,
            added: 1,
            removed: 2,
        }])
    );
}

#[test]
fn an_unsearchable_path_is_not_a_reversible_create() {
    let dir = TempDir::new("fiber-write-unsearch");
    let locked = dir.path().join("locked");
    fs::create_dir(&locked).unwrap();
    set_mode(&locked, 0o000);
    let child = locked.join("a.txt");
    assert!(!reversible(&child));
    let err = existing(&child).unwrap_err();
    set_mode(&locked, 0o755);
    assert_eq!(err.0, ErrorCode::ToolError);
    assert!(err.1.contains("a.txt"), "{}", err.1);
}

#[test]
fn a_file_that_vanishes_before_the_write_reads_it_is_a_create() {
    let dir = TempDir::new("fiber-write-vanished");
    let got = read_present(&dir.path().join("gone")).unwrap();
    assert!(got.is_none());
}

#[test]
fn an_unreadable_file_is_not_replaced() {
    let dir = TempDir::new("fiber-write-unreadable");
    let path = dir.path().join("a.txt");
    fs::write(&path, "old\n").unwrap();
    set_mode(&path, 0o000);
    let files = Files::new(dir.path().to_path_buf());
    let output = files.write().run(
        &args(json!({"path": "a.txt", "content": "new\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    set_mode(&path, 0o644);
    assert_eq!(code(&output), Some(ErrorCode::ToolError));
    assert_eq!(fs::read(&path).unwrap(), b"old\n");
}

#[test]
fn a_read_only_file_keeps_its_mode() {
    let dir = TempDir::new("fiber-write-mode");
    let path = dir.path().join("a.txt");
    fs::write(&path, "old\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    files.read().run(
        &args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o444);
    fs::set_permissions(&path, perms).unwrap();
    let output = files.write().run(
        &args(json!({"path": "a.txt", "content": "new\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    assert_eq!(fs::read(&path).unwrap(), b"new\n");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o444
    );
}

