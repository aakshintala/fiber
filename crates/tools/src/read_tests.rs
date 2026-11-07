use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use contract::ErrorCode;
use contract::shapes::{ContentPart, Effect};
use contract::tool::{Bound, Tool};
use fakes::{CancelToken, Recorder, TempDir};
use serde_json::{Value, json};

use crate::Files;
use crate::files::path_text;

fn run(dir: &Path, value: Value) -> contract::tool::Output {
    Files::new(dir.to_path_buf()).read().run(
        &fakes::tool_args(value),
        &CancelToken::new(),
        &Recorder::default(),
    )
}

fn code(output: &contract::tool::Output) -> Option<ErrorCode> {
    output.error.as_ref().map(|error| error.code.clone())
}

fn canonical(path: &Path) -> std::path::PathBuf {
    fs::canonicalize(path).unwrap()
}

#[test]
fn nullable_optional_arguments_read_the_whole_file_and_are_required_in_the_schema() {
    let dir = TempDir::new("fiber-read-nullable");
    fs::write(dir.path().join("a.txt"), "first\nsecond\n").unwrap();
    let output = run(
        dir.path(),
        json!({"path": "a.txt", "offset": null, "limit": null, "pages": null}),
    );
    assert!(output.error.is_none());
    assert_eq!(fakes::text_of(&output), "first\nsecond\n");
    let schema = Files::new(dir.path().to_path_buf())
        .read()
        .definition()
        .input_schema;
    assert_eq!(
        schema["required"],
        json!(["path", "offset", "limit", "pages"])
    );
    assert_eq!(
        schema["properties"]["offset"]["type"],
        json!(["integer", "null"])
    );
    assert_eq!(
        schema["properties"]["limit"]["type"],
        json!(["integer", "null"])
    );
    assert_eq!(
        schema["properties"]["pages"]["type"],
        json!(["string", "null"])
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
fn text_details_name_the_first_returned_line() {
    let dir = TempDir::new("fiber-read-details");
    fs::write(dir.path().join("ten.txt"), "line\n".repeat(10)).unwrap();
    fs::write(dir.path().join("empty.txt"), "").unwrap();
    for (arguments, name, start_line, body) in [
        (
            json!({"path": "ten.txt", "offset": 5}),
            "ten.txt",
            5,
            "line\n".repeat(10),
        ),
        (
            json!({"path": "ten.txt"}),
            "ten.txt",
            1,
            "line\n".repeat(10),
        ),
        (json!({"path": "empty.txt"}), "empty.txt", 1, String::new()),
    ] {
        let output = run(dir.path(), arguments);
        assert!(output.error.is_none());
        let path = canonical(dir.path()).join(name);
        let hash = crate::files::hash_bytes(body.as_bytes());
        assert_eq!(
            output.details,
            Some(
                json!({"start_line": start_line, "file_seen": {"path": path_text(&path), "hash": hash}})
            )
        );
    }
    let failed = run(dir.path(), json!({"path": "ten.txt", "offset": 11}));
    assert_eq!(code(&failed), Some(ErrorCode::InvalidArguments));
    assert_eq!(failed.details, None);
}

#[test]
fn image_details_do_not_number_lines() {
    let dir = TempDir::new("fiber-read-image-details");
    fs::write(dir.path().join("a.png"), b"\x89PNG\r\n\x1a\nimage").unwrap();
    let fiber = fakes::script(
        dir.path(),
        "image-details",
        r#"cat >/dev/null
printf '{"file":"%s.png","mime_type":"image/png","width":1,"height":1}\n' "$4""#,
    );
    let path = canonical(dir.path()).join("a.png");
    let hash = crate::files::hash_bytes(b"\x89PNG\r\n\x1a\nimage");
    let (send, recv) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let files =
            Files::new(dir.path().to_path_buf()).with_images(fiber, dir.path().join("artifacts"));
        let output = files.read().run(
            &fakes::tool_args(json!({"path": "a.png"})),
            &CancelToken::new(),
            &Recorder::default(),
        );
        send.send(output).unwrap();
    });
    let output = fakes::Deadline::after(std::time::Duration::from_secs(10))
        .recv_or_fail(&recv, "image result");
    assert!(output.error.is_none());
    assert!(matches!(
        output.content.get(1),
        Some(ContentPart::Image { .. })
    ));
    assert_eq!(
        output.details,
        Some(json!({"file_seen": {"path": path_text(&path), "hash": hash}}))
    );
}

