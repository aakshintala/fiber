//! Tests for Changed files' ranking, shell query, answers and frame
//! (`docs/tui.md`, "Swapped views").

use fakes::MUST_SUCCEED_WITHIN;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use contract::clock::Clock;
use contract::events::CommandResult;
use contract::shapes::Process;
use contract::{ActionId, Envelope, SCHEMA_VERSION, SessionId};
use contract::{HubEventKind, events::EventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{
    Diff, answer, diff_command, file_takes, first_hunk_line, frame, list_rows, numstat_command,
    ranked, resolve_figures, row_path,
};
use super::{ListRow, ResolvedFigures};
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

/// The grouped list frame for `changes` with the card's figures, an
/// empty no-diff set and no local root: what the view shows before the
/// numstat answer.
fn list_frame(changes: &BTreeMap<String, (u64, u64)>, list: List) -> crate::swapped::Frame {
    frame(changes, None, None, list, &BTreeSet::new(), None, None)
}

/// The grouped list frame over `resolved` figures with the card's
/// changes, an empty no-diff set and no local root.
fn resolved_frame(
    changes: &BTreeMap<String, (u64, u64)>,
    resolved: &ResolvedFigures,
    list: List,
) -> crate::swapped::Frame {
    frame(
        changes,
        Some(resolved),
        None,
        list,
        &BTreeSet::new(),
        None,
        None,
    )
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
        kind: HubEventKind::HubHello,
        ts: 0,
        schema_version: SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.on_line(Line::Session(Envelope {
        kind: EventKind::ToolCallCompleted,
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
    // `docs/tui.md`, "Swapped views" promises one numstat query, run when the view opens.
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
        kind: EventKind::CommandAccepted,
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
fn files_group_by_folder_in_path_order_with_top_level_under_dot_slash() {
    let card = changes(&[
        ("z.rs", 1, 0),
        ("src/b.rs", 2, 0),
        ("delegate/src/session/a.rs", 3, 0),
        ("delegate/src/c.rs", 4, 0),
        ("src/a.rs", 5, 0),
    ]);
    assert_eq!(
        list_rows(&card, None, None),
        [
            ListRow::Folder("./".to_owned()),
            ListRow::File("z.rs".to_owned()),
            ListRow::Folder("delegate/src/".to_owned()),
            ListRow::File("delegate/src/c.rs".to_owned()),
            ListRow::Folder("delegate/src/session/".to_owned()),
            ListRow::File("delegate/src/session/a.rs".to_owned()),
            ListRow::Folder("src/".to_owned()),
            ListRow::File("src/a.rs".to_owned()),
            ListRow::File("src/b.rs".to_owned()),
            ListRow::Rule,
        ]
    );
}

#[test]
fn displayed_paths_strip_the_workspace_root_but_mapping_keeps_the_card_path() {
    let dir = fakes::TempDir::new("tui-changed-files-display-root");
    let absolute = dir
        .path()
        .join("src/a.rs")
        .to_str()
        .unwrap_or_else(|| panic!("workspace path"))
        .to_owned();
    let card = changes(&[(absolute.as_str(), 4, 2)]);
    let frame = frame(
        &card,
        None,
        None,
        List::default(),
        &BTreeSet::new(),
        Some(dir.path()),
        None,
    );
    assert_eq!(row(&frame, 0), "src/");
    assert_eq!(row(&frame, 1), "    D  a.rs  +4 −2");
    // Selection and Enter read the card's own path, not the display.
    let plan = list_rows(&card, Some(dir.path()), None);
    assert_eq!(row_path(&plan, 1), Some(absolute.as_str()));
}

#[test]
fn displayed_paths_fall_back_to_the_viewed_workspace_without_a_local_root() {
    let dir = fakes::TempDir::new("tui-changed-files-display-viewed");
    let absolute = dir
        .path()
        .join("top.rs")
        .to_str()
        .unwrap_or_else(|| panic!("workspace path"))
        .to_owned();
    let card = changes(&[(absolute.as_str(), 1, 0)]);
    let frame = frame(
        &card,
        None,
        None,
        List::default(),
        &BTreeSet::new(),
        None,
        Some(dir.path()),
    );
    // A remote terminal has no local root to stat, so the letter stays
    // changed while the name shows workspace-relative.
    assert_eq!(row(&frame, 0), "./");
    assert_eq!(row(&frame, 1), "    M  top.rs  +1 −0");
}

#[test]
fn a_path_outside_every_root_shows_whole() {
    let dir = fakes::TempDir::new("tui-changed-files-display-outside");
    let card = changes(&[("/elsewhere/f.rs", 2, 1)]);
    let frame = frame(
        &card,
        None,
        None,
        List::default(),
        &BTreeSet::new(),
        Some(dir.path()),
        None,
    );
    assert_eq!(row(&frame, 0), "/elsewhere/");
    assert_eq!(row(&frame, 1), "    D  f.rs  +2 −1");
}

#[test]
fn an_empty_change_set_lists_no_rows_and_zero_totals() {
    let frame = list_frame(&changes(&[]), List::default());
    assert!(frame.rows.is_empty());
    assert_eq!(frame.below, ["0 files  +0 −0"]);
}

#[test]
fn a_file_row_shows_its_letter_name_and_right_aligned_figures() {
    use crate::swapped::Ink;
    use crate::theme::Role;
    let frame = list_frame(&changes(&[("src/a.rs", 4, 2)]), List::default());
    // The folder row, the file row, then the rule above the totals.
    assert_eq!(frame.rows.len(), 3);
    assert_eq!(frame.rows[0], [("src/".to_owned(), None, Ink::Muted)]);
    assert_eq!(
        frame.rows[1],
        [
            ("    ".to_owned(), None, Ink::Plain),
            ("M  ".to_owned(), None, Ink::Role(Role::Attention)),
            ("a.rs".to_owned(), None, Ink::Heading),
            ("  ".to_owned(), None, Ink::Plain),
            ("+4".to_owned(), None, Ink::Role(Role::Added)),
            (" ".to_owned(), None, Ink::Plain),
            ("−2".to_owned(), None, Ink::Role(Role::Removed)),
        ]
    );
    assert_eq!(row(&frame, 1), "    M  a.rs  +4 −2");
}

#[test]
fn figure_widths_pad_short_counts_to_the_widest() {
    // One, two and three digit counts: the added column spans the
    // widest "+N", the removed column the widest "−N".
    let card = changes(&[("a.rs", 9, 0), ("b.rs", 10, 1), ("c.rs", 100, 10)]);
    let frame = list_frame(&card, List::default());
    assert_eq!(row(&frame, 1), "    M  a.rs    +9  −0");
    assert_eq!(row(&frame, 2), "    M  b.rs   +10  −1");
    assert_eq!(row(&frame, 3), "    M  c.rs  +100 −10");
    assert_eq!(frame.below, ["3 files  +119 −11"]);
}

#[test]
fn letters_mark_untracked_added_missing_deleted_and_otherwise_changed() {
    use crate::swapped::Ink;
    use crate::theme::Role;
    let dir = fakes::TempDir::new("tui-changed-files-letters");
    std::fs::write(dir.path().join("kept.rs"), "x\n")
        .unwrap_or_else(|error| panic!("write kept file: {error}"));
    let card = changes(&[("gone.rs", 2, 0), ("kept.rs", 1, 0), ("new.rs", 3, 0)]);
    let mut figures = BTreeMap::new();
    figures.insert("gone.rs".to_owned(), (2, 0));
    figures.insert("kept.rs".to_owned(), (1, 0));
    figures.insert("new.rs".to_owned(), (3, 0));
    let mut untracked = BTreeSet::new();
    untracked.insert("new.rs".to_owned());
    let resolved = ResolvedFigures { figures, untracked };
    let lettered = frame(
        &card,
        Some(&resolved),
        None,
        List::default(),
        &BTreeSet::new(),
        Some(dir.path()),
        None,
    );
    assert_eq!(row(&lettered, 1), "    D  gone.rs  +2 −0");
    assert_eq!(lettered.rows[1][1].2, Ink::Role(Role::Removed));
    assert_eq!(row(&lettered, 2), "    M  kept.rs  +1 −0");
    assert_eq!(lettered.rows[2][1].2, Ink::Role(Role::Attention));
    assert_eq!(row(&lettered, 3), "    A  new.rs  +3 −0");
    assert_eq!(lettered.rows[3][1].2, Ink::Role(Role::Added));
    // Restoring the deleted file flips its letter back to changed.
    std::fs::write(dir.path().join("gone.rs"), "x\n")
        .unwrap_or_else(|error| panic!("restore deleted file: {error}"));
    let restored = frame(
        &card,
        Some(&resolved),
        None,
        List::default(),
        &BTreeSet::new(),
        Some(dir.path()),
        None,
    );
    assert_eq!(row(&restored, 1), "    M  gone.rs  +2 −0");
}

#[test]
fn a_missing_file_without_a_local_root_stays_changed() {
    let card = changes(&[("gone.rs", 2, 0)]);
    let frame = frame(
        &card,
        None,
        None,
        List::default(),
        &BTreeSet::new(),
        None,
        None,
    );
    assert_eq!(row(&frame, 1), "    M  gone.rs  +2 −0");
}

#[test]
fn the_focused_name_draws_bold_accent_with_an_accent_gutter_mark() {
    use crate::swapped::Ink;
    use crate::theme::Role;
    let card = changes(&[("src/a.rs", 4, 2), ("src/b.rs", 1, 0)]);
    let mut list = List::gutter_accent();
    list.select(2, 4, 24);
    let accented = frame(&card, None, None, list, &BTreeSet::new(), None, None);
    // The unfocused file row keeps its heading name.
    assert_eq!(accented.rows[1][2].2, Ink::Heading);
    assert_eq!(accented.rows[2][2].2, Ink::Bold(Role::Accent));
    let area = Rect::new(0, 0, 40, 10);
    let mut buf = Buffer::empty(area);
    crate::swapped::render(&accented, area, &mut buf, &mut Vec::new());
    assert_eq!(buf[(0, 3)].symbol(), "›");
    assert_eq!(buf[(0, 3)].fg, Role::Accent.color());
    // A plain gutter list keeps the uncoloured mark on the same row.
    let mut plain = List::gutter();
    plain.select(2, 4, 24);
    let unaccented = frame(&card, None, None, plain, &BTreeSet::new(), None, None);
    assert_eq!(unaccented.rows[2][2].2, Ink::Heading);
    let mut buf = Buffer::empty(area);
    crate::swapped::render(&unaccented, area, &mut buf, &mut Vec::new());
    assert_eq!(buf[(0, 3)].symbol(), "›");
    assert_eq!(buf[(0, 3)].fg, ratatui::style::Color::Reset);
}

#[test]
fn a_blurred_accent_list_draws_no_accent_name_or_mark() {
    use crate::swapped::Ink;
    let card = changes(&[("src/a.rs", 4, 2)]);
    let frame = frame(
        &card,
        None,
        None,
        List::gutter_accent().blur(),
        &BTreeSet::new(),
        None,
        None,
    );
    assert_eq!(frame.rows[1][2].2, Ink::Heading);
    let area = Rect::new(0, 0, 40, 8);
    let mut buf = Buffer::empty(area);
    crate::swapped::render(&frame, area, &mut buf, &mut Vec::new());
    assert_eq!(buf[(0, 2)].symbol(), " ");
}

#[test]
fn folder_and_rule_rows_hold_no_path_while_file_rows_do() {
    let card = changes(&[("src/b.rs", 1, 0), ("top.rs", 2, 0)]);
    let plan = list_rows(&card, None, None);
    assert_eq!(row_path(&plan, 0), None);
    assert_eq!(row_path(&plan, 1), Some("top.rs"));
    assert_eq!(row_path(&plan, 2), None);
    assert_eq!(row_path(&plan, 3), Some("src/b.rs"));
    assert_eq!(row_path(&plan, 4), None);
    assert_eq!(row_path(&plan, 5), None);
}

#[test]
fn file_takes_names_the_file_rows_only() {
    assert_eq!(
        file_takes(
            &changes(&[("src/b.rs", 1, 0), ("top.rs", 2, 0)]),
            None,
            None
        ),
        [1, 3]
    );
    assert!(file_takes(&changes(&[]), None, None).is_empty());
    assert_eq!(file_takes(&changes(&[("top.rs", 2, 0)]), None, None), [1]);
}

#[test]
fn totals_count_files_not_folders_and_the_rule_spans_the_content() {
    use crate::swapped::Ink;
    use crate::theme::Role;
    let card = changes(&[("src/a.rs", 4, 2), ("src/b.rs", 1, 3)]);
    let frame = list_frame(&card, List::default());
    assert_eq!(frame.rows.len(), 4);
    assert_eq!(frame.below, ["2 files  +5 −5"]);
    let widest = [0, 1, 2]
        .iter()
        .map(|at| crate::text::width(&row(&frame, *at)))
        .max()
        .unwrap_or(0);
    assert_eq!(frame.rows[3][0].0, "─".repeat(widest));
    assert_eq!(frame.rows[3][0].2, Ink::Role(Role::Rule));
}

#[test]
fn the_list_footer_names_file_and_open_in_editor_with_bold_keys() {
    use crate::swapped::Ink;
    use ratatui::style::{Modifier, Style};
    let frame = list_frame(&changes(&[("src/a.rs", 4, 2)]), List::default());
    assert_eq!(frame.footer, "↑↓ file · Ctrl+G open in editor");
    let chrome = frame.chrome.expect("the list carries styled keys");
    assert!(chrome.top.is_empty());
    assert!(chrome.draft.is_none());
    let text: String = chrome
        .footer
        .iter()
        .map(|(text, _)| text.as_str())
        .collect();
    assert_eq!(text, "↑↓ file · Ctrl+G open in editor");
    let bold = Style::new().add_modifier(Modifier::BOLD);
    assert_eq!(chrome.footer[0].1, bold);
    assert_eq!(chrome.footer[1].1, Ink::Muted.style());
    assert_eq!(chrome.footer[2].1, bold);
    assert_eq!(chrome.footer[3].1, Ink::Muted.style());
}

#[test]
fn a_no_diff_row_stays_dim_with_no_changes_where_the_figures_go() {
    use crate::swapped::Ink;
    use crate::theme::Role;
    let card = changes(&[("src/a.rs", 4, 2)]);
    let mut no_diff = BTreeSet::new();
    no_diff.insert("src/a.rs".to_owned());
    let dimmed = frame(&card, None, None, List::default(), &no_diff, None, None);
    assert_eq!(row(&dimmed, 1), "    ·  a.rs  no changes");
    assert!(
        dimmed.rows[1]
            .iter()
            .all(|(_, _, ink)| matches!(ink, Ink::Muted))
    );
    assert_eq!(dimmed.below, ["1 file  +4 −2"]);
    // Focus marks the gutter but leaves the dim row alone.
    let mut list = List::gutter_accent();
    list.select(1, 3, 24);
    let focused = frame(&card, None, None, list, &no_diff, None, None);
    assert!(
        focused.rows[1]
            .iter()
            .all(|(_, _, ink)| matches!(ink, Ink::Muted))
    );
    let area = Rect::new(0, 0, 40, 8);
    let mut buf = Buffer::empty(area);
    crate::swapped::render(&focused, area, &mut buf, &mut Vec::new());
    assert_eq!(buf[(0, 2)].symbol(), "›");
    assert_eq!(buf[(0, 2)].fg, Role::Accent.color());
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
    let displayed = resolve_figures(&card, Some(answered), &lines, None, None);
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
    assert_eq!(
        resolve_figures(&card, None, &lines, None, None).figures,
        card
    );
    assert!(
        resolve_figures(&card, None, &lines, None, None)
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
    let displayed = resolve_figures(&card, Some(&stdout), &lines, None, None);
    assert_eq!(displayed.figures.get("a.rs"), Some(&(2, 1)));
    assert_eq!(displayed.figures.get("new.txt"), Some(&(3, 0)));
}

#[test]
fn a_committed_file_reply_with_only_the_closing_line_reads_empty() {
    // The shell appends its closing "Exit code N." line to the
    // command's own output: stripped, the committed file's reply reads
    // empty, never "Exit code 0.".
    assert_eq!(
        answer(Some(&shell("Exit code 0.\n", Some(0), None))),
        Diff::Empty
    );
    assert_eq!(
        answer(Some(&shell("Exit code 0.", Some(0), None))),
        Diff::Empty
    );
}

#[test]
fn a_real_diff_keeps_its_hunks_without_the_closing_line() {
    assert_eq!(
        answer(Some(&shell(
            "diff --git a/a b/a\n@@ -1 +1 @@\n-old\n+new\nExit code 1.\n",
            Some(1),
            None,
        ))),
        Diff::Lines {
            lines: vec![
                "diff --git a/a b/a".to_owned(),
                "@@ -1 +1 @@".to_owned(),
                "-old".to_owned(),
                "+new".to_owned(),
            ],
            cut: None,
        }
    );
}

#[test]
fn only_the_answer_s_own_closing_line_strips() {
    // A closing line naming another exit is the diff's own text.
    assert_eq!(
        answer(Some(&shell("diff line\nExit code 0.\n", Some(1), None))),
        Diff::Lines {
            lines: vec!["diff line".to_owned(), "Exit code 0.".to_owned()],
            cut: None,
        }
    );
}

#[test]
fn no_exit_code_text_reaches_any_diff_frame() {
    let answered = answer(Some(&shell(
        "diff --git a/a b/a\n+new\nExit code 1.\n",
        Some(1),
        None,
    )));
    let frame = frame(
        &changes(&[("a", 1, 0)]),
        None,
        Some(("a", &answered)),
        List::default(),
        &BTreeSet::new(),
        None,
        None,
    );
    assert!(
        frame
            .rows
            .iter()
            .flatten()
            .all(|(text, _, _)| !text.contains("Exit code")),
        "{frame:?}"
    );
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
    let list = list_frame(&changes, List::default());
    assert_eq!(list.title, "Changed files");
    assert_eq!(list.command, "diff against HEAD");
    assert_eq!(row(&list, 0), "src/");
    assert_eq!(row(&list, 1), "    M  a.rs  +4 −2");
    assert_eq!(row(&list, 2), "    M  b.rs  +1 −0");
    assert!(row(&list, 3).chars().all(|cell| cell == '─'));
    assert_eq!(list.below, ["2 files  +5 −2"]);
    assert_eq!(list.footer, "↑↓ file · Ctrl+G open in editor");

    let reading = frame(
        &changes,
        None,
        Some(("src/a.rs", &Diff::Reading)),
        List::default(),
        &BTreeSet::new(),
        None,
        None,
    );
    assert_eq!(
        reading.title,
        "Changed files › src/a.rs · diff against HEAD"
    );
    assert_eq!(row(&reading, 0), "src/a.rs  Open in editor ›");
    assert_eq!(row(&reading, 1), "Reading the diff…");

    let diff = Diff::Lines {
        lines: vec!["+new".to_owned()],
        cut: None,
    };
    let local_diff = frame(
        &changes,
        None,
        Some(("src/a.rs", &diff)),
        List::default(),
        &BTreeSet::new(),
        None,
        None,
    );
    assert_eq!(row(&local_diff, 0), "src/a.rs  Open in editor ›");
    assert_eq!(row(&local_diff, 1), "+new");
    assert_eq!(
        local_diff.footer,
        "↑↓ scroll · Ctrl+G edits · ← files · Esc close"
    );
    let remote_diff = frame(
        &changes,
        None,
        Some(("src/a.rs", &diff)),
        List::default(),
        &BTreeSet::new(),
        None,
        None,
    );
    assert_eq!(
        remote_diff.footer,
        "↑↓ scroll · Ctrl+G edits · ← files · Esc close"
    );
    let empty = frame(
        &changes,
        None,
        Some(("src/a.rs", &Diff::Empty)),
        List::default(),
        &BTreeSet::new(),
        None,
        None,
    );
    assert_eq!(row(&empty, 0), "src/a.rs  Open in editor ›");
    assert_eq!(
        row(&empty, 1),
        "No changes against HEAD: committed or undone since Fiber changed it."
    );
    assert!(
        empty.rows[1]
            .iter()
            .all(|(_, _, ink)| matches!(ink, crate::swapped::Ink::Muted))
    );
    assert_eq!(empty.rows.len(), 2);
    let failed = Diff::Failed("denied".to_owned());
    assert_eq!(
        row(
            &frame(
                &changes,
                None,
                Some(("src/a.rs", &failed)),
                List::default(),
                &BTreeSet::new(),
                None,
                None,
            ),
            1
        ),
        "denied"
    );
    let cut = Diff::Lines {
        lines: vec!["line".to_owned()],
        cut: Some("artifacts/full.diff".to_owned()),
    };
    assert_eq!(
        frame(
            &changes,
            None,
            Some(("src/a.rs", &cut)),
            List::default(),
            &BTreeSet::new(),
            None,
            None,
        )
        .below,
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
        &BTreeSet::new(),
        None,
        None,
    )
}

#[test]
fn the_diff_frame_shows_the_path_with_an_open_in_editor_target() {
    use crate::swapped::{Ink, Spot};
    use crate::theme::Role;
    let diff = Diff::Lines {
        lines: vec!["+new".to_owned()],
        cut: None,
    };
    let frame = frame(
        &changes(&[("src/a.rs", 1, 1)]),
        None,
        Some(("src/a.rs", &diff)),
        List::default(),
        &BTreeSet::new(),
        None,
        None,
    );
    assert_eq!(
        frame.rows[0],
        [
            ("src/a.rs".to_owned(), None, Ink::Heading),
            ("  ".to_owned(), None, Ink::Plain),
            (
                "Open in editor ›".to_owned(),
                Some(Spot::Cell(0, 1)),
                Ink::Role(Role::Secondary),
            ),
        ]
    );
    assert_eq!(row(&frame, 0), "src/a.rs  Open in editor ›");
    assert_eq!(row(&frame, 1), "+new");
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
        .skip(1)
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
    let guttered: Vec<String> = frame.rows[2..]
        .iter()
        .map(|cells| cells.iter().map(|(text, _, _)| text.as_str()).collect())
        .collect();
    assert_eq!(guttered[0], " 9 -old");
    assert_eq!(guttered[1], "10 +new");
    // The gutter is dim; the hunk row is dim too.
    assert!(matches!(frame.rows[2][0].2, crate::swapped::Ink::Muted));
    assert!(matches!(frame.rows[1][1].2, crate::swapped::Ink::Muted));
}

#[test]
fn a_paired_line_marks_its_words_with_ink_marked() {
    use crate::theme::Role;
    let frame = diff_frame_of(&["@@ -1 +1 @@", "-call foo(a);", "+call bar(a);"]);
    let removed = &frame.rows[2];
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
    let added = &frame.rows[3];
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
    use crate::theme::Role;
    let card = changes(&[("src/a.rs", 4, 2), ("new.txt", 4, 1)]);
    let mut lines = BTreeMap::new();
    lines.insert("new.txt".to_owned(), 3u64);
    let resolved = resolve_figures(&card, Some("7\t1\tsrc/a.rs\n"), &lines, None, None);
    let frame = resolved_frame(&card, &resolved, List::default());
    assert_eq!(row(&frame, 0), "./");
    assert_eq!(row(&frame, 1), "    A  new.txt  +3 −0");
    assert_eq!(
        frame.rows[1],
        [
            ("    ".to_owned(), None, Ink::Plain),
            ("A  ".to_owned(), None, Ink::Role(Role::Added)),
            ("new.txt".to_owned(), None, Ink::Heading),
            ("  ".to_owned(), None, Ink::Plain),
            ("+3".to_owned(), None, Ink::Role(Role::Added)),
            (" ".to_owned(), None, Ink::Plain),
            ("−0".to_owned(), None, Ink::Role(Role::Removed)),
        ]
    );
    assert_eq!(row(&frame, 2), "src/");
    assert_eq!(row(&frame, 3), "    M  a.rs  +7 −1");
    assert_eq!(frame.below, ["2 files  +10 −1"]);
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
    let displayed = resolve_figures(&card, Some("7\t1\ta.rs\n"), &lines, Some(dir.path()), None);
    assert_eq!(displayed.figures.get(absolute.as_str()), Some(&(7, 1)));
    assert!(displayed.untracked.is_empty());
}

#[test]
fn resolve_figures_matches_git_s_relative_paths_against_viewed_card_paths() {
    let dir = fakes::TempDir::new("tui-changed-files-viewed-workspace");
    let absolute = dir
        .path()
        .join("a.rs")
        .to_str()
        .unwrap_or_else(|| panic!("workspace path"))
        .to_owned();
    let card = changes(&[(absolute.as_str(), 4, 2)]);
    let lines = BTreeMap::new();
    // No launch workspace strips the hub card path; the viewed one does.
    let displayed = resolve_figures(&card, Some("7\t1\ta.rs\n"), &lines, None, Some(dir.path()));
    assert_eq!(displayed.figures.get(absolute.as_str()), Some(&(7, 1)));
    assert!(displayed.untracked.is_empty());
}

#[test]
fn first_hunk_line_reads_the_first_hunks_new_side_start() {
    let lines = [
        "diff --git a/f.txt b/f.txt",
        "@@ -1,3 +10,4 @@ fn",
        " old",
        "@@ -8,2 +20,3 @@ fn",
        " old",
    ]
    .map(str::to_owned);
    assert_eq!(first_hunk_line(&lines), 10);
}

#[test]
fn first_hunk_line_clamps_a_zero_start_to_one() {
    for (header, want) in [
        ("@@ -1,1 +0,0 @@", 1),
        ("@@ -1 +1 @@", 1),
        ("@@ -1,3 +2,4 @@", 2),
    ] {
        assert_eq!(first_hunk_line(&[header.to_owned()]), want, "{header}");
    }
}

#[test]
fn first_hunk_line_skips_unparsable_headers() {
    let lines = ["@@ nope", "@@ -1,3 +x @@", "@@ -4,2 +7 @@"].map(str::to_owned);
    assert_eq!(first_hunk_line(&lines), 7);
}

#[test]
fn first_hunk_line_is_one_without_a_header() {
    let empty: Vec<String> = Vec::new();
    for lines in [
        empty,
        vec!["diff line".to_owned()],
        vec!["Binary files a/f.txt and b/f.txt differ".to_owned()],
    ] {
        assert_eq!(first_hunk_line(&lines), 1);
    }
}
