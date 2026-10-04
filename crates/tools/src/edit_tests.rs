use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::ErrorCode;
use contract::events::FileChange;
use contract::shapes::{ContentPart, Effect};
use contract::tool::Tool;
use fakes::{CancelToken, TempDir};
use serde_json::{Map, Value, json};

use crate::Files;
use crate::files::{hash_bytes, path_text};

const DEADLINE: Duration = Duration::from_secs(10);

const KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "description",
];

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
        Some(ContentPart::Image { .. } | ContentPart::Unknown) | None => String::new(),
    }
}

fn code(output: &contract::tool::Output) -> Option<ErrorCode> {
    output.error.as_ref().map(|error| error.code.clone())
}

fn message(output: &contract::tool::Output) -> String {
    output
        .error
        .as_ref()
        .map(|error| error.message.clone())
        .unwrap_or_default()
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

fn fiber_temps(dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        if name.contains(".fiber-") {
            names.push(name);
        }
    }
    names
}

fn wait_until(what: &str, pred: impl Fn() -> bool + Send + 'static) {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        while !pred() {
            thread::yield_now();
        }
        done.send(()).unwrap();
    });
    assert!(
        finished.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for {what}"
    );
}

fn strict(schema: &Value) {
    let Some(map) = schema.as_object() else {
        panic!("schema node is an object");
    };
    for key in map.keys() {
        assert!(KEYWORDS.contains(&key.as_str()), "{key}");
    }
    match map.get("type").and_then(Value::as_str) {
        Some("object") => {
            let properties = map.get("properties").unwrap().as_object().unwrap();
            let required: std::collections::BTreeSet<_> = map
                .get("required")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect();
            let names: std::collections::BTreeSet<_> =
                properties.keys().map(String::as_str).collect();
            assert_eq!(required, names);
            assert_eq!(map.get("additionalProperties"), Some(&Value::Bool(false)));
            for property in properties.values() {
                strict(property);
            }
        }
        Some("array") => strict(map.get("items").unwrap()),
        Some("string" | "number" | "integer" | "boolean" | "null") => {}
        _ => panic!("schema type"),
    }
}

fn edit_of(dir: &Path, value: Value) -> contract::tool::Output {
    Files::new(dir.to_path_buf())
        .edit()
        .run(&args(value), &CancelToken::new())
}

#[test]
fn the_schema_fits_the_strict_shape() {
    let files = Files::new(Path::new("/ws").to_path_buf());
    let definition = files.edit().definition();
    assert_eq!(definition.name, "edit");
    strict(&definition.input_schema);
}

