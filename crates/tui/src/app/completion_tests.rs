//! Tests for the completion panels: the `/` list, the `@` file
//! search, and the approvals row.

use super::super::commands::Rows;
use super::super::commands::commands_testkit::{
    SESSION, answer_commands, answer_rows, attached, connected, enter, fold_file_answer, key, now,
    paste_image, paths, reloaded, remote_search_app, route_file_answer, selected, sent,
    session_line, shown, table_lines, turn_starts, type_any, type_text,
};
use crate::app::{App, Effect};
use crate::keys::{Edit, Key};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

#[test]
fn slash_alone_shows_every_row_from_the_top() {
    let mut app = connected();
    assert_eq!(app.completions(), None);
    type_text(&mut app, "/");
    let rows = shown(&app);
    assert_eq!(rows.len(), crate::slash::SHOWN);
    assert_eq!(rows.first(), table_lines().first());
    assert_eq!(selected(&app), rows.first().cloned());
}

#[test]
fn typing_filters_prefix_matches_first() {
    let mut app = attached();
    answer_commands(&mut app, &["tdd", "areview"]);
    type_text(&mut app, "/re");
    assert_eq!(
        shown(&app),
        [
            "/resume  Goes home, focused on the first exited row.  command",
            "/reviewer-setup  Sets up the reviewer's notes with you.  command",
            "/rewind  Opens the rewind view.  command",
            "/name /rename <text>  Names the session.  command",
            "/reload  Reloads configuration, MCP servers and extensions.  command",
            "/credential [<label>]  Switches the credential label, or opens the credential picker.  command",
            "/areview  Runs areview.  skill",
        ]
    );
    type_text(&mut app, "zz");
    // An empty list shows its dim row, with no selection and no range.
    let completions = app.completions().expect("the dim row shows");
    assert_eq!(completions.lines(), ["no matches"]);
    assert_eq!(completions.selected, None);
    assert!(matches!(completions.rows, Rows::Message(_)));
}

#[test]
fn a_skill_from_the_commands_answer_is_a_row_and_runs_as_a_prompt() {
    let mut app = attached();
    answer_commands(&mut app, &["tdd"]);
    type_text(&mut app, "/td");
    assert_eq!(shown(&app), ["/tdd  Runs tdd.  skill"]);
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "prompt");
    assert_eq!(lines[0]["args"]["content"][0]["text"], "/tdd");
}

#[test]
fn another_sessions_reloaded_asks_nothing() {
    let mut app = attached();
    assert!(app.on_line(reloaded("s_bbbbbbbbbbbbbbbb")).is_empty());
}

#[test]
fn reloaded_with_the_link_down_asks_nothing() {
    let mut app = App::new(PathBuf::from("/w"));
    app.attach(contract::SessionId(SESSION.to_owned()));
    assert!(app.on_line(reloaded(SESSION)).is_empty());
}

#[test]
fn up_and_down_move_the_selection_clamped_and_scroll_the_window() {
    let table = table_lines();
    let shown_rows = crate::slash::SHOWN;
    assert!(
        table.len() > shown_rows + 1,
        "the table outgrows the window"
    );
    let mut app = connected();
    type_text(&mut app, "/");
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(selected(&app), table.first().cloned());
    app.on_key(Key::Down, now());
    assert_eq!(selected(&app), table.get(1).cloned());
    app.on_key(Key::Up, now());
    assert_eq!(selected(&app), table.first().cloned());
    // Down to the last row in view: the window has not moved.
    for _ in 1..shown_rows {
        app.on_key(Key::Down, now());
    }
    assert_eq!(selected(&app), table.get(shown_rows - 1).cloned());
    assert_eq!(
        app.completions().and_then(|c| c.selected),
        Some(shown_rows - 1)
    );
    assert_eq!(shown(&app).first(), table.first());
    // One more selects the first row past the window: it scrolls by one.
    app.on_key(Key::Down, now());
    assert_eq!(selected(&app), table.get(shown_rows).cloned());
    assert_eq!(
        app.completions().and_then(|c| c.selected),
        Some(shown_rows - 1)
    );
    assert_eq!(shown(&app).first(), table.get(1));
    // Down clamps at the last row.
    for _ in 0..table.len() {
        app.on_key(Key::Down, now());
    }
    assert_eq!(selected(&app), table.last().cloned());
    app.on_key(Key::Up, now());
    assert_eq!(selected(&app), table.get(table.len() - 2).cloned());
    // The draft is untouched by moving.
    assert_eq!(app.draft(), "/");
}

#[test]
fn tab_completes_the_selected_name_and_closes_the_panel() {
    let mut app = connected();
    type_text(&mut app, "/han");
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.draft(), "/handoff ");
    assert_eq!(app.completions(), None);
}

#[test]
fn tab_with_no_panel_and_no_match_does_nothing() {
    let mut app = connected();
    type_text(&mut app, "hi");
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.on_key(Key::BackTab, now()), Effect::None);
    assert_eq!(app.draft(), "hi");
    let mut app = connected();
    type_text(&mut app, "/zz");
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.draft(), "/zz");
}

