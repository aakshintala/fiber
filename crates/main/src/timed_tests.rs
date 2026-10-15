//! `builtin_timed` wires one timing into both image drivers: the file
//! tools' child, reached through the `read` tool, and the shared
//! standalone child. A stub `fiber` answers both, so no image code runs.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use contract::diag::{Op, Timing};
use contract::tool::Cancel;
use fakes::Watchdog;

struct Never;

impl Cancel for Never {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
}

struct Quiet;

impl contract::emit::Emit for Quiet {
    fn emit(&self, _event: &contract::events::Event) {}
}

/// Counts timing calls and closed scopes.
struct Probe {
    calls: AtomicUsize,
    closed: AtomicUsize,
}

struct Scope {
    probe: Arc<Probe>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        self.probe.closed.fetch_add(1, Ordering::SeqCst);
    }
}

fn timing(probe: &Arc<Probe>) -> Timing {
    let probe = Arc::clone(probe);
    Arc::new(
        move |op: Op, tool: Option<&str>, _started: Option<std::time::Instant>| {
            assert_eq!(op, Op::PictureRun);
            assert_eq!(tool, None);
            probe.calls.fetch_add(1, Ordering::SeqCst);
            Some(Box::new(Scope {
                probe: Arc::clone(&probe),
            }) as Box<dyn Send>)
        },
    )
}

/// How long one builtin tool call may take: starting a child can stall
/// for seconds, and a hang fails the test instead of hanging it.
const CALL_WITHIN: Duration = Duration::from_secs(10);

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";

/// One stub-child fixture: its directory and a matching watchdog armed
/// before any stub exists. The drivers reap their own children, which
/// [`ChildFixture::finish`] checks on the success path before standing
/// the watchdog down; a panic or a timeout drops the watchdog, which kills
/// every match (`docs/testing.md`, "Running tests"). `fakes::within`
/// bounds the caller, not the children: this owns them.
struct ChildFixture {
    root: fakes::TempDir,
    leaf: String,
    matching: Watchdog,
}

impl ChildFixture {
    fn new(prefix: &str) -> Self {
        let root = fakes::TempDir::new(prefix);
        let leaf = root
            .path()
            .file_name()
            .expect("a leaf")
            .to_string_lossy()
            .into_owned();
        let matching = Watchdog::matching(&leaf);
        Self {
            root,
            leaf,
            matching,
        }
    }

    /// No stub child outlives the drivers' own reaps: the success path.
    /// Panic and timeout paths drop the watchdog, which kills every match.
    fn finish(self, within: Duration) {
        assert!(
            fakes::matching_exits(&self.leaf, within),
            "a stub child outlived the test"
        );
        self.matching.stand_down(within);
    }
}

/// The `builtin_timed` session over the fixture's stub `fiber`, which
/// answers image calls from this invocation's stem.
#[allow(
    clippy::type_complexity,
    reason = "one test session: its tools and shared images"
)]
fn session(
    fixture: &ChildFixture,
    timing: Option<Timing>,
) -> (
    Vec<(String, Arc<dyn contract::tool::Tool>)>,
    Arc<dyn contract::images::Images>,
) {
    let root = &fixture.root;
    std::fs::write(root.path().join("a.png"), PNG).expect("the image");
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let fiber = fakes::script(
        root.path(),
        "fiber-stub",
        "cat >/dev/null; printf '{\"file\":\"%s.png\",\"mime_type\":\"image/png\",\"width\":1,\"height\":1}\\n' \"$4\"",
    );
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (tools, _infos, _driver, _forget, images) = super::builtin_timed(
        fiber,
        &root.path().join("home"),
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        None,
        &super::tests::delegates(),
        super::tests::skills(),
        timing,
    )
    .expect("the built-ins");
    (tools, images)
}

fn read_tool(tools: &[(String, Arc<dyn contract::tool::Tool>)]) -> Arc<dyn contract::tool::Tool> {
    let (_, read) = tools
        .iter()
        .find(|(_, tool)| tool.definition().name == "read")
        .expect("read is registered");
    Arc::clone(read)
}

fn read_output(read: Arc<dyn contract::tool::Tool>) -> contract::tool::Output {
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "path".to_owned(),
        serde_json::Value::String("a.png".to_owned()),
    );
    fakes::within("the read call", CALL_WITHIN, move || {
        read.run(&arguments, &Never, &Quiet)
    })
}

#[test]
fn timed_files_and_images_share_one_timing() {
    let fixture = ChildFixture::new("fiber-builtin-timed");
    let probe = Arc::new(Probe {
        calls: AtomicUsize::new(0),
        closed: AtomicUsize::new(0),
    });
    let (tools, images) = session(&fixture, Some(timing(&probe)));

    let output = read_output(read_tool(&tools));
    assert_eq!(output.error, None);
    assert!(
        output
            .content
            .iter()
            .any(|part| matches!(part, contract::shapes::ContentPart::Image { .. })),
        "{output:?}"
    );
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    assert_eq!(probe.closed.load(Ordering::SeqCst), 1);

    let bytes = PNG.to_vec();
    let reference = fakes::within("the images call", CALL_WITHIN, move || {
        images.process(&bytes, &Never)
    });
    let reference = reference.expect("the image");
    assert_eq!(reference.width, 1);
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    assert_eq!(probe.closed.load(Ordering::SeqCst), 2);
    fixture.finish(CALL_WITHIN);
}

#[test]
fn untimed_builtin_matches_with_no_timing_calls() {
    let fixture = ChildFixture::new("fiber-builtin-untimed");
    let names = ChildFixture::new("fiber-builtin-names");
    let probe = Arc::new(Probe {
        calls: AtomicUsize::new(0),
        closed: AtomicUsize::new(0),
    });
    let (tools, images) = session(&fixture, None);
    let timed_names: Vec<String> = {
        let (timed_tools, _) = session(&names, Some(timing(&probe)));
        timed_tools
            .iter()
            .map(|(_, tool)| tool.definition().name)
            .collect()
    };

    let output = read_output(read_tool(&tools));
    assert_eq!(output.error, None);
    assert!(
        output
            .content
            .iter()
            .any(|part| matches!(part, contract::shapes::ContentPart::Image { .. })),
        "{output:?}"
    );
    let bytes = PNG.to_vec();
    let reference = fakes::within("the images call", CALL_WITHIN, move || {
        images.process(&bytes, &Never)
    })
    .expect("the image");
    assert_eq!(reference.width, 1);
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0, "no callable runs");
    let tool_names: Vec<String> = tools
        .iter()
        .map(|(_, tool)| tool.definition().name)
        .collect();
    assert_eq!(tool_names, timed_names, "the same registration");
    fixture.finish(CALL_WITHIN);
    names.finish(CALL_WITHIN);
}
