//! Tests beside [`crate::image`] and [`crate::pdf`]: every `picture.run`
//! scope the image child owns, through the public drivers. The child is a
//! `/bin/sh` script, so `tools` links no image code; time comes from a
//! test-local scripted clock, never the process clock, and no test advances
//! a fake clock: the only waits are a real child's exit and a cancel, which
//! no clock can signal.

use std::collections::VecDeque;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use contract::ErrorCode;
use contract::diag::{Op, Timing};
use contract::images::{ImageError, Images};
use contract::provider::ImageRef;
use contract::shapes::ContentPart;
use contract::tool::{Output, Tool};
use fakes::{CancelToken, Deadline, Recorder, TempDir, Watchdog};
use serde_json::{Map, Value, json};

use crate::Files;
use crate::image::{ImageChild, test_spawn};

/// One bound per action and receive sequence: a wait that reaches it fails
/// the test instead of hanging it.
const STEP: Duration = Duration::from_secs(5);

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot really a png";

/// A stub `fiber` that runs `body` with `$2` the input, `$3` the artifacts
/// directory and `$4` the stem.
fn stub(dir: &Path, body: &str) -> PathBuf {
    fakes::script(dir, "fiber-stub", body)
}

/// The one JSON line a stored image has, built from this invocation's stem,
/// after taking stdin into the artifacts directory.
const IMAGE_ANSWER: &str = r#"mkdir -p "$3"
cat > "$3/$4.png"
printf '{"file":"%s.png","mime_type":"image/png","width":80,"height":60}\n' "$4""#;

/// A clock the test scripts: each read takes the next instant, defaulting
/// to the origin, and counts its reads. The origin is a fake clock's, so no
/// test reads the process clock.
struct Scripted {
    origin: Instant,
    next: Mutex<VecDeque<Instant>>,
    reads: AtomicUsize,
}

impl Scripted {
    fn plain() -> Arc<Self> {
        Arc::new(Self {
            origin: fakes::clock::FakeClock::new().origin(),
            next: Mutex::new(VecDeque::new()),
            reads: AtomicUsize::new(0),
        })
    }

    fn origin(&self) -> Instant {
        self.origin
    }

    fn script(&self, instants: Vec<Instant>) {
        *lock(&self.next) = instants.into();
    }

    fn now(&self) -> Instant {
        self.reads.fetch_add(1, Ordering::SeqCst);
        lock(&self.next).pop_front().unwrap_or(self.origin)
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One closed `picture.run` scope: the operation the invocation passed the
/// timing, and the whole milliseconds its guard held. No path, argument or
/// content travels with it: the tests pin that below by asserting the
/// record carries only the operation and the milliseconds.
#[derive(Debug)]
struct Record {
    op: Op,
    tool: Option<String>,
    started: Option<Instant>,
    ms: u64,
}

/// A test-only `Timing`: each call counts itself and, unless declining,
/// holds one active guard whose drop closes it with one record.
struct Probe {
    inner: Arc<ProbeInner>,
}

struct ProbeInner {
    calls: AtomicUsize,
    active: AtomicUsize,
    clock: Arc<Scripted>,
    decline: bool,
    records: mpsc::Sender<Record>,
}

struct Scope {
    inner: Arc<ProbeInner>,
    start: Instant,
    op: Op,
    tool: Option<String>,
    started: Option<Instant>,
}

impl Probe {
    fn recording(clock: Arc<Scripted>, records: mpsc::Sender<Record>) -> Self {
        Self {
            inner: Arc::new(ProbeInner {
                calls: AtomicUsize::new(0),
                active: AtomicUsize::new(0),
                clock,
                decline: false,
                records,
            }),
        }
    }

    fn declining() -> Self {
        let (records, _) = mpsc::channel();
        Self {
            inner: Arc::new(ProbeInner {
                calls: AtomicUsize::new(0),
                active: AtomicUsize::new(0),
                clock: Scripted::plain(),
                decline: true,
                records,
            }),
        }
    }

    fn timing(&self) -> Timing {
        let inner = Arc::clone(&self.inner);
        Arc::new(
            move |op: Op, tool: Option<&str>, started: Option<Instant>| {
                inner.calls.fetch_add(1, Ordering::SeqCst);
                if inner.decline {
                    return None;
                }
                inner.active.fetch_add(1, Ordering::SeqCst);
                let scope = Scope {
                    inner: Arc::clone(&inner),
                    start: inner.clock.now(),
                    op,
                    tool: tool.map(str::to_owned),
                    started,
                };
                Some(Box::new(scope) as Box<dyn Send>)
            },
        )
    }

    fn calls(&self) -> usize {
        self.inner.calls.load(Ordering::SeqCst)
    }

    fn active(&self) -> usize {
        self.inner.active.load(Ordering::SeqCst)
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        let end = self.inner.clock.now();
        let ms = u64::try_from(end.saturating_duration_since(self.start).as_millis())
            .unwrap_or(u64::MAX);
        self.inner.active.fetch_sub(1, Ordering::SeqCst);
        let record = Record {
            op: self.op,
            tool: self.tool.clone(),
            started: self.started,
            ms,
        };
        match self.inner.records.send(record) {
            Ok(()) | Err(_) => {}
        }
    }
}

/// One guarded fixture: its unique directory, a matching watchdog armed
/// before any start, and spawn reporting for the group watchdog.
struct Fixture {
    dir: TempDir,
    _matching: Watchdog,
    tx: Option<mpsc::Sender<u32>>,
    rx: mpsc::Receiver<u32>,
}

impl Fixture {
    fn new(prefix: &str) -> Self {
        let dir = TempDir::new(prefix);
        let leaf = dir
            .path()
            .file_name()
            .expect("a leaf")
            .to_string_lossy()
            .into_owned();
        let matching = Watchdog::matching(&leaf);
        let (tx, rx) = mpsc::channel();
        Self {
            dir,
            _matching: matching,
            tx: Some(tx),
            rx,
        }
    }

    /// A sender for spawn reporting inside a blocking call's worker: the
    /// worker installs it there, so the child joins a new process group
    /// owned by the matching watchdog (`test_spawn` in `crate::image`).
    fn reporter(&self) -> mpsc::Sender<u32> {
        self.tx.as_ref().expect("the report sender").clone()
    }

    /// The next reported spawn's pid, bounded by the deadline.
    fn spawned(&self, deadline: Deadline) -> u32 {
        deadline.recv_or_fail(&self.rx, "a spawn report")
    }

    /// The reported spawn's group watchdog: the spawn joined a new group,
    /// so its pid is the group.
    fn watch(pid: u32) -> Watchdog {
        Watchdog::group(pid)
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// A recording probe and its records.
fn probe() -> (Probe, mpsc::Receiver<Record>) {
    let (tx, rx) = mpsc::channel();
    (Probe::recording(Scripted::plain(), tx), rx)
}

/// A timed child over `fiber`.
fn timed_child(fiber: &Path, dir: &Path, probe: &Probe) -> ImageChild {
    ImageChild::new(fiber.to_path_buf(), dir.join("artifacts")).with_timing(probe.timing())
}

/// One record, failing the test when the scope never closed.
fn closed(records: &mpsc::Receiver<Record>, deadline: Deadline) -> Record {
    deadline.recv_or_fail(records, "the closed scope")
}

/// No record is waiting: the last scope closed exactly once.
fn assert_single(records: &mpsc::Receiver<Record>, what: &str) {
    assert!(records.try_recv().is_err(), "{what}: one entry, one record");
}

/// Runs `read` of `path` on a detached worker and receives its output
/// within `deadline`, failing naming `what`. The worker installs spawn
/// reporting, so the child joins a new process group owned by the
/// fixture's matching watchdog; a stuck child fails the named wait,
/// never nextest's kill (`docs/testing.md`, "Waits and timeouts"). The
/// thread is never joined: expiry fails the test while the watchdog owns
/// the child.
#[track_caller]
fn read_bounded(
    files: Arc<Files>,
    path: String,
    pages: Option<String>,
    cancel: CancelToken,
    deadline: Deadline,
    report: mpsc::Sender<u32>,
    what: &'static str,
) -> Output {
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _hook = test_spawn::install(report);
        let output = read_with(&files, &path, pages.as_deref(), &cancel);
        drop(done_tx.send(output));
    });
    deadline.recv_or_fail(&done_rx, what)
}

/// Runs `process` of `bytes` on a detached worker and receives its result
/// within `deadline`, failing naming `what`, under the same ownership as
/// [`read_bounded`].
#[track_caller]
fn process_bounded(
    child: Arc<ImageChild>,
    bytes: Vec<u8>,
    cancel: CancelToken,
    deadline: Deadline,
    report: mpsc::Sender<u32>,
    what: &'static str,
) -> Result<ImageRef, ImageError> {
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _hook = test_spawn::install(report);
        let result = child.process(&bytes, &cancel);
        drop(done_tx.send(result));
    });
    deadline.recv_or_fail(&done_rx, what)
}