#[test]
fn enter_runs_the_selected_row() {
    let mut app = attached();
    type_text(&mut app, "/rel");
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "reload");
    assert_eq!(app.draft(), "");
}

#[test]
fn esc_closes_the_slash_panel_and_keeps_the_draft_until_it_is_emptied() {
    let mut app = attached();
    turn_starts(&mut app);
    type_text(&mut app, "/re");
    // Esc closes the panel; it does not interrupt the turn.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert_eq!(app.draft(), "/re");
    assert_eq!(app.completions(), None);
    // Typing again does not reopen it.
    type_text(&mut app, "l");
    assert_eq!(app.completions(), None);
    // Backspace back to `/` keeps it closed; to empty, then `/`, reopens.
    for _ in 0..3 {
        app.on_key(Key::Backspace, now());
    }
    assert_eq!(app.draft(), "/");
    assert_eq!(app.completions(), None);
    app.on_key(Key::Backspace, now());
    type_text(&mut app, "/");
    assert_eq!(shown(&app).len(), crate::slash::SHOWN);
    // A draft that stops starting with `/` also reopens it after Esc.
    app.on_key(Key::Esc, now());
    app.on_key(Key::CtrlC, now());
    type_text(&mut app, "/");
    assert_eq!(shown(&app).len(), crate::slash::SHOWN);
}

#[test]
fn a_space_closes_the_slash_panel() {
    let mut app = connected();
    type_text(&mut app, "/handoff ");
    assert_eq!(app.completions(), None);
    app.on_key(Key::Backspace, now());
    assert_eq!(shown(&app).len(), 1);
}

#[test]
fn the_approvals_row_opens_the_waiting_queue() {
    let mut app = connected();
    assert_eq!(enter(&mut app, "/approvals"), Effect::None);
    assert_eq!(app.notice(), Some("No requests waiting."));
    assert_eq!(app.draft(), "");
}

#[test]
fn the_slash_panel_reserves_no_rows() {
    let mut app = connected();
    app.set_size(80, 24);
    assert_eq!(app.conversation_height(), 21);
    // The `/` panel draws over the conversation, reserving nothing,
    // however many rows match (`docs/tui.md`, "Look", "Overlays").
    type_text(&mut app, "/");
    assert_eq!(app.conversation_height(), 21);
    type_text(&mut app, "han");
    assert_eq!(app.conversation_height(), 21);
}

#[test]
fn an_open_approval_panel_wins_over_the_slash_panel() {
    let mut app = attached();
    type_text(&mut app, "/");
    let request = session_line(
        "permission_requested",
        json!({"request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "review", "rule": {"subject": "npm test", "prefix": "npm test"}}),
    );
    assert!(app.on_line(request).is_empty());
    assert!(app.panel().is_some());
    assert_eq!(app.completions(), None);
    // Esc goes to the approval panel, which puts the request aside; the
    // slash panel shows again, still open.
    app.on_key(Key::Esc, now());
    assert!(app.panel().is_none());
    assert_eq!(shown(&app).len(), crate::slash::SHOWN);
}

#[test]
fn at_opens_the_file_panel_and_each_keystroke_searches_anew() {
    let mut app = connected();
    assert_eq!(key(&mut app, '@'), Effect::ListFiles);
    assert!(app.files_open());
    let opened = app.generation();
    assert_eq!(
        key(&mut app, 's'),
        Effect::Search {
            generation: opened + 1,
            query: "s".to_owned()
        }
    );
    assert_eq!(
        key(&mut app, 'r'),
        Effect::Search {
            generation: opened + 2,
            query: "sr".to_owned()
        }
    );
    assert_eq!(
        app.on_key(Key::Backspace, now()),
        Effect::Search {
            generation: opened + 3,
            query: "s".to_owned()
        }
    );
    // A superseded result changes nothing; the current one shows.
    app.on_files(opened + 2, paths(&["old.rs"]));
    assert_eq!(app.completions(), None);
    app.on_files(opened + 3, paths(&["src/a.rs", "src/b.rs"]));
    assert_eq!(shown(&app), ["src/a.rs", "src/b.rs"]);
    app.on_key(Key::Down, now());
    assert_eq!(selected(&app).as_deref(), Some("src/b.rs"));
    // Tab puts the path in place of `@s`, with a space, and closes.
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.draft(), "src/b.rs ");
    assert!(!app.files_open());
}

#[test]
fn choosing_a_directory_row_keeps_the_panel_and_searches_the_new_base() {
    let mut app = connected();
    assert_eq!(key(&mut app, '@'), Effect::ListFiles);
    type_any(&mut app, "~/proj/s");
    app.on_files(
        app.generation(),
        paths(&["~/proj/sub/", "~/proj/sub/inner.txt"]),
    );
    assert_eq!(selected(&app).as_deref(), Some("~/proj/sub/"));
    let before = app.generation();
    assert_eq!(
        app.on_key(Key::Tab, now()),
        Effect::Search {
            generation: before + 1,
            query: "~/proj/sub/".to_owned(),
        }
    );
    assert_eq!(app.draft(), "@~/proj/sub/");
    assert!(app.files_open());
    // The new search's rows show once it answers.
    app.on_files(app.generation(), paths(&["~/proj/sub/inner.txt"]));
    assert_eq!(shown(&app), ["~/proj/sub/inner.txt"]);
}

#[test]
fn a_directory_choice_clears_the_old_rows() {
    let mut app = connected();
    assert_eq!(key(&mut app, '@'), Effect::ListFiles);
    type_any(&mut app, "~/proj/s");
    app.on_files(
        app.generation(),
        paths(&["~/proj/sub/", "~/proj/sub/inner.txt"]),
    );
    let before = app.generation();
    assert_eq!(
        app.on_key(Key::Tab, now()),
        Effect::Search {
            generation: before + 1,
            query: "~/proj/sub/".to_owned(),
        }
    );
    // Before the new search answers the panel holds no stale rows, and
    // a late answer for the old base changes nothing.
    assert!(app.files_open());
    assert_eq!(app.completions(), None);
    app.on_files(before, paths(&["~/proj/stale.txt"]));
    assert_eq!(app.completions(), None);
}

#[test]
fn tab_and_enter_both_enter_a_directory() {
    for press in [Key::Tab, Key::Enter] {
        let mut app = connected();
        assert_eq!(key(&mut app, '@'), Effect::ListFiles);
        type_any(&mut app, "~/proj/s");
        app.on_files(app.generation(), paths(&["~/proj/sub/"]));
        let before = app.generation();
        assert_eq!(
            app.on_key(press.clone(), now()),
            Effect::Search {
                generation: before + 1,
                query: "~/proj/sub/".to_owned(),
            },
            "{press:?}"
        );
        assert_eq!(app.draft(), "@~/proj/sub/");
        assert!(app.files_open());
    }
}

#[test]
fn enter_chooses_a_file_mid_draft_without_sending() {
    let mut app = attached();
    type_text(&mut app, "look at ");
    assert_eq!(key(&mut app, '@'), Effect::ListFiles);
    key(&mut app, 'm');
    app.on_files(app.generation(), paths(&["main.rs"]));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(app.draft(), "look at main.rs ");
    // The next Enter sends the draft.
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines[0]["args"]["content"][0]["text"], "look at main.rs ");
}

