//! Tests for Changed files' ranking, shell query, answers and frame
//! (`docs/tui.md`, "Swapped views").

use fakes::MUST_SUCCEED_WITHIN;
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

use super::{Diff, answer, diff_command, frame, numstat_command, ranked, resolve_figures};
use crate::app::{App, Effect, SessionView};
use crate::keys::Key;
use crate::link::Line;
use crate::swapped::List;
use fakes::Deadline;

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
        .map(|(text, _, _)| text.as_str())
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
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
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
    let Effect::Send(lines) = app.open_session_view(SessionView::ChangedFiles) else {
        panic!("opening the list sends the numstat query");
    };
    // docs/tui.md:830 promises one numstat query, run when the view opens.
    assert_eq!(lines.len(), 1);
    let request: serde_json::Value = serde_json::from_str(&lines[0])
        .unwrap_or_else(|error| panic!("shell request JSON: {error}"));
    assert_eq!(
        request["args"]["command"],
        numstat_command(&["src/a.rs", "src/b.rs"])
    );
    app
}

fn app_with_diff() -> App {
    let mut app = app_with_files();
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
fn a_file_row_is_its_path_bold_then_dim_dot_added_and_removed() {
    use crate::swapped::Ink;
    use crate::theme::Role;
    let frame = frame(&changes(&[("src/a.rs", 4, 2)]), None, None, List::default());
    assert_eq!(
        frame.rows[0],
        [
            ("src/a.rs".to_owned(), None, Ink::Heading),
            (" · ".to_owned(), None, Ink::Muted),
            ("+4".to_owned(), None, Ink::Role(Role::Added)),
            (" ".to_owned(), None, Ink::Plain),
            ("−2".to_owned(), None, Ink::Role(Role::Removed)),
        ]
    );
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
    let output = Deadline::after(Duration::from_secs(10))
        .recv(&finished)
        .expect("waited for the shell diff command");
    watchdog.stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
    let stdout = String::from_utf8(output.stdout)
        .unwrap_or_else(|error| panic!("diff output was not UTF-8: {error}"));
    assert!(stdout.contains("literal-star-only"), "{stdout}");
    assert!(!stdout.contains("tracked changed"), "{stdout}");
}

#[test]
fn numstat_command_lists_the_given_paths_in_one_numstat_call() {
    let command = numstat_command(&["src/a.rs", "src/b c.rs"]);
    assert!(command.starts_with("git "), "{command}");
    assert!(command.contains("--numstat"), "{command}");
    assert!(command.contains("HEAD --"), "{command}");
    assert!(command.contains("'src/a.rs'"), "{command}");
    assert!(command.contains("'src/b c.rs'"), "{command}");
    assert!(!command.contains("ls-files"), "{command}");
    assert!(!command.contains("--no-index"), "{command}");
    assert!(!command.contains("&&"), "{command}");
    assert_eq!(command.matches("git ").count(), 1, "{command}");
}

#[test]
fn numstat_command_quotes_embedded_quotes() {
    let command = numstat_command(&["src/a'b.rs"]);
    assert!(command.ends_with("'src/a'\\''b.rs'"), "{command}");
}

#[test]
fn parse_numstat_reads_counts_and_marks_binary() {
    use super::{NumstatFigure, parse_numstat};
    let parsed = parse_numstat("10\t3\tsrc/a.rs\n-\t-\tbin/x\nnot a row\n\n7\t0\tmy file.rs\n");
    assert_eq!(parsed.get("src/a.rs"), Some(&NumstatFigure::Counted(10, 3)));
    assert_eq!(parsed.get("bin/x"), Some(&NumstatFigure::Binary));
    assert_eq!(
        parsed.get("my file.rs"),
        Some(&NumstatFigure::Counted(7, 0))
    );
    assert_eq!(parsed.len(), 3);
}

#[test]
fn parse_numstat_drops_a_row_with_a_dash_in_only_one_column() {
    use super::{NumstatFigure, parse_numstat};
    let parsed = parse_numstat("-\t5\tsrc/half-a.rs\n5\t-\tsrc/half-b.rs\n7\t1\tsrc/full.rs\n");
    assert!(!parsed.contains_key("src/half-a.rs"));
    assert!(!parsed.contains_key("src/half-b.rs"));
    assert_eq!(
        parsed.get("src/full.rs"),
        Some(&NumstatFigure::Counted(7, 1))
    );
}

#[test]
fn resolve_figures_prefers_git_counts_untracked_lines_then_card() {
    let card = changes(&[
        ("src/a.rs", 4, 2),
        ("bin/x", 1, 1),
        ("new.txt", 4, 1),
        ("gone.txt", 2, 0),
    ]);
    let answered = "7\t1\tsrc/a.rs\n-\t-\tbin/x\n";
    let mut lines = BTreeMap::new();
    lines.insert("new.txt".to_owned(), 3u64);
    let displayed = resolve_figures(&card, Some(answered), &lines, None);
    assert_eq!(displayed.figures.get("src/a.rs"), Some(&(7, 1)));
    // A binary path keeps the card's figures.
    assert_eq!(displayed.figures.get("bin/x"), Some(&(1, 1)));
    // An answered-but-omitted path resolves to its line count as added.
    assert_eq!(displayed.figures.get("new.txt"), Some(&(3, 0)));
    // An answered-but-omitted and uncounted path keeps the card's figures.
    assert_eq!(displayed.figures.get("gone.txt"), Some(&(2, 0)));
    // Only the counted omitted path draws the untracked row form.
    assert!(displayed.untracked.contains("new.txt"));
    assert!(!displayed.untracked.contains("gone.txt"));
    assert!(!displayed.untracked.contains("bin/x"));
    assert!(!displayed.untracked.contains("src/a.rs"));
}

#[test]
fn resolve_figures_without_an_answer_keeps_the_card() {
    let card = changes(&[("src/a.rs", 4, 2)]);
    let mut lines = BTreeMap::new();
    lines.insert("src/a.rs".to_owned(), 9u64);
    assert_eq!(resolve_figures(&card, None, &lines, None).figures, card);
    assert!(
        resolve_figures(&card, None, &lines, None)
            .untracked
            .is_empty()
    );
}

#[test]
fn numstat_command_output_parses_and_resolves_against_git() {
    let dir = fakes::TempDir::new("tui-changed-files-numstat");
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
    std::fs::write(root.join("a.rs"), "one\ntwo\n")
        .unwrap_or_else(|error| panic!("write a.rs: {error}"));
    setup(&["add", "--", "a.rs"]);
    setup(&["commit", "-qm", "initial"]);
    std::fs::write(root.join("a.rs"), "one\nTWO\nthree\n")
        .unwrap_or_else(|error| panic!("change a.rs: {error}"));
    std::fs::write(root.join("new.txt"), "x\ny\nz\n")
        .unwrap_or_else(|error| panic!("write new.txt: {error}"));

    let command = numstat_command(&["a.rs", "new.txt"]);
    let mut child = Command::new("/bin/sh");
    child
        .args(["-c", command.as_str()])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = child
        .spawn()
        .unwrap_or_else(|error| panic!("spawn numstat command: {error}"));
    let watchdog = fakes::Watchdog::matching(&command);
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let output = child
            .wait_with_output()
            .unwrap_or_else(|error| panic!("wait for numstat command: {error}"));
        done.send(output).unwrap_or(());
    });
    let output = Deadline::after(Duration::from_secs(10))
        .recv(&finished)
        .expect("waited for the shell numstat command");
    watchdog.stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
    let stdout = String::from_utf8(output.stdout)
        .unwrap_or_else(|error| panic!("numstat output was not UTF-8: {error}"));
    assert!(output.status.success(), "numstat failed: {stdout}");
    // The card's sums are deliberately wrong: the resolved figures
    // must be git's, not the card's.
    let card = changes(&[("a.rs", 5, 5), ("new.txt", 9, 9)]);
    let mut lines = BTreeMap::new();
    lines.insert("new.txt".to_owned(), 3u64);
    let displayed = resolve_figures(&card, Some(&stdout), &lines, None);
    assert_eq!(displayed.figures.get("a.rs"), Some(&(2, 1)));
    assert_eq!(displayed.figures.get("new.txt"), Some(&(3, 0)));
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
    let list = frame(&changes, None, None, List::default());
    assert_eq!(list.title, "Changed files");
    assert_eq!(row(&list, 0), "src/a.rs · +4 −2");
    assert_eq!(row(&list, 1), "src/b.rs · +1 −0");
    assert_eq!(list.below, ["2 files changed  +5 −2"]);
    assert_eq!(list.footer, "↑↓ move · Enter show diff · Esc close");

    let reading = frame(
        &changes,
        None,
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
            &frame(&changes, None, Some(("src/a.rs", &diff)), List::default()),
            0
        ),
        "+new"
    );
    assert_eq!(
        row(
            &frame(
                &changes,
                None,
                Some(("src/a.rs", &Diff::Empty)),
                List::default()
            ),
            0
        ),
        "No changes against HEAD."
    );
    let failed = Diff::Failed("denied".to_owned());
    assert_eq!(
        row(
            &frame(&changes, None, Some(("src/a.rs", &failed)), List::default()),
            0
        ),
        "denied"
    );
    let cut = Diff::Lines {
        lines: vec!["line".to_owned()],
        cut: Some("artifacts/full.diff".to_owned()),
    };
    assert_eq!(
        frame(&changes, None, Some(("src/a.rs", &cut)), List::default()).below,
        ["Cut: the whole diff is in artifacts/full.diff"]
    );
}

