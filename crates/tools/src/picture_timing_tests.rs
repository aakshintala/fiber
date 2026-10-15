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

use contract::diag::{Op, Timing};
use contract::images::{ImageError, Images};
use contract::provider::ImageRef;
use fakes::{CancelToken, Deadline, TempDir, Watchdog};

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
/// content travels with it.
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

    /// Installs spawn reporting on the calling thread until the guard
    /// drops: the spawning thread installs it, so an off-thread call
    /// installs it inside its worker.
    fn install(&mut self) -> test_spawn::Guard {
        test_spawn::install(self.tx.take().expect("installed once"))
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

#[test]
fn a_timed_success_returns_the_same_answer_with_one_picture_run() {
    let mut fixture = Fixture::new("fiber-picture-success");
    let _hook = fixture.install();
    let fiber = stub(fixture.path(), IMAGE_ANSWER);
    let (probe, records) = probe();
    let child = timed_child(&fiber, fixture.path(), &probe);
    let deadline = Deadline::after(STEP);

    let result = child.process(PNG, &CancelToken::new());

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
    let mut fixture = Fixture::new("fiber-picture-build");
    let _hook = fixture.install();
    let fiber = stub(fixture.path(), IMAGE_ANSWER);
    let (probe, records) = probe();

    drop(timed_child(&fiber, fixture.path(), &probe));

    assert_eq!(probe.calls(), 0);
    assert!(records.try_recv().is_err(), "building starts no scope");
}

#[test]
fn without_timing_the_same_work_emits_nothing() {
    let mut fixture = Fixture::new("fiber-picture-untimed");
    let _hook = fixture.install();
    let fiber = stub(fixture.path(), IMAGE_ANSWER);
    let child = ImageChild::new(fiber, fixture.path().join("artifacts"));

    let result = child.process(PNG, &CancelToken::new());

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
    let mut fixture = Fixture::new("fiber-picture-decline");
    let _hook = fixture.install();
    let fiber = stub(fixture.path(), IMAGE_ANSWER);
    let probe = Probe::declining();
    let child = ImageChild::new(fiber.clone(), fixture.path().join("artifacts"))
        .with_timing(probe.timing());

    let first = child.process(PNG, &CancelToken::new());

    assert!(first.is_ok(), "{first:?}");
    assert_eq!(probe.calls(), 1);
    let failing = stub(fixture.path(), "echo boom >&2\nexit 3");
    let declined =
        ImageChild::new(failing, fixture.path().join("artifacts")).with_timing(probe.timing());
    let second = declined.process(PNG, &CancelToken::new());
    assert!(matches!(second, Err(ImageError::Failed(_))), "{second:?}");
    assert_eq!(probe.calls(), 2, "one call per entered invocation");
}

#[test]
fn the_guard_is_live_while_the_child_is_held() {
    let fixture = Fixture::new("fiber-picture-held");
    let ready = fakes::children::Ready::new(fixture.path());
    let release = fixture.path().join("release.fifo");
    let mkfifo = std::process::Command::new("mkfifo")
        .arg(&release)
        .status()
        .expect("mkfifo starts");
    assert!(mkfifo.success(), "mkfifo {release:?}");
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
    ready.wait(STEP);
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
    let mut fixture = Fixture::new("fiber-picture-clock");
    let _hook = fixture.install();
    let fiber = stub(fixture.path(), IMAGE_ANSWER);
    let clock = Scripted::plain();
    let origin = clock.origin();
    clock.script(vec![origin, origin + Duration::from_millis(6)]);
    let (tx, records) = mpsc::channel();
    let probe = Probe::recording(Arc::clone(&clock), tx);
    let child = timed_child(&fiber, fixture.path(), &probe);

    let result = child.process(PNG, &CancelToken::new());

    assert!(result.is_ok(), "{result:?}");
    let record = closed(&records, Deadline::after(STEP));
    assert_eq!(record.ms, 6);
    assert_eq!(clock.reads(), 2, "one read at entry, one at exit");
    clock.now();
    assert_eq!(clock.reads(), 3);
    assert_eq!(record.ms, 6, "a later read rewrites nothing");
}

#[test]
fn repeated_calls_open_independent_scopes_with_fresh_names() {
    let mut fixture = Fixture::new("fiber-picture-repeat");
    let _hook = fixture.install();
    let fiber = stub(fixture.path(), IMAGE_ANSWER);
    let (probe, records) = probe();
    let child = timed_child(&fiber, fixture.path(), &probe);
    let deadline = Deadline::after(STEP);

    let first = child.process(PNG, &CancelToken::new()).expect("first");
    let second = child.process(PNG, &CancelToken::new()).expect("second");

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
            body: Some("printf 'not json\\n'"),
            want: ImageError::Failed("the image child printed a line that is not JSON.".to_owned()),
            bytes: PNG.to_vec(),
        },
        FailureCase {
            name: "missing fields",
            body: Some("printf '{\"file\":\"%s.png\"}\\n' \"$4\""),
            want: ImageError::Failed(
                "the image child printed a line without file, mime_type, width and height."
                    .to_owned(),
            ),
            bytes: PNG.to_vec(),
        },
        FailureCase {
            name: "an extra line",
            body: Some(
                "printf '{\"file\":\"%s.png\",\"mime_type\":\"image/png\",\"width\":1,\"height\":1}\\nsecond\\n' \"$4\"",
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
        let mut fixture = Fixture::new("fiber-picture-failure");
        let _hook = fixture.install();
        let fiber = match case.body {
            Some(body) => stub(fixture.path(), body),
            None => fixture.path().join("no-such-binary"),
        };
        let (probe, records) = probe();
        let child = timed_child(&fiber, fixture.path(), &probe);
        let deadline = Deadline::after(STEP);

        let result = child.process(&case.bytes, &CancelToken::new());

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
    let mut fixture = Fixture::new("fiber-picture-precancel");
    let _hook = fixture.install();
    let fiber = stub(fixture.path(), "touch \"$(dirname \"$0\")/ran\"");
    let (probe, records) = probe();
    let child = timed_child(&fiber, fixture.path(), &probe);
    let cancel = CancelToken::new();
    cancel.cancel();

    let result = child.process(PNG, &cancel);

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
/// `ready` then blocks, cancels it, and returns the cancelled call. The
/// child's pid is reported through the spawn hook, and its group watchdog
/// is armed while it is still blocked.
fn cancelled_call(
    prefix: &str,
    bytes: Vec<u8>,
    body: &dyn Fn(&Path, &Path) -> String,
) -> CancelledCall {
    let mut client = Fixture::new(prefix);
    let ready = fakes::children::Ready::new(client.path());
    let fiber = stub(client.path(), &body(client.path(), ready.path()));
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
    ready.wait(STEP);
    assert!(
        done_rx.try_recv().is_err(),
        "the call ends only at the cancel"
    );
    assert_eq!(probe.active(), 1, "the scope is open while held");
    cancel.cancel();
    let result = deadline.recv_or_fail(&done_rx, "the cancelled call");
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
    assert!(
        fakes::group_empties(pid, deadline.left()),
        "the group emptied"
    );
    group.stand_down(deadline.cleanup());
}

#[test]
fn a_cancel_after_the_child_started_stops_and_times_it() {
    let call = cancelled_call("fiber-picture-cancel", PNG.to_vec(), &|_, ready| {
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
        &|_, ready| format!("echo $$ > '{}'\nexec sleep 3600", ready.display()),
    );
    assert_cancelled(call);
}

#[test]
fn a_cancel_after_stdout_and_stderr_closed_stops_and_times_it() {
    // The child closes both pipes, then sleeps: the drains are already
    // joined when the cancel lands.
    let call = cancelled_call("fiber-picture-closed", PNG.to_vec(), &|_, ready| {
        format!("echo $$ > '{}'\nexec sleep 3600 >&- 2>&-", ready.display())
    });
    assert_cancelled(call);
}

#[test]
fn filled_pipes_complete_without_deadlock_with_one_scope() {
    let mut fixture = Fixture::new("fiber-picture-pipes");
    let _hook = fixture.install();
    let fiber = stub(
        fixture.path(),
        r#"cat > /dev/null
head -c 300000 /dev/zero | tr '\0' x >&2
head -c 300000 /dev/zero | tr '\0' y
exit 3"#,
    );
    let (probe, records) = probe();
    let child = timed_child(&fiber, fixture.path(), &probe);
    let deadline = Deadline::after(STEP);
    let bytes = vec![b'z'; 64 * 1024];

    let result = child.process(&bytes, &CancelToken::new());

    let Err(ImageError::Failed(message)) = result else {
        panic!("failed: {result:?}");
    };
    assert!(message.contains("exited with status 3"), "{message}");
    assert_eq!(probe.calls(), 1);
    let record = closed(&records, deadline);
    assert_eq!(record.op, Op::PictureRun);
    assert!(records.try_recv().is_err(), "one entry, one record");
}