#[test]
fn a_small_file_comes_back_as_it_is() {
    let dir = TempDir::new("fiber-read-small");
    fs::write(dir.path().join("a.txt"), "a\r\nb\r\n").unwrap();
    let output = run(dir.path(), json!({"path": "a.txt"}));
    assert!(output.error.is_none());
    assert_eq!(fakes::text_of(&output), "a\r\nb\r\n");
}

#[test]
fn a_byte_order_mark_is_dropped() {
    let dir = TempDir::new("fiber-read-bom");
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(b"hi\n");
    fs::write(dir.path().join("a.txt"), &bytes).unwrap();
    let output = run(dir.path(), json!({"path": "a.txt"}));
    assert_eq!(fakes::text_of(&output), "hi\n");
}

#[test]
fn a_lone_carriage_return_stays_in_the_line() {
    let dir = TempDir::new("fiber-read-cr");
    fs::write(dir.path().join("a.txt"), "a\rb").unwrap();
    let output = run(dir.path(), json!({"path": "a.txt"}));
    assert_eq!(fakes::text_of(&output), "a\rb");
}

#[test]
fn a_file_with_no_trailing_newline_keeps_its_last_line() {
    let dir = TempDir::new("fiber-read-nonewline");
    fs::write(dir.path().join("a.txt"), "a\nb").unwrap();
    let output = run(dir.path(), json!({"path": "a.txt"}));
    assert_eq!(fakes::text_of(&output), "a\nb");
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
        fakes::text_of(&output),
        "y\n[Showing lines 2-2 of 3. Continue with offset=3.]"
    );
}

