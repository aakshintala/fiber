use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use contract::ErrorCode;
use contract::shapes::{ContentPart, Effect};
use contract::tool::{Bound, Tool};
use fakes::{CancelToken, TempDir};
use serde_json::{Map, Value, json};

use crate::Files;
use crate::files::{hash_bytes, path_text};

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            panic!("arguments are an object")
        }
    }
}

fn run(dir: &Path, value: Value) -> contract::tool::Output {
    Files::new(dir.to_path_buf())
        .read()
        .run(&args(value), &CancelToken::new())
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

fn canonical(path: &Path) -> std::path::PathBuf {
    fs::canonicalize(path).unwrap()
}

#[test]
fn the_schema_leaves_offset_and_limit_optional() {
    let files = Files::new(Path::new("/ws").to_path_buf());
    let schema = files.read().definition().input_schema;
    let properties = schema.get("properties").unwrap().as_object().unwrap();
    assert!(properties.contains_key("path"));
    assert!(properties.contains_key("offset"));
    assert!(properties.contains_key("limit"));
    assert!(schema.get("pages").is_none());
    assert!(
        properties
            .get("offset")
            .and_then(|value| value.get("pages"))
            .is_none()
    );
    let required: Vec<_> = schema
        .get("required")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert_eq!(required, ["path"]);
    assert_eq!(
        schema.get("additionalProperties"),
        Some(&Value::Bool(false))
    );
}

#[test]
fn the_bound_sits_above_the_tools_own_cut() {
    let files = Files::new(Path::new("/ws").to_path_buf());
    assert_eq!(
        files.read().bound(),
        Bound {
            start: 32_768,
            end: 0
        }
    );
}

#[test]
fn a_small_file_comes_back_as_it_is() {
    let dir = TempDir::new("fiber-read-small");
    fs::write(dir.path().join("a.txt"), "a\r\nb\r\n").unwrap();
    let output = run(dir.path(), json!({"path": "a.txt"}));
    assert!(output.error.is_none());
    assert_eq!(text(&output), "a\r\nb\r\n");
}

#[test]
fn a_byte_order_mark_is_dropped() {
    let dir = TempDir::new("fiber-read-bom");
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(b"hi\n");
    fs::write(dir.path().join("a.txt"), &bytes).unwrap();
    let output = run(dir.path(), json!({"path": "a.txt"}));
    assert_eq!(text(&output), "hi\n");
}

#[test]
fn a_lone_carriage_return_stays_in_the_line() {
    let dir = TempDir::new("fiber-read-cr");
    fs::write(dir.path().join("a.txt"), "a\rb").unwrap();
    let output = run(dir.path(), json!({"path": "a.txt"}));
    assert_eq!(text(&output), "a\rb");
}

#[test]
fn a_file_with_no_trailing_newline_keeps_its_last_line() {
    let dir = TempDir::new("fiber-read-nonewline");
    fs::write(dir.path().join("a.txt"), "a\nb").unwrap();
    let output = run(dir.path(), json!({"path": "a.txt"}));
    assert_eq!(text(&output), "a\nb");
}

#[test]
fn offset_and_limit_name_the_next_line() {
    let dir = TempDir::new("fiber-read-range");
    fs::write(dir.path().join("a.txt"), "x\ny\nz\n").unwrap();
    let output = run(
        dir.path(),
        json!({"path": "a.txt", "offset": 2, "limit": 1}),
    );
    assert_eq!(
        text(&output),
        "y\n[Showing lines 2-2 of 3. Continue with offset=3.]"
    );
}

#[test]
fn a_limit_that_stops_early_gives_the_notice() {
    let dir = TempDir::new("fiber-read-limit");
    fs::write(dir.path().join("a.txt"), "a\nb\nc\n").unwrap();
    let output = run(dir.path(), json!({"path": "a.txt", "limit": 2}));
    assert_eq!(
        text(&output),
        "a\nb\n[Showing lines 1-2 of 3. Continue with offset=3.]"
    );
}

#[test]
fn a_file_just_over_the_cap_is_cut_at_a_line_boundary() {
    let dir = TempDir::new("fiber-read-cap");
    let line = format!("{}\n", "a".repeat(63));
    assert_eq!(line.len(), 64);
    let lines = 16_384 / 64 + 1;
    let body = line.repeat(lines);
    fs::write(dir.path().join("a.txt"), &body).unwrap();
    let output = run(dir.path(), json!({"path": "a.txt"}));
    let shown = text(&output);
    let notice = "[Showing lines 1-256 of 257. Continue with offset=257.]";
    assert!(shown.ends_with(notice), "{shown}");
    assert_eq!(shown.len(), 16_384 + notice.len());
}

#[test]
fn a_first_line_over_the_cap_is_cut_inside_the_line() {
    let dir = TempDir::new("fiber-read-long");
    let mut body = "b".repeat(16_385);
    body.push('\n');
    body.push_str("tail\n");
    fs::write(dir.path().join("a.txt"), &body).unwrap();
    let output = run(dir.path(), json!({"path": "a.txt"}));
    let shown = text(&output);
    let notice = "[Line 1 is longer than 16384 bytes; showing its first 16384 bytes. \
         Read the rest by byte range through the shell, such as cut -c or dd. \
         The file has 2 lines.]";
    assert!(shown.ends_with(notice), "{shown}");
    assert!(shown.starts_with(&"b".repeat(16_384)));
}

#[test]
fn a_cut_inside_a_line_stops_on_a_character_boundary() {
    let dir = TempDir::new("fiber-read-char");
    let mut body = "c".repeat(16_383);
    body.push('\u{00e9}');
    body.push_str("more\n");
    fs::write(dir.path().join("a.txt"), &body).unwrap();
    let output = run(dir.path(), json!({"path": "a.txt"}));
    let shown = text(&output);
    assert!(shown.contains("showing its first 16383 bytes"), "{shown}");
    assert!(shown.starts_with(&"c".repeat(16_383)));
    assert!(!shown.contains('\u{00e9}'));
}

#[test]
fn a_bad_offset_or_limit_is_invalid_arguments() {
    let dir = TempDir::new("fiber-read-bad");
    fs::write(dir.path().join("a.txt"), "a\nb\nc\n").unwrap();
    let cases = [
        json!({"path": "a.txt", "offset": 0}),
        json!({"path": "a.txt", "limit": 0}),
        json!({"path": "a.txt", "offset": 1.5}),
        json!({"path": "a.txt", "offset": "2"}),
        json!({"path": "a.txt", "limit": -3}),
    ];
    for value in cases {
        let output = run(dir.path(), value);
        assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    }
}

#[test]
fn an_offset_past_the_end_names_the_line_count() {
    let dir = TempDir::new("fiber-read-past");
    fs::write(dir.path().join("a.txt"), "a\nb\nc\n").unwrap();
    let output = run(dir.path(), json!({"path": "a.txt", "offset": 4}));
    assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    assert!(text(&output).contains("3"), "{}", text(&output));
}

#[test]
fn an_empty_file_at_offset_1_succeeds() {
    let dir = TempDir::new("fiber-read-empty");
    fs::write(dir.path().join("a.txt"), "").unwrap();
    let output = run(dir.path(), json!({"path": "a.txt", "offset": 1}));
    assert!(output.error.is_none());
    assert_eq!(text(&output), "");
}

#[test]
fn a_missing_file_is_not_found() {
    let dir = TempDir::new("fiber-read-missing");
    let output = run(dir.path(), json!({"path": "nope.txt"}));
    assert_eq!(code(&output), Some(ErrorCode::NotFound));
    assert!(
        text(&output).contains("does not exist"),
        "{}",
        text(&output)
    );
}

#[test]
fn a_directory_points_at_the_shell() {
    let dir = TempDir::new("fiber-read-dir");
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    let output = run(dir.path(), json!({"path": "sub"}));
    assert_eq!(code(&output), Some(ErrorCode::UnsupportedFile));
    let message = text(&output);
    assert!(message.contains("directory"), "{message}");
    assert!(message.contains("shell"), "{message}");
    assert!(
        message.contains(&fs::symlink_metadata(&sub).unwrap().len().to_string()),
        "{message}"
    );
}

#[test]
fn a_fifo_does_not_block() {
    let dir = TempDir::new("fiber-read-fifo");
    let path = dir.path().join("pipe");
    assert!(
        Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let output = run(dir.path(), json!({"path": "pipe"}));
    assert_eq!(code(&output), Some(ErrorCode::UnsupportedFile));
    assert!(text(&output).contains("fifo"), "{}", text(&output));
}

#[test]
fn images_pdfs_binary_and_non_utf8_are_unsupported() {
    let dir = TempDir::new("fiber-read-types");
    let cases: &[(&str, &[u8], &str)] = &[
        ("a.png", b"\x89PNG\r\n\x1a\nxx", "PNG"),
        ("a.pdf", b"%PDF-1.4", "PDF"),
        ("a.bin", b"hello\0world", "binary data"),
        ("a.dat", b"\xff\xfe", "not UTF-8 text"),
    ];
    for (name, bytes, expect) in cases {
        fs::write(dir.path().join(name), bytes).unwrap();
        let output = run(dir.path(), json!({"path": name}));
        assert_eq!(code(&output), Some(ErrorCode::UnsupportedFile), "{name}");
        let message = text(&output);
        assert!(message.contains(expect), "{name}: {message}");
        assert!(
            message.contains(&bytes.len().to_string()),
            "{name}: {message}"
        );
        if *expect == "PNG" || *expect == "PDF" {
            assert!(
                message.contains("Images and PDFs are not read yet"),
                "{message}"
            );
        }
    }
}

#[test]
fn a_symlink_reads_its_target() {
    let dir = TempDir::new("fiber-read-link");
    fs::write(dir.path().join("real.txt"), "target\n").unwrap();
    symlink("real.txt", dir.path().join("link")).unwrap();
    let output = run(dir.path(), json!({"path": "link"}));
    assert_eq!(text(&output), "target\n");
}

#[test]
fn effects_declare_a_reversible_read_of_the_resolved_path() {
    let dir = TempDir::new("fiber-read-effects");
    fs::write(dir.path().join("a.txt"), "hi\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let effects = files
        .read()
        .effects(&args(json!({"path": "a.txt"})))
        .unwrap();
    let resolved = path_text(&canonical(dir.path()).join("a.txt"));
    assert_eq!(effects.declared.effects, vec![Effect::Reads]);
    assert!(effects.declared.reversible);
    assert_eq!(effects.declared.paths, Some(vec![resolved.clone()]));
    assert_eq!(effects.subject, Some(resolved));
    assert_eq!(
        effects.prefix,
        Some(format!("{}/", path_text(&canonical(dir.path()))))
    );
}

#[test]
fn effects_of_a_missing_file_still_name_the_resolved_path() {
    let dir = TempDir::new("fiber-read-effects-missing");
    let files = Files::new(dir.path().to_path_buf());
    let effects = files
        .read()
        .effects(&args(json!({"path": "newdir/a.txt"})))
        .unwrap();
    let resolved = path_text(&canonical(dir.path()).join("newdir").join("a.txt"));
    assert_eq!(effects.subject, Some(resolved));
}

#[test]
fn a_non_string_path_and_an_unresolvable_path_are_arguments() {
    let dir = TempDir::new("fiber-read-effects-bad");
    let files = Files::new(dir.path().to_path_buf());
    let bad = files.read().effects(&args(json!({"path": 1}))).unwrap_err();
    assert!(matches!(bad, contract::tool::EffectsError::Arguments(_)));
    let dotdot = files
        .read()
        .effects(&args(json!({"path": "nope/../a.txt"})))
        .unwrap_err();
    assert!(matches!(dotdot, contract::tool::EffectsError::Arguments(_)));
    let output = run(dir.path(), json!({"path": "nope/../a.txt"}));
    assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
}

#[test]
fn a_ranged_read_records_the_whole_file() {
    let dir = TempDir::new("fiber-read-seen");
    let bytes = b"x\ny\nz\n".to_vec();
    fs::write(dir.path().join("a.txt"), &bytes).unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let output = files.read().run(
        &args(json!({"path": "a.txt", "offset": 2, "limit": 1})),
        &CancelToken::new(),
    );
    assert!(output.error.is_none());
    let resolved = canonical(dir.path()).join("a.txt");
    assert_eq!(files.seen_hash(&resolved), Some(hash_bytes(&bytes)));
}

#[test]
fn a_failed_read_records_nothing() {
    let dir = TempDir::new("fiber-read-unseen");
    fs::write(dir.path().join("a.txt"), "a\nb\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let output = files.read().run(
        &args(json!({"path": "a.txt", "offset": 9})),
        &CancelToken::new(),
    );
    assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    let resolved = canonical(dir.path()).join("a.txt");
    assert_eq!(files.seen_hash(&resolved), None);
}