#[test]
fn list_and_diff_screens_render_as_whole_80_by_24_frames() {
    insta::assert_snapshot!("changed_files_80x24", screen(&app_with_files()));
    insta::assert_snapshot!("changed_files_diff_80x24", screen(&app_with_diff()));
}

/// The diff frame over `lines` for `src/a.rs`.
fn diff_frame_of(lines: &[&str]) -> crate::swapped::Frame {
    let diff = Diff::Lines {
        lines: lines.iter().map(|line| line.to_string()).collect(),
        cut: None,
    };
    frame(
        &changes(&[("src/a.rs", 1, 1)]),
        None,
        Some(("src/a.rs", &diff)),
        List::default(),
    )
}

#[test]
fn the_diff_frame_drops_git_headers() {
    let frame = diff_frame_of(&[
        "diff --git a/a.rs b/a.rs",
        "index 123..456 100644",
        "--- a/a.rs",
        "+++ b/a.rs",
        "@@ -1 +1 @@",
        "-old",
        "+new",
    ]);
    let texts: Vec<String> = frame
        .rows
        .iter()
        .map(|cells| cells.iter().map(|(text, _, _)| text.as_str()).collect())
        .collect();
    assert_eq!(
        texts,
        vec![
            "  @@ -1 +1 @@".to_owned(),
            "1 -old".to_owned(),
            "1 +new".to_owned(),
        ]
    );
}