/// The call's arguments object.
fn arguments(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            panic!("arguments are an object")
        }
    }
}

/// Runs `read` of `path` through `files`, with `pages` for a PDF.
fn read_with(files: &Files, path: &str, pages: Option<&str>, cancel: &CancelToken) -> Output {
    let mut value = json!({"path": path});
    if let Some(pages) = pages {
        value["pages"] = Value::String(pages.to_owned());
    }
    files
        .read()
        .run(&arguments(value), cancel, &Recorder::default())
}

fn code(output: &Output) -> Option<ErrorCode> {
    output.error.as_ref().map(|error| error.code.clone())
}

fn message(output: &Output) -> String {
    match output.content.first() {
        Some(ContentPart::Text { text }) => text.clone(),
        _ => String::new(),
    }
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

/// File tools with a timed image child over `fiber`.
fn image_files(dir: &Path, fiber: &Path, probe: &Probe) -> Files {
    Files::new(dir.to_path_buf())
        .with_timing(probe.timing())
        .with_images(fiber.to_path_buf(), dir.join("artifacts"))
}

#[test]
fn files_timing_reaches_the_built_child() {
    let fixture = Fixture::new("fiber-picture-files");
    fs::write(fixture.path().join("a.png"), PNG).expect("the image");
    let fiber = stub(fixture.path(), IMAGE_ANSWER);
    let (probe, records) = probe();
    let files = image_files(fixture.path(), &fiber, &probe);
    let files = Arc::new(files);
    let deadline = Deadline::after(STEP);

    let output = read_bounded(
        Arc::clone(&files),
        "a.png".to_owned(),
        None,
        CancelToken::new(),
        deadline,
        fixture.reporter(),
        "the read",
    );

    assert_eq!(code(&output), None);
    assert_eq!(message(&output), "Image: 80x60 image/png.\n");
    // `resolve` canonicalises the workspace prefix, so the seen lookup
    // uses the canonical path too.
    let seen = fs::canonicalize(fixture.path().join("a.png")).expect("the path");
    assert!(
        files.seen_hash(&seen).is_some(),
        "the read marks the file seen for a later write"
    );
    assert_eq!(probe.calls(), 1);
    let record = closed(&records, deadline);
    assert_eq!((record.op, record.ms), (Op::PictureRun, 0));
    assert_single(&records, "the read");
}

#[test]
fn first_timing_wins_over_a_second_builder() {
    let fixture = Fixture::new("fiber-picture-first");
    fs::write(fixture.path().join("a.png"), PNG).expect("the image");
    let fiber = stub(fixture.path(), IMAGE_ANSWER);
    let (first, first_records) = probe();
    let (second, second_records) = probe();
    let files = Files::new(fixture.path().to_path_buf())
        .with_timing(first.timing())
        .with_timing(second.timing())
        .with_images(fiber, fixture.path().join("artifacts"));
    let files = Arc::new(files);

    let deadline = Deadline::after(STEP);
    let output = read_bounded(
        Arc::clone(&files),
        "a.png".to_owned(),
        None,
        CancelToken::new(),
        deadline,
        fixture.reporter(),
        "the read",
    );

    assert_eq!(code(&output), None);
    assert_eq!(first.calls(), 1);
    assert_eq!(second.calls(), 0, "the second installation never runs");
    closed(&first_records, deadline);
    assert_single(&first_records, "the read");
    assert!(
        second_records.try_recv().is_err(),
        "the second timing hears nothing"
    );
}

#[test]
fn timing_after_with_images_leaves_the_built_child_unchanged() {
    let fixture = Fixture::new("fiber-picture-late");
    fs::write(fixture.path().join("a.png"), PNG).expect("the image");
    let fiber = stub(fixture.path(), IMAGE_ANSWER);
    let (probe, records) = probe();
    let files = Files::new(fixture.path().to_path_buf())
        .with_images(fiber, fixture.path().join("artifacts"))
        .with_timing(probe.timing());
    let files = Arc::new(files);

    let deadline = Deadline::after(STEP);
    let output = read_bounded(
        Arc::clone(&files),
        "a.png".to_owned(),
        None,
        CancelToken::new(),
        deadline,
        fixture.reporter(),
        "the read",
    );

    assert_eq!(code(&output), None, "the read still works");
    assert_eq!(probe.calls(), 0, "the late timing never runs");
    assert!(
        records.try_recv().is_err(),
        "the built child keeps no timing"
    );
}

#[test]
fn without_timing_reads_match_the_old_constructors() {
    let fixture = Fixture::new("fiber-picture-old");
    fs::write(fixture.path().join("a.png"), PNG).expect("the image");
    fs::write(fixture.path().join("a.pdf"), PDF_MAGIC).expect("the pdf");
    let fiber = fiber_stub(fixture.path(), PDF_ONE_PAGE);
    let files = Files::new(fixture.path().to_path_buf())
        .with_images(fiber, fixture.path().join("artifacts"));
    let files = Arc::new(files);

    let deadline = Deadline::after(STEP);
    let image = read_bounded(
        Arc::clone(&files),
        "a.png".to_owned(),
        None,
        CancelToken::new(),
        deadline,
        fixture.reporter(),
        "the read",
    );
    assert_eq!(code(&image), None);
    assert_eq!(message(&image), "Image: 80x60 image/png.\n");
    let pdf = read_bounded(
        Arc::clone(&files),
        "a.pdf".to_owned(),
        None,
        CancelToken::new(),
        deadline,
        fixture.reporter(),
        "the read",
    );
    assert_eq!(code(&pdf), None, "{pdf:?}");
}

/// A stub `fiber`: in PDF mode (`$2` is `pdf`) it runs `pdf_body` with
/// `$5` the stem, else it answers an image child call.
fn fiber_stub(dir: &Path, pdf_body: &str) -> PathBuf {
    fakes::script(
        dir,
        "fiber-stub",
        &format!("if [ \"$2\" = pdf ]; then\n{pdf_body}\nelse\n{IMAGE_ANSWER}\nfi"),
    )
}

/// A stub `pdftoppm` running `body` with its argv.
fn ppm_stub(dir: &Path, body: &str) -> PathBuf {
    fakes::script(dir, "ppm-stub", body)
}

const PDF_ONE_PAGE: &str = r#"printf '{"file":"%s.pdf","page_count":1,"total":1}\n' "$5""#;
const PDF_TWO_PAGES: &str = r#"printf '{"file":"%s.pdf","page_count":2,"total":2}\n' "$5""#;

/// A PDF's first bytes, enough for inspection to classify it.
const PDF_MAGIC: &[u8] = b"%PDF-1.4 a test pdf";

/// A stub `pdftoppm` writing `<root>.png` for its last argument.
const PPM_OK: &str = r#"for root; do :; done
mkdir -p "$(dirname "$root")"
printf 'fakepng' > "$root.png""#;

/// A stub `pdftoppm` logging each call, signalling `ready`, then waiting
/// for its numbered release before writing `<root>.png`.
fn ppm_gated(log: &Path, ready: &Path, release_dir: &Path) -> String {
    format!(
        r#"echo "$@" >> '{}'
n=$(wc -l < '{}' | tr -d ' ')
echo $$ > '{}'
read ack < '{}/release-{}.fifo'
for root; do :; done
mkdir -p "$(dirname "$root")"
printf 'fakepng' > "$root.png""#,
        log.display(),
        log.display(),
        ready.display(),
        release_dir.display(),
        "$n",
    )
}

/// Release pipes the test holds open until the call's result: dropping a
/// holder before the stub opens its pipe would leave the open without a
/// writer, blocking past the buffered line.
fn releases(dir: &Path, count: u32) -> Vec<fs::File> {
    let mut held = Vec::new();
    for n in 1..=count {
        let pipe = dir.join(format!("release-{n}.fifo"));
        fakes::fifo(&pipe);
        held.push(
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&pipe)
                .expect("the release pipe"),
        );
    }
    held
}

