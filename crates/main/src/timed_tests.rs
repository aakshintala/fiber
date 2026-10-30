//! `builtin_timed` wires one timing into both image drivers: the file
//! tools' child, reached through the `read` tool, and the shared
//! standalone child. A stub `fiber` answers both, so no image code runs.

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use contract::diag::{Op, Timing};
use contract::tool::Cancel;
use fakes::Deadline;
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
/// before any stub exists. The executable path interposes a test-binary
/// wrapper that starts the answering stub in its own guarded process group.
/// Drivers reap their children; [`ChildFixture::finish`] checks both the
/// wrappers and their groups before standing the watchdog down. A panic or
/// timeout drops the outer watchdog, whose wrapper's group watchdog kills
/// the answering group (`docs/testing.md`, "Running tests").
struct ChildFixture {
    root: fakes::TempDir,
    leaf: String,
    groups: std::path::PathBuf,
    fiber: std::path::PathBuf,
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
        let answer = fakes::script(
            root.path(),
            "fiber-answer",
            "cat >/dev/null; printf '{\"file\":\"%s.png\",\"mime_type\":\"image/png\",\"width\":1,\"height\":1}\\n' \"$4\"",
        );
        let groups = root.path().join("stub-groups");
        let output = root.path().join("stub-output");
        let executable = std::env::current_exe().expect("the test executable");
        let fiber = fakes::script(
            root.path(),
            "fiber-stub",
            &format!(
                "{} --ignored --exact builtin::timed_tests::guarded_stub_child -- {} {} {} \"$@\" > /dev/null\nstatus=$?\n[ \"$status\" -eq 0 ] || exit \"$status\"\nwhile IFS= read -r line || [ -n \"$line\" ]; do printf '%s\\n' \"$line\"; done < {}",
                shell_quote(&executable),
                shell_quote(&answer),
                shell_quote(&groups),
                shell_quote(&output),
                shell_quote(&output),
            ),
        );
        Self {
            root,
            leaf,
            groups,
            fiber,
            matching,
        }
    }

    /// Checks the success path after drivers reap their wrappers and groups.
    /// Panic and timeout paths drop the outer watchdog; each wrapper's group
    /// watchdog then kills its answering group.
    fn finish(self, within: Duration) {
        assert!(
            fakes::matching_exits(&self.leaf, fakes::Deadline::after(within)),
            "a stub wrapper outlived the test"
        );
        for group in self.group_ids() {
            assert!(
                fakes::group_empties(group, fakes::Deadline::after(within)),
                "stub process group {group} outlived the test"
            );
        }
        self.matching.stand_down(fakes::Deadline::after(within));
    }

    fn group_ids(&self) -> Vec<u32> {
        match std::fs::read_to_string(&self.groups) {
            Ok(ids) => ids
                .lines()
                .map(|id| id.parse().expect("a recorded process-group id"))
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("reading {}: {error}", self.groups.display()),
        }
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
    let fiber = fixture.fiber.clone();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let sandbox = std::sync::Arc::new(std::sync::Mutex::new(tools::SandboxSettings {
        enabled: false,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: root.path().join("home"),
    }));
    let artifacts = root.path().join("artifacts");
    let images = super::session_images(fiber.clone(), &artifacts, timing.clone());
    let (tools, _infos, _driver, _forget, images) = super::builtin_timed(
        fiber,
        &root.path().join("home"),
        root.path(),
        &artifacts,
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        images,
        None,
        &super::tests::delegates(),
        super::tests::skills(),
        timing,
        None,
        Default::default(),
        sandbox,
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
    fakes::within("the read call", Deadline::after(CALL_WITHIN), move || {
        read.run(&arguments, &Never, &Quiet)
    })
}

/// Entry point for the fixture wrapper. The answering stub is a process
/// group leader; this helper's watchdog owns that group even if the wrapper
/// process or the test process is killed before the stub exits.
#[test]
#[ignore = "started only by the timed-test stub wrapper"]
fn guarded_stub_child() {
    let mut arguments = std::env::args_os().skip_while(|arg| arg != "--").skip(1);
    let answer = arguments.next().expect("the answering stub path");
    let groups = arguments.next().expect("the process-group ledger");
    let output_path = arguments.next().expect("the answer output path");
    // A script the test wrote is never executed directly: its first
    // execution can stall on macOS, so it always runs through /bin/sh.
    let mut child_command = Command::new("/bin/sh");
    child_command
        .arg(answer)
        .args(arguments)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .process_group(0);
    let child = child_command.spawn().expect("the answering stub");
    let group = child.id();
    let watchdog = Watchdog::group(group);
    use std::io::Write as _;
    let mut ledger = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(groups)
        .expect("the process-group ledger");
    writeln!(ledger, "{group}").expect("recording the process group");
    drop(ledger);

    let output = child
        .wait_with_output()
        .expect("waiting for the answering stub");
    std::fs::write(output_path, &output.stdout).expect("saving the answering output");
    assert!(
        fakes::group_empties(group, fakes::Deadline::after(CALL_WITHIN)),
        "answering process group {group} did not empty"
    );
    watchdog.stand_down(fakes::Deadline::after(CALL_WITHIN));
    assert!(
        output.status.success(),
        "answering stub exited with {}",
        output.status
    );
}

fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
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
    let reference = fakes::within("the images call", Deadline::after(CALL_WITHIN), move || {
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
    let reference = fakes::within("the images call", Deadline::after(CALL_WITHIN), move || {
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

#[cfg(target_os = "linux")]
#[test]
fn sandbox_unavailable_reports_once_without_wall_clock() {
    let dir = fakes::TempDir::new("fiber-timed-sandbox-unavailable");
    let workspace = dir.path().to_path_buf();
    let home_base = std::env::var_os("HOME").map_or_else(
        || std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from,
    );
    let fiber_home = home_base.join(format!("fiber-timed-sandbox-{}", std::process::id()));
    let denied = workspace.join("secret");
    let settings = tools::SandboxSettings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: vec![denied.clone()],
        fiber_home,
    };
    let first = tools::sandbox_check_available(&settings, &workspace).expect_err("overlap");
    let second = tools::sandbox_check_available(&settings, &workspace).expect_err("overlap");
    // The same named case twice, not the same reason string: the check
    // takes no wall-clock time to repeat.
    for unavailable in [&first, &second] {
        assert_eq!(
            *unavailable,
            tools::SandboxUnavailable::DenyInsideWrite {
                configured: denied.clone(),
                overlapping: denied.clone(),
                root: workspace.clone(),
            }
        );
    }
}
