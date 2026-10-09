//! Tests for Changed files' ranking, shell query, answers and frame
//! (`docs/tui.md`, "Swapped views").

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use contract::clock::Clock;
use contract::events::CommandResult;
use contract::shapes::Process;
use contract::{ActionId, Envelope, SCHEMA_VERSION, SessionId};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{Diff, answer, diff_command, frame, ranked};
use crate::app::{App, Effect, SessionView};
use crate::keys::Key;
use crate::link::Line;
use crate::swapped::List;

fn changes(entries: &[(&str, u64, u64)]) -> BTreeMap<String, (u64, u64)> {
    entries
        .iter()
        .map(|(path, added, removed)| ((*path).to_owned(), (*added, *removed)))
        .collect()
}

fn shell(output: &str, exit_code: Option<i32>, signal: Option<&str>) -> CommandResult {
    CommandResult::Shell {
        output: output.to_owned(),
        artifact: None,
        process: Process {
            exit_code,
            signal: signal.map(str::to_owned),
            timed_out: false,
        },
    }
}

fn row(frame: &crate::swapped::Frame, at: usize) -> String {
    frame
        .rows
        .get(at)
        .into_iter()
        .flatten()
        .map(|(text, _)| text.as_str())
        .collect()
}

fn screen(app: &App) -> String {
    let area = Rect::new(0, 0, 80, 24);
    let mut buffer = Buffer::empty(area);
    crate::view::render(app, area, &mut buffer, None);
    crate::view::text(&buffer)
}