fn release(held: &mut [fs::File], n: usize) {
    held.get_mut(n - 1)
        .expect("the release pipe")
        .write_all(b"go\n")
        .expect("release");
}

/// File tools with a timed image child and the stub renderer over `fiber`.
fn pdf_files(dir: &Path, fiber: &Path, ppm: &Path, probe: &Probe) -> Files {
    Files::new(dir.to_path_buf())
        .with_renderer(ppm.to_path_buf())
        .with_timing(probe.timing())
        .with_images(fiber.to_path_buf(), dir.join("artifacts"))
}

fn write_pdf(dir: &Path) {
    fs::write(dir.join("a.pdf"), PDF_MAGIC).expect("the pdf");
}

#[test]
fn renderer_and_timing_apply_in_either_order() {
    for (name, renderer_first) in [("renderer first", true), ("timing first", false)] {
        let fixture = Fixture::new("fiber-picture-order");
        write_pdf(fixture.path());
        let fiber = fiber_stub(fixture.path(), PDF_TWO_PAGES);
        let ppm = ppm_stub(
            fixture.path(),
            &format!(
                "echo \"$@\" >> \"{}\"/ppm.log\n{PPM_OK}",
                fixture.path().display()
            ),
        );
        let (probe, records) = probe();
        let files = if renderer_first {
            Files::new(fixture.path().to_path_buf())
                .with_renderer(ppm.clone())
                .with_timing(probe.timing())
                .with_images(fiber, fixture.path().join("artifacts"))
        } else {
            Files::new(fixture.path().to_path_buf())
                .with_timing(probe.timing())
                .with_renderer(ppm.clone())
                .with_images(fiber, fixture.path().join("artifacts"))
        };
        let files = Arc::new(files);
        let deadline = Deadline::after(STEP);

        let output = read_bounded(
            Arc::clone(&files),
            "a.pdf".to_owned(),
            None,
            CancelToken::new(),
            deadline,
            fixture.reporter(),
            "the read",
        );

        assert_eq!(code(&output), None, "{name}");
        assert_eq!(
            pdf_part(&output).map(|part| (part.1, part.2)),
            Some((2, Some(2))),
            "{name}"
        );
        let log = fs::read_to_string(fixture.path().join("ppm.log")).expect("the renderer log");
        assert_eq!(log.lines().count(), 2, "{name}: the selected renderer ran");
        assert_eq!(probe.calls(), 3, "{name}");
        closed(&records, deadline);
        closed(&records, deadline);
        closed(&records, deadline);
        assert_single(&records, name);
    }
}

#[test]
fn a_missing_renderer_keeps_the_cut_with_one_scope() {
    let fixture = Fixture::new("fiber-picture-no-ppm");
    write_pdf(fixture.path());
    let fiber = fiber_stub(fixture.path(), PDF_ONE_PAGE);
    let (probe, records) = probe();
    let files = Files::new(fixture.path().to_path_buf())
        .with_renderer(fixture.path().join("no-such-ppm"))
        .with_timing(probe.timing())
        .with_images(fiber, fixture.path().join("artifacts"));
    let files = Arc::new(files);
    let deadline = Deadline::after(STEP);

    let output = read_bounded(
        Arc::clone(&files),
        "a.pdf".to_owned(),
        None,
        CancelToken::new(),
        deadline,
        fixture.reporter(),
        "the read",
    );

    assert_eq!(code(&output), None);
    assert!(
        message(&output).contains("pdftoppm is not installed"),
        "{}",
        message(&output)
    );
    assert_eq!(
        pdf_part(&output).map(|part| (part.1, part.2)),
        Some((1, None))
    );
    assert_eq!(probe.calls(), 1, "only the count/cut call");
    let record = closed(&records, deadline);
    assert_eq!(record.op, Op::PictureRun);
    assert_single(&records, "the count/cut");
}

#[test]
fn two_rendered_pages_emit_three_scopes_with_no_renderer_hold() {
    let mut fixture = Fixture::new("fiber-picture-pages");
    write_pdf(fixture.path());
    let ready = fakes::children::Ready::new(fixture.path());
    let mut held = releases(fixture.path(), 2);
    let fiber = fiber_stub(fixture.path(), PDF_TWO_PAGES);
    let ppm = ppm_stub(
        fixture.path(),
        &ppm_gated(
            &fixture.path().join("ppm.log"),
            ready.path(),
            fixture.path(),
        ),
    );
    let (probe, records) = probe();
    let files = pdf_files(fixture.path(), &fiber, &ppm, &probe);
    let report = fixture.tx.take().expect("the report sender");
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _hook = test_spawn::install(report);
        drop(done_tx.send(read_with(&files, "a.pdf", None, &CancelToken::new())));
    });
    let deadline = Deadline::after(STEP);
    // The first render starts only after the count/cut scope closed: its
    // record is already waiting, and no page scope is open during the
    // renderer's hold.
    ready.wait(fakes::Deadline::after(STEP));
    let count_cut = closed(&records, deadline);
    assert_eq!(count_cut.op, Op::PictureRun);
    assert!(records.try_recv().is_err(), "no guard during the hold");
    release(&mut held, 1);
    // The second render starts only after the first page's scope closed.
    ready.wait(fakes::Deadline::after(STEP));
    let page_one = closed(&records, deadline);
    assert_eq!(page_one.op, Op::PictureRun);
    assert!(records.try_recv().is_err(), "no guard during the hold");
    release(&mut held, 2);
    let output = deadline.recv_or_fail(&done_rx, "the read result");
    assert_eq!(code(&output), None, "{output:?}");
    assert_eq!(message(&output), "PDF: 2 pages.\n");
    assert_eq!(
        pdf_part(&output).map(|part| (part.1, part.2)),
        Some((2, Some(2)))
    );
    let page_two = closed(&records, deadline);
    assert_eq!(page_two.op, Op::PictureRun);
    assert_eq!(probe.calls(), 3);
    assert_single(&records, "count/cut plus two pages");
    drop(held);
}

/// One entered PDF failure: the count/cut body (or `None` for a missing
/// binary) and the output it keeps.
struct PdfFailure {
    name: &'static str,
    body: Option<&'static str>,
    code: Option<ErrorCode>,
    want: &'static str,
}

#[test]
fn entered_pdf_failures_keep_their_output_and_drop_once() {
    let cases = [
        PdfFailure {
            name: "malformed count/cut",
            body: Some("printf 'garbage\\n'"),
            code: Some(ErrorCode::ToolError),
            want: "printed a line that is not JSON",
        },
        PdfFailure {
            name: "an unreadable child",
            body: Some("printf 'bad pdf\\n' >&2\nexit 1"),
            code: Some(ErrorCode::UnsupportedFile),
            want: "cannot be read as a PDF",
        },
        PdfFailure {
            name: "past the end",
            body: Some("printf '{\"total\":9}\\n'\nexit 4"),
            code: Some(ErrorCode::InvalidArguments),
            want: "has 9 pages",
        },
        PdfFailure {
            name: "a missing binary",
            body: None,
            code: Some(ErrorCode::ToolError),
            want: "could not start",
        },
    ];
    for case in cases {
        let fixture = Fixture::new("fiber-picture-pdf-fail");
        write_pdf(fixture.path());
        let fiber = match case.body {
            Some(body) => fiber_stub(fixture.path(), body),
            None => fixture.path().join("no-such-binary"),
        };
        let (probe, records) = probe();
        let files = Files::new(fixture.path().to_path_buf())
            .with_timing(probe.timing())
            .with_images(fiber, fixture.path().join("artifacts"));
        let files = Arc::new(files);
        let deadline = Deadline::after(STEP);

        let output = read_bounded(
            Arc::clone(&files),
            "a.pdf".to_owned(),
            None,
            CancelToken::new(),
            deadline,
            fixture.reporter(),
            "the read",
        );

        assert_eq!(code(&output), case.code.clone(), "{}", case.name);
        assert!(
            message(&output).contains(case.want),
            "{}: {}",
            case.name,
            message(&output)
        );
        assert!(pdf_part(&output).is_none(), "{}: no part", case.name);
        assert_eq!(probe.calls(), 1, "{}", case.name);
        let record = closed(&records, deadline);
        assert_eq!(record.op, Op::PictureRun, "{}", case.name);
        assert_single(&records, case.name);
    }
}