#[test]
fn the_diff_frame_has_a_gutter_from_the_hunk_header() {
    let frame = diff_frame_of(&["@@ -9 +10 @@", "-old", "+new"]);
    let guttered: Vec<String> = frame.rows[1..]
        .iter()
        .map(|cells| cells.iter().map(|(text, _, _)| text.as_str()).collect())
        .collect();
    assert_eq!(guttered[0], " 9 -old");
    assert_eq!(guttered[1], "10 +new");
    // The gutter is dim; the hunk row is dim too.
    assert!(matches!(frame.rows[1][0].2, crate::swapped::Ink::Muted));
    assert!(matches!(frame.rows[0][1].2, crate::swapped::Ink::Muted));
}

#[test]
fn a_paired_line_marks_its_words_with_ink_marked() {
    use crate::theme::Role;
    let frame = diff_frame_of(&["@@ -1 +1 @@", "-call foo(a);", "+call bar(a);"]);
    let removed = &frame.rows[1];
    let marked: Vec<_> = removed
        .iter()
        .filter(|(_, _, ink)| matches!(ink, crate::swapped::Ink::Marked(_)))
        .collect();
    assert_eq!(marked.len(), 1);
    assert_eq!(marked[0].0, "foo(a);");
    assert!(matches!(
        marked[0].2,
        crate::swapped::Ink::Marked(Role::Removed)
    ));
    let added = &frame.rows[2];
    let marked: Vec<_> = added
        .iter()
        .filter(|(_, _, ink)| matches!(ink, crate::swapped::Ink::Marked(_)))
        .collect();
    assert_eq!(marked.len(), 1);
    assert_eq!(marked[0].0, "bar(a);");
}