#[test]
fn a_limit_that_stops_early_gives_the_notice() {
    let dir = TempDir::new("fiber-read-limit");
    fs::write(dir.path().join("a.txt"), "a\nb\nc\n").unwrap();
    let output = run(dir.path(), json!({"path": "a.txt", "limit": 2}));
    assert_eq!(
        fakes::text_of(&output),
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
    let shown = fakes::text_of(&output);
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
    let shown = fakes::text_of(&output);
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
    let shown = fakes::text_of(&output);
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
    assert!(
        fakes::text_of(&output).contains("3"),
        "{}",
        fakes::text_of(&output)
    );
}

#[test]
fn an_empty_file_at_offset_1_succeeds() {
    let dir = TempDir::new("fiber-read-empty");
    fs::write(dir.path().join("a.txt"), "").unwrap();
    let output = run(dir.path(), json!({"path": "a.txt", "offset": 1}));
    assert!(output.error.is_none());
    assert_eq!(fakes::text_of(&output), "");
}

#[test]
fn an_offset_past_an_empty_file_names_zero_lines() {
    let dir = TempDir::new("fiber-read-empty-past");
    fs::write(dir.path().join("a.txt"), "").unwrap();
    let output = run(dir.path(), json!({"path": "a.txt", "offset": 2}));
    assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    assert!(
        fakes::text_of(&output).contains('0'),
        "{}",
        fakes::text_of(&output)
    );
}

#[test]
fn a_line_exactly_at_the_cap_is_not_cut_inside() {
    let dir = TempDir::new("fiber-read-exact-cap");
    let body = "a".repeat(16_384);
    fs::write(dir.path().join("a.txt"), &body).unwrap();
    let output = run(dir.path(), json!({"path": "a.txt"}));
    assert_eq!(fakes::text_of(&output), body);
}

#[test]
fn a_missing_file_is_not_found() {
    let dir = TempDir::new("fiber-read-missing");
    let output = run(dir.path(), json!({"path": "nope.txt"}));
    assert_eq!(code(&output), Some(ErrorCode::NotFound));
    assert!(
        fakes::text_of(&output).contains("does not exist"),
        "{}",
        fakes::text_of(&output)
    );
}

#[test]
fn space_lookalike_single_names_and_opens_nothing() {
    let dir = TempDir::new("fiber-lookalike-single");
    let sibling = "shot\u{202f}AM.png";
    let payload = "single-lookalike-payload-7q\n";
    fs::write(dir.path().join(sibling), payload).unwrap();
    let output = run(dir.path(), json!({"path": "shot AM.png"}));
    assert_eq!(code(&output), Some(ErrorCode::NotFound));
    let resolved = canonical(dir.path()).join("shot AM.png");
    assert_eq!(
        fakes::text_of(&output),
        format!(
            "`{}` does not exist. An entry in the same directory matches it apart from Unicode spaces, so none was opened: `{}`.\n",
            resolved.display(),
            sibling,
        )
    );
    assert!(
        !fakes::text_of(&output).contains(payload),
        "{}",
        fakes::text_of(&output)
    );
}

#[test]
fn space_lookalike_several_list_and_open_neither() {
    let dir = TempDir::new("fiber-lookalike-several");
    let first = "shot\u{202f}AM.png";
    let second = "shot\u{a0}AM.png";
    fs::write(dir.path().join(first), "several-payload-narrow-nbsp\n").unwrap();
    fs::write(dir.path().join(second), "several-payload-nbsp\n").unwrap();
    let output = run(dir.path(), json!({"path": "shot AM.png"}));
    assert_eq!(code(&output), Some(ErrorCode::NotFound));
    let resolved = canonical(dir.path()).join("shot AM.png");
    // (`docs/tools.md`, "read"): several matches list at most the first 10
    // sorted basenames. Byte order puts U+00A0 before U+202F, so the list
    // names `second` first.
    assert_eq!(
        fakes::text_of(&output),
        format!(
            "`{}` does not exist. These names match it apart from Unicode spaces, so none was opened: `{}`, `{}`.\n",
            resolved.display(),
            second,
            first,
        )
    );
    assert!(
        !fakes::text_of(&output).contains("several-payload"),
        "{}",
        fakes::text_of(&output)
    );
}

#[test]
fn space_lookalike_none_is_unchanged() {
    let dir = TempDir::new("fiber-lookalike-none");
    fs::write(dir.path().join("other.txt"), "unrelated\n").unwrap();
    let output = run(dir.path(), json!({"path": "nope.txt"}));
    assert_eq!(code(&output), Some(ErrorCode::NotFound));
    let resolved = canonical(dir.path()).join("nope.txt");
    assert_eq!(
        fakes::text_of(&output),
        format!("`{}` does not exist.\n", resolved.display())
    );
}

#[test]
fn space_lookalike_hit_has_no_scan() {
    let dir = TempDir::new("fiber-lookalike-hit");
    let body = "exact hit body\n";
    fs::write(dir.path().join("my file.txt"), body).unwrap();
    fs::write(dir.path().join("my\u{202f}file.txt"), "lookalike body\n").unwrap();
    let output = run(dir.path(), json!({"path": "my file.txt"}));
    assert!(output.error.is_none());
    assert_eq!(fakes::text_of(&output), body);
}

#[test]
fn space_lookalike_fold_covers_separator_spaces() {
    for (tag, space) in [
        ("nnbsp", "\u{202f}"),
        ("nbsp", "\u{a0}"),
        ("emspace", "\u{2003}"),
        ("line-separator", "\u{2028}"),
        ("tab", "\t"),
        ("newline", "\n"),
        ("nel", "\u{85}"),
    ] {
        let dir = TempDir::new("fiber-lookalike-fold");
        let sibling = format!("fold{space}here.txt");
        fs::write(dir.path().join(&sibling), "fold payload\n").unwrap();
        let output = run(dir.path(), json!({"path": "fold here.txt"}));
        assert_eq!(code(&output), Some(ErrorCode::NotFound), "{tag}");
        let text = fakes::text_of(&output);
        assert!(
            text.contains("An entry in the same directory matches it apart from Unicode spaces"),
            "{tag}: {text}"
        );
        assert!(text.contains(&format!("`{sibling}`")), "{tag}: {text}");
        assert!(!text.contains("fold payload"), "{tag}: {text}");
    }
}

#[test]
fn space_lookalike_fold_keeps_zero_width_space() {
    let dir = TempDir::new("fiber-lookalike-zwsp");
    fs::write(dir.path().join("a\u{200b}b.txt"), "zwsp payload\n").unwrap();
    let output = run(dir.path(), json!({"path": "a b.txt"}));
    assert_eq!(code(&output), Some(ErrorCode::NotFound));
    let resolved = canonical(dir.path()).join("a b.txt");
    assert_eq!(
        fakes::text_of(&output),
        format!("`{}` does not exist.\n", resolved.display())
    );
}

#[test]
#[cfg(target_os = "linux")]
fn space_lookalike_replacement_char_matches_nothing_undecodable() {
    use std::os::unix::ffi::OsStrExt;
    let dir = TempDir::new("fiber-lookalike-undecodable");
    fs::write(
        dir.path()
            .join(Path::new(std::ffi::OsStr::from_bytes(b"q\xffw.txt"))),
        "undecodable payload\n",
    )
    .unwrap();
    let mut saw_undecodable = false;
    for entry in fs::read_dir(dir.path()).unwrap() {
        if entry.unwrap().file_name().to_str().is_none() {
            saw_undecodable = true;
        }
    }
    assert!(saw_undecodable, "the sibling name really does not decode");
    let output = run(dir.path(), json!({"path": "q\u{fffd}w.txt"}));
    assert_eq!(code(&output), Some(ErrorCode::NotFound));
    let resolved = canonical(dir.path()).join("q\u{fffd}w.txt");
    assert_eq!(
        fakes::text_of(&output),
        format!("`{}` does not exist.\n", resolved.display())
    );
}

#[test]
fn space_lookalike_exact_ten_lists_without_count() {
    // (`docs/tools.md`, "read"): several matches list at most the first 10
    // sorted basenames plus how many more are not listed; with 10 of 10
    // there is no count, and the backtick count pins all 10 are shown.
    let dir = TempDir::new("fiber-lookalike-ten");
    let spaces = [
        "\t", "\n", "\u{b}", "\u{c}", "\r", "\u{85}", "\u{a0}", "\u{1680}", "\u{2000}", "\u{2003}",
    ];
    let mut basenames = Vec::new();
    for space in spaces {
        let sibling = format!("shot{space}AM.png");
        fs::write(dir.path().join(&sibling), "ten payload\n").unwrap();
        basenames.push(format!("`{sibling}`"));
    }
    let output = run(dir.path(), json!({"path": "shot AM.png"}));
    assert_eq!(code(&output), Some(ErrorCode::NotFound));
    let text = fakes::text_of(&output);
    assert!(
        text.contains("These names match it apart from Unicode spaces, so none was opened: "),
        "{text}"
    );
    for name in &basenames {
        assert!(text.contains(name), "{text}");
    }
    assert!(!text.contains("more are not listed"), "{text}");
    assert!(!text.contains("(showing"), "{text}");
    assert!(!text.contains("ten payload"), "{text}");
    assert_eq!(
        text.chars().filter(|c| *c == '`').count(),
        2 * (1 + basenames.len())
    );
}

#[test]
fn space_lookalike_long_list_is_bounded() {
    let dir = TempDir::new("fiber-lookalike-twelve");
    // (`docs/tools.md`, "read"): several matches list at most the first 10
    // sorted basenames plus how many more are not listed. Sorted by UTF-8
    // bytes, the last two spaces fall outside the first 10.
    let spaces = [
        "\t", "\n", "\u{b}", "\u{c}", "\r", "\u{85}", "\u{a0}", "\u{1680}", "\u{2000}", "\u{2003}",
        "\u{2028}", "\u{202f}",
    ];
    for space in spaces {
        let sibling = format!("shot{space}AM.png");
        fs::write(dir.path().join(&sibling), "twelve payload\n").unwrap();
    }
    let output = run(dir.path(), json!({"path": "shot AM.png"}));
    assert_eq!(code(&output), Some(ErrorCode::NotFound));
    let text = fakes::text_of(&output);
    assert!(text.contains("(showing 10 of 12): "), "{text}");
    assert!(text.contains("and 2 more are not listed."), "{text}");
    for space in &spaces[..10] {
        assert!(text.contains(&format!("`shot{space}AM.png`")), "{text}");
    }
    for space in &spaces[10..] {
        assert!(!text.contains(&format!("shot{space}AM.png")), "{text}");
    }
    assert!(!text.contains("twelve payload"), "{text}");
}

/// One lookalike of any kind is named by basename and never opened
/// (`docs/tools.md`, "read"): the shared shape behind the single-image and
/// single-PDF cases. Both callers stay so each kind keeps its own pin.
fn assert_single_lookalike_named_not_opened(
    dir_tag: &str,
    sibling: &str,
    requested: &str,
    marker: &str,
) {
    let dir = TempDir::new(dir_tag);
    fs::write(dir.path().join(sibling), marker).unwrap();
    let output = run(dir.path(), json!({"path": requested}));
    assert_eq!(code(&output), Some(ErrorCode::NotFound));
    let resolved = canonical(dir.path()).join(requested);
    assert_eq!(
        fakes::text_of(&output),
        format!(
            "`{}` does not exist. An entry in the same directory matches it apart from Unicode spaces, so none was opened: `{}`.\n",
            resolved.display(),
            sibling,
        )
    );
    assert!(
        !output
            .content
            .iter()
            .any(|part| matches!(part, ContentPart::Image { .. } | ContentPart::Pdf(_))),
        "a named lookalike is never opened"
    );
}

#[test]
fn space_lookalike_single_image_is_named_not_opened() {
    assert_single_lookalike_named_not_opened(
        "fiber-lookalike-image",
        "pic\u{202f}01.png",
        "pic 01.png",
        "lookalike-image-marker-bytes\n",
    );
}

#[test]
fn space_lookalike_single_pdf_is_named_not_opened() {
    assert_single_lookalike_named_not_opened(
        "fiber-lookalike-pdf",
        "doc\u{202f}1.pdf",
        "doc 1.pdf",
        "lookalike-pdf-marker-bytes\n",
    );
}

#[test]
fn a_directory_points_at_the_shell() {
    let dir = TempDir::new("fiber-read-dir");
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    let output = run(dir.path(), json!({"path": "sub"}));
    assert_eq!(code(&output), Some(ErrorCode::UnsupportedFile));
    let message = fakes::text_of(&output);
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
    fakes::fifo(&path);
    let output = run(dir.path(), json!({"path": "pipe"}));
    assert_eq!(code(&output), Some(ErrorCode::UnsupportedFile));
    assert!(
        fakes::text_of(&output).contains("fifo"),
        "{}",
        fakes::text_of(&output)
    );
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
        let message = fakes::text_of(&output);
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
        fakes::text_of(&output).contains("PDF reading is not configured"),
        "{}",
        fakes::text_of(&output)
    );
}

