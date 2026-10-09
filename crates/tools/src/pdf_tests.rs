//! Tests beside [`super::read`]: the `pages` grammar and the stub-driven
//! child call and render loop. The `fiber` child and `pdftoppm` are both
//! `/bin/sh` scripts, so `tools` links no PDF code.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use contract::ErrorCode;
use contract::clock::Wake;
use contract::shapes::ContentPart;
use contract::tool::{Cancel, Output, Tool};
use fakes::{CancelToken, Recorder, TempDir};
use serde_json::{Map, Value, json};

use super::page_range;
use super::{PDF_MAX_BYTES, pdf_over_cap};
use crate::Files;

/// How long a cancel test waits for the renderer to start, and for the call
/// to end after the cancel. A wait that reaches it fails the test.
const LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

const PDF: &[u8] = b"%PDF-1.4 a test pdf";

const IMAGE_BODY: &str = r#"cat >/dev/null
printf '{"file":"%s.png","mime_type":"image/png","width":80,"height":60}\n' "$4""#;

const PDF_TWO_PAGES: &str = r#"echo "$6" > "$(dirname "$0")/what"
printf '{"file":"%s.pdf","page_count":2,"total":2}\n' "$5""#;

/// A stub `fiber`: in PDF mode (`$2` is `pdf`) it runs `pdf_body` with `$3`
/// the input, `$4` the artifacts directory, `$5` the stem and `$6` the
/// request; otherwise it runs `image_body` with `$2` the input, `$3` the
/// artifacts directory and `$4` the stem.
fn fiber_stub(dir: &Path, pdf_body: &str, image_body: &str) -> PathBuf {
    fakes::script(
        dir,
        "fiber-stub",
        &format!("if [ \"$2\" = pdf ]; then\n{pdf_body}\nelse\n{image_body}\nfi"),
    )
}

/// A stub `pdftoppm` that appends its argv as one line to `ppm.log` and
/// writes `<root>.png`, where `<root>` is its last argument.
fn ppm_stub(dir: &Path, body: &str) -> PathBuf {
    fakes::script(dir, "ppm-stub", body)
}

const PPM_OK: &str = r#"echo "$@" >> "$(dirname "$0")/ppm.log"
for root; do :; done
mkdir -p "$(dirname "$root")"
printf 'fakepng' > "$root.png""#;

fn workspace(name: &str) -> TempDir {
    let dir = TempDir::new(name);
    fs::write(dir.path().join("a.pdf"), PDF).unwrap();
    dir
}

fn arguments(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            panic!("arguments are an object")
        }
    }
}

fn run_with(dir: &Path, fiber: &Path, ppm: &Path, value: Value, cancel: &dyn Cancel) -> Output {
    let files = Files::new(dir.to_path_buf())
        .with_renderer(ppm.to_path_buf())
        .with_images(fiber.to_path_buf(), dir.join("artifacts"));
    files
        .read()
        .run(&arguments(value), cancel, &Recorder::default())
}