#[test]
fn only_an_at_at_the_start_or_after_whitespace_opens_the_panel() {
    let mut app = connected();
    type_text(&mut app, "me@x");
    assert!(!app.files_open());
    // An `@` inside an open panel's query is part of the query.
    app.on_key(Key::CtrlC, now());
    key(&mut app, '@');
    assert_eq!(
        key(&mut app, '@'),
        Effect::Search {
            generation: app.generation(),
            query: "@".to_owned()
        }
    );
    // A second panel opens only after whitespace closes the first.
    assert_eq!(key(&mut app, ' '), Effect::None);
    assert!(!app.files_open());
    assert_eq!(key(&mut app, '@'), Effect::ListFiles);
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.draft(), "@@ @");
}

#[test]
fn esc_and_backspace_past_the_at_close_the_panel() {
    let mut app = connected();
    type_any(&mut app, "x @ab");
    app.on_files(app.generation(), paths(&["ab.rs"]));
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert_eq!(app.draft(), "x @ab");
    assert!(!app.files_open());
    // Typing on does not reopen it.
    assert_eq!(key(&mut app, 'c'), Effect::None);
    assert!(!app.files_open());
    let mut app = connected();
    type_any(&mut app, "x @a");
    app.on_key(Key::Backspace, now());
    assert!(app.files_open());
    assert_eq!(app.on_key(Key::Backspace, now()), Effect::None);
    assert_eq!(app.draft(), "x ");
    assert!(!app.files_open());
}

#[test]
fn a_result_after_close_or_reopen_is_dropped() {
    let mut app = connected();
    key(&mut app, '@');
    let first = app.generation();
    app.on_key(Key::Esc, now());
    app.on_files(first, paths(&["late.rs"]));
    assert_eq!(app.completions(), None);
    // Reopened: the generation moved on, never back.
    app.on_key(Key::CtrlC, now());
    assert_eq!(key(&mut app, '@'), Effect::ListFiles);
    assert!(app.generation() > first);
    app.on_files(first, paths(&["late.rs"]));
    assert_eq!(app.completions(), None);
    app.on_files(app.generation(), paths(&["new.rs"]));
    assert_eq!(shown(&app), ["new.rs"]);
}

#[test]
fn a_failed_listing_is_one_row_that_cannot_be_chosen() {
    let mut app = connected();
    key(&mut app, '@');
    app.on_files(
        app.generation(),
        Err("fatal: not a git repository".to_owned()),
    );
    let completions = app.completions();
    assert_eq!(
        completions.as_ref().map(|c| c.lines()),
        Some(vec!["No files: fatal: not a git repository".to_owned()])
    );
    assert_eq!(completions.and_then(|c| c.selected), None);
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.draft(), "@");
}