#[test]
fn a_retargeted_symlink_reads_nothing() {
    let dir = TempDir::new("fiber-read-retarget");
    fs::write(dir.path().join("a.txt"), "harmless\n").unwrap();
    fs::write(dir.path().join("b.txt"), "secret\n").unwrap();
    symlink("a.txt", dir.path().join("link")).unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let arguments = fakes::tool_args(json!({"path": "link"}));
    files.read().effects(&arguments).unwrap();
    fs::remove_file(dir.path().join("link")).unwrap();
    symlink("b.txt", dir.path().join("link")).unwrap();
    let output = files
        .read()
        .run(&arguments, &CancelToken::new(), &Recorder::default());
    assert_eq!(code(&output), Some(ErrorCode::PathChanged));
    assert!(
        !fakes::text_of(&output).contains("secret"),
        "{}",
        fakes::text_of(&output)
    );
    assert!(
        !fakes::text_of(&output).contains("harmless"),
        "{}",
        fakes::text_of(&output)
    );
}

#[test]
fn a_symlink_reads_its_target() {
    let dir = TempDir::new("fiber-read-link");
    fs::write(dir.path().join("real.txt"), "target\n").unwrap();
    symlink("real.txt", dir.path().join("link")).unwrap();
    let output = run(dir.path(), json!({"path": "link"}));
    assert_eq!(fakes::text_of(&output), "target\n");
}