#[test]
fn a_cancel_during_the_count_cut_times_it() {
    let mut fixture = Fixture::new("fiber-picture-pdf-cancel");
    write_pdf(fixture.path());
    let ready = fakes::children::Ready::new(fixture.path());
    let held = releases(fixture.path(), 1);
    let fiber = fiber_stub(
        fixture.path(),
        &format!(
            "echo $$ > '{}'\nread ack < '{}'\n{}",
            ready.path().display(),
            fixture.path().join("release-1.fifo").display(),
            PDF_ONE_PAGE,
        ),
    );
    let (probe, records) = probe();
    let timing = probe.timing();
    let dir = fixture.path().to_path_buf();
    let artifacts = dir.join("artifacts");
    let fiber_path = fiber;
    let cancel = CancelToken::new();
    let call_cancel = cancel.clone();
    let report = fixture.tx.take().expect("the report sender");
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _hook = test_spawn::install(report);
        let files = Files::new(dir)
            .with_timing(timing)
            .with_images(fiber_path, artifacts);
        drop(done_tx.send(read_with(&files, "a.pdf", None, &call_cancel)));
    });
    let deadline = Deadline::after(STEP);
    let pid = fixture.spawned(deadline);
    let group = Fixture::watch(pid);
    ready.wait(fakes::Deadline::after(STEP));
    assert_eq!(probe.active(), 1, "the count/cut scope is open");
    assert!(done_rx.try_recv().is_err(), "no result before the cancel");
    cancel.cancel();
    let output = deadline.recv_or_fail(&done_rx, "the cancelled read");
    drop(held);
    assert_eq!(code(&output), None);
    assert_eq!(message(&output), "Cancelled and stopped.\n");
    assert!(pdf_part(&output).is_none());
    assert_eq!(probe.calls(), 1);
    let record = closed(&records, deadline);
    assert_eq!(record.op, Op::PictureRun);
    assert_single(&records, "the count/cut");
    assert!(fakes::group_empties(pid, deadline), "the group emptied");
    group.stand_down(deadline.cleanup_phase());
}

#[test]
fn a_failing_renderer_adds_no_picture_scope() {
    let fixture = Fixture::new("fiber-picture-ppm-fail");
    write_pdf(fixture.path());
    let fiber = fiber_stub(fixture.path(), PDF_ONE_PAGE);
    let ppm = ppm_stub(fixture.path(), "printf 'nope\\n' >&2\nexit 2");
    let (probe, records) = probe();
    let files = pdf_files(fixture.path(), &fiber, &ppm, &probe);
    let files = Arc::new(files);
    let deadline = Deadline::after(STEP);

    let output = read_bounded(
        Arc::clone(&files),
        "a.pdf".to_owned(),
        None,
        CancelToken::new(),
        deadline,
        fixture.reporter(),
        "the read",
    );

    assert_eq!(code(&output), None);
    assert!(
        message(&output).contains("pdftoppm exited with status 2"),
        "{}",
        message(&output)
    );
    assert_eq!(
        pdf_part(&output).map(|part| (part.1, part.2)),
        Some((1, None))
    );
    assert_eq!(probe.calls(), 1, "only the count/cut call");
    let record = closed(&records, deadline);
    assert_eq!(record.op, Op::PictureRun);
    assert_single(&records, "the count/cut");
}

#[test]
fn a_cancel_between_pages_adds_no_picture_scope() {
    let mut fixture = Fixture::new("fiber-picture-ppm-cancel");
    write_pdf(fixture.path());
    let ready = fakes::children::Ready::new(fixture.path());
    let held = releases(fixture.path(), 1);
    let fiber = fiber_stub(fixture.path(), PDF_ONE_PAGE);
    let ppm = ppm_stub(
        fixture.path(),
        &format!(
            "echo $$ > '{}'\nread ack < '{}'\n{}",
            ready.path().display(),
            fixture.path().join("release-1.fifo").display(),
            PPM_OK,
        ),
    );
    let (probe, records) = probe();
    let timing = probe.timing();
    let dir = fixture.path().to_path_buf();
    let artifacts = dir.join("artifacts");
    let (fiber_path, ppm_path) = (fiber, ppm);
    let cancel = CancelToken::new();
    let call_cancel = cancel.clone();
    let report = fixture.tx.take().expect("the report sender");
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _hook = test_spawn::install(report);
        let files = Files::new(dir)
            .with_renderer(ppm_path)
            .with_timing(timing)
            .with_images(fiber_path, artifacts);
        drop(done_tx.send(read_with(&files, "a.pdf", None, &call_cancel)));
    });
    let deadline = Deadline::after(STEP);
    // The count/cut child already exited; the renderer's pid is second.
    let _count_cut = fixture.spawned(deadline);
    let renderer = fixture.spawned(deadline);
    let group = Fixture::watch(renderer);
    ready.wait(fakes::Deadline::after(STEP));
    // The render starts only after the count/cut scope closed.
    let count_cut = closed(&records, deadline);
    assert_eq!(count_cut.op, Op::PictureRun);
    cancel.cancel();
    let output = deadline.recv_or_fail(&done_rx, "the cancelled read");
    drop(held);
    assert_eq!(code(&output), None);
    assert_eq!(message(&output), "Cancelled and stopped.\n");
    assert!(pdf_part(&output).is_none(), "no part on cancel");
    assert_eq!(probe.calls(), 1, "no page scope started");
    assert_single(&records, "the count/cut");
    assert!(
        fakes::group_empties(renderer, deadline),
        "the group emptied"
    );
    group.stand_down(deadline.cleanup_phase());
}

#[test]
fn a_failing_page_after_a_good_page_times_both_attempts() {
    let fixture = Fixture::new("fiber-picture-page-fail");
    write_pdf(fixture.path());
    // The first page child answers; the second refuses. The count file in
    // the fixture directory tells the attempts apart.
    let image_body = r#"cat >/dev/null
dir="$(dirname "$0")"
n=$(cat "$dir/count" 2>/dev/null || echo 0)
n=$((n + 1))
echo "$n" > "$dir/count"
if [ "$n" = 2 ]; then printf 'refused\n' >&2; exit 3; fi
printf '{"file":"%s.png","mime_type":"image/png","width":1,"height":1}\n' "$4""#;
    let fiber = fakes::script(
        fixture.path(),
        "fiber-stub",
        &format!("if [ \"$2\" = pdf ]; then\n{PDF_TWO_PAGES}\nelse\n{image_body}\nfi"),
    );
    let ppm = ppm_stub(fixture.path(), PPM_OK);
    let (probe, records) = probe();
    let files = pdf_files(fixture.path(), &fiber, &ppm, &probe);
    let files = Arc::new(files);
    let deadline = Deadline::after(STEP);

    let output = read_bounded(
        Arc::clone(&files),
        "a.pdf".to_owned(),
        None,
        CancelToken::new(),
        deadline,
        fixture.reporter(),
        "the read",
    );

    assert_eq!(code(&output), None, "a failed render keeps the cut");
    assert!(
        message(&output).contains("refused a page"),
        "{}",
        message(&output)
    );
    assert!(
        pdf_part(&output).map(|part| (part.1, part.2)) == Some((2, None)),
        "never partial pages: {output:?}"
    );
    assert_eq!(probe.calls(), 3, "the count/cut plus both attempts");
    closed(&records, deadline);
    closed(&records, deadline);
    closed(&records, deadline);
    assert_single(&records, "the attempts");
}

#[test]
fn a_pre_cancelled_read_spawns_nothing_and_times_nothing() {
    let fixture = Fixture::new("fiber-picture-read-cancel");
    fs::write(fixture.path().join("a.png"), PNG).expect("the image");
    let fiber = stub(
        fixture.path(),
        r#"touch "$(dirname "$0")/ran"
cat >/dev/null"#,
    );
    let (probe, records) = probe();
    let files = image_files(fixture.path(), &fiber, &probe);
    let files = Arc::new(files);
    let cancel = CancelToken::new();
    cancel.cancel();

    let deadline = Deadline::after(STEP);
    let output = read_bounded(
        Arc::clone(&files),
        "a.png".to_owned(),
        None,
        cancel.clone(),
        deadline,
        fixture.reporter(),
        "the read",
    );

    assert_eq!(code(&output), None);
    assert_eq!(message(&output), "Cancelled before it started.\n");
    assert!(!fixture.path().join("ran").exists(), "no child ran");
    assert_eq!(probe.calls(), 0);
    assert!(records.try_recv().is_err(), "no scope without entry");
}