#[test]
fn a_multi_block_edit_is_written_once() {
    let dir = TempDir::new("fiber-edit-multi");
    fs::write(dir.path().join("a.txt"), "aaa\nbbb\nccc\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let output = files.edit().run(
        &args(json!({
            "path": "a.txt",
            "edits": [
                {"old_text": "ccc", "new_text": "CCC"},
                {"old_text": "aaa", "new_text": "AAA"}
            ]
        })),
        &CancelToken::new(),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    let path = resolved(dir.path(), "a.txt");
    assert_eq!(fs::read(&path).unwrap(), b"AAA\nbbb\nCCC\n");
    assert!(fiber_temps(dir.path()).is_empty());
    let shown = path_text(&path);
    assert_eq!(
        text(&output),
        format!(
            "edits[0]: replaced lines 3-3 with lines 3-3.\n\
             edits[1]: replaced lines 1-1 with lines 1-1.\n\
             Wrote {shown}: 12 bytes."
        )
    );
    assert!(!text(&output).contains("normalising"), "{}", text(&output));
    let diff = output.details.as_ref().unwrap()["diff"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        diff.starts_with(&format!("--- {shown}\n+++ {shown}\n")),
        "{diff}"
    );
    assert!(!text(&output).contains(&diff), "{}", text(&output));
    assert_eq!(
        output.changes,
        Some(vec![FileChange {
            path: shown,
            added: 2,
            removed: 2,
        }])
    );
    assert_eq!(
        files.seen_hash(&path),
        Some(hash_bytes(&fs::read(&path).unwrap()))
    );
}

#[test]
fn a_normalised_match_says_so_and_the_diff_is_not_in_the_content() {
    let dir = TempDir::new("fiber-edit-norm");
    fs::write(dir.path().join("a.txt"), "it\u{2019}s\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let output = files.edit().run(
        &args(json!({
            "path": "a.txt",
            "edits": [{"old_text": "it's", "new_text": "it is"}]
        })),
        &CancelToken::new(),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    let path = resolved(dir.path(), "a.txt");
    assert_eq!(fs::read(&path).unwrap(), b"it is\n");
    let shown = path_text(&path);
    assert_eq!(
        text(&output),
        format!(
            "edits[0]: replaced lines 1-1 with lines 1-1. Matched after normalising quotes, dashes and spaces.\n\
             Wrote {shown}: 6 bytes."
        )
    );
    let diff = output.details.as_ref().unwrap()["diff"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!text(&output).contains(&diff));
}

#[test]
fn the_diff_keeps_three_lines_of_context_and_names_the_path() {
    let dir = TempDir::new("fiber-edit-diff");
    fs::write(dir.path().join("a.txt"), "1\n2\n3\n4\n5\n6\n7\n8\n").unwrap();
    let output = edit_of(
        dir.path(),
        json!({
            "path": "a.txt",
            "edits": [{"old_text": "5\n", "new_text": "FIVE\n"}]
        }),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    let shown = path_text(&resolved(dir.path(), "a.txt"));
    let diff = output.details.unwrap()["diff"].as_str().unwrap().to_owned();
    assert!(
        diff.starts_with(&format!("--- {shown}\n+++ {shown}\n")),
        "{diff}"
    );
    assert!(diff.contains("\n 2\n"), "{diff}");
    assert!(diff.contains("\n 8\n"), "{diff}");
    assert!(diff.contains("-5\n"), "{diff}");
    assert!(diff.contains("+FIVE\n"), "{diff}");
    assert!(!diff.contains("\n 1\n"), "{diff}");
    assert_eq!(
        output.changes,
        Some(vec![FileChange {
            path: shown,
            added: 1,
            removed: 1,
        }])
    );
}

#[test]
fn a_line_split_counts_the_added_and_removed_lines() {
    let dir = TempDir::new("fiber-edit-changes");
    fs::write(dir.path().join("a.txt"), "a\nb\nc\n").unwrap();
    let output = edit_of(
        dir.path(),
        json!({
            "path": "a.txt",
            "edits": [{"old_text": "b\n", "new_text": "x\ny\n"}]
        }),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"a\nx\ny\nc\n");
    let path = path_text(&resolved(dir.path(), "a.txt"));
    assert_eq!(
        output.changes,
        Some(vec![FileChange {
            path,
            added: 2,
            removed: 1,
        }])
    );
}

#[test]
fn a_failing_block_writes_nothing() {
    let dir = TempDir::new("fiber-edit-fail");
    fs::write(dir.path().join("a.txt"), "aaa\nbbb\n").unwrap();
    let output = edit_of(
        dir.path(),
        json!({
            "path": "a.txt",
            "edits": [
                {"old_text": "aaa", "new_text": "AAA"},
                {"old_text": "zzz", "new_text": "no"}
            ]
        }),
    );
    assert_eq!(code(&output), Some(ErrorCode::NoMatch));
    assert_eq!(
        message(&output),
        "edits[1]: old_text was not found. Read the file again and copy the text exactly."
    );
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"aaa\nbbb\n");
    assert!(output.details.is_none());
    assert!(output.changes.is_none());
    assert!(fiber_temps(dir.path()).is_empty());
}

#[test]
fn an_ambiguous_block_names_its_index_and_count() {
    let dir = TempDir::new("fiber-edit-ambig");
    fs::write(dir.path().join("a.txt"), "aa\naa\n").unwrap();
    let output = edit_of(
        dir.path(),
        json!({
            "path": "a.txt",
            "edits": [{"old_text": "aa", "new_text": "b"}]
        }),
    );
    assert_eq!(code(&output), Some(ErrorCode::AmbiguousMatch));
    assert_eq!(
        message(&output),
        "edits[0]: old_text matched 2 times. Read the file again and include more surrounding text."
    );
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"aa\naa\n");
}

#[test]
fn a_missing_file_is_not_found_and_nothing_is_created() {
    let dir = TempDir::new("fiber-edit-miss");
    let output = edit_of(
        dir.path(),
        json!({
            "path": "nope.txt",
            "edits": [{"old_text": "a", "new_text": "b"}]
        }),
    );
    assert_eq!(code(&output), Some(ErrorCode::NotFound));
    assert!(
        message(&output).contains("does not exist"),
        "{}",
        message(&output)
    );
    assert!(!dir.path().join("nope.txt").exists());
}

#[test]
fn a_directory_a_fifo_and_a_binary_file_are_unsupported() {
    let dir = TempDir::new("fiber-edit-kind");
    fs::create_dir(dir.path().join("sub")).unwrap();
    let directory = edit_of(
        dir.path(),
        json!({"path": "sub", "edits": [{"old_text": "a", "new_text": "b"}]}),
    );
    assert_eq!(code(&directory), Some(ErrorCode::UnsupportedFile));
    assert!(
        text(&directory).contains("directory"),
        "{}",
        text(&directory)
    );

    let pipe = dir.path().join("pipe");
    assert!(
        Command::new("mkfifo")
            .arg(&pipe)
            .status()
            .unwrap()
            .success()
    );
    let fifo = edit_of(
        dir.path(),
        json!({"path": "pipe", "edits": [{"old_text": "a", "new_text": "b"}]}),
    );
    assert_eq!(code(&fifo), Some(ErrorCode::UnsupportedFile));
    assert!(text(&fifo).contains("fifo"), "{}", text(&fifo));

    fs::write(dir.path().join("a.png"), b"\x89PNG\r\n\x1a\nxx").unwrap();
    let image = edit_of(
        dir.path(),
        json!({"path": "a.png", "edits": [{"old_text": "a", "new_text": "b"}]}),
    );
    assert_eq!(code(&image), Some(ErrorCode::UnsupportedFile));
    assert!(text(&image).contains("PNG"), "{}", text(&image));
    assert_eq!(
        fs::read(dir.path().join("a.png")).unwrap(),
        b"\x89PNG\r\n\x1a\nxx"
    );
}

#[test]
fn edit_does_not_need_a_prior_read_and_a_later_write_does_not_either() {
    let dir = TempDir::new("fiber-edit-seen");
    fs::write(dir.path().join("a.txt"), "old\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let edited = files.edit().run(
        &args(json!({
            "path": "a.txt",
            "edits": [{"old_text": "old", "new_text": "new"}]
        })),
        &CancelToken::new(),
    );
    assert!(edited.error.is_none(), "{}", text(&edited));
    let replaced = files.write().run(
        &args(json!({"path": "a.txt", "content": "later\n"})),
        &CancelToken::new(),
    );
    assert!(replaced.error.is_none(), "{}", text(&replaced));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"later\n");
}