#[test]
fn effects_declare_a_reversible_read_of_the_resolved_path() {
    let dir = TempDir::new("fiber-read-effects");
    fs::write(dir.path().join("a.txt"), "hi\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let effects = files
        .read()
        .effects(&fakes::tool_args(json!({"path": "a.txt"})))
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
        .effects(&fakes::tool_args(json!({"path": "newdir/a.txt"})))
        .unwrap();
    let resolved = path_text(&canonical(dir.path()).join("newdir").join("a.txt"));
    assert_eq!(effects.subject, Some(resolved));
}

#[test]
fn a_non_string_path_and_an_unresolvable_path_are_arguments() {
    let dir = TempDir::new("fiber-read-effects-bad");
    let files = Files::new(dir.path().to_path_buf());
    let bad = files
        .read()
        .effects(&fakes::tool_args(json!({"path": 1})))
        .unwrap_err();
    assert!(matches!(bad, contract::tool::EffectsError::Arguments(_)));
    let dotdot = files
        .read()
        .effects(&fakes::tool_args(json!({"path": "nope/../a.txt"})))
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
        &fakes::tool_args(json!({"path": "a.txt", "offset": 2, "limit": 1})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    let replaced = files.write().run(
        &fakes::tool_args(json!({"path": "a.txt", "content": "w\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(replaced.error.is_none(), "{}", fakes::text_of(&replaced));
    assert!(fakes::text_of(&replaced).starts_with("Replaced"));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"w\n");
}

#[test]
fn a_failed_read_records_nothing() {
    let dir = TempDir::new("fiber-read-unseen");
    fs::write(dir.path().join("a.txt"), "a\nb\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let output = files.read().run(
        &fakes::tool_args(json!({"path": "a.txt", "offset": 9})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    let refused = files.write().run(
        &fakes::tool_args(json!({"path": "a.txt", "content": "w\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(code(&refused), Some(ErrorCode::StaleFile));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"a\nb\n");
}

#[test]
fn a_configured_cap_cuts_at_a_line_boundary_with_the_offset_notice() {
    let dir = TempDir::new("fiber-read-cap-100");
    let line = format!("{}\n", "a".repeat(63));
    fs::write(dir.path().join("a.txt"), line.repeat(3)).unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let capped = files.read().with_cap(100).unwrap();
    let output = capped.run(
        &fakes::tool_args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    assert_eq!(
        fakes::text_of(&output),
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
        &fakes::tool_args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    let shown = fakes::text_of(&output);
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
        &fakes::tool_args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    assert_eq!(fakes::text_of(&output), body);
}

#[test]
fn an_uncapped_read_returns_a_200kb_single_line_file_whole() {
    // The inner path runs a head-only own cut uncapped (the loop keeps a
    // head-only `with_cap(usize::MAX)` form and discards a both-ends one):
    // a default-cap file that the outer path would cut at its own 16 KiB
    // slice reaches the caller whole, with no long-line notice. The outer
    // cut stays pinned by the tests below.
    let dir = TempDir::new("fiber-read-uncapped-200kb");
    let body = "R".repeat(200 * 1024);
    fs::write(dir.path().join("a.txt"), &body).unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let output = files.read().with_cap(usize::MAX).unwrap().run(
        &fakes::tool_args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    assert_eq!(fakes::text_of(&output).len(), 204_800);
    assert_eq!(fakes::text_of(&output), body);
}

#[test]
fn a_zero_cap_shows_only_the_notice() {
    let dir = TempDir::new("fiber-read-cap-zero");
    fs::write(dir.path().join("a.txt"), "a\nb\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let capped = files.read().with_cap(0).unwrap();
    let output = capped.run(
        &fakes::tool_args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    assert_eq!(
        fakes::text_of(&output),
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
fn a_ranged_read_logs_the_whole_files_hash_beside_the_resolved_path() {
    let dir = TempDir::new("fiber-read-seen-details");
    fs::write(dir.path().join("a.txt"), "first\nsecond\nthird\n").unwrap();
    let output = run(
        dir.path(),
        json!({"path": "a.txt", "offset": 2, "limit": 1}),
    );
    assert!(output.error.is_none());
    assert!(
        fakes::text_of(&output).starts_with("second\n"),
        "{}",
        fakes::text_of(&output)
    );
    let path = canonical(dir.path()).join("a.txt");
    let hash = crate::files::hash_bytes("first\nsecond\nthird\n".as_bytes());
    assert_eq!(
        output.details,
        Some(json!({"start_line": 2, "file_seen": {"path": path_text(&path), "hash": hash}}))
    );
}

#[test]
fn a_capped_read_shares_the_session_file_state() {
    let dir = TempDir::new("fiber-read-cap-seen");
    fs::write(dir.path().join("a.txt"), "old\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let output = files.read().with_cap(100).unwrap().run(
        &fakes::tool_args(json!({"path": "a.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    let output = files.write().run(
        &fakes::tool_args(json!({"path": "a.txt", "content": "new\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none(), "{}", fakes::text_of(&output));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"new\n");
}