/// One stderr shape and the capped message it keeps.
struct CapCase {
    name: &'static str,
    body: String,
    want: String,
}

#[test]
fn stderr_caps_hold_with_timing() {
    let prefix = "the image child exited with status 3: ";
    let cases = [
        CapCase {
            name: "2047 bytes stay whole",
            body: "head -c 2047 /dev/zero | tr '\\0' x >&2\nexit 3".to_owned(),
            want: format!("{prefix}{}", "x".repeat(2047)),
        },
        CapCase {
            name: "2048 bytes stay whole",
            body: "head -c 2048 /dev/zero | tr '\\0' x >&2\nexit 3".to_owned(),
            want: format!("{prefix}{}", "x".repeat(2048)),
        },
        CapCase {
            name: "2049 bytes cut to 2048",
            body: "head -c 2049 /dev/zero | tr '\\0' x >&2\nexit 3".to_owned(),
            want: format!("{prefix}{}", "x".repeat(2048)),
        },
        CapCase {
            name: "a trailing newline trims",
            body: "printf 'hello\\n\\n' >&2\nexit 3".to_owned(),
            want: format!("{prefix}hello"),
        },
        CapCase {
            name: "a character ending at 2048 stays",
            body: "head -c 2046 /dev/zero | tr '\\0' a >&2\nprintf '\\303\\251' >&2\nexit 3"
                .to_owned(),
            want: format!("{prefix}{}é", "a".repeat(2046)),
        },
        CapCase {
            name: "a character crossing 2048 cuts whole",
            body: "head -c 2047 /dev/zero | tr '\\0' a >&2\nprintf '\\303\\251' >&2\nexit 3"
                .to_owned(),
            want: format!("{prefix}{}", "a".repeat(2047)),
        },
    ];
    for case in cases {
        let fixture = Fixture::new("fiber-picture-cap");
        let fiber = stub(fixture.path(), &case.body);
        let (probe, records) = probe();
        let child = timed_child(&fiber, fixture.path(), &probe);
        let child = Arc::new(child);
        let deadline = Deadline::after(STEP);

        let result = process_bounded(
            Arc::clone(&child),
            PNG.to_vec(),
            CancelToken::new(),
            deadline,
            fixture.reporter(),
            "the image",
        );

        let Err(ImageError::Failed(message)) = result else {
            panic!("{}: failed: {result:?}", case.name);
        };
        assert_eq!(message, case.want, "{}", case.name);
        assert_eq!(probe.calls(), 1, "{}", case.name);
        let record = closed(&records, deadline);
        assert_eq!(
            (record.op, record.tool, record.started, record.ms),
            (Op::PictureRun, None, None, 0)
        );
        assert_single(&records, case.name);
    }
}

#[test]
fn the_capture_carries_no_stderr_sentinel() {
    let fixture = Fixture::new("fiber-picture-sentinel");
    let fiber = stub(fixture.path(), "printf 'CAP-SENTINEL-7\\n' >&2\nexit 3");
    let (probe, records) = probe();
    let child = timed_child(&fiber, fixture.path(), &probe);
    let child = Arc::new(child);
    let deadline = Deadline::after(STEP);

    let result = process_bounded(
        Arc::clone(&child),
        PNG.to_vec(),
        CancelToken::new(),
        deadline,
        fixture.reporter(),
        "the image",
    );

    let Err(ImageError::Failed(message)) = result else {
        panic!("failed: {result:?}");
    };
    assert!(message.contains("CAP-SENTINEL-7"), "{message}");
    let record = closed(&records, deadline);
    assert_eq!(record.op, Op::PictureRun);
    assert!(
        !format!("{record:?}").contains("CAP-SENTINEL-7"),
        "the capture holds only the operation and the milliseconds"
    );
}

/// One child answer and whether it validates.
struct NameCase {
    name: &'static str,
    body: String,
    ok: bool,
}