#[test]
fn an_unpaired_run_marks_nothing() {
    let frame = diff_frame_of(&["@@ -1,2 +1 @@", "-one", "-two", "+both"]);
    assert!(
        frame
            .rows
            .iter()
            .flatten()
            .all(|(_, _, ink)| !matches!(ink, crate::swapped::Ink::Marked(_))),
        "{frame:?}"
    );
}

#[test]
fn answered_rows_show_git_figures_and_the_untracked_row_form() {
    use crate::swapped::Ink;
    let card = changes(&[("src/a.rs", 4, 2), ("new.txt", 4, 1)]);
    let mut lines = BTreeMap::new();
    lines.insert("new.txt".to_owned(), 3u64);
    let resolved = resolve_figures(&card, Some("7\t1\tsrc/a.rs\n"), &lines, None);
    let frame = frame(&card, Some(&resolved), None, List::default());
    assert_eq!(row(&frame, 0), "src/a.rs · +7 −1");
    assert_eq!(row(&frame, 1), "new.txt · untracked, wholly added");
    assert_eq!(
        frame.rows[1],
        [
            ("new.txt".to_owned(), None, Ink::Heading),
            (" · ".to_owned(), None, Ink::Muted),
            ("untracked, wholly added".to_owned(), None, Ink::Muted),
        ]
    );
    assert_eq!(frame.below, ["2 files changed  +10 −1"]);
}

#[test]
fn count_lines_counts_newlines_and_skips_unreadable_files() {
    let dir = fakes::TempDir::new("tui-changed-files-count-lines");
    std::fs::write(dir.path().join("a.rs"), "one\ntwo\nthree\n")
        .unwrap_or_else(|error| panic!("write counted file: {error}"));
    let counts = super::count_lines(dir.path(), &["a.rs", "missing.rs"]);
    assert_eq!(counts.get("a.rs"), Some(&3u64));
    assert!(!counts.contains_key("missing.rs"));
}

#[test]
fn numstat_fully_parsed_rejects_garbage_lines() {
    assert!(super::numstat_fully_parsed("7\t1\tsrc/a.rs\n-\t-\tbin/x\n"));
    assert!(!super::numstat_fully_parsed("7\t1\tsrc/a.rs\ngarbage\n"));
    assert!(super::numstat_fully_parsed(""));
}

#[test]
fn numstat_body_strips_only_the_answer_s_own_closing_line() {
    assert_eq!(
        super::numstat_body("7\t1\tsrc/a.rs\nExit code 0.\n", Some(0)),
        "7\t1\tsrc/a.rs\n"
    );
    assert_eq!(super::numstat_body("Exit code 0.\n", Some(0)), "");
    assert_eq!(
        super::numstat_body("7\t1\tsrc/a.rs\n", Some(0)),
        "7\t1\tsrc/a.rs\n"
    );
    assert_eq!(
        super::numstat_body("7\t1\tsrc/a.rs\nExit code 1.\n", Some(0)),
        "7\t1\tsrc/a.rs\nExit code 1.\n"
    );
}

#[test]
fn resolve_figures_matches_git_s_relative_paths_against_absolute_card_paths() {
    let dir = fakes::TempDir::new("tui-changed-files-workspace-root");
    let absolute = dir
        .path()
        .join("a.rs")
        .to_str()
        .unwrap_or_else(|| panic!("workspace path"))
        .to_owned();
    let card = changes(&[(absolute.as_str(), 4, 2)]);
    let lines = BTreeMap::new();
    let displayed = resolve_figures(&card, Some("7\t1\ta.rs\n"), &lines, Some(dir.path()));
    assert_eq!(displayed.figures.get(absolute.as_str()), Some(&(7, 1)));
    assert!(displayed.untracked.is_empty());
}