#[test]
fn a_shorter_result_clamps_the_selection() {
    let mut app = connected();
    key(&mut app, '@');
    app.on_files(app.generation(), paths(&["a", "b", "c"]));
    app.on_key(Key::Down, now());
    app.on_key(Key::Down, now());
    assert_eq!(selected(&app).as_deref(), Some("c"));
    app.on_files(app.generation(), paths(&["a", "b"]));
    assert_eq!(selected(&app).as_deref(), Some("b"));
}

#[test]
fn the_file_panel_follows_the_cursor_in_a_multi_line_draft() {
    // Whitespace after the query closes the panel, and moving back into
    // the query does not reopen it.
    let mut app = connected();
    type_any(&mut app, "first");
    assert_eq!(app.on_edit(Edit::ShiftEnter), Effect::None);
    type_any(&mut app, "@ma tail");
    assert!(!app.files_open());
    for _ in 0.." tail".len() {
        assert_eq!(app.on_edit(Edit::Left), Effect::None);
    }
    assert!(!app.files_open());
    let mut app = connected();
    type_any(&mut app, "first");
    app.on_edit(Edit::CtrlJ);
    key(&mut app, '@');
    key(&mut app, 'a');
    // A paste at the cursor searches anew.
    let generation = app.generation();
    assert_eq!(
        app.on_edit(Edit::Paste("in".to_owned())),
        Effect::Search {
            generation: generation + 1,
            query: "ain".to_owned()
        }
    );
    // Moving inside the query keeps the panel; choosing replaces the whole
    // query, not only the part before the cursor.
    app.on_edit(Edit::Left);
    assert!(app.files_open());
    app.on_files(app.generation(), paths(&["src/main.rs"]));
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.draft(), "first\nsrc/main.rs ");
    // Left past the `@` closes the panel.
    let mut app = connected();
    type_any(&mut app, "x @ab");
    app.on_edit(Edit::LineStart);
    assert!(!app.files_open());
}

#[test]
fn a_file_chosen_mid_line_keeps_the_text_after_it() {
    let mut app = connected();
    type_any(&mut app, "see  now");
    for _ in 0.." now".len() {
        app.on_edit(Edit::Left);
    }
    assert_eq!(key(&mut app, '@'), Effect::ListFiles);
    key(&mut app, 'a');
    app.on_files(app.generation(), paths(&["a.rs"]));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(app.draft(), "see a.rs  now");
    // The cursor is after the inserted path and its space.
    key(&mut app, '!');
    assert_eq!(app.draft(), "see a.rs ! now");
}

#[test]
fn a_line_break_closes_the_slash_panel() {
    let mut app = connected();
    type_any(&mut app, "/re");
    assert!(app.completions().is_some());
    app.on_edit(Edit::ShiftEnter);
    assert_eq!(app.completions(), None);
    app.on_key(Key::Backspace, now());
    assert!(app.completions().is_some());
}

/// The structured `/` panel: the windowed rows, the selection among
/// them, the window's start and the match count, with today's text
/// form beside them (`docs/tui.md`, "Rules").
#[test]
fn slash_completions_carry_their_window() {
    let mut app = attached();
    answer_commands(&mut app, &["tdd", "areview"]);
    type_text(&mut app, "/re");
    let completions = app.completions().expect("the panel shows");
    assert_eq!(completions.query, "re");
    // The `/` list shows built-ins in the table order (`docs/tui.md`, "Slash commands").
    assert_eq!(completions.total, 7);
    assert_eq!(completions.start, 0);
    assert_eq!(completions.selected, Some(0));
    let names: Vec<&str> = match &completions.rows {
        Rows::Slash(rows) => rows.iter().map(|row| row.name.as_str()).collect(),
        Rows::Files(_) | Rows::Search(_) | Rows::Message(_) => {
            panic!("a slash window, not {:?}", completions.rows)
        }
    };
    assert_eq!(
        names,
        [
            "resume",
            "reviewer-setup",
            "rewind",
            "name",
            "reload",
            "credential",
            "areview"
        ]
    );
    assert_eq!(
        completions.lines(),
        [
            "/resume  Goes home, focused on the first exited row.  command",
            "/reviewer-setup  Sets up the reviewer's notes with you.  command",
            "/rewind  Opens the rewind view.  command",
            "/name /rename <text>  Names the session.  command",
            "/reload  Reloads configuration, MCP servers and extensions.  command",
            "/credential [<label>]  Switches the credential label, or opens the credential picker.  command",
            "/areview  Runs areview.  skill",
        ]
    );
    // The `/` panel draws over the conversation, reserving nothing.
}