#[test]
fn two_edits_of_one_file_serialise_and_both_apply() {
    let dir = TempDir::new("fiber-edit-race");
    fs::write(dir.path().join("a.txt"), "aaa\nbbb\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let path = resolved(dir.path(), "a.txt");
    let held = files.locks();
    let hold = held.lock(&path);
    let first = files.edit();
    let second = files.edit();
    let (left_tx, left_rx) = mpsc::channel();
    let (right_tx, right_rx) = mpsc::channel();
    thread::spawn(move || {
        left_tx
            .send(first.run(
                &args(json!({
                    "path": "a.txt",
                    "edits": [{"old_text": "aaa", "new_text": "AAA"}]
                })),
                &CancelToken::new(),
            ))
            .unwrap();
    });
    thread::spawn(move || {
        right_tx
            .send(second.run(
                &args(json!({
                    "path": "a.txt",
                    "edits": [{"old_text": "bbb", "new_text": "BBB"}]
                })),
                &CancelToken::new(),
            ))
            .unwrap();
    });
    let locks = files.locks();
    wait_until("both edits to be waiting on the path", move || {
        locks.waiting() == 2
    });
    drop(hold);
    let left = left_rx
        .recv_timeout(DEADLINE)
        .expect("waited 10s for the first edit");
    let right = right_rx
        .recv_timeout(DEADLINE)
        .expect("waited 10s for the second edit");
    assert!(left.error.is_none(), "{}", text(&left));
    assert!(right.error.is_none(), "{}", text(&right));
    assert_eq!(fs::read(&path).unwrap(), b"AAA\nBBB\n");
}

#[test]
fn a_held_lock_blocks_edit_until_it_is_released() {
    let dir = TempDir::new("fiber-edit-lock");
    fs::write(dir.path().join("a.txt"), "old\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let path = resolved(dir.path(), "a.txt");
    let locks = files.locks();
    let hold = locks.lock(&path);
    let edit = files.edit();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        done_tx
            .send(edit.run(
                &args(json!({
                    "path": "a.txt",
                    "edits": [{"old_text": "old", "new_text": "new"}]
                })),
                &CancelToken::new(),
            ))
            .unwrap();
    });
    let locks = files.locks();
    wait_until("edit to be waiting on the path", move || {
        locks.waiting() == 1
    });
    assert!(done_rx.try_recv().is_err());
    drop(hold);
    let output = done_rx
        .recv_timeout(DEADLINE)
        .expect("waited 10s for edit to finish");
    assert!(output.error.is_none(), "{}", text(&output));
    assert_eq!(fs::read(&path).unwrap(), b"new\n");
}

