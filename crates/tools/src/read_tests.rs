use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use contract::ErrorCode;
use contract::shapes::{ContentPart, Effect};
use contract::tool::{Bound, Tool};
use fakes::{CancelToken, Recorder, TempDir};
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
    Files::new(dir.to_path_buf()).read().run(
        &args(value),
        &CancelToken::new(),
        &Recorder::default(),
    )
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

#[test]
fn the_schema_leaves_offset_and_limit_optional() {
    let files = Files::new(Path::new("/ws").to_path_buf());
    let schema = files.read().definition().input_schema;
    let properties = schema.get("properties").unwrap().as_object().unwrap();
    assert!(properties.contains_key("path"));
    assert!(properties.contains_key("offset"));
    assert!(properties.contains_key("limit"));
    assert!(properties.contains_key("pages"));
    assert_eq!(
        properties.get("pages").and_then(|value| value.get("type")),
        Some(&Value::String("string".to_owned()))
    );
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
fn an_offset_past_an_empty_file_names_zero_lines() {
    let dir = TempDir::new("fiber-read-empty-past");
    fs::write(dir.path().join("a.txt"), "").unwrap();
    let output = run(dir.path(), json!({"path": "a.txt", "offset": 2}));
    assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    assert!(text(&output).contains('0'), "{}", text(&output));
}

#[test]
fn a_line_exactly_at_the_cap_is_not_cut_inside() {
    let dir = TempDir::new("fiber-read-exact-cap");
    let body = "a".repeat(16_384);
    fs::write(dir.path().join("a.txt"), &body).unwrap();
    let output = run(dir.path(), json!({"path": "a.txt"}));
    assert_eq!(text(&output), body);
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
fn binary_and_non_utf8_are_unsupported() {
    let dir = TempDir::new("fiber-read-types");
    let cases: &[(&str, &[u8], &str)] = &[
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
    }
}

#[test]
fn a_pdf_without_a_configured_child_is_tool_error() {
    let dir = TempDir::new("fiber-read-pdf-unconfigured");
    fs::write(dir.path().join("a.pdf"), b"%PDF-1.4").unwrap();
    let output = run(dir.path(), json!({"path": "a.pdf"}));
    assert_eq!(code(&output), Some(ErrorCode::ToolError));
    assert!(
        text(&output).contains("PDF reading is not configured"),
        "{}",
        text(&output)
    );
}

#[test]
fn a_retargeted_symlink_reads_nothing() {
    let dir = TempDir::new("fiber-read-retarget");
    fs::write(dir.path().join("a.txt"), "harmless\n").unwrap();
    fs::write(dir.path().join("b.txt"), "secret\n").unwrap();
    symlink("a.txt", dir.path().join("link")).unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let arguments = args(json!({"path": "link"}));
    files.read().effects(&arguments).unwrap();
    fs::remove_file(dir.path().join("link")).unwrap();
    symlink("b.txt", dir.path().join("link")).unwrap();
    let output = files
        .read()
        .run(&arguments, &CancelToken::new(), &Recorder::default());
    assert_eq!(code(&output), Some(ErrorCode::PathChanged));
    assert!(!text(&output).contains("secret"), "{}", text(&output));
    assert!(!text(&output).contains("harmless"), "{}", text(&output));
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
        &Recorder::default(),
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
        &Recorder::default(),
    );
    assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    let resolved = canonical(dir.path()).join("a.txt");
    assert_eq!(files.seen_hash(&resolved), None);
}

#[test]
fn a_configured_cap_cuts_at_a_line_boundary_with_the_offset_notice() {
    let dir = TempDir::new("fiber-read-cap-100");
    let line = format!("{}\n", "a".repeat(63));
    fs::write(dir.path().join("a.txt"), line.repeat(3)).unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let capped = files.read().with_cap(100).unwrap();
    let output = capped.run(
        &args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    assert_eq!(
        text(&output),
        format!("{line}[Showing lines 1-1 of 3. Continue with offset=2.]")
    );
}

#[test]
fn a_configured_cap_cuts_a_long_first_line_inside_it() {
    let dir = TempDir::new("fiber-read-cap-long");
    fs::write(dir.path().join("a.txt"), "b".repeat(150)).unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let capped = files.read().with_cap(100).unwrap();
    let output = capped.run(
        &args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    let shown = text(&output);
    assert!(shown.starts_with(&"b".repeat(100)), "{shown}");
    assert!(
        shown.ends_with(
            "[Line 1 is longer than 100 bytes; showing its first 100 bytes. \
             Read the rest by byte range through the shell, such as cut -c or dd. \
             The file has 1 lines.]"
        ),
        "{shown}"
    );
}

#[test]
fn a_configured_cap_above_the_default_shows_more() {
    let dir = TempDir::new("fiber-read-cap-above");
    let line = format!("{}\n", "a".repeat(63));
    let body = line.repeat(320);
    assert_eq!(body.len(), 20_480);
    fs::write(dir.path().join("a.txt"), &body).unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let capped = files.read().with_cap(32_768).unwrap();
    let output = capped.run(
        &args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    assert_eq!(text(&output), body);
}

#[test]
fn a_zero_cap_shows_only_the_notice() {
    let dir = TempDir::new("fiber-read-cap-zero");
    fs::write(dir.path().join("a.txt"), "a\nb\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let capped = files.read().with_cap(0).unwrap();
    let output = capped.run(
        &args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    assert_eq!(
        text(&output),
        "[Line 1 is longer than 0 bytes; showing its first 0 bytes. \
         Read the rest by byte range through the shell, such as cut -c or dd. \
         The file has 2 lines.]"
    );
}

#[test]
fn a_capped_reads_bound_sits_above_its_own_cut() {
    let files = Files::new(std::path::Path::new("/ws").to_path_buf());
    assert_eq!(
        files.read().with_cap(100).unwrap().bound(),
        Bound {
            start: 16_484,
            end: 0
        }
    );
    assert_eq!(
        files.read().with_cap(0).unwrap().bound(),
        Bound {
            start: 16_384,
            end: 0
        }
    );
    assert_eq!(
        files.read().with_cap(usize::MAX).unwrap().bound(),
        Bound {
            start: usize::MAX,
            end: 0
        }
    );
}

#[test]
fn a_capped_read_keeps_its_name() {
    let files = Files::new(std::path::Path::new("/ws").to_path_buf());
    assert_eq!(
        files.read().with_cap(100).unwrap().definition(),
        files.read().definition()
    );
}

#[test]
fn a_capped_read_shares_the_session_file_state() {
    let dir = TempDir::new("fiber-read-cap-seen");
    fs::write(dir.path().join("a.txt"), "old\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let output = files.read().with_cap(100).unwrap().run(
        &args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    let output = files.write().run(
        &args(json!({"path": "a.txt", "content": "new\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"new\n");
}

#[test]
fn guidelines_are_the_read_section() {
    let files = Files::new(std::path::Path::new("/ws").to_path_buf());
    let text = files.read().guidelines().unwrap();
    assert_eq!(text, crate::guidelines::of("read").unwrap());
    assert!(!text.is_empty(), "{text}");
    let md = include_str!("../prompt/guidelines.md");
    let rest = &md[md.find("## read\n").unwrap() + "## read\n".len()..];
    let end = rest.find("\n## ").map(|i| i + 1).unwrap_or(rest.len());
    assert_eq!(text, rest[..end].trim(), "{text}");
    assert!(text.contains("Read files with `read`"), "{text}");
}