/// A long `/` list windows around the selection, keeping it in view.
#[test]
fn a_long_slash_list_windows_around_the_selection() {
    let mut app = attached();
    let skills: Vec<String> = (0..30).map(|n| format!("skill{n:02}")).collect();
    let refs: Vec<&str> = skills.iter().map(String::as_str).collect();
    answer_commands(&mut app, &refs);
    type_text(&mut app, "/");
    let total = crate::slash::rows(&[]).len() + skills.len();
    assert!(total > crate::slash::SHOWN + 1);
    for _ in 0..10 {
        assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    }
    let completions = app.completions().expect("the panel shows");
    assert_eq!(completions.total, total);
    assert_eq!(completions.start, 3);
    assert_eq!(completions.selected, Some(7));
    match &completions.rows {
        Rows::Slash(rows) => {
            assert_eq!(rows.len(), crate::slash::SHOWN);
            assert_eq!(rows.first().map(|row| row.name.as_str()), Some("model"));
            // `/mcp` sits after `/tools`: the window's last row moves
            // with the list, its start and selection unchanged.
            assert_eq!(rows.last().map(|row| row.name.as_str()), Some("usage"));
        }
        Rows::Files(_) | Rows::Search(_) | Rows::Message(_) => {
            panic!("a slash window, not {:?}", completions.rows)
        }
    }
}

/// An empty `/` list shows its dim row, with no selection and no range.
#[test]
fn an_empty_slash_list_shows_no_matches() {
    let mut app = attached();
    type_text(&mut app, "/zzz");
    let completions = app.completions().expect("the dim row shows");
    assert_eq!(completions.query, "zzz");
    assert_eq!(completions.total, 0);
    assert_eq!(completions.start, 0);
    assert_eq!(completions.selected, None);
    assert_eq!(completions.lines(), ["no matches"]);
    assert!(matches!(completions.rows, Rows::Message(_)));
}

/// The structured `@` panel: the windowed paths and the query behind
/// the `@`, with today's text form beside them.
#[test]
fn file_completions_carry_their_window() {
    let mut app = connected();
    assert_eq!(key(&mut app, '@'), Effect::ListFiles);
    key(&mut app, 's');
    app.on_files(app.generation(), paths(&["src/a.rs", "src/b.rs"]));
    let completions = app.completions().expect("the panel shows");
    assert_eq!(completions.query, "s");
    assert_eq!(completions.total, 2);
    assert_eq!(completions.start, 0);
    assert_eq!(completions.selected, Some(0));
    match &completions.rows {
        Rows::Files(rows) => assert_eq!(rows, &["src/a.rs", "src/b.rs"]),
        Rows::Slash(_) | Rows::Search(_) | Rows::Message(_) => {
            panic!("a file window, not {:?}", completions.rows)
        }
    }
    assert_eq!(completions.lines(), ["src/a.rs", "src/b.rs"]);
}

/// An empty `@` result shows its dim row, with no selection and no range.
#[test]
fn an_empty_file_result_shows_no_files_match() {
    let mut app = connected();
    assert_eq!(key(&mut app, '@'), Effect::ListFiles);
    app.on_files(app.generation(), Ok(Vec::new()));
    let completions = app.completions().expect("the dim row shows");
    assert_eq!(completions.query, "");
    assert_eq!(completions.total, 0);
    assert_eq!(completions.start, 0);
    assert_eq!(completions.selected, None);
    assert_eq!(completions.lines(), ["no files match"]);
    assert!(matches!(completions.rows, Rows::Message(_)));
}

/// An `@` error keeps its text as one dim row with no selection.
#[test]
fn a_file_error_is_a_message_with_no_selection() {
    let mut app = connected();
    assert_eq!(key(&mut app, '@'), Effect::ListFiles);
    app.on_files(app.generation(), Err("gone".to_owned()));
    let completions = app.completions().expect("the dim row shows");
    assert_eq!(completions.lines(), ["No files: gone"]);
    assert_eq!(completions.selected, None);
    assert!(matches!(completions.rows, Rows::Message(_)));
}

#[test]
fn a_stale_commands_answer_sets_neither_rows_nor_extensions() {
    let mut app = attached();
    let asked = app.on_line(reloaded(SESSION));
    assert_eq!(asked.len(), 1);
    let first: Value = serde_json::from_str(&asked[0]).unwrap_or_default();
    let asked = app.on_line(reloaded(SESSION));
    assert_eq!(asked.len(), 1);
    let second: Value = serde_json::from_str(&asked[0]).unwrap_or_default();
    let answer = session_line(
        "command_accepted",
        json!({"command_id": first["id"],
            "result": {"commands": [{"name": "deploy", "description": "Deploys.",
                "tag": "acme", "extension": true}]}}),
    );
    assert!(app.on_line(answer).is_empty());
    let answer = session_line(
        "command_accepted",
        json!({"command_id": second["id"], "result": {"commands": []}}),
    );
    assert!(app.on_line(answer).is_empty());
    // A stale name is refused: the latest answer holds neither rows
    // nor extensions for it.
    assert_eq!(enter(&mut app, "/deploy"), Effect::None);
    assert_eq!(
        app.notice(),
        Some("Unknown command /deploy. Type / for the list.")
    );
    assert_eq!(app.draft(), "/deploy");
}