#[test]
fn a_retargeted_symlink_writes_nothing() {
    let dir = TempDir::new("fiber-edit-retarget");
    fs::write(dir.path().join("a.txt"), "A\n").unwrap();
    fs::write(dir.path().join("b.txt"), "B\n").unwrap();
    symlink("a.txt", dir.path().join("link")).unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let arguments = args(json!({
        "path": "link",
        "edits": [{"old_text": "A", "new_text": "Z"}]
    }));
    files.edit().effects(&arguments).unwrap();
    fs::remove_file(dir.path().join("link")).unwrap();
    symlink("b.txt", dir.path().join("link")).unwrap();
    let output = files.edit().run(&arguments, &CancelToken::new());
    assert_eq!(code(&output), Some(ErrorCode::PathChanged));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"A\n");
    assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"B\n");
    assert!(fiber_temps(dir.path()).is_empty());
}

#[test]
fn a_symlink_retargeted_while_the_lock_is_held_writes_nothing() {
    let dir = TempDir::new("fiber-edit-window");
    fs::write(dir.path().join("a.txt"), "A\n").unwrap();
    fs::write(dir.path().join("b.txt"), "B\n").unwrap();
    symlink("a.txt", dir.path().join("link")).unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let target = resolved(dir.path(), "a.txt");
    let locks = files.locks();
    let hold = locks.lock(&target);
    let edit = files.edit();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        done_tx
            .send(edit.run(
                &args(json!({
                    "path": "link",
                    "edits": [{"old_text": "A", "new_text": "Z"}]
                })),
                &CancelToken::new(),
            ))
            .unwrap();
    });
    let locks = files.locks();
    wait_until("edit to be waiting on the old target", move || {
        locks.waiting() == 1
    });
    fs::remove_file(dir.path().join("link")).unwrap();
    symlink("b.txt", dir.path().join("link")).unwrap();
    drop(hold);
    let output = done_rx
        .recv_timeout(DEADLINE)
        .expect("waited 10s for edit to finish");
    assert_eq!(code(&output), Some(ErrorCode::PathChanged));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"A\n");
    assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"B\n");
}

#[test]
fn edit_follows_a_symlink_to_its_target() {
    let dir = TempDir::new("fiber-edit-link");
    fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
    symlink("a.txt", dir.path().join("link")).unwrap();
    let output = edit_of(
        dir.path(),
        json!({
            "path": "link",
            "edits": [{"old_text": "hello", "new_text": "hi"}]
        }),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"hi\n");
}

#[test]
fn a_read_only_file_keeps_its_mode() {
    let dir = TempDir::new("fiber-edit-mode");
    let path = dir.path().join("a.txt");
    fs::write(&path, "old\n").unwrap();
    set_mode(&path, 0o444);
    let output = edit_of(
        dir.path(),
        json!({
            "path": "a.txt",
            "edits": [{"old_text": "old", "new_text": "new"}]
        }),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    assert_eq!(fs::read(&path).unwrap(), b"new\n");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o444
    );
}