fn app_with_files() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.attach(SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app.set_size(80, 24);
    app.on_line(Line::Session(Envelope {
        kind: "tool_call_completed".to_owned(),
        session_id: SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(ActionId("a_1".to_owned())),
        seq: None,
        payload: serde_json::json!({
            "status": "completed", "content": [],
            "changes": [
                {"path": "src/a.rs", "added": 4, "removed": 2},
                {"path": "src/b.rs", "added": 1, "removed": 0},
            ],
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }));
    assert_eq!(
        app.open_session_view(SessionView::ChangedFiles),
        Effect::None
    );
    app
}

fn app_with_diff() -> App {
    let mut app = app_with_files();
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    let Effect::Send(lines) = app.on_key(Key::Enter, fakes::clock::FakeClock::new().now()) else {
        panic!("choosing a file sends its shell request");
    };
    let request: serde_json::Value = serde_json::from_str(&lines[0])
        .unwrap_or_else(|error| panic!("shell request JSON: {error}"));
    app.on_line(Line::Session(Envelope {
        kind: "command_accepted".to_owned(),
        session_id: SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(ActionId("a_1".to_owned())),
        seq: None,
        payload: serde_json::json!({
            "command_id": request["id"],
            "result": {"output": "diff --git a/src/a.rs b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
                "process": {"exit_code": 1, "timed_out": false}},
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }));
    app
}

#[test]
fn ranked_orders_by_changed_lines_then_path() {
    let changes = changes(&[
        ("z.rs", 5, 1),
        ("b.rs", 4, 2),
        ("a.rs", 3, 3),
        ("large.rs", 8, 0),
    ]);
    let paths: Vec<&str> = ranked(&changes)
        .into_iter()
        .map(|(path, _, _)| path)
        .collect();
    assert_eq!(paths, ["large.rs", "a.rs", "b.rs", "z.rs"]);
}

#[test]
fn ranked_breaks_equal_change_totals_by_path_ascending() {
    let changes = changes(&[("z.rs", 3, 3), ("a.rs", 5, 1), ("m.rs", 4, 2)]);
    let paths: Vec<&str> = ranked(&changes)
        .into_iter()
        .map(|(path, _, _)| path)
        .collect();
    assert_eq!(paths, ["a.rs", "m.rs", "z.rs"]);
}

#[test]
fn diff_command_quotes_plain_spaced_quoted_and_leading_dash_paths() {
    for (path, quoted) in [
        ("src/a.rs", "'src/a.rs'"),
        ("src/a b.rs", "'src/a b.rs'"),
        ("src/a'b.rs", "'src/a'\\''b.rs'"),
        ("src/new\nline.rs", "'src/new\nline.rs'"),
        ("-a.rs", "'-a.rs'"),
    ] {
        let command = diff_command(path);
        assert!(
            command.contains(&format!("ls-files --error-unmatch -- {quoted}")),
            "{command}"
        );
        assert!(
            command.contains(&format!("diff --no-color --no-ext-diff HEAD -- {quoted}")),
            "{command}"
        );
        assert!(
            command.ends_with(&format!("--no-index -- /dev/null {quoted}")),
            "{command}"
        );
    }
}

#[test]
fn diff_command_keeps_glob_and_pathspec_magic_literal_on_both_tracked_queries() {
    for path in ["*", "src/[ab].rs", ":(top)x"] {
        let command = diff_command(path);
        let quoted = format!("'{path}'");
        assert!(
            command.contains(&format!(
                "--literal-pathspecs ls-files --error-unmatch -- {quoted}"
            )),
            "{command}"
        );
        assert!(
            command.contains(&format!(
                "--literal-pathspecs diff --no-color --no-ext-diff HEAD -- {quoted}"
            )),
            "{command}"
        );
    }
}

#[test]
fn diff_command_under_sh_reads_only_a_literal_untracked_star_path() {
    let dir = fakes::TempDir::new("tui-changed-files-git");
    let root = dir.path();
    let setup = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap_or_else(|error| panic!("run git {args:?}: {error}"));
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    setup(&["init", "-q"]);
    setup(&["config", "user.email", "fiber@example.invalid"]);
    setup(&["config", "user.name", "Fiber test"]);
    std::fs::write(root.join("a.rs"), "tracked original\n")
        .unwrap_or_else(|error| panic!("write a.rs: {error}"));
    setup(&["add", "--", "a.rs"]);
    setup(&["commit", "-qm", "initial"]);
    std::fs::write(root.join("a.rs"), "tracked changed\n")
        .unwrap_or_else(|error| panic!("change a.rs: {error}"));
    std::fs::write(root.join("*"), "literal-star-only\n")
        .unwrap_or_else(|error| panic!("write star path: {error}"));

    let command = diff_command("*");
    let mut child = Command::new("/bin/sh");
    child
        .args(["-c", command.as_str()])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = child
        .spawn()
        .unwrap_or_else(|error| panic!("spawn diff command: {error}"));
    let watchdog = fakes::Watchdog::matching(&command);
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let output = child
            .wait_with_output()
            .unwrap_or_else(|error| panic!("wait for diff command: {error}"));
        done.send(output).unwrap_or(());
    });
    let output = finished
        .recv_timeout(Duration::from_secs(10))
        .expect("waited for the shell diff command");
    watchdog.stand_down(Duration::from_secs(5));
    let stdout = String::from_utf8(output.stdout)
        .unwrap_or_else(|error| panic!("diff output was not UTF-8: {error}"));
    assert!(stdout.contains("literal-star-only"), "{stdout}");
    assert!(!stdout.contains("tracked changed"), "{stdout}");
}

#[test]
fn nonempty_output_is_shown_even_when_the_command_exits_one() {
    assert_eq!(
        answer(Some(&shell("diff --git a/a b/a\n", Some(1), None))),
        Diff::Lines {
            lines: vec!["diff --git a/a b/a".to_owned()],
            cut: None,
        }
    );
}

#[test]
fn empty_output_with_success_means_no_changes() {
    assert_eq!(answer(Some(&shell("", Some(0), None))), Diff::Empty);
}

#[test]
fn empty_output_with_nonzero_exit_or_signal_cannot_be_read() {
    assert_eq!(
        answer(Some(&shell("", Some(1), None))),
        Diff::Failed("The diff could not be read.".to_owned())
    );
    assert_eq!(
        answer(Some(&shell("", Some(0), Some("SIGTERM")))),
        Diff::Failed("The diff could not be read.".to_owned())
    );
}

#[test]
fn a_cut_answer_names_its_artifact() {
    let result = CommandResult::Shell {
        output: "diff line\n".to_owned(),
        artifact: Some("artifacts/full.diff".to_owned()),
        process: Process {
            exit_code: Some(1),
            signal: None,
            timed_out: false,
        },
    };
    assert_eq!(
        answer(Some(&result)),
        Diff::Lines {
            lines: vec!["diff line".to_owned()],
            cut: Some("artifacts/full.diff".to_owned()),
        }
    );
}

#[test]
fn tabs_expand_and_other_control_characters_are_removed() {
    let result = shell("\tred\u{1b}[31m\u{07}text\u{1} end\n", Some(1), None);
    assert_eq!(
        answer(Some(&result)),
        Diff::Lines {
            lines: vec!["    red[31mtext end".to_owned()],
            cut: None,
        }
    );
}

#[test]
fn a_missing_or_non_shell_answer_cannot_be_read() {
    assert_eq!(
        answer(None),
        Diff::Failed("The diff could not be read.".to_owned())
    );
    assert_eq!(
        answer(Some(&CommandResult::Tools { tools: Vec::new() })),
        Diff::Failed("The diff could not be read.".to_owned())
    );
}

#[test]
fn frames_cover_list_reading_diff_empty_and_failed_answers() {
    let changes = changes(&[("src/a.rs", 4, 2), ("src/b.rs", 1, 0)]);
    let list = frame(&changes, None, List::default());
    assert_eq!(list.title, "Changed files");
    assert_eq!(row(&list, 0), "src/a.rs  +4 −2");
    assert_eq!(row(&list, 1), "src/b.rs  +1 −0");
    assert_eq!(list.below, ["2 files changed  +5 −2"]);
    assert_eq!(list.footer, "↑↓ move · Enter show diff · Esc close");

    let reading = frame(
        &changes,
        Some(("src/a.rs", &Diff::Reading)),
        List::default(),
    );
    assert_eq!(
        reading.title,
        "Changed files › src/a.rs · diff against HEAD"
    );
    assert_eq!(row(&reading, 0), "Reading the diff…");

    let diff = Diff::Lines {
        lines: vec!["+new".to_owned()],
        cut: None,
    };
    assert_eq!(
        row(
            &frame(&changes, Some(("src/a.rs", &diff)), List::default()),
            0
        ),
        "+new"
    );
    assert_eq!(
        row(
            &frame(&changes, Some(("src/a.rs", &Diff::Empty)), List::default()),
            0
        ),
        "No changes against HEAD."
    );
    let failed = Diff::Failed("denied".to_owned());
    assert_eq!(
        row(
            &frame(&changes, Some(("src/a.rs", &failed)), List::default()),
            0
        ),
        "denied"
    );
    let cut = Diff::Lines {
        lines: vec!["line".to_owned()],
        cut: Some("artifacts/full.diff".to_owned()),
    };
    assert_eq!(
        frame(&changes, Some(("src/a.rs", &cut)), List::default()).below,
        ["Cut: the whole diff is in artifacts/full.diff"]
    );
}

#[test]
fn list_and_diff_screens_render_as_whole_80_by_24_frames() {
    insta::assert_snapshot!("changed_files_80x24", screen(&app_with_files()));
    insta::assert_snapshot!("changed_files_diff_80x24", screen(&app_with_diff()));
}