#[test]
fn image_filename_validation_with_timing() {
    // The shell fills `%s` from `"$4"`, this invocation's stem, so a
    // rejection names the shape, not a stale stem. A name with no `%s`
    // needs no argument.
    let answer = |file: &str, arg: &str| {
        format!(
            "cat >/dev/null\nprintf '{{\"file\":\"{file}\",\"mime_type\":\"image/png\",\"width\":1,\"height\":1}}\\n' {arg}"
        )
    };
    let cases = [
        NameCase {
            name: "png",
            body: answer("%s.png", "\"$4\""),
            ok: true,
        },
        NameCase {
            name: "jpg",
            body: answer("%s.jpg", "\"$4\""),
            ok: true,
        },
        NameCase {
            name: "alphanumeric",
            body: answer("%s.pNg9", "\"$4\""),
            ok: true,
        },
        NameCase {
            name: "a wrong stem",
            body: answer("other.png", ""),
            ok: false,
        },
        NameCase {
            name: "no extension",
            body: answer("%s", "\"$4\""),
            ok: false,
        },
        NameCase {
            name: "an empty extension",
            body: answer("%s.", "\"$4\""),
            ok: false,
        },
        NameCase {
            name: "a separator",
            body: answer("sub/%s.png", "\"$4\""),
            ok: false,
        },
        NameCase {
            name: "a traversal",
            body: answer("../outside.png", ""),
            ok: false,
        },
        NameCase {
            name: "a second dot",
            body: answer("%s.png.x", "\"$4\""),
            ok: false,
        },
    ];
    for case in cases {
        let fixture = Fixture::new("fiber-picture-names");
        let fiber = stub(fixture.path(), &case.body);
        let (probe, records) = probe();
        let child = timed_child(&fiber, fixture.path(), &probe);
        let child = Arc::new(child);
        let deadline = Deadline::after(STEP);

        let result = process_bounded(
            Arc::clone(&child),
            PNG.to_vec(),
            CancelToken::new(),
            deadline,
            fixture.reporter(),
            "the image",
        );

        if case.ok {
            assert!(result.is_ok(), "{}: {result:?}", case.name);
        } else {
            let Err(ImageError::Failed(message)) = result else {
                panic!("{}: failed: {result:?}", case.name);
            };
            assert!(
                message.contains("other than the one asked for"),
                "{}: {message}",
                case.name
            );
        }
        assert_eq!(probe.calls(), 1, "{}", case.name);
        let record = closed(&records, deadline);
        assert_eq!(record.op, Op::PictureRun, "{}", case.name);
        assert_single(&records, case.name);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn unusual_answers_fail_with_timing_on_linux() {
    // Non-UTF-8 output and names outside the portable table: Linux-only,
    // as the shell's byte handling differs by platform.
    let cases = [
        NameCase {
            name: "non-UTF-8 output",
            body: "printf '\\377\\n'".to_owned(),
            ok: false,
        },
        NameCase {
            name: "a high byte in the name",
            body: "printf '{\"file\":\"%s\\377.png\",\"mime_type\":\"image/png\",\"width\":1,\"height\":1}\\n' \"$4\""
                .to_owned(),
            ok: false,
        },
        NameCase {
            name: "a newline in the name",
            body: "printf '{\"file\":\"%s\\n.png\",\"mime_type\":\"image/png\",\"width\":1,\"height\":1}\\n' \"$4\""
                .to_owned(),
            ok: false,
        },
    ];
    for case in cases {
        let fixture = Fixture::new("fiber-picture-linux");
        let fiber = stub(fixture.path(), &case.body);
        let (probe, records) = probe();
        let child = timed_child(&fiber, fixture.path(), &probe);
        let child = Arc::new(child);
        let deadline = Deadline::after(STEP);

        let result = process_bounded(
            Arc::clone(&child),
            PNG.to_vec(),
            CancelToken::new(),
            deadline,
            fixture.reporter(),
            "the image",
        );

        assert!(
            matches!(result, Err(ImageError::Failed(_))),
            "{}: {result:?}",
            case.name
        );
        assert_eq!(probe.calls(), 1, "{}", case.name);
        let record = closed(&records, deadline);
        assert_eq!(record.op, Op::PictureRun, "{}", case.name);
        assert_single(&records, case.name);
    }
}

/// One count/cut answer and whether it validates.
struct PdfNameCase {
    name: &'static str,
    body: String,
    pages: Option<(u32, Option<usize>)>,
    want: &'static str,
}

#[test]
fn pdf_filename_validation_with_timing() {
    let answer = |file: &str, arg: &str, page_count: &str| {
        format!("printf '{{\"file\":\"{file}\",\"page_count\":{page_count},\"total\":2}}\\n' {arg}")
    };
    let cases = [
        PdfNameCase {
            name: "the exact stem",
            body: answer("%s.pdf", "$5", "2"),
            pages: Some((2, None)),
            want: "PDF: 2 pages.",
        },
        PdfNameCase {
            name: "a wrong stem",
            body: answer("other.pdf", "", "2"),
            pages: None,
            want: "other than the one asked for",
        },
        PdfNameCase {
            name: "a wrong case",
            body: answer("%s.PDF", "$5", "2"),
            pages: None,
            want: "other than the one asked for",
        },
        PdfNameCase {
            name: "a missing file",
            body: "printf '{\"page_count\":2,\"total\":2}\\n'".to_owned(),
            pages: None,
            want: "without file, page_count and total",
        },
        PdfNameCase {
            name: "a zero page count",
            body: answer("%s.pdf", "$5", "0"),
            pages: None,
            want: "a page count of zero",
        },
    ];
    for case in cases {
        let fixture = Fixture::new("fiber-picture-pdf-names");
        write_pdf(fixture.path());
        let fiber = fiber_stub(fixture.path(), &case.body);
        let (probe, records) = probe();
        // No working renderer: the cut validates, then stays without pages.
        let files = Files::new(fixture.path().to_path_buf())
            .with_renderer(fixture.path().join("no-such-ppm"))
            .with_timing(probe.timing())
            .with_images(fiber, fixture.path().join("artifacts"));
        let files = Arc::new(files);
        let deadline = Deadline::after(STEP);

        let output = read_bounded(
            Arc::clone(&files),
            "a.pdf".to_owned(),
            None,
            CancelToken::new(),
            deadline,
            fixture.reporter(),
            "the read",
        );

        assert!(
            message(&output).contains(case.want),
            "{}: {}",
            case.name,
            message(&output)
        );
        assert_eq!(
            pdf_part(&output).map(|part| (part.1, part.2)),
            case.pages,
            "{}",
            case.name
        );
        assert_eq!(probe.calls(), 1, "{}", case.name);
        let record = closed(&records, deadline);
        assert_eq!(record.op, Op::PictureRun, "{}", case.name);
        assert_single(&records, case.name);
    }
}

/// One path that does no picture work, and the output it keeps: no error
/// and a message holding `want` for text, or the error and a message
/// holding `want` otherwise.
struct QuietCase {
    name: &'static str,
    setup: fn(&Path),
    path: &'static str,
    pages: Option<&'static str>,
    images: bool,
    code: Option<ErrorCode>,
    want: &'static str,
}

#[test]
fn paths_without_picture_work_emit_nothing() {
    fn write_text(dir: &Path) {
        fs::write(dir.join("note.txt"), "hello\n").expect("the note");
    }
    fn write_binary(dir: &Path) {
        fs::write(dir.join("blob.bin"), b"\0\0binary").expect("the blob");
    }
    fn write_directory(dir: &Path) {
        fs::create_dir(dir.join("sub")).expect("the directory");
    }
    fn write_missing(_dir: &Path) {}
    fn write_image(dir: &Path) {
        fs::write(dir.join("a.png"), PNG).expect("the image");
    }
    fn write_pdf(dir: &Path) {
        fs::write(dir.join("a.pdf"), PDF_MAGIC).expect("the pdf");
    }
    fn write_big_pdf(dir: &Path) {
        fs::write(dir.join("big.pdf"), b"%PDF-1.4 sparse").expect("the pdf");
        std::fs::OpenOptions::new()
            .write(true)
            .open(dir.join("big.pdf"))
            .expect("the pdf")
            .set_len(crate::pdf::PDF_MAX_BYTES + 1)
            .expect("the length");
    }
    let cases = [
        QuietCase {
            name: "text",
            setup: write_text,
            path: "note.txt",
            pages: None,
            images: true,
            code: None,
            want: "hello",
        },
        QuietCase {
            name: "binary",
            setup: write_binary,
            path: "blob.bin",
            pages: None,
            images: true,
            code: Some(ErrorCode::UnsupportedFile),
            want: "binary data",
        },
        QuietCase {
            name: "a directory",
            setup: write_directory,
            path: "sub",
            pages: None,
            images: true,
            code: Some(ErrorCode::UnsupportedFile),
            want: "a directory",
        },
        QuietCase {
            name: "a missing file",
            setup: write_missing,
            path: "no-such.txt",
            pages: None,
            images: true,
            code: Some(ErrorCode::NotFound),
            want: "does not exist",
        },
        QuietCase {
            name: "an unconfigured image",
            setup: write_image,
            path: "a.png",
            pages: None,
            images: false,
            code: Some(ErrorCode::ToolError),
            want: "image reading is not configured",
        },
        QuietCase {
            name: "invalid pages",
            setup: write_pdf,
            path: "a.pdf",
            pages: Some("abc"),
            images: true,
            code: Some(ErrorCode::InvalidArguments),
            want: "must be a page or a range",
        },
        QuietCase {
            name: "pages on text",
            setup: write_text,
            path: "note.txt",
            pages: Some("1-2"),
            images: true,
            code: Some(ErrorCode::InvalidArguments),
            want: "applies only to a PDF",
        },
        QuietCase {
            name: "a PDF over the cap",
            setup: write_big_pdf,
            path: "big.pdf",
            pages: None,
            images: true,
            code: Some(ErrorCode::UnsupportedFile),
            want: "over the 100 MiB cap",
        },
    ];
    let (probe, records) = probe();
    for case in cases {
        let fixture = Fixture::new("fiber-picture-quiet");
        (case.setup)(fixture.path());
        let fiber = stub(
            fixture.path(),
            r#"touch "$(dirname "$0")/ran"
cat >/dev/null"#,
        );
        let files = if case.images {
            image_files(fixture.path(), &fiber, &probe)
        } else {
            Files::new(fixture.path().to_path_buf()).with_timing(probe.timing())
        };
        let files = Arc::new(files);

        let deadline = Deadline::after(STEP);
        let output = read_bounded(
            Arc::clone(&files),
            case.path.to_owned(),
            case.pages.map(str::to_owned),
            CancelToken::new(),
            deadline,
            fixture.reporter(),
            "the read",
        );

        assert_eq!(code(&output), case.code.clone(), "{}", case.name);
        assert!(
            message(&output).contains(case.want),
            "{}: {}",
            case.name,
            message(&output)
        );
        assert_eq!(probe.calls(), 0, "{}", case.name);
        assert!(
            !fixture.path().join("ran").exists(),
            "{}: no child ran",
            case.name
        );
        assert!(
            records.try_recv().is_err(),
            "{}: no picture work",
            case.name
        );
    }
}

#[test]
fn a_timed_success_returns_the_same_answer_with_one_picture_run() {
    let fixture = Fixture::new("fiber-picture-success");
    let fiber = stub(fixture.path(), IMAGE_ANSWER);
    let (probe, records) = probe();
    let child = timed_child(&fiber, fixture.path(), &probe);
    let child = Arc::new(child);
    let deadline = Deadline::after(STEP);

    let result = process_bounded(
        Arc::clone(&child),
        PNG.to_vec(),
        CancelToken::new(),
        deadline,
        fixture.reporter(),
        "the image",
    );

    let reference = result.expect("success");
    assert_eq!(
        (
            reference.mime_type.as_str(),
            reference.width,
            reference.height
        ),
        ("image/png", 80, 60)
    );
    assert!(
        reference.path.starts_with("artifacts/i_") && reference.path.ends_with(".png"),
        "{}",
        reference.path
    );
    assert_eq!(
        fs::read(fixture.path().join(&reference.path)).expect("the artifact"),
        PNG
    );
    assert_eq!(probe.calls(), 1);
    assert_eq!(probe.active(), 0);
    let record = closed(&records, deadline);
    assert_eq!(record.op, Op::PictureRun);
    assert_eq!(record.tool, None);
    assert_eq!(record.started, None);
    assert!(records.try_recv().is_err(), "the scope closed exactly once");
}

#[test]
fn building_with_timing_starts_no_scope() {
    let fixture = Fixture::new("fiber-picture-build");
    let fiber = stub(fixture.path(), IMAGE_ANSWER);
    let (probe, records) = probe();

    drop(timed_child(&fiber, fixture.path(), &probe));

    assert_eq!(probe.calls(), 0);
    assert!(records.try_recv().is_err(), "building starts no scope");
}

#[test]
fn without_timing_the_same_work_emits_nothing() {
    let fixture = Fixture::new("fiber-picture-untimed");
    let fiber = stub(fixture.path(), IMAGE_ANSWER);
    let child = ImageChild::new(fiber, fixture.path().join("artifacts"));
    let child = Arc::new(child);

    let deadline = Deadline::after(STEP);
    let result = process_bounded(
        Arc::clone(&child),
        PNG.to_vec(),
        CancelToken::new(),
        deadline,
        fixture.reporter(),
        "the image",
    );

    let reference = result.expect("success");
    assert_eq!(
        (
            reference.mime_type.as_str(),
            reference.width,
            reference.height
        ),
        ("image/png", 80, 60)
    );
}

#[test]
fn a_declining_callable_runs_once_per_call_without_a_scope() {
    let fixture = Fixture::new("fiber-picture-decline");
    let fiber = stub(fixture.path(), IMAGE_ANSWER);
    let probe = Probe::declining();
    let child = ImageChild::new(fiber.clone(), fixture.path().join("artifacts"))
        .with_timing(probe.timing());
    let child = Arc::new(child);

    let deadline = Deadline::after(STEP);
    let first = process_bounded(
        Arc::clone(&child),
        PNG.to_vec(),
        CancelToken::new(),
        deadline,
        fixture.reporter(),
        "the image",
    );

    assert!(first.is_ok(), "{first:?}");
    assert_eq!(probe.calls(), 1);
    let failing = stub(fixture.path(), "echo boom >&2\nexit 3");
    let declined =
        ImageChild::new(failing, fixture.path().join("artifacts")).with_timing(probe.timing());
    let declined = Arc::new(declined);
    let second = process_bounded(
        Arc::clone(&declined),
        PNG.to_vec(),
        CancelToken::new(),
        deadline,
        fixture.reporter(),
        "the image",
    );
    assert!(matches!(second, Err(ImageError::Failed(_))), "{second:?}");
    assert_eq!(probe.calls(), 2, "one call per entered invocation");
}

#[test]
fn the_guard_is_live_while_the_child_is_held() {
    let fixture = Fixture::new("fiber-picture-held");
    let ready = fakes::children::Ready::new(fixture.path());
    let release = fixture.path().join("release.fifo");
    fakes::fifo(&release);
    let mut held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&release)
        .expect("the release pipe");
    let fiber = stub(
        fixture.path(),
        &format!(
            "echo $$ > '{}'\ncat >/dev/null\nread ack < '{}'\n{IMAGE_ANSWER}",
            ready.path().display(),
            release.display()
        ),
    );
    let (probe, records) = probe();
    let timing = probe.timing();
    let dir = fixture.path().to_path_buf();
    let (done_tx, done_rx) = mpsc::channel();
    let (report_tx, report_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _hook = test_spawn::install(report_tx);
        let child = ImageChild::new(fiber, dir.join("artifacts")).with_timing(timing);
        drop(done_tx.send(child.process(PNG, &CancelToken::new())));
    });
    let deadline = Deadline::after(STEP);
    ready.wait(fakes::Deadline::after(STEP));
    assert_eq!(probe.active(), 1, "the scope is open while held");
    assert!(
        done_rx.try_recv().is_err(),
        "no completion while the child is held"
    );
    assert!(
        records.try_recv().is_err(),
        "no record while the scope is open"
    );
    held.write_all(b"go\n").expect("release");
    // `held` stays open until the result: dropping it before the child
    // opens the pipe would leave the child's open without a writer, and
    // the open would block past the buffered line.
    let result = deadline.recv_or_fail(&done_rx, "the operation result");
    drop(held);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(probe.active(), 0);
    let record = closed(&records, deadline);
    assert_eq!(record.op, Op::PictureRun);
    drop(report_rx);
}

