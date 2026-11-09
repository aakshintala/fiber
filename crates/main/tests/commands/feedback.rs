//! Binary-level tests of `fiber feedback` (`docs/testing.md`, "Levels"):
//! the built `fiber` writes a report file and prints its path with the
//! issue link, or fails as a usage error. Each run goes to its exit under
//! the test's [`support::Deadline`].

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use crate::support;

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::Watchdog;
use serde_json::json;
use support::{Setup, expired};

/// Runs `fiber` with `args`: `stdin` writes the text on a thread, as
/// `ask`'s runner does, since a child that never reads still ends the
/// exit wait within its bound. A missing `stdin` inherits the null pipe.
#[track_caller]
fn run(
    setup: &Setup,
    what: &str,
    args: &[&str],
    stdin: Option<Stdio>,
    text: Option<&str>,
) -> Output {
    let mut command: Command = setup.fiber(args);
    if let Some(stdin) = stdin {
        command.stdin(stdin);
    }
    let mut child = command.spawn().unwrap();
    if let (Some(mut pipe), Some(text)) = (child.stdin.take(), text) {
        let text = text.to_owned();
        thread::spawn(move || match pipe.write_all(text.as_bytes()) {
            Ok(()) | Err(_) => {}
        });
    }
    wait(setup, what, child)
}

/// The exit wait `run_to_exit` runs: the output under the deadline, and
/// no process left in the group behind.
#[track_caller]
fn wait(setup: &Setup, what: &str, child: Child) -> Output {
    let group = child.id();
    let watchdog = Watchdog::group(group);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match setup.deadline.recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(_) => expired(setup.deadline, group, &finished, &format!("{what} to exit")),
    };
    assert!(
        !fakes::group_alive(group),
        "{what} left a process in its group behind"
    );
    watchdog.stand_down(setup.deadline.cleanup_phase());
    output
}

// debt: one stdin-terminal opener in main/tests/support shared with ask.rs and login.rs, when a third lane touches any of the three copies
/// A pseudo-terminal, opened through rustix's safe calls. The main side
/// stays open while the run uses the terminal side, so stdin is a
/// terminal, as `ask`'s runner holds it.
struct Terminal {
    #[allow(dead_code, reason = "holding the main side keeps the terminal open")]
    main: OwnedFd,
    terminal: fs::File,
}

impl Terminal {
    #[track_caller]
    fn open() -> Self {
        let main =
            rustix::pty::openpt(rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY)
                .unwrap();
        rustix::pty::grantpt(&main).unwrap();
        rustix::pty::unlockpt(&main).unwrap();
        let name = rustix::pty::ptsname(&main, Vec::new()).unwrap();
        let path = PathBuf::from(OsStr::from_bytes(name.as_bytes()));
        let terminal = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        Self { main, terminal }
    }