#[test]
fn home_clears_the_extension_names() {
    let mut app = attached();
    answer_rows(
        &mut app,
        json!([{"name": "deploy", "description": "Deploys.", "tag": "acme", "extension": true}]),
    );
    app.on_key(Key::Char('/'), now());
    // Going home clears the rows and names: re-attach sends prompt until
    // a new answer arrives.
    app.go_home();
    app.attach(contract::SessionId(SESSION.to_owned()));
    // Going home clears the rows and names: a re-attached name is
    // refused until a new answer arrives.
    assert_eq!(enter(&mut app, "/deploy"), Effect::None);
    assert_eq!(
        app.notice(),
        Some("Unknown command /deploy. Type / for the list.")
    );
    assert_eq!(app.draft(), "/deploy");
}

#[test]
fn the_unanswered_interval_shows_built_ins_only_and_refuses_stale_names() {
    let mut app = attached();
    answer_rows(
        &mut app,
        json!([{"name": "deploy", "description": "Deploys.", "tag": "acme", "extension": true},
            {"name": "tdd", "description": "Runs tdd.", "tag": "skill"}]),
    );
    let asked = app.on_line(reloaded(SESSION));
    assert_eq!(asked.len(), 1);
    let asked: Value = serde_json::from_str(&asked[0]).unwrap_or_default();
    let table = table_lines();
    type_text(&mut app, "/");
    assert_eq!(shown(&app), table[..crate::slash::SHOWN].to_vec());
    app.on_key(Key::CtrlC, now());
    type_text(&mut app, "/td");
    assert_eq!(shown(&app), ["no matches"]);
    app.on_key(Key::CtrlC, now());
    // A stale extension name is refused until its answer arrives.
    assert_eq!(enter(&mut app, "/deploy now"), Effect::None);
    assert_eq!(
        app.notice(),
        Some("Unknown command /deploy. Type / for the list.")
    );
    assert_eq!(app.draft(), "/deploy now");
    let answer = session_line(
        "command_accepted",
        json!({"command_id": asked["id"],
            "result": {"commands": [{"name": "deploy", "description": "Deploys.",
                "tag": "acme", "extension": true}]}}),
    );
    assert!(app.on_line(answer).is_empty());
    app.on_key(Key::CtrlC, now());
    let lines = sent(enter(&mut app, "/deploy now"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "command");
}

#[test]
fn remote_mentions_use_the_hub_and_keep_only_the_latest_generation() {
    let d = fakes::Deadline::start();
    let (mut app, router, lines, inputs) = remote_search_app("/remote/ws");
    assert_eq!(app.on_key(Key::Char('@'), now()), Effect::None);
    let first = d.recv_or_fail(&lines, "new remote panel");
    assert_eq!(first["command"], "search_files");
    assert_eq!(
        first["args"],
        json!({"workspace":"/remote/ws","text":"","fresh":true})
    );
    assert_eq!(app.on_key(Key::Char('s'), now()), Effect::None);
    let latest = d.recv_or_fail(&lines, "query edit");
    assert_eq!(latest["args"], json!({"workspace":"/remote/ws","text":"s"}));
    route_file_answer(
        &router,
        &latest,
        "command_accepted",
        json!({"rows":["src/","settings.rs"]}),
    );
    fold_file_answer(&mut app, &inputs, &d);
    assert_eq!(app.file_paths(), ["src/", "settings.rs"]);
    route_file_answer(
        &router,
        &first,
        "command_accepted",
        json!({"rows":["stale"]}),
    );
    fold_file_answer(&mut app, &inputs, &d);
    assert_eq!(app.file_paths(), ["src/", "settings.rs"]);
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    let directory = d.recv_or_fail(&lines, "directory row");
    assert_eq!(
        directory["args"],
        json!({"workspace":"/remote/ws","text":"src/"})
    );
    route_file_answer(
        &router,
        &directory,
        "command_accepted",
        json!({"rows":["src/lib.rs"]}),
    );
    fold_file_answer(&mut app, &inputs, &d);
    assert_eq!(app.file_paths(), ["src/lib.rs"]);
    app.on_key(Key::Esc, now());
    app.on_key(Key::Char(' '), now());
    assert_eq!(app.on_key(Key::Char('@'), now()), Effect::None);
    let reopened = d.recv_or_fail(&lines, "reopened panel");
    assert_eq!(reopened["args"]["fresh"], true);
}

#[test]
fn remote_search_errors_are_panel_errors_and_stale_changes_nothing() {
    let d = fakes::Deadline::start();
    let (mut app, router, lines, inputs) = remote_search_app("/remote/ws");
    for (kind, result, expected) in [
        (
            "command_accepted",
            json!({"error":"walk failed"}),
            "walk failed",
        ),
        (
            "command_rejected",
            json!({"code":"invalid_arguments","message":"hub refused"}),
            "hub refused",
        ),
        (
            "command_accepted",
            json!({"rows":7}),
            "The search_files answer could not be read.",
        ),
    ] {
        app.on_key(Key::Esc, now());
        app.on_key(Key::Char(' '), now());
        assert_eq!(app.on_key(Key::Char('@'), now()), Effect::None);
        let line = d.recv_or_fail(&lines, "search that fails");
        route_file_answer(&router, &line, kind, result);
        fold_file_answer(&mut app, &inputs, &d);
        assert_eq!(
            app.completion.files.as_ref().unwrap().result,
            Some(Err(expected.into()))
        );
    }
    app.on_key(Key::Char('x'), now());
    let line = d.recv_or_fail(&lines, "stale search");
    route_file_answer(
        &router,
        &line,
        "command_rejected",
        json!({"code":"stale_request","message":"old"}),
    );
    assert!(inputs.try_recv().is_err());
    assert_eq!(
        app.completion.files.as_ref().unwrap().result,
        Some(Err("The search_files answer could not be read.".into()))
    );
}

#[test]
fn remote_search_write_failure_is_a_panel_error() {
    let (mut app, router, _lines, _inputs) = remote_search_app("/remote/ws");
    let (client, peer) = std::os::unix::net::UnixStream::pair().unwrap();
    router.attach(client);
    drop(peer);
    assert_eq!(app.on_key(Key::Char('@'), now()), Effect::None);
    assert_eq!(
        app.completion.files.as_ref().unwrap().result,
        Some(Err("connection lost".into()))
    );
    assert_eq!(router.registered(), 0);
}

#[test]
fn remote_mentions_without_workspace_or_link_do_not_fall_back_to_client_disk() {
    for empty in [false, true] {
        let (mut app, router, lines, _inputs) =
            remote_search_app(if empty { "" } else { "/remote/ws" });
        if !empty {
            router.detach();
        }
        assert_eq!(app.on_key(Key::Char('@'), now()), Effect::None);
        assert_eq!(app.on_key(Key::Char('x'), now()), Effect::None);
        assert!(lines.try_recv().is_err());
        let expected = if empty {
            None
        } else {
            Some(Err("connection lost".into()))
        };
        assert_eq!(app.completion.files.as_ref().unwrap().result, expected);
    }
    let mut local = connected();
    assert_eq!(local.on_key(Key::Char('@'), now()), Effect::ListFiles);
    assert!(matches!(local.on_key(Key::Char('x'),now()),Effect::Search{query,..} if query=="x"));
}

/// An attached app whose seam lists file themes `solar` and `solar dawn`.
fn theme_attached() -> (App, std::sync::Arc<crate::configure_fake::Fake>) {
    let mut fake = crate::configure_fake::Fake::new(Vec::new());
    fake.themes = vec!["solar".to_owned(), "solar dawn".to_owned()];
    let seam = std::sync::Arc::new(fake);
    let mut app = attached();
    app.set_configure(Some(Arc::clone(&seam) as Arc<dyn crate::Configure>));
    (app, seam)
}

#[test]
fn slash_theme_space_lists_builtin_and_file_themes() {
    let (mut app, _) = theme_attached();
    type_text(&mut app, "/theme ");
    assert_eq!(
        shown(&app),
        ["auto", "dark", "light", "solar", "solar dawn"]
    );
    assert_eq!(selected(&app), Some("auto".to_owned()));
}

#[test]
fn slash_theme_names_filter_by_prefix() {
    let (mut app, _) = theme_attached();
    type_text(&mut app, "/theme s");
    assert_eq!(shown(&app), ["solar", "solar dawn"]);
    let (mut app, _) = theme_attached();
    type_text(&mut app, "/theme l");
    assert_eq!(shown(&app), ["light"]);
    // With no match there is no panel: Esc still interrupts, Enter
    // still refuses the unknown name.
    let (mut app, _) = theme_attached();
    type_text(&mut app, "/theme zzz");
    assert!(app.completions().is_none());
}

#[test]
fn slash_themes_without_the_space_offers_no_theme_names() {
    // `/themes` is a `/` query, not the `/theme ` boundary: it shows the
    // `/` list's empty row, never theme names.
    let (mut app, _) = theme_attached();
    type_text(&mut app, "/themes");
    assert_eq!(
        app.completions().map(|completions| completions.lines()),
        Some(vec!["no matches".to_owned()])
    );
}

#[test]
fn tab_accepts_the_theme_name_replacing_the_rest() {
    let (mut app, _) = theme_attached();
    type_text(&mut app, "/theme l");
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.draft(), "/theme light");
    // The accepted rest still matches: the panel shows its one row.
    assert_eq!(shown(&app), ["light"]);
    // A spaced name survives whole.
    let (mut app, _) = theme_attached();
    type_text(&mut app, "/theme solar d");
    assert_eq!(app.on_key(Key::Tab, now()), Effect::None);
    assert_eq!(app.draft(), "/theme solar dawn");
    assert_eq!(shown(&app), ["solar dawn"]);
}

