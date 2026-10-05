//! Tests beside [`super::read`]: the child's output and exit codes become a
//! tool result. The child is a `/bin/sh` script, so `tools` links no image
//! code.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use contract::ErrorCode;
use contract::shapes::ContentPart;
use contract::tool::{Output, Tool};
use fakes::{CancelToken, Recorder, TempDir};
use serde_json::{Value, json};

use crate::Files;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot really a png";

/// A stub `fiber` that runs `body` with `$2` the input, `$3` the artifacts
/// directory and `$4` the stem.
fn stub(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("fiber-stub");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn run_with(dir: &Path, fiber: &Path, name: &str) -> Output {
    let files =
        Files::new(dir.to_path_buf()).with_images(fiber.to_path_buf(), dir.join("artifacts"));
    let Value::Object(arguments) = json!({"path": name}) else {
        panic!("an object");
    };
    files
        .read()
        .run(&arguments, &CancelToken::new(), &Recorder::default())
}

fn message(output: &Output) -> String {
    match output.content.first() {
        Some(ContentPart::Text { text }) => text.clone(),
        _ => String::new(),
    }
}

fn code(output: &Output) -> Option<ErrorCode> {
    output.error.as_ref().map(|error| error.code.clone())
}

fn workspace() -> TempDir {
    let dir = TempDir::new("fiber-read-image");
    fs::write(dir.path().join("a.png"), PNG).unwrap();
    dir
}

#[test]
fn the_childs_line_becomes_text_then_an_image_part() {
    let dir = workspace();
    let fiber = stub(
        dir.path(),
        r#"printf '{"file":"%s.png","mime_type":"image/png","width":80,"height":60}\n' "$4""#,
    );
    let output = run_with(dir.path(), &fiber, "a.png");
    assert_eq!(code(&output), None);
    assert_eq!(output.content.len(), 2);
    assert_eq!(message(&output), "Image: 80x60 image/png.\n");
    let Some(ContentPart::Image {
        path,
        mime_type,
        width,
        height,
    }) = output.content.get(1)
    else {
        panic!("an image part: {:?}", output.content);
    };
    assert!(
        path.starts_with("artifacts/i_") && path.ends_with(".png"),
        "{path}"
    );
    assert_eq!(path.len(), "artifacts/i_".len() + 16 + ".png".len());
    assert_eq!((mime_type.as_str(), *width, *height), ("image/png", 80, 60));
}

#[test]
fn the_child_gets_the_resolved_path_the_artifacts_directory_and_a_fresh_stem() {
    let dir = workspace();
    let fiber = stub(
        dir.path(),
        r#"echo "$1 $2 $3 $4" > "$(dirname "$0")/argv"
printf '{"file":"%s.png","mime_type":"image/png","width":1,"height":1}\n' "$4""#,
    );
    let first = run_with(dir.path(), &fiber, "a.png");
    let argv = fs::read_to_string(dir.path().join("argv")).unwrap();
    let words: Vec<&str> = argv.split_whitespace().collect();
    assert_eq!(words.first(), Some(&"image"));
    assert_eq!(
        words.get(1).map(|word| fs::canonicalize(word).unwrap()),
        Some(fs::canonicalize(dir.path().join("a.png")).unwrap())
    );
    assert_eq!(words.get(2).copied(), dir.path().join("artifacts").to_str());
    // The same file read twice is two artifacts.
    let second = run_with(dir.path(), &fiber, "a.png");
    assert_ne!(first.content.get(1), second.content.get(1));
}

#[test]
fn exit_1_is_unsupported_file_with_the_childs_message() {
    let dir = workspace();
    let fiber = stub(
        dir.path(),
        "echo '8000x7000 is 56000000 pixels; the limit is 50000000' >&2; exit 1",
    );
    let output = run_with(dir.path(), &fiber, "a.png");
    assert_eq!(code(&output), Some(ErrorCode::UnsupportedFile));
    assert!(
        message(&output).contains("8000x7000 is 56000000 pixels; the limit is 50000000"),
        "{}",
        message(&output)
    );
    assert_eq!(output.content.len(), 1);
}

#[test]
fn a_long_message_is_cut_to_2048_bytes_at_a_character_boundary() {
    let dir = workspace();
    // 2,047 ASCII bytes and then two-byte characters: byte 2,048 is inside one.
    let fiber = stub(
        dir.path(),
        r#"i=0; while [ $i -lt 2047 ]; do printf a >&2; i=$((i+1)); done; printf 'ééé' >&2; exit 1"#,
    );
    let output = run_with(dir.path(), &fiber, "a.png");
    let failure = output.error.unwrap();
    let kept = failure.message.rsplit(": ").next().unwrap().to_owned();
    assert_eq!(kept.len(), 2047);
    assert!(kept.bytes().all(|byte| byte == b'a'));
}

#[test]
fn a_message_of_exactly_2048_bytes_is_kept_whole() {
    let dir = workspace();
    let fiber = stub(
        dir.path(),
        r#"i=0; while [ $i -lt 2048 ]; do printf a >&2; i=$((i+1)); done; exit 1"#,
    );
    let output = run_with(dir.path(), &fiber, "a.png");
    let failure = output.error.unwrap();
    let kept = failure.message.rsplit(": ").next().unwrap().to_owned();
    assert_eq!(kept.len(), 2048);
}

#[test]
fn exit_3_exit_2_and_a_signal_are_tool_error() {
    let dir = workspace();
    for (body, expect) in [
        ("echo disk full >&2; exit 3", "exited with status 3"),
        ("exit 2", "exited with status 2"),
        ("kill -9 $$", "killed by a signal"),
    ] {
        let fiber = stub(dir.path(), body);
        let output = run_with(dir.path(), &fiber, "a.png");
        assert_eq!(code(&output), Some(ErrorCode::ToolError), "{body}");
        assert!(
            message(&output).contains(expect),
            "{body}: {}",
            message(&output)
        );
    }
}

#[test]
fn unparseable_extra_or_incomplete_output_is_tool_error() {
    let dir = workspace();
    for (body, expect) in [
        ("echo hello", "not JSON"),
        ("echo '{\"file\":\"x.png\"}'", "without file"),
        (
            "echo '{\"file\":\"x.png\",\"mime_type\":\"image/png\",\"width\":1,\"height\":1}'; echo more",
            "more than one line",
        ),
        ("", "not JSON"),
        (
            "printf '{\"file\":\"x.png\",\"mime_type\":\"image/png\",\"width\":-1,\"height\":1}\\n'",
            "without file",
        ),
    ] {
        let fiber = stub(dir.path(), body);
        let output = run_with(dir.path(), &fiber, "a.png");
        assert_eq!(code(&output), Some(ErrorCode::ToolError), "{body}");
        assert!(
            message(&output).contains(expect),
            "{body}: {}",
            message(&output)
        );
    }
}

#[test]
fn a_child_that_cannot_start_is_tool_error() {
    let dir = workspace();
    let output = run_with(dir.path(), &dir.path().join("no-such-binary"), "a.png");
    assert_eq!(code(&output), Some(ErrorCode::ToolError));
    assert!(
        message(&output).contains("could not start"),
        "{}",
        message(&output)
    );
}

#[test]
fn without_the_wiring_an_image_is_tool_error() {
    let dir = workspace();
    let Value::Object(arguments) = json!({"path": "a.png"}) else {
        panic!("an object");
    };
    let output = Files::new(dir.path().to_path_buf()).read().run(
        &arguments,
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(code(&output), Some(ErrorCode::ToolError));
    assert!(
        message(&output).contains("not configured"),
        "{}",
        message(&output)
    );
}

#[test]
fn offset_and_limit_are_ignored_for_an_image() {
    let dir = workspace();
    let fiber = stub(
        dir.path(),
        r#"printf '{"file":"%s.png","mime_type":"image/png","width":2,"height":2}\n' "$4""#,
    );
    let files =
        Files::new(dir.path().to_path_buf()).with_images(fiber, dir.path().join("artifacts"));
    let Value::Object(arguments) = json!({"path": "a.png", "offset": 99, "limit": 1}) else {
        panic!("an object");
    };
    let output = files
        .read()
        .run(&arguments, &CancelToken::new(), &Recorder::default());
    assert_eq!(code(&output), None);
    assert_eq!(output.content.len(), 2);
}

#[test]
fn a_cancelled_call_spawns_no_child() {
    let dir = workspace();
    let fiber = stub(dir.path(), r#"touch "$(dirname "$0")/ran""#);
    let files =
        Files::new(dir.path().to_path_buf()).with_images(fiber, dir.path().join("artifacts"));
    let Value::Object(arguments) = json!({"path": "a.png"}) else {
        panic!("an object");
    };
    let cancel = CancelToken::new();
    cancel.cancel();
    let output = files.read().run(&arguments, &cancel, &Recorder::default());
    assert_eq!(code(&output), None);
    assert!(!dir.path().join("ran").exists());
}

#[test]
fn a_pdf_and_a_binary_file_never_reach_the_child() {
    let dir = workspace();
    let fiber = stub(dir.path(), r#"touch "$(dirname "$0")/ran""#);
    fs::write(dir.path().join("a.pdf"), b"%PDF-1.4").unwrap();
    fs::write(dir.path().join("a.bin"), b"hello\0world").unwrap();
    let pdf = run_with(dir.path(), &fiber, "a.pdf");
    assert_eq!(code(&pdf), Some(ErrorCode::UnsupportedFile));
    assert!(
        message(&pdf).contains("PDFs are not read yet"),
        "{}",
        message(&pdf)
    );
    let binary = run_with(dir.path(), &fiber, "a.bin");
    assert_eq!(code(&binary), Some(ErrorCode::UnsupportedFile));
    assert!(
        message(&binary).contains("binary data"),
        "{}",
        message(&binary)
    );
    assert!(!dir.path().join("ran").exists());
}

#[test]
fn a_directory_and_a_fifo_never_reach_the_child() {
    let dir = workspace();
    let fiber = stub(dir.path(), r#"touch "$(dirname "$0")/ran""#);
    fs::create_dir(dir.path().join("pics.png")).unwrap();
    let output = run_with(dir.path(), &fiber, "pics.png");
    assert_eq!(code(&output), Some(ErrorCode::UnsupportedFile));
    assert!(!dir.path().join("ran").exists());
}

/// How long the test waits for the child to start, and for the call to end
/// after the cancel. A wait that reaches it fails the test.
const LIMIT: std::time::Duration = std::time::Duration::from_secs(20);

#[test]
fn a_cancel_after_the_child_started_stops_and_reaps_it() {
    cancel_stops_and_reaps("");
}

#[test]
fn a_cancel_after_one_pipe_closed_stops_and_reaps_the_child() {
    cancel_stops_and_reaps("exec 2>&-\n");
}

#[test]
fn a_cancel_after_both_pipes_closed_stops_and_reaps_the_child() {
    cancel_stops_and_reaps("exec 1>&- 2>&-\n");
}

/// Starts a child that runs `prelude`, then sleeps, cancels once it is up,
/// and checks the call ended and the child is gone.
fn cancel_stops_and_reaps(prelude: &str) {
    let dir = workspace();
    let ready = fakes::children::Ready::new(dir.path());
    // `exec` keeps the pid: the one process the call must stop.
    let fiber = stub(
        dir.path(),
        &format!(
            "{prelude}echo $$ > '{}'\nexec sleep 3600",
            ready.path().display()
        ),
    );
    let files =
        Files::new(dir.path().to_path_buf()).with_images(fiber, dir.path().join("artifacts"));
    let Value::Object(arguments) = json!({"path": "a.png"}) else {
        panic!("an object");
    };
    let cancel = CancelToken::new();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let call_cancel = cancel.clone();
    let call = std::thread::spawn(move || {
        let output = files
            .read()
            .run(&arguments, &call_cancel, &Recorder::default());
        drop(done_tx.send(output));
    });
    let pid = ready.wait(LIMIT).first().copied().unwrap();
    assert!(
        done_rx.try_recv().is_err(),
        "the call ended before the cancel"
    );
    cancel.cancel();
    let output = done_rx.recv_timeout(LIMIT).expect("the call ends");
    call.join().unwrap();
    assert_eq!(code(&output), None);
    assert_eq!(message(&output), "Cancelled and stopped.\n");
    // Reaped: the process no longer exists.
    let alive = std::process::Command::new("ps")
        .args(["-p", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(!alive, "the child {pid} still exists");
}

#[test]
fn a_child_that_fills_both_pipes_does_not_deadlock() {
    let dir = workspace();
    // 300 KiB on each pipe, past any pipe buffer, stderr first.
    let fiber = stub(
        dir.path(),
        r#"head -c 300000 /dev/zero | tr '\0' x >&2
head -c 300000 /dev/zero | tr '\0' y
exit 3"#,
    );
    // On a thread, so a deadlock fails at the limit instead of hanging.
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let root = dir.path().to_path_buf();
    let call = std::thread::spawn(move || drop(done_tx.send(run_with(&root, &fiber, "a.png"))));
    let output = done_rx.recv_timeout(LIMIT).expect("the call ends");
    call.join().unwrap();
    assert_eq!(code(&output), Some(ErrorCode::ToolError));
    assert!(
        message(&output).contains("exited with status 3"),
        "{}",
        message(&output)
    );
}