fn run(dir: &Path, fiber: &Path, ppm: &Path, value: Value) -> Output {
    run_with(dir, fiber, ppm, value, &CancelToken::new())
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

fn pdf_part(output: &Output) -> Option<(String, u32, Option<usize>)> {
    output.content.iter().find_map(|part| match part {
        ContentPart::Pdf(pdf) => Some((
            pdf.path().to_owned(),
            pdf.page_count(),
            pdf.pages().map(|pages| pages.len()),
        )),
        ContentPart::Text { .. } | ContentPart::Image { .. } | ContentPart::Unknown => None,
    })
}

fn pages_argument(text: &str) -> Map<String, Value> {
    arguments(json!({"path": "a.pdf", "pages": text}))
}

#[test]
fn the_pdf_cap_fails_over_not_at() {
    for (len, over) in [
        (0, false),
        (PDF_MAX_BYTES - 1, false),
        (PDF_MAX_BYTES, false),
        (PDF_MAX_BYTES + 1, true),
    ] {
        assert_eq!(pdf_over_cap(len), over, "{len}");
    }
    assert_eq!(PDF_MAX_BYTES, 67_108_864);
}

/// A sparse PDF of `len` bytes: PDF magic up front, zeros after. Instant
/// to make at any size, and reading it touches no clock.
fn sparse_pdf(path: &Path, len: u64) {
    fs::write(path, b"%PDF-1.4 sparse").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_len(len)
        .unwrap();
}

#[test]
fn a_pdf_one_byte_over_the_cap_never_reaches_the_child() {
    let dir = TempDir::new("fiber-pdf-cap-over");
    sparse_pdf(&dir.path().join("big.pdf"), PDF_MAX_BYTES + 1);
    let fiber = fiber_stub(dir.path(), r#"touch "$(dirname "$0")/ran""#, IMAGE_BODY);
    let ppm = ppm_stub(dir.path(), PPM_OK);
    let output = run(dir.path(), &fiber, &ppm, json!({"path": "big.pdf"}));
    assert_eq!(code(&output), Some(ErrorCode::UnsupportedFile));
    let text = message(&output);
    assert!(text.contains("64 MiB"), "{text}");
    assert!(text.contains(&PDF_MAX_BYTES.to_string()), "{text}");
    assert!(text.contains(&(PDF_MAX_BYTES + 1).to_string()), "{text}");
    assert!(!dir.path().join("ran").exists());
}

#[test]
fn a_pdf_exactly_at_the_cap_reaches_the_child() {
    let dir = TempDir::new("fiber-pdf-cap-exact");
    sparse_pdf(&dir.path().join("edge.pdf"), PDF_MAX_BYTES);
    let fiber = fiber_stub(
        dir.path(),
        r#"touch "$(dirname "$0")/ran"
printf '{"file":"%s.pdf","page_count":2,"total":2}\n' "$5""#,
        IMAGE_BODY,
    );
    let ppm = ppm_stub(dir.path(), PPM_OK);
    let output = run(dir.path(), &fiber, &ppm, json!({"path": "edge.pdf"}));
    assert!(
        dir.path().join("ran").exists(),
        "the child was called: {}",
        message(&output)
    );
}

#[test]
fn page_ranges_accept_a_page_and_a_range_up_to_20() {
    for (text, first, last) in [("3", 3, 3), ("1-20", 1, 20), ("5-5", 5, 5)] {
        let range = page_range(&pages_argument(text)).unwrap().unwrap();
        assert_eq!((range.first, range.last), (first, last), "{text}");
    }
    assert!(
        page_range(&arguments(json!({"path": "a.pdf"})))
            .unwrap()
            .is_none()
    );
}

#[test]
fn page_ranges_refuse_anything_else() {
    for text in [
        "1-21",
        "0-3",
        "5-3",
        "",
        "1-",
        "-3",
        "1-2-3",
        " 1-2",
        "1-2 ",
        "a",
        "99999999999",
        "4294967296-4294967296",
        "+1-3",
        "1-+3",
        "+3",
    ] {
        let error = page_range(&pages_argument(text)).unwrap_err();
        if text == "1-21" {
            assert_eq!(
                error, "`pages` 1-21 names 21 pages; a request takes at most 20.",
                "{text}"
            );
        } else {
            assert_eq!(
                error, "`pages` must be a page or a range such as `3` or `1-5`, counted from 1.",
                "{text}"
            );
        }
    }
    let error = page_range(&arguments(json!({"path": "a.pdf", "pages": 3}))).unwrap_err();
    assert_eq!(error, "`pages` must be a string.");
}

#[test]
fn the_whole_path_renders_two_pages_in_order() {
    let dir = workspace("fiber-pdf-whole");
    let fiber = fiber_stub(dir.path(), PDF_TWO_PAGES, IMAGE_BODY);
    let ppm = ppm_stub(dir.path(), PPM_OK);
    let output = run(dir.path(), &fiber, &ppm, json!({"path": "a.pdf"}));
    assert_eq!(code(&output), None);
    assert_eq!(message(&output), "PDF: 2 pages.\n");
    let Some((path, count, pages)) = pdf_part(&output) else {
        panic!("a PDF part: {:?}", output.content);
    };
    assert!(
        path.starts_with("artifacts/p_") && path.ends_with(".pdf"),
        "{path}"
    );
    assert_eq!((count, pages), (2, Some(2)));
    // The child gets `whole=10`.
    assert_eq!(
        fs::read_to_string(dir.path().join("what")).unwrap().trim(),
        "whole=10"
    );
}

#[test]
fn a_read_pdf_is_seen_so_a_later_write_is_not_stale() {
    let dir = workspace("fiber-pdf-seen");
    let fiber = fiber_stub(dir.path(), PDF_TWO_PAGES, IMAGE_BODY);
    let ppm = ppm_stub(dir.path(), PPM_OK);
    let files = Files::new(dir.path().to_path_buf())
        .with_renderer(ppm.clone())
        .with_images(fiber.clone(), dir.path().join("artifacts"));
    let output = files.read().run(
        &arguments(json!({"path": "a.pdf"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(code(&output), None);
    let Value::Object(write) = json!({"path": "a.pdf", "content": "new\n"}) else {
        panic!("an object");
    };
    let written = files
        .write()
        .run(&write, &CancelToken::new(), &Recorder::default());
    assert_eq!(code(&written), None);
}

#[test]
fn a_range_read_names_the_pages_and_the_total() {
    let dir = workspace("fiber-pdf-range");
    let fiber = fiber_stub(
        dir.path(),
        r#"printf '{"file":"%s.pdf","page_count":2,"total":30}\n' "$5""#,
        IMAGE_BODY,
    );
    let ppm = ppm_stub(dir.path(), PPM_OK);
    let output = run(
        dir.path(),
        &fiber,
        &ppm,
        json!({"path": "a.pdf", "pages": "2-3"}),
    );
    assert_eq!(code(&output), None);
    assert_eq!(message(&output), "PDF: pages 2-3 of 30.\n");
    assert_eq!(pdf_part(&output).map(|part| part.1), Some(2));
}

#[test]
fn eleven_pages_without_pages_names_the_cap() {
    let dir = workspace("fiber-pdf-eleven");
    let fiber = fiber_stub(dir.path(), r#"echo '{"total":11}'; exit 4"#, IMAGE_BODY);
    let ppm = ppm_stub(dir.path(), PPM_OK);
    let output = run(dir.path(), &fiber, &ppm, json!({"path": "a.pdf"}));
    assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    assert_eq!(
        message(&output),
        format!(
            "`{}` has 11 pages. A PDF of more than 10 pages needs `pages`, such as `1-5`; a request takes at most 20 pages.\n",
            fs::canonicalize(dir.path())
                .unwrap()
                .join("a.pdf")
                .display()
        )
    );
    assert!(pdf_part(&output).is_none());
}

#[test]
fn a_range_past_the_end_names_the_page_count() {
    let dir = workspace("fiber-pdf-past");
    let fiber = fiber_stub(dir.path(), r#"echo '{"total":11}'; exit 4"#, IMAGE_BODY);
    let ppm = ppm_stub(dir.path(), PPM_OK);
    let output = run(
        dir.path(),
        &fiber,
        &ppm,
        json!({"path": "a.pdf", "pages": "12-13"}),
    );
    assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    assert!(
        message(&output).contains("`pages` 12-13 is past the end"),
        "{}",
        message(&output)
    );
    assert!(
        message(&output).contains("has 11 pages"),
        "{}",
        message(&output)
    );
}

#[test]
fn an_unreadable_pdf_is_unsupported_file_with_the_childs_message() {
    let dir = workspace("fiber-pdf-unreadable");
    let fiber = fiber_stub(
        dir.path(),
        r#"echo 'failed to parse' >&2; exit 1"#,
        IMAGE_BODY,
    );
    let ppm = ppm_stub(dir.path(), PPM_OK);
    let output = run(dir.path(), &fiber, &ppm, json!({"path": "a.pdf"}));
    assert_eq!(code(&output), Some(ErrorCode::UnsupportedFile));
    assert!(
        message(&output).contains("cannot be read as a PDF: failed to parse"),
        "{}",
        message(&output)
    );
}

#[test]
fn pages_on_a_text_a_png_and_a_directory_are_invalid_arguments() {
    let dir = TempDir::new("fiber-pdf-r9");
    fs::write(dir.path().join("a.txt"), "hi\n").unwrap();
    fs::write(dir.path().join("a.png"), b"\x89PNG\r\n\x1a\nxxxx").unwrap();
    fs::create_dir(dir.path().join("sub")).unwrap();
    let fiber = fiber_stub(dir.path(), r#"touch "$(dirname "$0")/ran""#, IMAGE_BODY);
    let ppm = ppm_stub(dir.path(), PPM_OK);
    for path in ["a.txt", "a.png", "sub"] {
        let output = run(
            dir.path(),
            &fiber,
            &ppm,
            json!({"path": path, "pages": "1-2"}),
        );
        assert_eq!(code(&output), Some(ErrorCode::InvalidArguments), "{path}");
        assert!(
            message(&output).contains("`pages` applies only to a PDF"),
            "{path}: {}",
            message(&output)
        );
    }
    assert!(!dir.path().join("ran").exists());
}

#[test]
fn pages_on_a_missing_path_is_not_found_and_a_bad_range_wins() {
    let dir = TempDir::new("fiber-pdf-r9-missing");
    let fiber = fiber_stub(dir.path(), PDF_TWO_PAGES, IMAGE_BODY);
    let ppm = ppm_stub(dir.path(), PPM_OK);
    let missing = run(
        dir.path(),
        &fiber,
        &ppm,
        json!({"path": "nope.pdf", "pages": "1-2"}),
    );
    assert_eq!(code(&missing), Some(ErrorCode::NotFound));
    let grammar = run(
        dir.path(),
        &fiber,
        &ppm,
        json!({"path": "nope.pdf", "pages": "bogus"}),
    );
    assert_eq!(code(&grammar), Some(ErrorCode::InvalidArguments));
}

#[test]
fn a_missing_renderer_keeps_the_pdf_and_names_poppler() {
    let dir = workspace("fiber-pdf-norenderer");
    let fiber = fiber_stub(dir.path(), PDF_TWO_PAGES, IMAGE_BODY);
    let missing = dir.path().join("no-such-ppmtoppm");
    let output = run(dir.path(), &fiber, &missing, json!({"path": "a.pdf"}));
    assert_eq!(code(&output), None);
    assert_eq!(
        message(&output),
        "PDF: 2 pages.\nThe pages could not be rendered as images: pdftoppm is not installed. It comes with poppler (poppler-utils on Debian and Ubuntu, brew install poppler on macOS).\n"
    );
    assert_eq!(pdf_part(&output).map(|part| part.2), Some(None));
}

#[test]
fn a_renderer_that_cannot_start_for_another_reason_names_the_start() {
    let dir = workspace("fiber-pdf-ppmperm");
    let fiber = fiber_stub(dir.path(), PDF_TWO_PAGES, IMAGE_BODY);
    // A directory is not executable: the spawn fails with a kind other
    // than `NotFound`.
    let output = run(dir.path(), &fiber, dir.path(), json!({"path": "a.pdf"}));
    assert_eq!(code(&output), None);
    assert!(
        message(&output).contains("pdftoppm failed to start"),
        "{}",
        message(&output)
    );
    assert_eq!(pdf_part(&output).map(|part| part.2), Some(None));
}

#[test]
fn a_renderer_killed_by_a_signal_keeps_the_pdf_with_the_reason() {
    let dir = workspace("fiber-pdf-ppmsignal");
    let fiber = fiber_stub(dir.path(), PDF_TWO_PAGES, IMAGE_BODY);
    let ppm = ppm_stub(dir.path(), "kill -9 $$");
    let output = run(dir.path(), &fiber, &ppm, json!({"path": "a.pdf"}));
    assert_eq!(code(&output), None);
    assert!(
        message(&output).contains("killed by a signal"),
        "{}",
        message(&output)
    );
    assert_eq!(pdf_part(&output).map(|part| part.2), Some(None));
}

#[test]
fn a_renderer_that_exits_1_keeps_the_pdf_with_the_reason() {
    let dir = workspace("fiber-pdf-ppmfail");
    let fiber = fiber_stub(dir.path(), PDF_TWO_PAGES, IMAGE_BODY);
    let ppm = ppm_stub(dir.path(), "echo boom >&2; exit 1");
    let output = run(dir.path(), &fiber, &ppm, json!({"path": "a.pdf"}));
    assert_eq!(code(&output), None);
    assert!(
        message(&output).starts_with("PDF: 2 pages.\nThe pages could not be rendered as images: "),
        "{}",
        message(&output)
    );
    assert!(
        message(&output).contains("status 1"),
        "{}",
        message(&output)
    );
    assert_eq!(pdf_part(&output).map(|part| part.2), Some(None));
}

#[test]
fn an_image_child_that_refuses_the_second_page_gives_no_pages() {
    let dir = workspace("fiber-pdf-pagerefused");
    let fiber = fiber_stub(
        dir.path(),
        PDF_TWO_PAGES,
        r#"cat >/dev/null
count=$(ls "$(dirname "$0")" | grep -c '^page-')
touch "$(dirname "$0")/page-$count"
if [ "$count" = 1 ]; then echo refused >&2; exit 1; fi
printf '{"file":"%s.png","mime_type":"image/png","width":80,"height":60}\n' "$4""#,
    );
    let ppm = ppm_stub(dir.path(), PPM_OK);
    let output = run(dir.path(), &fiber, &ppm, json!({"path": "a.pdf"}));
    assert_eq!(code(&output), None);
    // Never one page: the failure after one processed page gives `None`.
    assert_eq!(pdf_part(&output).map(|part| part.2), Some(None));
    assert!(
        message(&output).contains("The pages could not be rendered as images: "),
        "{}",
        message(&output)
    );
}

#[test]
fn the_renderer_gets_one_argv_per_page() {
    let dir = workspace("fiber-pdf-argv");
    let fiber = fiber_stub(dir.path(), PDF_TWO_PAGES, IMAGE_BODY);
    let ppm = ppm_stub(dir.path(), PPM_OK);
    let output = run(dir.path(), &fiber, &ppm, json!({"path": "a.pdf"}));
    assert_eq!(code(&output), None);
    let Some((path, _, _)) = pdf_part(&output) else {
        panic!("a PDF part");
    };
    let stem = path
        .strip_prefix("artifacts/")
        .unwrap()
        .strip_suffix(".pdf")
        .unwrap();
    let log = fs::read_to_string(dir.path().join("ppm.log")).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2);
    for (index, line) in lines.iter().enumerate() {
        let n = index + 1;
        assert_eq!(
            *line,
            format!(
                "-png -scale-to 2000 -f {n} -l {n} -singlefile {} {}",
                dir.path()
                    .join("artifacts")
                    .join(format!("{stem}.pdf"))
                    .display(),
                dir.path()
                    .join("artifacts")
                    .join(format!("{stem}-{n}"))
                    .display()
            )
        );
    }
}

#[test]
fn no_raw_page_file_is_left_after_success_or_failure() {
    let dir = workspace("fiber-pdf-noraw");
    let fiber = fiber_stub(dir.path(), PDF_TWO_PAGES, IMAGE_BODY);
    let ppm = ppm_stub(dir.path(), PPM_OK);
    let output = run(dir.path(), &fiber, &ppm, json!({"path": "a.pdf"}));
    assert_eq!(code(&output), None);
    let artifacts = dir.path().join("artifacts");
    let raws: Vec<_> = fs::read_dir(&artifacts)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    entry.path().extension().is_some_and(|ext| ext == "png")
                        && entry
                            .path()
                            .file_stem()
                            .is_some_and(|stem| stem.to_string_lossy().contains('-'))
                })
                .collect()
        })
        .unwrap_or_default();
    assert!(raws.is_empty(), "{raws:?}");

    let failing = workspace("fiber-pdf-noraw-fail");
    let fiber_fail = fiber_stub(dir.path(), PDF_TWO_PAGES, IMAGE_BODY);
    let ppm_fail = ppm_stub(failing.path(), "exit 1");
    let output = run(
        failing.path(),
        &fiber_fail,
        &ppm_fail,
        json!({"path": "a.pdf"}),
    );
    assert_eq!(code(&output), None);
    if failing.path().join("artifacts").exists() {
        let left: Vec<_> = fs::read_dir(failing.path().join("artifacts"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }
}

/// A cancel fired by another thread once the first page rendered.
struct Flag(Arc<AtomicBool>);

impl Cancel for Flag {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

#[test]
fn a_cancel_between_pages_stops_with_no_part_and_one_render() {
    let dir = workspace("fiber-pdf-cancel");
    // The renderer writes one line to the FIFO as its first action; the
    // reader is open before the call runs, so the line is never lost.
    let ready = fakes::children::Ready::new(dir.path());
    let fiber = fiber_stub(
        dir.path(),
        PDF_TWO_PAGES,
        r#"cat >/dev/null
exec sleep 3600"#,
    );
    // The log line comes before the FIFO line: when the wait below
    // returns, the render count is already on disk. Either order still
    // cancels the call with no part.
    let ppm = ppm_stub(
        dir.path(),
        &format!(
            r#"echo "$@" >> "$(dirname "$0")/ppm.log"
echo 1 > '{}'
for root; do :; done
mkdir -p "$(dirname "$root")"
printf 'fakepng' > "$root.png""#,
            ready.path().display()
        ),
    );
    let flag = Arc::new(AtomicBool::new(false));
    let cancel = Flag(Arc::clone(&flag));
    let root = dir.path().to_path_buf();
    let fiber_path = fiber.clone();
    let ppm_path = ppm.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(done_tx.send(run_with(
            &root,
            &fiber_path,
            &ppm_path,
            json!({"path": "a.pdf"}),
            &cancel,
        )));
    });
    ready.wait(LIMIT);
    assert!(
        done_rx.try_recv().is_err(),
        "the call ended before the cancel"
    );
    flag.store(true, Ordering::SeqCst);
    let output = done_rx.recv_timeout(LIMIT).expect("the call ends");
    assert_eq!(code(&output), None);
    assert_eq!(message(&output), "Cancelled and stopped.\n");
    assert!(pdf_part(&output).is_none());
    let log = fs::read_to_string(dir.path().join("ppm.log")).unwrap();
    assert_eq!(log.lines().count(), 1, "{log}");
    // The image child never answered, so nothing is referenced; no raw file.
    if dir.path().join("artifacts").exists() {
        let left: Vec<_> = fs::read_dir(dir.path().join("artifacts"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }
}

#[test]
fn offset_and_limit_are_ignored_for_a_pdf() {
    let dir = workspace("fiber-pdf-offset");
    let fiber = fiber_stub(dir.path(), PDF_TWO_PAGES, IMAGE_BODY);
    let ppm = ppm_stub(dir.path(), PPM_OK);
    let output = run(
        dir.path(),
        &fiber,
        &ppm,
        json!({"path": "a.pdf", "offset": 99, "limit": 1}),
    );
    assert_eq!(code(&output), None);
    assert_eq!(output.content.len(), 2);
}

#[test]
fn a_child_that_reports_zero_pages_is_tool_error() {
    let dir = workspace("fiber-pdf-zeropages");
    let fiber = fiber_stub(
        dir.path(),
        r#"printf '{"file":"%s.pdf","page_count":0,"total":2}\n' "$5""#,
        IMAGE_BODY,
    );
    let ppm = ppm_stub(dir.path(), PPM_OK);
    let output = run(dir.path(), &fiber, &ppm, json!({"path": "a.pdf"}));
    assert_eq!(code(&output), Some(ErrorCode::ToolError));
    assert!(
        message(&output).contains("page count of zero"),
        "{}",
        message(&output)
    );
}

#[test]
fn a_renderer_that_writes_no_file_keeps_the_pdf_with_the_reason() {
    let dir = workspace("fiber-pdf-nowrite");
    let fiber = fiber_stub(dir.path(), PDF_TWO_PAGES, IMAGE_BODY);
    let ppm = ppm_stub(dir.path(), "exit 0");
    let output = run(dir.path(), &fiber, &ppm, json!({"path": "a.pdf"}));
    assert_eq!(code(&output), None);
    assert!(
        message(&output).contains("wrote no file"),
        "{}",
        message(&output)
    );
    assert_eq!(pdf_part(&output).map(|part| part.2), Some(None));
}

#[test]
fn a_child_that_names_another_file_is_tool_error() {
    let dir = workspace("fiber-pdf-names");
    for (label, body) in [
        (
            "upper",
            r#"printf '{"file":"%s.PDF","page_count":2,"total":2}\n' "$5""#,
        ),
        (
            "png",
            r#"printf '{"file":"%s.png","page_count":2,"total":2}\n' "$5""#,
        ),
        (
            "other",
            r#"printf '{"file":"other.pdf","page_count":2,"total":2}\n'"#,
        ),
        (
            "suffix",
            r#"printf '{"file":"%s.pdf.x","page_count":2,"total":2}\n' "$5""#,
        ),
    ] {
        let fiber = fiber_stub(dir.path(), body, IMAGE_BODY);
        let ppm = ppm_stub(dir.path(), PPM_OK);
        let output = run(dir.path(), &fiber, &ppm, json!({"path": "a.pdf"}));
        assert_eq!(code(&output), Some(ErrorCode::ToolError), "{label}");
        assert!(
            message(&output).contains("other than the one asked for"),
            "{label}: {}",
            message(&output)
        );
    }
}