#[test]
fn exact_milliseconds_come_from_the_two_clock_reads() {
    let fixture = Fixture::new("fiber-picture-clock");
    let fiber = stub(fixture.path(), IMAGE_ANSWER);
    let clock = Scripted::plain();
    let origin = clock.origin();
    clock.script(vec![origin, origin + Duration::from_millis(6)]);
    let (tx, records) = mpsc::channel();
    let probe = Probe::recording(Arc::clone(&clock), tx);
    let child = timed_child(&fiber, fixture.path(), &probe);
    let child = Arc::new(child);

    let deadline = Deadline::after(STEP);
    let result = process_bounded(
        Arc::clone(&child),
        PNG.to_vec(),
        CancelToken::new(),
        deadline,
        fixture.reporter(),
        "the image",
    );

    assert!(result.is_ok(), "{result:?}");
    let record = closed(&records, deadline);
    assert_eq!(record.ms, 6);
    assert_eq!(clock.reads(), 2, "one read at entry, one at exit");
    clock.now();
    assert_eq!(clock.reads(), 3);
    assert_eq!(record.ms, 6, "a later read rewrites nothing");
}

#[test]
fn repeated_calls_open_independent_scopes_with_fresh_names() {
    let fixture = Fixture::new("fiber-picture-repeat");
    let fiber = stub(fixture.path(), IMAGE_ANSWER);
    let (probe, records) = probe();
    let child = timed_child(&fiber, fixture.path(), &probe);
    let child = Arc::new(child);
    let deadline = Deadline::after(STEP);

    let first = process_bounded(
        Arc::clone(&child),
        PNG.to_vec(),
        CancelToken::new(),
        deadline,
        fixture.reporter(),
        "the image",
    )
    .expect("first");
    let second = process_bounded(
        Arc::clone(&child),
        PNG.to_vec(),
        CancelToken::new(),
        deadline,
        fixture.reporter(),
        "the image",
    )
    .expect("second");

    assert_ne!(first.path, second.path, "a fresh stem per call");
    assert_eq!(probe.calls(), 2);
    assert_eq!(probe.active(), 0);
    let one = closed(&records, deadline);
    let two = closed(&records, deadline);
    assert_eq!((one.op, two.op), (Op::PictureRun, Op::PictureRun));
    assert!(records.try_recv().is_err(), "two scopes, two records");
}

/// One entered failure: the shell body (or `None` for a missing binary)
/// and the exact error the call keeps.
struct FailureCase {
    name: &'static str,
    body: Option<&'static str>,
    want: ImageError,
    bytes: Vec<u8>,
}