#[test]
fn enter_runs_the_accepted_theme_name() {
    let (mut app, seam) = theme_attached();
    type_text(&mut app, "/theme li");
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(
        seam.writes(),
        [(
            PathBuf::from("/w"),
            crate::configure::Layer::Global,
            "tui.theme".to_owned(),
            "\"light\"".to_owned()
        )]
    );
    assert_eq!(app.take_theme_choice(), Some(crate::ThemeSetting::Light));
    assert_eq!(app.draft(), "");
}

#[test]
fn an_extension_theme_blanks_fiber_candidates_and_runs_in_place() {
    let (mut app, seam) = theme_attached();
    answer_rows(
        &mut app,
        json!([{"name": "theme", "description": "The theme extension.", "tag": "acme", "extension": true}]),
    );
    // While the extension owns the name, no Fiber candidates show.
    type_text(&mut app, "/theme ");
    assert!(app.completions().is_none());
    // Enter sends the draft out as the extension's command instead.
    let mut app = {
        let (mut app, _) = theme_attached();
        answer_rows(
            &mut app,
            json!([{"name": "theme", "description": "The theme extension.", "tag": "acme", "extension": true}]),
        );
        app
    };
    let lines = sent(enter(&mut app, "/theme light"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "command");
    assert_eq!(lines[0]["args"]["name"], "theme");
    assert_eq!(lines[0]["args"]["text"], "light");
    assert!(seam.writes().is_empty());
}

#[test]
fn slash_theme_with_an_image_is_a_prompt() {
    // The draft-image guard holds: with an image `/theme light` sends a
    // prompt, saving and queueing nothing.
    let (mut app, seam) = theme_attached();
    paste_image(&mut app);
    type_text(&mut app, "/theme light");
    let lines = sent(app.on_key(Key::Enter, now()));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "prompt");
    assert!(seam.writes().is_empty());
    assert_eq!(app.take_theme_choice(), None);
}