#[test]
fn a_hard_link_outside_the_workspace_sees_the_same_bytes() {
    let dir = TempDir::new("fiber-edit-hard");
    let outside = TempDir::new("fiber-edit-hard-out");
    let primary = dir.path().join("a.txt");
    let other = outside.path().join("b.txt");
    fs::write(&primary, "old\n").unwrap();
    fs::hard_link(&primary, &other).unwrap();
    let inode = fs::metadata(&primary).unwrap().ino();
    let output = edit_of(
        dir.path(),
        json!({
            "path": "a.txt",
            "edits": [{"old_text": "old", "new_text": "new"}]
        }),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    assert_eq!(fs::read(&primary).unwrap(), b"new\n");
    assert_eq!(fs::read(&other).unwrap(), b"new\n");
    assert_eq!(fs::metadata(&primary).unwrap().ino(), inode);
    assert!(fiber_temps(dir.path()).is_empty());
}

#[test]
fn effects_are_an_irreversible_write_whether_or_not_the_file_exists() {
    let dir = TempDir::new("fiber-edit-effects");
    let files = Files::new(dir.path().to_path_buf());
    let missing = files
        .edit()
        .effects(&args(json!({
            "path": "a.txt",
            "edits": [{"old_text": "a", "new_text": "b"}]
        })))
        .unwrap();
    assert_eq!(missing.declared.effects, vec![Effect::Writes]);
    assert!(!missing.declared.reversible);
    let path = resolved(dir.path(), "a.txt");
    assert_eq!(missing.subject, Some(path_text(&path)));
    assert_eq!(
        missing.prefix,
        Some(format!("{}/", path_text(&canonical(dir.path()))))
    );
    assert_eq!(missing.declared.paths, Some(vec![path_text(&path)]));
    fs::write(&path, "a\n").unwrap();
    let present = files
        .edit()
        .effects(&args(json!({
            "path": "a.txt",
            "edits": [{"old_text": "a", "new_text": "b"}]
        })))
        .unwrap();
    assert!(!present.declared.reversible);
    assert_eq!(present.declared.effects, vec![Effect::Writes]);
}

#[test]
fn a_run_with_no_judged_path_still_writes() {
    let dir = TempDir::new("fiber-edit-no-judge");
    fs::write(dir.path().join("a.txt"), "old\n").unwrap();
    let output = edit_of(
        dir.path(),
        json!({
            "path": "a.txt",
            "edits": [{"old_text": "old", "new_text": "new"}]
        }),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"new\n");
}

#[test]
fn bad_arguments_fail_before_anything_is_written() {
    let dir = TempDir::new("fiber-edit-args");
    fs::write(dir.path().join("a.txt"), "old\n").unwrap();
    let cases = [
        (
            json!({"edits": [{"old_text": "old", "new_text": "new"}]}),
            "Give the file path as `path`.",
        ),
        (
            json!({"path": 1, "edits": [{"old_text": "old", "new_text": "new"}]}),
            "`path` must be a string.",
        ),
        (json!({"path": "a.txt"}), "Give the edits as `edits`."),
        (
            json!({"path": "a.txt", "edits": "no"}),
            "`edits` must be a list of blocks.",
        ),
        (
            json!({"path": "a.txt", "edits": []}),
            "`edits` must contain at least one block.",
        ),
        (
            json!({"path": "a.txt", "edits": ["no"]}),
            "`edits[0]` must be an object.",
        ),
        (
            json!({"path": "a.txt", "edits": [{"new_text": "new"}]}),
            "edits[0]: Give `old_text`.",
        ),
        (
            json!({"path": "a.txt", "edits": [{"old_text": 1, "new_text": "new"}]}),
            "edits[0]: `old_text` must be a string.",
        ),
        (
            json!({"path": "a.txt", "edits": [{"old_text": "old"}, {"old_text": "x", "new_text": "y"}]}),
            "edits[0]: Give `new_text`.",
        ),
        (
            json!({"path": "a.txt", "edits": [{"old_text": "old", "new_text": 1}]}),
            "edits[0]: `new_text` must be a string.",
        ),
    ];
    for (value, expect) in cases {
        let output = edit_of(dir.path(), value);
        assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
        assert_eq!(message(&output), expect);
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old\n");
    }
}

#[test]
fn effects_reject_the_same_bad_arguments() {
    let files = Files::new(Path::new("/ws").to_path_buf());
    let missing = files.edit().effects(&args(json!({
        "path": "a.txt",
        "edits": []
    })));
    assert!(missing.is_err());
    let typed = files.edit().effects(&args(json!({
        "path": 1,
        "edits": [{"old_text": "a", "new_text": "b"}]
    })));
    assert!(typed.is_err());
}

#[test]
fn a_path_through_a_missing_directory_is_invalid_arguments() {
    let dir = TempDir::new("fiber-edit-dotdot");
    let output = edit_of(
        dir.path(),
        json!({
            "path": "missing/../a.txt",
            "edits": [{"old_text": "a", "new_text": "b"}]
        }),
    );
    assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    assert!(message(&output).contains(".."), "{}", message(&output));
}

#[test]
fn a_parent_that_is_a_file_writes_nothing() {
    let dir = TempDir::new("fiber-edit-parent");
    fs::write(dir.path().join("f"), "x").unwrap();
    let output = edit_of(
        dir.path(),
        json!({
            "path": "f/child",
            "edits": [{"old_text": "a", "new_text": "b"}]
        }),
    );
    assert_eq!(code(&output), Some(ErrorCode::ToolError));
    assert_eq!(fs::read(dir.path().join("f")).unwrap(), b"x");
    assert!(!dir.path().join("f").join("child").exists());
}

#[test]
fn a_symlink_loop_is_a_tool_error() {
    let dir = TempDir::new("fiber-edit-loop");
    symlink("b", dir.path().join("a")).unwrap();
    symlink("a", dir.path().join("b")).unwrap();
    let output = edit_of(
        dir.path(),
        json!({
            "path": "a",
            "edits": [{"old_text": "a", "new_text": "b"}]
        }),
    );
    assert_eq!(code(&output), Some(ErrorCode::ToolError));
}

#[test]
fn an_unreadable_file_is_not_edited() {
    let dir = TempDir::new("fiber-edit-unreadable");
    let path = dir.path().join("a.txt");
    fs::write(&path, "old\n").unwrap();
    set_mode(&path, 0o000);
    let output = edit_of(
        dir.path(),
        json!({
            "path": "a.txt",
            "edits": [{"old_text": "old", "new_text": "new"}]
        }),
    );
    set_mode(&path, 0o644);
    assert_eq!(code(&output), Some(ErrorCode::ToolError));
    assert_eq!(fs::read(&path).unwrap(), b"old\n");
}

#[test]
fn a_directory_that_cannot_be_written_leaves_the_file() {
    let dir = TempDir::new("fiber-edit-unwritable");
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    let path = sub.join("a.txt");
    fs::write(&path, "old\n").unwrap();
    set_mode(&sub, 0o555);
    let output = edit_of(
        dir.path(),
        json!({
            "path": "sub/a.txt",
            "edits": [{"old_text": "old", "new_text": "new"}]
        }),
    );
    set_mode(&sub, 0o755);
    assert_eq!(
        code(&output),
        Some(ErrorCode::ToolError),
        "{}",
        text(&output)
    );
    assert_eq!(fs::read(&path).unwrap(), b"old\n");
    assert!(fiber_temps(&sub).is_empty());
}

#[test]
fn an_overlap_and_a_no_op_write_nothing() {
    let dir = TempDir::new("fiber-edit-noop");
    fs::write(dir.path().join("a.txt"), "abcd\n").unwrap();
    let overlap = edit_of(
        dir.path(),
        json!({
            "path": "a.txt",
            "edits": [
                {"old_text": "abc", "new_text": "X"},
                {"old_text": "cd", "new_text": "Y"}
            ]
        }),
    );
    assert_eq!(code(&overlap), Some(ErrorCode::InvalidArguments));
    assert_eq!(message(&overlap), "edits[0] and edits[1] overlap.");
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"abcd\n");
    let noop = edit_of(
        dir.path(),
        json!({
            "path": "a.txt",
            "edits": [{"old_text": "abc", "new_text": "abc"}]
        }),
    );
    assert_eq!(code(&noop), Some(ErrorCode::InvalidArguments));
    assert_eq!(message(&noop), "The edits leave the file unchanged.");
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"abcd\n");
}

#[test]
fn already_cancelled_edits_nothing() {
    let dir = TempDir::new("fiber-edit-cancel");
    fs::write(dir.path().join("a.txt"), "old\n").unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();
    let output = Files::new(dir.path().to_path_buf()).edit().run(
        &args(json!({
            "path": "a.txt",
            "edits": [{"old_text": "old", "new_text": "new"}]
        })),
        &cancel,
    );
    assert!(output.error.is_none());
    assert_eq!(text(&output), "Cancelled before it started.\n");
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old\n");
}

#[test]
fn deleting_a_line_reports_that_no_lines_were_written() {
    let dir = TempDir::new("fiber-edit-delete");
    fs::write(dir.path().join("a.txt"), "a\nb\nc\n").unwrap();
    let output = edit_of(
        dir.path(),
        json!({
            "path": "a.txt",
            "edits": [{"old_text": "b\n", "new_text": ""}]
        }),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"a\nc\n");
    let shown = path_text(&resolved(dir.path(), "a.txt"));
    assert_eq!(
        text(&output),
        format!("edits[0]: replaced lines 2-2 with no lines.\nWrote {shown}: 4 bytes.")
    );
}