#[test]
fn entered_failures_keep_their_error_and_drop_once() {
    let cases = [
        FailureCase {
            name: "unreadable exit 1",
            body: Some("printf 'font missing\\n' >&2\nexit 1"),
            want: ImageError::Unreadable("font missing".to_owned()),
            bytes: PNG.to_vec(),
        },
        FailureCase {
            name: "exit 2",
            body: Some("printf 'badness\\n' >&2\nexit 2"),
            want: ImageError::Failed("the image child exited with status 2: badness".to_owned()),
            bytes: PNG.to_vec(),
        },
        FailureCase {
            name: "exit 3",
            body: Some("printf 'worse\\n' >&2\nexit 3"),
            want: ImageError::Failed("the image child exited with status 3: worse".to_owned()),
            bytes: PNG.to_vec(),
        },
        FailureCase {
            name: "malformed JSON",
            body: Some("cat >/dev/null; printf 'not json\\n'"),
            want: ImageError::Failed("the image child printed a line that is not JSON.".to_owned()),
            bytes: PNG.to_vec(),
        },
        FailureCase {
            name: "missing fields",
            body: Some("cat >/dev/null; printf '{\"file\":\"%s.png\"}\\n' \"$4\""),
            want: ImageError::Failed(
                "the image child printed a line without file, mime_type, width and height."
                    .to_owned(),
            ),
            bytes: PNG.to_vec(),
        },
        FailureCase {
            name: "an extra line",
            body: Some(
                "cat >/dev/null; printf '{\"file\":\"%s.png\",\"mime_type\":\"image/png\",\"width\":1,\"height\":1}\\nsecond\\n' \"$4\"",
            ),
            want: ImageError::Failed("the image child printed more than one line.".to_owned()),
            bytes: PNG.to_vec(),
        },
        FailureCase {
            name: "a missing binary",
            body: None,
            want: ImageError::Failed("the image child could not start".to_owned()),
            bytes: PNG.to_vec(),
        },
        FailureCase {
            name: "an early exit with a broken write",
            body: Some("exit 0"),
            want: ImageError::Failed("the image child".to_owned()),
            bytes: vec![b'y'; 1024 * 1024],
        },
    ];
    for case in cases {
        let fixture = Fixture::new("fiber-picture-failure");
        let fiber = match case.body {
            Some(body) => stub(fixture.path(), body),
            None => fixture.path().join("no-such-binary"),
        };
        let (probe, records) = probe();
        let child = timed_child(&fiber, fixture.path(), &probe);
        let child = Arc::new(child);
        let deadline = Deadline::after(STEP);

        let result = process_bounded(
            Arc::clone(&child),
            case.bytes.clone(),
            CancelToken::new(),
            deadline,
            fixture.reporter(),
            "the image",
        );

        match (&result, &case.want) {
            (Err(ImageError::Failed(got)), ImageError::Failed(want)) => {
                assert!(got.contains(want.as_str()), "{}: {got}", case.name);
            }
            _ => assert_eq!(result, Err(case.want.clone()), "{}", case.name),
        }
        assert_eq!(probe.calls(), 1, "{}", case.name);
        assert_eq!(probe.active(), 0, "{}", case.name);
        let record = closed(&records, deadline);
        assert_eq!(record.op, Op::PictureRun, "{}", case.name);
        assert!(
            records.try_recv().is_err(),
            "{}: one entry, one record",
            case.name
        );
    }
}

#[test]
fn a_pre_cancelled_call_spawns_nothing_and_times_nothing() {
    let fixture = Fixture::new("fiber-picture-precancel");
    let fiber = stub(fixture.path(), "touch \"$(dirname \"$0\")/ran\"");
    let (probe, records) = probe();
    let child = timed_child(&fiber, fixture.path(), &probe);
    let child = Arc::new(child);
    let cancel = CancelToken::new();
    cancel.cancel();

    let deadline = Deadline::after(STEP);
    let result = process_bounded(
        Arc::clone(&child),
        PNG.to_vec(),
        cancel.clone(),
        deadline,
        fixture.reporter(),
        "the image",
    );

    assert_eq!(result, Err(ImageError::Cancelled));
    assert_eq!(probe.calls(), 0, "no timing invocation before entry");
    assert!(!fixture.path().join("ran").exists(), "no child ran");
    assert!(fixture.rx.try_recv().is_err(), "no spawn");
    assert!(records.try_recv().is_err(), "no scope without entry");
}

/// One cancelled call: its result, its probe and records, the child's pid
/// and the group watchdog armed while the child was blocked.
struct CancelledCall {
    result: Result<ImageRef, ImageError>,
    probe: Probe,
    records: mpsc::Receiver<Record>,
    pid: u32,
    group: Watchdog,
}

/// Runs `process` of `bytes` off-thread against a child that signals
/// `ready` then blocks on the test-held release pipe, cancels it, and
/// returns the cancelled call. The child's pid is reported through the
/// spawn hook, and its group watchdog is armed while it is still blocked.
/// A body that needs the cancel to land after some other child state
/// reaches that state before signalling readiness, then blocks on the
/// release like every other body, so the race is forced, never waited for.
fn cancelled_call(
    prefix: &str,
    bytes: Vec<u8>,
    body: &dyn Fn(&Path, &Path, &Path) -> String,
) -> CancelledCall {
    let mut client = Fixture::new(prefix);
    let ready = fakes::children::Ready::new(client.path());
    let release = client.path().join("release.fifo");
    fakes::fifo(&release);
    // Held open until the cancelled result is in: dropping it before the
    // child opens the pipe would leave the open without a writer.
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&release)
        .expect("the release pipe");
    let fiber = stub(client.path(), &body(client.path(), ready.path(), &release));
    let (probe, records) = probe();
    let timing = probe.timing();
    let dir = client.path().to_path_buf();
    let cancel = CancelToken::new();
    let call_cancel = cancel.clone();
    let report = client.tx.take().expect("the report sender");
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _hook = test_spawn::install(report);
        let child = ImageChild::new(fiber, dir.join("artifacts")).with_timing(timing);
        drop(done_tx.send(child.process(&bytes, &call_cancel)));
    });
    let deadline = Deadline::after(STEP);
    let pid = client.spawned(deadline);
    let group = Fixture::watch(pid);
    ready.wait(fakes::Deadline::after(STEP));
    assert!(
        done_rx.try_recv().is_err(),
        "the call ends only at the cancel"
    );
    assert_eq!(probe.active(), 1, "the scope is open while held");
    cancel.cancel();
    let result = deadline.recv_or_fail(&done_rx, "the cancelled call");
    drop(held);
    CancelledCall {
        result,
        probe,
        records,
        pid,
        group,
    }
}

/// One cancelled call keeps `Cancelled`, closes exactly one scope, and
/// leaves its group empty.
fn assert_cancelled(call: CancelledCall) {
    let CancelledCall {
        result,
        probe,
        records,
        pid,
        group,
    } = call;
    assert_eq!(result, Err(ImageError::Cancelled));
    assert_eq!(probe.calls(), 1);
    assert_eq!(probe.active(), 0);
    let deadline = Deadline::after(STEP);
    let record = closed(&records, deadline);
    assert_eq!(record.op, Op::PictureRun);
    assert!(records.try_recv().is_err(), "one entry, one record");
    assert!(fakes::group_empties(pid, deadline), "the group emptied");
    group.stand_down(deadline.cleanup_phase());
}

#[test]
fn a_cancel_after_the_child_started_stops_and_times_it() {
    let call = cancelled_call("fiber-picture-cancel", PNG.to_vec(), &|_, ready, _| {
        format!("echo $$ > '{}'\nexec sleep 3600", ready.display())
    });
    assert_cancelled(call);
}

#[test]
fn a_cancel_while_the_write_is_blocked_stops_and_times_it() {
    // The child never reads stdin while 4 MiB is passed, so the write
    // blocks on a full pipe until the cancel kills it.
    let call = cancelled_call(
        "fiber-picture-blocked",
        vec![b'x'; 4 * 1024 * 1024],
        &|_, ready, _| format!("echo $$ > '{}'\nexec sleep 3600", ready.display()),
    );
    assert_cancelled(call);
}

#[test]
fn a_cancel_after_stdout_and_stderr_closed_stops_and_times_it() {
    // The child closes both pipes before signalling readiness, then waits
    // on the release pipe: readiness proves the drains already joined, so
    // the cancel below deterministically lands after the close.
    let call = cancelled_call(
        "fiber-picture-closed",
        PNG.to_vec(),
        &|_, ready, release| {
            format!(
                "exec 1>&- 2>&-\necho $$ > '{}'\nread ack < '{}'",
                ready.display(),
                release.display()
            )
        },
    );
    assert_cancelled(call);
}

#[test]
fn filled_pipes_complete_without_deadlock_with_one_scope() {
    let fixture = Fixture::new("fiber-picture-pipes");
    let fiber = stub(
        fixture.path(),
        r#"cat > /dev/null
head -c 300000 /dev/zero | tr '\0' x >&2
head -c 300000 /dev/zero | tr '\0' y
exit 3"#,
    );
    let (probe, records) = probe();
    let child = timed_child(&fiber, fixture.path(), &probe);
    let child = Arc::new(child);
    let deadline = Deadline::after(STEP);
    let bytes = vec![b'z'; 64 * 1024];

    let result = process_bounded(
        Arc::clone(&child),
        bytes.clone(),
        CancelToken::new(),
        deadline,
        fixture.reporter(),
        "the image",
    );

    let Err(ImageError::Failed(message)) = result else {
        panic!("failed: {result:?}");
    };
    assert!(message.contains("exited with status 3"), "{message}");
    assert_eq!(probe.calls(), 1);
    let record = closed(&records, deadline);
    assert_eq!(record.op, Op::PictureRun);
    assert!(records.try_recv().is_err(), "one entry, one record");
}
