//! Tests beside [`super::read`]: the child's output and exit codes become a
//! tool result. The child is a `/bin/sh` script, so `tools` links no image
//! code.

use std::fs;
use std::path::{Path, PathBuf};

use contract::ErrorCode;
use contract::shapes::ContentPart;
use contract::tool::{Output, Tool};
use fakes::Deadline;
use fakes::{CancelToken, Recorder, TempDir};
use serde_json::{Value, json};

use crate::Files;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot really a png";

/// A stub `fiber` that runs `body` with `$2` the input, `$3` the artifacts
/// directory and `$4` the stem.
fn stub(dir: &Path, body: &str) -> PathBuf {
    fakes::script(dir, "fiber-stub", body)
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
        r#"cat >/dev/null
printf '{"file":"%s.png","mime_type":"image/png","width":80,"height":60}\n' "$4""#,
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
fn the_child_reads_stdin_into_the_artifacts_directory_under_a_fresh_stem() {
    let dir = workspace();
    let fiber = stub(
        dir.path(),
        r#"echo "$1 $2 $3 $4" > "$(dirname "$0")/argv"
mkdir -p "$3"
cat > "$3/$4.png"
printf '{"file":"%s.png","mime_type":"image/png","width":1,"height":1}\n' "$4""#,
    );
    let first = run_with(dir.path(), &fiber, "a.png");
    assert_eq!(code(&first), None);
    let argv = fs::read_to_string(dir.path().join("argv")).unwrap();
    let words: Vec<&str> = argv.split_whitespace().collect();
    assert_eq!(words.first(), Some(&"image"));
    assert_eq!(words.get(1), Some(&"/dev/stdin"));
    assert_eq!(words.get(2).copied(), dir.path().join("artifacts").to_str());
    let stem = words.get(3).expect("a stem");
    assert!(stem.starts_with("i_"), "{stem}");
    let stored = dir.path().join("artifacts").join(format!("{stem}.png"));
    assert_eq!(fs::read(&stored).unwrap(), PNG);
    // The same file read twice is two artifacts.
    let second = run_with(dir.path(), &fiber, "a.png");
    assert_ne!(first.content.get(1), second.content.get(1));
}

#[test]
fn a_read_image_reaches_the_child_once() {
    let dir = workspace();
    let fiber = stub(
        dir.path(),
        r#"mkdir -p "$3"
cat > "$3/$4.png"
echo ran >> "$(dirname "$0")/markers"
printf '{"file":"%s.png","mime_type":"image/png","width":1,"height":1}\n' "$4""#,
    );
    let output = run_with(dir.path(), &fiber, "a.png");
    assert_eq!(code(&output), None);
    let markers = fs::read_to_string(dir.path().join("markers")).unwrap();
    assert_eq!(markers.lines().count(), 1, "{markers}");
}

#[test]
fn exit_1_is_unsupported_file_with_the_childs_message() {
    let dir = workspace();
    let fiber = stub(
        dir.path(),
        "cat >/dev/null\necho '8000x7000 is 56000000 pixels; the limit is 50000000' >&2; exit 1",
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
        r#"cat >/dev/null
i=0; while [ $i -lt 2047 ]; do printf a >&2; i=$((i+1)); done; printf 'ééé' >&2; exit 1"#,
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
        r#"cat >/dev/null
i=0; while [ $i -lt 2048 ]; do printf a >&2; i=$((i+1)); done; exit 1"#,
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
        ("cat >/dev/null; echo disk full >&2; exit 3", "exited with status 3"),
        ("cat >/dev/null; exit 2", "exited with status 2"),
        ("cat >/dev/null; kill -9 $$", "killed by a signal"),
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
        ("cat >/dev/null; echo hello", "not JSON"),
        ("cat >/dev/null; echo '{\"file\":\"x.png\"}'", "without file"),
        (
            "cat >/dev/null; echo '{\"file\":\"x.png\",\"mime_type\":\"image/png\",\"width\":1,\"height\":1}'; echo more",
            "more than one line",
        ),
        ("cat >/dev/null", "not JSON"),
        (
            "cat >/dev/null; printf '{\"file\":\"x.png\",\"mime_type\":\"image/png\",\"width\":-1,\"height\":1}\\n'",
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
        r#"cat >/dev/null
printf '{"file":"%s.png","mime_type":"image/png","width":2,"height":2}\n' "$4""#,
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
fn a_binary_file_never_reaches_the_child() {
    let dir = workspace();
    let fiber = stub(dir.path(), r#"touch "$(dirname "$0")/ran""#);
    fs::write(dir.path().join("a.bin"), b"hello\0world").unwrap();
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
#[track_caller]
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
    let files = std::sync::Arc::new(
        Files::new(dir.path().to_path_buf()).with_images(fiber, dir.path().join("artifacts")),
    );
    let Value::Object(arguments) = json!({"path": "a.png"}) else {
        panic!("an object");
    };
    let cancel = CancelToken::new();
    let call_files = std::sync::Arc::clone(&files);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let call_cancel = cancel.clone();
    let call = std::thread::spawn(move || {
        let output = call_files
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
    let output = Deadline::after(LIMIT)
        .recv(&done_rx)
        .expect("the call ends");
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
    // A cancelled read took in no image: a `write` is still stale.
    assert_eq!(code(&write_png(&files)), Some(ErrorCode::StaleFile));
}

#[test]
fn a_child_that_fills_both_pipes_does_not_deadlock() {
    let dir = workspace();
    // 300 KiB on each pipe, past any pipe buffer, stderr first.
    let fiber = stub(
        dir.path(),
        r#"cat > /dev/null
head -c 300000 /dev/zero | tr '\0' x >&2
head -c 300000 /dev/zero | tr '\0' y
exit 3"#,
    );
    // On a thread, so a deadlock fails at the limit instead of hanging.
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let root = dir.path().to_path_buf();
    let call = std::thread::spawn(move || drop(done_tx.send(run_with(&root, &fiber, "a.png"))));
    let output = Deadline::after(LIMIT)
        .recv(&done_rx)
        .expect("the call ends");
    call.join().unwrap();
    assert_eq!(code(&output), Some(ErrorCode::ToolError));
    assert!(
        message(&output).contains("exited with status 3"),
        "{}",
        message(&output)
    );
}

const OK_CHILD: &str =
    r#"cat >/dev/null
printf '{"file":"%s.png","mime_type":"image/png","width":1,"height":1}\n' "$4""#;

/// A `write` of `a.png` through the same `Files` that read it.
fn write_png(files: &Files) -> Output {
    let Value::Object(arguments) = json!({"path": "a.png", "content": "new"}) else {
        panic!("an object");
    };
    files
        .write()
        .run(&arguments, &CancelToken::new(), &Recorder::default())
}

#[test]
fn a_read_image_is_seen_so_a_later_write_is_not_stale() {
    let dir = workspace();
    let fiber = stub(dir.path(), OK_CHILD);
    let files =
        Files::new(dir.path().to_path_buf()).with_images(fiber, dir.path().join("artifacts"));
    let Value::Object(arguments) = json!({"path": "a.png"}) else {
        panic!("an object");
    };
    let read = files
        .read()
        .run(&arguments, &CancelToken::new(), &Recorder::default());
    assert_eq!(code(&read), None);
    assert_eq!(code(&write_png(&files)), None);
}

#[test]
fn an_image_changed_after_the_read_is_stale_and_a_refused_read_sees_nothing() {
    let dir = workspace();
    let fiber = stub(dir.path(), OK_CHILD);
    let files =
        Files::new(dir.path().to_path_buf()).with_images(fiber, dir.path().join("artifacts"));
    let Value::Object(arguments) = json!({"path": "a.png"}) else {
        panic!("an object");
    };
    files
        .read()
        .run(&arguments, &CancelToken::new(), &Recorder::default());
    fs::write(
        dir.path().join("a.png"),
        [PNG, b" changed".as_slice()].concat(),
    )
    .unwrap();
    assert_eq!(code(&write_png(&files)), Some(ErrorCode::StaleFile));

    let refusing = workspace();
    let fiber = stub(refusing.path(), "cat >/dev/null\necho refused >&2\nexit 1");
    let files = Files::new(refusing.path().to_path_buf())
        .with_images(fiber, refusing.path().join("artifacts"));
    let failed = files
        .read()
        .run(&arguments, &CancelToken::new(), &Recorder::default());
    assert_eq!(code(&failed), Some(ErrorCode::UnsupportedFile));
    assert_eq!(code(&write_png(&files)), Some(ErrorCode::StaleFile));
}

// `process` tests: the bytes reach the child on its standard input.
// The child is a `/bin/sh` script, so `tools` links no image code.

use contract::images::{ImageError, Images as _};

use crate::image::ImageChild;

fn process_with(
    dir: &Path,
    fiber: &Path,
    bytes: &[u8],
    cancel: &CancelToken,
) -> Result<contract::provider::ImageRef, ImageError> {
    let child = ImageChild::new(fiber.to_path_buf(), dir.join("artifacts"));
    child.process(bytes, cancel)
}

#[test]
fn process_copies_stdin_to_artifacts_and_names_it() {
    let dir = workspace();
    let fiber = stub(
        dir.path(),
        r#"echo "$2" > "$(dirname "$0")/argv"
mkdir -p "$3"
cp "$2" "$3/$4.png"
printf '{"file":"%s.png","mime_type":"image/png","width":80,"height":60}\n' "$4""#,
    );
    let bytes = b"\x89PNG\r\n\x1a\nprocessed bytes";
    let got = process_with(dir.path(), &fiber, bytes, &CancelToken::new()).unwrap();
    assert_eq!(
        (got.mime_type.as_str(), got.width, got.height),
        ("image/png", 80, 60)
    );
    assert!(
        got.path.starts_with("artifacts/i_") && got.path.ends_with(".png"),
        "{}",
        got.path
    );
    assert_eq!(got.path.len(), "artifacts/i_".len() + 16 + ".png".len());
    let argv = fs::read_to_string(dir.path().join("argv")).unwrap();
    assert!(argv.contains("/dev/stdin"), "{argv}");
    let file = dir.path().join(&got.path);
    assert_eq!(fs::read(&file).unwrap(), bytes);
}

#[test]
fn two_process_calls_get_different_stems() {
    let dir = workspace();
    let fiber = stub(
        dir.path(),
        r#"cat >/dev/null
mkdir -p "$3"
: > "$3/$4.png"
printf '{"file":"%s.png","mime_type":"image/png","width":1,"height":1}\n' "$4""#,
    );
    let first = process_with(dir.path(), &fiber, b"one", &CancelToken::new()).unwrap();
    let second = process_with(dir.path(), &fiber, b"two", &CancelToken::new()).unwrap();
    assert_ne!(first.path, second.path);
}

#[test]
fn process_exit_1_is_unreadable_with_the_childs_message() {
    let dir = workspace();
    let fiber = stub(
        dir.path(),
        "cat >/dev/null\necho '8000x7000 is 56000000 pixels; the limit is 50000000' >&2; exit 1",
    );
    let Err(ImageError::Unreadable(message)) =
        process_with(dir.path(), &fiber, b"bytes", &CancelToken::new())
    else {
        panic!("unreadable");
    };
    assert!(
        message.contains("8000x7000 is 56000000 pixels; the limit is 50000000"),
        "{message}"
    );
}

#[test]
fn process_exit_2_exit_3_and_a_signal_are_failed() {
    let dir = workspace();
    for (body, expect) in [
        ("cat >/dev/null; echo disk full >&2; exit 3", "exited with status 3"),
        ("cat >/dev/null; exit 2", "exited with status 2"),
        ("cat >/dev/null; kill -9 $$", "killed by a signal"),
    ] {
        let fiber = stub(dir.path(), body);
        let Err(ImageError::Failed(message)) =
            process_with(dir.path(), &fiber, b"bytes", &CancelToken::new())
        else {
            panic!("failed for {body}");
        };
        assert!(message.contains(expect), "{body}: {message}");
    }
}

#[test]
fn process_unparseable_extra_or_incomplete_output_is_failed() {
    let dir = workspace();
    for (body, expect) in [
        ("cat >/dev/null; echo hello", "not JSON"),
        ("cat >/dev/null; echo '{\"file\":\"x.png\"}'", "without file"),
        (
            "echo '{\"file\":\"x.png\",\"mime_type\":\"image/png\",\"width\":1,\"height\":1}'; echo more",
            "more than one line",
        ),
        ("cat >/dev/null", "not JSON"),
    ] {
        let fiber = stub(dir.path(), &format!("cat >/dev/null; {body}"));
        let Err(ImageError::Failed(message)) =
            process_with(dir.path(), &fiber, b"bytes", &CancelToken::new())
        else {
            panic!("failed for {body}");
        };
        assert!(message.contains(expect), "{body}: {message}");
    }
}

#[test]
fn process_with_a_missing_binary_is_failed() {
    let dir = workspace();
    let Err(ImageError::Failed(message)) = process_with(
        dir.path(),
        &dir.path().join("no-such-binary"),
        b"bytes",
        &CancelToken::new(),
    ) else {
        panic!("failed");
    };
    assert!(message.contains("could not start"), "{message}");
}

#[test]
fn process_cancelled_before_the_spawn_runs_no_child() {
    let dir = workspace();
    let fiber = stub(dir.path(), r#"touch "$(dirname "$0")/ran""#);
    let cancel = CancelToken::new();
    cancel.cancel();
    let result = process_with(dir.path(), &fiber, b"bytes", &cancel);
    assert_eq!(result, Err(ImageError::Cancelled));
    assert!(!dir.path().join("ran").exists());
}

/// How long a cancel test waits for the child to start, and for the call
/// to end after the cancel. A wait that reaches it fails the test.
const PROCESS_LIMIT: std::time::Duration = std::time::Duration::from_secs(20);

#[test]
fn process_cancel_after_the_child_started_stops_and_reaps_it() {
    let dir = workspace();
    let ready = fakes::children::Ready::new(dir.path());
    let fiber = stub(
        dir.path(),
        &format!("echo $$ > '{}'\nexec sleep 3600", ready.path().display()),
    );
    let cancel = CancelToken::new();
    let call_cancel = cancel.clone();
    let root = dir.path().to_path_buf();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let child = ImageChild::new(fiber, root.join("artifacts"));
        drop(done_tx.send(child.process(b"small", &call_cancel)));
    });
    let pid = ready.wait(PROCESS_LIMIT).first().copied().unwrap();
    assert!(
        done_rx.try_recv().is_err(),
        "the call ended before the cancel"
    );
    cancel.cancel();
    let result = Deadline::after(PROCESS_LIMIT)
        .recv(&done_rx)
        .expect("the call ends");
    assert_eq!(result, Err(ImageError::Cancelled));
    let alive = std::process::Command::new("ps")
        .args(["-p", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(!alive, "the child {pid} still exists");
}

#[test]
fn process_cancel_stops_a_stalled_child_while_the_write_is_blocked() {
    let dir = workspace();
    // A child that records its pid and then sleeps without reading
    // stdin, while 4 MiB is passed, so the write blocks on a full pipe.
    let ready = fakes::children::Ready::new(dir.path());
    let fiber = stub(
        dir.path(),
        &format!("echo $$ > '{}'\nexec sleep 3600", ready.path().display()),
    );
    let bytes = vec![b'x'; 4 * 1024 * 1024];
    let cancel = CancelToken::new();
    let call_cancel = cancel.clone();
    let root = dir.path().to_path_buf();
    let fiber_path = fiber.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let child = ImageChild::new(fiber_path, root.join("artifacts"));
        drop(done_tx.send(child.process(&bytes, &call_cancel)));
    });
    let pid = ready.wait(PROCESS_LIMIT).first().copied().unwrap();
    cancel.cancel();
    let result = Deadline::after(PROCESS_LIMIT)
        .recv(&done_rx)
        .expect("the call ends within the deadline");
    assert_eq!(result, Err(ImageError::Cancelled));
    let alive = std::process::Command::new("ps")
        .args(["-p", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(!alive, "the child {pid} still exists");
}

fn bad_body_for(label: &str) -> String {
    // Each body builds its filename from this invocation's `$4`, the stem
    // the call minted, so a rejection names the shape, not a stale stem.
    match label {
        "../outside.png" => {
            r#"printf '{"file":"../outside.png","mime_type":"image/png","width":1,"height":1}\n'"#
                .to_owned()
        }
        "other.png" => {
            r#"printf '{"file":"other.png","mime_type":"image/png","width":1,"height":1}\n'"#
                .to_owned()
        }
        "sub/stem.png" => {
            r#"printf '{"file":"sub/%s.png","mime_type":"image/png","width":1,"height":1}\n' "$4""#
                .to_owned()
        }
        "stem.png.x" => {
            r#"printf '{"file":"%s.png.x","mime_type":"image/png","width":1,"height":1}\n' "$4""#
                .to_owned()
        }
        "stem." => r#"printf '{"file":"%s.","mime_type":"image/png","width":1,"height":1}\n' "$4""#
            .to_owned(),
        "stem" => r#"printf '{"file":"%s","mime_type":"image/png","width":1,"height":1}\n' "$4""#
            .to_owned(),
        _ => panic!("a bad file case: {label}"),
    }
}

#[test]
fn process_bad_file_names_are_failed_and_read_calls_them_tool_error() {
    let dir = workspace();
    let labels = [
        "../outside.png",
        "sub/stem.png",
        "other.png",
        "stem.png.x",
        "stem.",
        "stem",
    ];
    for label in labels {
        let fiber = stub(
            dir.path(),
            &format!("cat >/dev/null\n{}", bad_body_for(label)),
        );
        let Err(ImageError::Failed(failure)) =
            process_with(dir.path(), &fiber, b"bytes", &CancelToken::new())
        else {
            panic!("failed for {label}");
        };
        assert!(
            failure.contains("other than the one asked for"),
            "{label}: {failure}"
        );
        let output = run_with(dir.path(), &fiber, "a.png");
        assert_eq!(code(&output), Some(ErrorCode::ToolError), "{label}");
        assert!(
            message(&output).contains("other than the one asked for"),
            "{label}: {}",
            message(&output)
        );
    }
    for ext in ["png", "jpg"] {
        let fiber = stub(
            dir.path(),
            &format!(
                r#"cat >/dev/null
mkdir -p "$3"
: > "$3/$4.{ext}"
printf '{{"file":"%s.{ext}","mime_type":"image/png","width":1,"height":1}}\n' "$4""#
            ),
        );
        let result = process_with(dir.path(), &fiber, b"bytes", &CancelToken::new());
        assert!(result.is_ok(), "{ext}: {result:?}");
    }
}

#[test]
fn process_child_that_exits_early_without_reading_does_not_hang() {
    let dir = workspace();
    let fiber = stub(dir.path(), "exit 0");
    let bytes = vec![b'y'; 1024 * 1024];
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let root = dir.path().to_path_buf();
    std::thread::spawn(move || {
        let child = ImageChild::new(fiber, root.join("artifacts"));
        drop(done_tx.send(child.process(&bytes, &CancelToken::new())));
    });
    let result = Deadline::after(PROCESS_LIMIT)
        .recv(&done_rx)
        .expect("the call ends without hanging");
    assert!(matches!(result, Err(ImageError::Failed(_))), "{result:?}");
}

#[test]
fn process_child_that_reads_stdin_then_fills_both_pipes_does_not_deadlock() {
    let dir = workspace();
    let fiber = stub(
        dir.path(),
        r#"cat > /dev/null
head -c 300000 /dev/zero | tr '\0' x >&2
head -c 300000 /dev/zero | tr '\0' y
exit 3"#,
    );
    let bytes = vec![b'z'; 64 * 1024];
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let root = dir.path().to_path_buf();
    std::thread::spawn(move || {
        let child = ImageChild::new(fiber, root.join("artifacts"));
        drop(done_tx.send(child.process(&bytes, &CancelToken::new())));
    });
    let result = Deadline::after(PROCESS_LIMIT)
        .recv(&done_rx)
        .expect("the call ends");
    let Err(ImageError::Failed(message)) = result else {
        panic!("failed: {result:?}");
    };
    assert!(message.contains("exited with status 3"), "{message}");
}