#[test]
fn slash_theme_name_without_a_seam_is_a_notice() {
    let mut app = attached();
    let lines = sent(enter(&mut app, "/theme light"));
    assert!(lines.is_empty());
    assert_eq!(app.draft(), "");
    assert!(
        app.notices().iter().any(
            |notice| matches!(notice, crate::app::NoticeLine::Notice { code, text, .. } if code == "io_failed" && text == "Not available in this terminal.")
        ),
        "a notice names why nothing changed"
    );
}

#[test]
fn extensions_loaded_asks_again_and_its_answer_runs_the_new_command() {
    // An offer's approval loads its extensions with no `reloaded`: the
    // `extensions_loaded` that follows brings their commands in.
    let mut app = attached();
    answer_commands(&mut app, &["tdd"]);
    let asked = app.on_line(session_line(
        "extensions_loaded",
        json!({"extensions": [{"name": "acme", "version": "1.0.0"}]}),
    ));
    assert_eq!(asked.len(), 1);
    let asked: Value = serde_json::from_str(&asked[0]).unwrap_or_default();
    assert_eq!(asked["command"], "commands");
    let answer = session_line(
        "command_accepted",
        json!({"command_id": asked["id"], "result": {"commands": [
            {"name": "deploy", "description": "Deploys.", "tag": "acme",
                "extension": true}]}}),
    );
    assert!(app.on_line(answer).is_empty());
    let lines = sent(enter(&mut app, "/deploy now"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "command");
    assert_eq!(lines[0]["args"], json!({"name": "deploy", "text": "now"}));
}

#[test]
fn closing_the_panels_leaves_the_next_slash_panel_able_to_open() {
    // An empty draft closes nothing: the next `/` still opens.
    let mut app = attached();
    app.close_completion_panels();
    type_text(&mut app, "/");
    assert!(app.completions().is_some());
    // A `/` draft closes the `/` panel; emptying the draft reopens it.
    let mut app = attached();
    type_text(&mut app, "/");
    assert!(app.completions().is_some());
    app.close_completion_panels();
    assert!(app.completions().is_none());
    app.on_key(Key::Backspace, now());
    assert_eq!(app.draft(), "");
    type_text(&mut app, "/");
    assert!(app.completions().is_some());
    // A `/theme ` draft closes the names panel.
    let mut app = attached();
    type_text(&mut app, "/theme ");
    assert!(app.completions().is_some());
    app.close_completion_panels();
    assert!(app.completions().is_none());
    // A `/theme zzz` draft with no matching name closes nothing: the
    // names still open once the draft matches again.
    let mut app = attached();
    type_text(&mut app, "/theme zzz");
    assert!(app.completions().is_none());
    app.close_completion_panels();
    for _ in 0..3 {
        app.on_key(Key::Backspace, now());
    }
    assert_eq!(app.draft(), "/theme ");
    assert!(app.completions().is_some());
    // A `/` draft under the search panel leaves the search untouched,
    // and the `/` panel still opens after the search closes.
    let mut app = attached();
    type_text(&mut app, "/");
    assert_eq!(app.on_key(Key::CtrlR, now()), Effect::None);
    assert!(app.search_panel().is_some());
    app.close_completion_panels();
    assert!(app.search_panel().is_some());
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.search_panel().is_none());
    assert!(app.completions().is_some());
    // An open `@` panel closes, and a late search answer is ignored.
    let mut app = attached();
    assert_eq!(app.on_key(Key::Char('@'), now()), Effect::ListFiles);
    let generation = app.generation();
    app.on_files(generation, Ok(vec!["a.rs".to_owned()]));
    assert!(app.files_open());
    app.close_completion_panels();
    assert!(!app.files_open());
    assert!(app.completions().is_none());
    app.on_files(generation, Ok(vec!["a.rs".to_owned()]));
    assert!(app.completions().is_none());
}