    fn stdin(&self) -> Stdio {
        Stdio::from(self.terminal.try_clone().unwrap())
    }
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn report_path(output: &Output) -> PathBuf {
    let text = stdout_text(output);
    let mut lines = text.lines();
    let path = lines.next().unwrap();
    let link = lines.next().unwrap();
    assert!(
        link.starts_with("https://github.com/aakshintala/fiber/issues/new?"),
        "{link}"
    );
    assert!(lines.next().is_none());
    PathBuf::from(path)
}

/// A fake home holding a session marker: the report must never carry
/// session artifacts, only version, platform, description and doctor
/// output.
fn home_with_session_marker(setup: &Setup) {
    let marker = setup
        .home()
        .join("sessions")
        .join("s_0123456789abcdef")
        .join("marker");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(&marker, "session marker s_0123456789abcdef").unwrap();
}

fn assert_report(output: &Output, description: &str) -> String {
    let path = report_path(output);
    assert!(path.is_absolute());
    let report = fs::read_to_string(&path).unwrap();
    assert!(
        report.contains(env!("CARGO_PKG_VERSION")),
        "version: {report}"
    );
    assert!(
        report.contains(std::env::consts::OS) && report.contains(std::env::consts::ARCH),
        "platform: {report}"
    );
    assert!(report.contains(description), "description: {report}");
    assert!(
        report.contains("hub: not running"),
        "doctor output: {report}"
    );
    assert!(
        !report.contains("s_0123456789abcdef") && !report.contains("session marker"),
        "no session marker: {report}"
    );
    report
}

#[test]
fn feedback_by_argument_writes_the_report_and_prints_the_link() {
    let setup = Setup::new();
    home_with_session_marker(&setup);
    let output = run(
        &setup,
        "feedback by argument",
        &["feedback", "the sound stopped"],
        None,
        None,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_report(&output, "the sound stopped");
}

#[test]
fn feedback_by_stdin_with_trailing_dash_writes_the_same_report() {
    let setup = Setup::new();
    home_with_session_marker(&setup);
    let output = run(
        &setup,
        "feedback by stdin",
        &["feedback", "-"],
        Some(Stdio::piped()),
        Some("cold air"),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_report(&output, "cold air");
}

#[test]
fn feedback_empty_description_is_a_usage_error() {
    let setup = Setup::new();
    let output = run(
        &setup,
        "feedback without a description",
        &["feedback"],
        None,
        None,
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(
        stderr_text(&output),
        "fiber: No description. Run `fiber feedback \"<description>\"` or `fiber feedback < <file>`.\n"
    );
}

#[test]
fn feedback_stdin_terminal_without_arg_is_a_usage_error() {
    let setup = Setup::new();
    let terminal = Terminal::open();
    let output = run(
        &setup,
        "feedback on a terminal without a description",
        &["feedback"],
        Some(terminal.stdin()),
        None,
    );
    drop(terminal);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(
        stderr_text(&output),
        "fiber: No description. Run `fiber feedback \"<description>\"` or `fiber feedback < <file>`.\n"
    );
}

#[test]
fn feedback_extra_positionals_name_the_shape() {
    let setup = Setup::new();
    let output = run(
        &setup,
        "feedback with two descriptions",
        &["feedback", "one", "two"],
        None,
        None,
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(
        stderr_text(&output),
        "fiber: `fiber feedback` takes one description, then an optional `-`; quote the description. Run `fiber --help` for usage.\n"
    );
}

#[test]
fn feedback_unreadable_stdin_reports_the_read() {
    let setup = Setup::new();
    let directory = fs::File::open(setup.workspace()).unwrap();
    let output = run(
        &setup,
        "feedback with unreadable stdin",
        &["feedback"],
        Some(Stdio::from(directory)),
        None,
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        stderr_text(&output)
    );
    assert!(
        stderr_text(&output).starts_with("fiber: stdin could not be read: "),
        "stderr: {}",
        stderr_text(&output)
    );
}

/// Text that must never reach a report: it is planted in every place
/// session content, a credential or an extension's own text lives.
const MARKER: &str = "hunter2-marker";

const NEWER: &str = "s_00000000000000b2";
const OLDER: &str = "s_00000000000000a1";
const ELSEWHERE: &str = "s_00000000000000c3";
const DELEGATE: &str = "s_00000000000000d4";
const UNREADABLE: &str = "s_00000000000000e5";

/// The directory the child runs in, as it resolves it: its project.
fn project(setup: &Setup) -> PathBuf {
    fs::canonicalize(setup.root.path()).unwrap()
}

/// A session of `workspace`'s project: a log holding a prompt with the
/// marker, an artifact holding it, and a first line that makes the
/// session a delegate of `parent` when given. `ts` is its last line's
/// time, which orders sessions for the no-flag choice.
fn session(setup: &Setup, workspace: &Path, id: &str, ts: u64, parent: Option<&str>) {
    let dir = log::sessions_dir(&setup.home(), workspace).join(id);
    fs::create_dir_all(dir.join("artifacts")).unwrap();
    let mut payload = json!({"workspace": workspace,
        "variables": {"path": "/usr/bin", "names": [], "source": "inherited"}});
    if let Some(parent) = parent {
        payload["parent"] = json!({"session_id": parent, "job_id": "j_1"});
    }
    let first = json!({"kind": "session_started", "session_id": id, "ts": 0,
        "schema_version": 1, "seq": 0, "payload": payload});
    let prompt = json!({"kind": "user_message", "session_id": id, "ts": ts,
        "schema_version": 1, "seq": 1, "payload": {"text": MARKER}});
    fs::write(dir.join("events.jsonl"), format!("{first}\n{prompt}\n")).unwrap();
    fs::write(dir.join("artifacts/a_1.txt"), MARKER).unwrap();
}

/// One diagnostic line of `process`, naming `id` when given.
fn diagnostic(process: &str, id: Option<&str>, code: &str, message: &str) -> String {
    let mut line = json!({"ts": 1, "level": "warn", "process": process,
        "code": code, "message": message});
    if let Some(id) = id {
        line["session_id"] = json!(id);
    }
    format!("{line}\n")
}

/// The files outside the log a report must not leak: diagnostics, a
/// stored credential and a crash report whose panic message is the marker.
fn leaky_files(setup: &Setup, id: &str) {
    let home = setup.home();
    fs::create_dir_all(home.join("logs")).unwrap();
    fs::create_dir_all(home.join("crashes")).unwrap();
    fs::create_dir_all(home.join("credentials")).unwrap();
    fs::write(
        home.join(format!("logs/session-{id}.log")),
        diagnostic("session", Some(id), "provider_failed", "kept-session-line")
            + &diagnostic("session", Some(id), "extension_log", MARKER),
    )
    .unwrap();
    fs::write(
        home.join("logs/hub.log"),
        diagnostic("hub", Some(id), "session_started", "kept-hub-line")
            + &diagnostic("hub", Some(ELSEWHERE), "session_started", "other-hub-line"),
    )
    .unwrap();
    fs::write(
        home.join(format!("crashes/{id}-5.txt")),
        format!(
            "thread 'worker' panicked at crates/x/src/y.rs:3:9:\n{MARKER}\nstack backtrace:\n   0: kept_frame\n"
        ),
    )
    .unwrap();
    fs::write(home.join("credentials/stored"), MARKER).unwrap();
}

/// The report files `fiber feedback` left in the directory it ran in.
fn reports(setup: &Setup) -> Vec<PathBuf> {
    fs::read_dir(setup.root.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with("fiber-feedback-"))
        })
        .collect()
}

#[test]
fn the_report_carries_the_newest_sessions_diagnostics_and_nothing_private() {
    let setup = Setup::new();
    let workspace = project(&setup);
    session(&setup, &workspace, OLDER, 5, None);
    session(&setup, &workspace, NEWER, 9, None);
    leaky_files(&setup, NEWER);
    fs::write(
        setup.home().join(format!("logs/tui-{OLDER}.log")),
        diagnostic("tui", Some(OLDER), "x", "older-session-line"),
    )
    .unwrap();
    let output = run(
        &setup,
        "feedback with diagnostics",
        &["feedback", "the sound stopped"],
        None,
        None,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let path = report_path(&output);
    let report = fs::read_to_string(&path).unwrap();
    for kept in [
        "## Diagnostics",
        &format!("Session: {NEWER}"),
        &format!("fiber sessions export {NEWER}"),
        "kept-session-line",
        "kept-hub-line",
        "thread 'worker' panicked at crates/x/src/y.rs:3:9:",
        "0: kept_frame",
    ] {
        assert!(report.contains(kept), "{kept}: {report}");
    }
    for left_out in [MARKER, "other-hub-line", "older-session-line", OLDER] {
        assert!(!report.contains(left_out), "{left_out}: {report}");
    }
    // The link, in either of its forms, never carries the diagnostics.
    let stdout = stdout_text(&output);
    for left_out in [MARKER, "Diagnostics", "kept-session-line", "kept_frame"] {
        assert!(!stdout.contains(left_out), "{left_out}: {stdout}");
    }
}

#[test]
fn an_explicit_session_may_belong_to_another_project() {
    let setup = Setup::new();
    let elsewhere = project(&setup).join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    session(&setup, &elsewhere, ELSEWHERE, 5, None);
    session(&setup, &project(&setup), NEWER, 9, None);
    leaky_files(&setup, ELSEWHERE);
    let output = run(
        &setup,
        "feedback for another project's session",
        &["feedback", "--session", ELSEWHERE, "the sound stopped"],
        None,
        None,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let report = fs::read_to_string(report_path(&output)).unwrap();
    assert!(
        report.contains(&format!("Session: {ELSEWHERE}")),
        "{report}"
    );
    assert!(report.contains("kept-session-line"), "{report}");
    assert!(!report.contains(NEWER), "{report}");
    assert!(!report.contains(MARKER), "{report}");
}

#[test]
fn a_session_with_no_diagnostics_here_still_gets_the_notice() {
    let setup = Setup::new();
    session(&setup, &project(&setup), NEWER, 9, None);
    let output = run(
        &setup,
        "feedback for a session with no diagnostics",
        &["feedback", "the sound stopped"],
        None,
        None,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let report = fs::read_to_string(report_path(&output)).unwrap();
    assert!(
        report.contains(&format!(
            "This machine has no diagnostic lines for session {NEWER}."
        )),
        "{report}"
    );
}

#[test]
fn without_a_session_in_the_project_the_report_holds_the_hub_only() {
    let setup = Setup::new();
    let elsewhere = project(&setup).join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    session(&setup, &elsewhere, ELSEWHERE, 5, None);
    leaky_files(&setup, ELSEWHERE);
    let output = run(
        &setup,
        "feedback with no session in the project",
        &["feedback", "the sound stopped"],
        None,
        None,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let report = fs::read_to_string(report_path(&output)).unwrap();
    assert!(report.contains("## Diagnostics"), "{report}");
    assert!(!report.contains("Session:"), "{report}");
    assert!(!report.contains("kept-session-line"), "{report}");
    assert!(!report.contains("0: kept_frame"), "{report}");
}

#[test]
fn a_session_that_names_no_usable_session_fails_and_writes_no_report() {
    let setup = Setup::new();
    let workspace = project(&setup);
    session(&setup, &workspace, DELEGATE, 5, Some(NEWER));
    let broken = log::sessions_dir(&setup.home(), &workspace).join(UNREADABLE);
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join("events.jsonl"), "not json\n").unwrap();
    session(&setup, &workspace, NEWER, 9, None);
    for id in ["s_00000000000000ff", DELEGATE, UNREADABLE] {
        let output = run(
            &setup,
            "feedback for no usable session",
            &["feedback", "--session", id, "the sound stopped"],
            None,
            None,
        );
        assert_eq!(
            output.status.code(),
            Some(1),
            "{id} stderr: {}",
            stderr_text(&output)
        );
        assert_eq!(stderr_text(&output), format!("fiber: No session `{id}`.\n"));
        assert!(stdout_text(&output).is_empty());
        assert!(reports(&setup).is_empty(), "{id}");
    }
}

#[test]
fn a_malformed_session_id_is_a_usage_error() {
    let setup = Setup::new();
    for id in ["s_abc", "../x", "S_00000000000000B2"] {
        let output = run(
            &setup,
            "feedback with a malformed session id",
            &["feedback", "--session", id, "the sound stopped"],
            None,
            None,
        );
        assert_eq!(
            output.status.code(),
            Some(2),
            "{id} stderr: {}",
            stderr_text(&output)
        );
        assert!(reports(&setup).is_empty(), "{id}");
    }
}

#[test]
fn a_session_recorded_for_another_workspace_is_not_this_projects_newest() {
    let setup = Setup::new();
    let elsewhere = project(&setup).join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    // A project key shared by two workspaces lands both in one sessions
    // directory: the first line says which one a session belongs to.
    session(&setup, &elsewhere, ELSEWHERE, 9, None);
    let ours = log::sessions_dir(&setup.home(), &project(&setup));
    fs::create_dir_all(&ours).unwrap();
    fs::rename(
        log::sessions_dir(&setup.home(), &elsewhere).join(ELSEWHERE),
        ours.join(ELSEWHERE),
    )
    .unwrap();
    leaky_files(&setup, ELSEWHERE);
    let output = run(
        &setup,
        "feedback with another workspace's session in the directory",
        &["feedback", "the sound stopped"],
        None,
        None,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let report = fs::read_to_string(report_path(&output)).unwrap();
    assert!(!report.contains("Session:"), "{report}");
    assert!(!report.contains("kept-session-line"), "{report}");
}

/// The headless door draws no frames: its report holds Diagnostics
/// with the empty-ring notice and no shape rows.
#[test]
fn headless_report_holds_the_empty_ring_notice() {
    let setup = Setup::new();
    let output = run(
        &setup,
        "headless feedback with no frames",
        &["feedback", "the sound stopped"],
        None,
        None,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let report = fs::read_to_string(report_path(&output)).unwrap();
    assert!(report.contains("## Diagnostics"), "{report}");
    assert!(
        report.contains("No frames captured on this machine."),
        "{report}"
    );
    assert!(!report.contains("frame 1/"), "{report}");
    assert!(!report.contains("palette "), "{report}");
}

/// A seeded 2-frame snapshot through a unit-level `ReportContext`:
/// the written shapes section parses back to the seed, proving the
/// door passes the slice through.
// `docs/user/reporting.md`, "Diagnostics" plus `docs/invocation.md`, "Commands and flags" (shapes of the last
// 20 painted frames with scroll state, at most 20): with 2 frames the whole
// seed survives in order, oldest first.
#[test]
fn seeded_shapes_section_parses_back_to_the_seed() {
    use contract::feedback::{FrameShape, ShapeCell, ShapeColor, ShapeKind, ShapeRow, StyleValue};
    fn base() -> StyleValue {
        StyleValue {
            fg: ShapeColor::Reset,
            bg: ShapeColor::Reset,
            ul: ShapeColor::Reset,
            mods: 0,
        }
    }
    let seed = vec![
        FrameShape {
            width: 4,
            height: 1,
            top: None,
            has_new: false,
            palette: vec![
                base(),
                StyleValue {
                    fg: ShapeColor::Red,
                    bg: ShapeColor::Indexed(7),
                    ul: ShapeColor::Rgb(1, 2, 3),
                    mods: 2,
                },
            ],
            rows: vec![ShapeRow::new(vec![
                ShapeCell::new(ShapeKind::Content, false, 1),
                ShapeCell::new(ShapeKind::Continuation, false, 1),
                ShapeCell::new(ShapeKind::Blank, false, 0),
                ShapeCell::new(ShapeKind::Exempt, false, 0),
            ])],
        },
        FrameShape {
            width: 2,
            height: 2,
            top: Some(5),
            has_new: true,
            palette: vec![base()],
            rows: vec![
                ShapeRow::new(vec![
                    ShapeCell::new(ShapeKind::Content, true, 0),
                    ShapeCell::new(ShapeKind::Continuation, false, 0),
                ]),
                ShapeRow::new(vec![
                    ShapeCell::new(ShapeKind::Blank, true, 0),
                    ShapeCell::new(ShapeKind::Continuation, false, 0),
                ]),
            ],
        },
    ];
    let root = fakes::TempDir::new("fiber-feedback-seed");
    let clock = fakes::clock::FakeClock::new();
    let context = cli::ReportContext {
        version: "0.3.0 (abc123)",
        cwd: root.path(),
        clock: clock.as_ref(),
        doctor: &|| Ok("doctor".to_owned()),
        diagnostics: &|_| String::new(),
        frames: &seed,
    };
    let done = cli::write("the sound stopped", None, &context).unwrap();
    let saved = fs::read_to_string(&done.path).unwrap();
    assert_eq!(parse_shapes(&saved), seed);
}

/// One wire colour name back to its colour.
fn parse_color(name: &str) -> Option<contract::feedback::ShapeColor> {
    use contract::feedback::ShapeColor;
    match name {
        "reset" => Some(ShapeColor::Reset),
        "black" => Some(ShapeColor::Black),
        "red" => Some(ShapeColor::Red),
        "green" => Some(ShapeColor::Green),
        "yellow" => Some(ShapeColor::Yellow),
        "blue" => Some(ShapeColor::Blue),
        "magenta" => Some(ShapeColor::Magenta),
        "cyan" => Some(ShapeColor::Cyan),
        "gray" => Some(ShapeColor::Gray),
        "darkgray" => Some(ShapeColor::DarkGray),
        "lightred" => Some(ShapeColor::LightRed),
        "lightgreen" => Some(ShapeColor::LightGreen),
        "lightyellow" => Some(ShapeColor::LightYellow),
        "lightblue" => Some(ShapeColor::LightBlue),
        "lightmagenta" => Some(ShapeColor::LightMagenta),
        "lightcyan" => Some(ShapeColor::LightCyan),
        "white" => Some(ShapeColor::White),
        indexed if indexed.starts_with("indexed(") => {
            let index = indexed
                .strip_prefix("indexed(")
                .and_then(|rest| rest.strip_suffix(')'))?;
            Some(ShapeColor::Indexed(index.parse().ok()?))
        }
        rgb if rgb.starts_with("rgb(") => {
            let triple = rgb
                .strip_prefix("rgb(")
                .and_then(|rest| rest.strip_suffix(')'))?;
            let mut channels = triple.split(',');
            Some(ShapeColor::Rgb(
                channels.next()?.parse().ok()?,
                channels.next()?.parse().ok()?,
                channels.next()?.parse().ok()?,
            ))
        }
        _ => None,
    }
}

/// One palette entry back to its style: split on the channel
/// prefixes, never on commas, which a truecolour triple holds.
fn parse_entry(entry: &str) -> contract::feedback::StyleValue {
    use contract::feedback::StyleValue;
    let (_, channels) = entry.split_once(':').unwrap();
    let (channels, mods) = channels.rsplit_once(",mods=").unwrap();
    let (fg, rest) = channels.split_once(",bg=").unwrap();
    let (bg, ul) = rest.split_once(",ul=").unwrap();
    StyleValue {
        fg: parse_color(fg.strip_prefix("fg=").unwrap()).unwrap(),
        bg: parse_color(bg).unwrap(),
        ul: parse_color(ul).unwrap(),
        mods: mods.parse().unwrap(),
    }
}

/// One frame header back to its geometry and scroll state.
fn parse_header(header: &str) -> (u16, u16, Option<usize>, bool) {
    let mut parts = header.split(", ");
    parts.next().unwrap();
    let geometry = parts.next().unwrap();
    let (width, height) = geometry.split_once('x').unwrap();
    let top = parts.next().unwrap().strip_prefix("top=").unwrap();
    let has_new = parts.next().unwrap().strip_prefix("has_new=").unwrap();
    (
        width.parse().unwrap(),
        height.parse().unwrap(),
        if top == "none" {
            None
        } else {
            Some(
                top.strip_prefix("Some(")
                    .and_then(|rest| rest.strip_suffix(')'))
                    .unwrap()
                    .parse()
                    .unwrap(),
            )
        },
        has_new == "true",
    )
}

/// The report's shapes section back to its frames.
fn parse_shapes(saved: &str) -> Vec<contract::feedback::FrameShape> {
    use contract::feedback::{FrameShape, ShapeCell, ShapeKind, ShapeRow};
    fn wire(kind: char) -> Option<(ShapeKind, bool)> {
        match kind {
            '.' => Some((ShapeKind::Blank, false)),
            ':' => Some((ShapeKind::Blank, true)),
            '#' => Some((ShapeKind::Content, false)),
            'W' => Some((ShapeKind::Content, true)),
            '>' => Some((ShapeKind::Continuation, false)),
            '~' => Some((ShapeKind::Exempt, false)),
            _ => None,
        }
    }
    let rest = saved.split("### Frame shapes\n\n").nth(1).unwrap();
    let mut pieces = rest.split("palette ");
    pieces.next().unwrap();
    let mut frames = Vec::new();
    for piece in pieces {
        let mut lines = piece.lines();
        let palette = lines
            .next()
            .unwrap()
            .split(' ')
            .map(parse_entry)
            .collect::<Vec<_>>();
        let (width, height, top, has_new) = parse_header(lines.next().unwrap());
        let mut rows = Vec::new();
        while let Some(shape) = lines.next() {
            if shape.is_empty() {
                break;
            }
            let shape = shape
                .strip_prefix("shape \"")
                .and_then(|line| line.strip_suffix('"'))
                .unwrap();
            let style = lines
                .next()
                .unwrap()
                .strip_prefix("style \"")
                .and_then(|line| line.strip_suffix('"'))
                .unwrap();
            let cells = shape
                .chars()
                .zip(style.split(','))
                .map(|(glyph, id)| {
                    let (kind, wide) = wire(glyph).unwrap();
                    ShapeCell::new(kind, wide, id.parse().unwrap())
                })
                .collect::<Vec<_>>();
            rows.push(ShapeRow::new(cells));
        }
        frames.push(FrameShape {
            width,
            height,
            top,
            has_new,
            palette,
            rows,
        });
    }
    frames
}
