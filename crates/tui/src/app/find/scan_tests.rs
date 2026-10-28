//! Tests for the page scanners and snippet builders
//! (`docs/tui.md`, "Search"): what a match is in a page's text.

use super::super::tests::{attached, count, search_all, text_turn};

#[test]
fn file_calls_name_their_paths_but_replies_and_other_calls_do_not() {
    let mut app = attached(80, 24);
    let mut log = Vec::new();
    let mut seq = 0;
    let mut feed = |kind: &str, action: Option<&str>, payload: serde_json::Value| {
        seq += 1;
        let line = contract::Envelope {
            kind: kind.to_owned(),
            session_id: contract::SessionId(super::super::tests::SESSION.to_owned()),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: Some(contract::TurnId("t_files".to_owned())),
            action_id: action.map(|id| contract::ActionId(id.to_owned())),
            seq: Some(contract::Seq(seq)),
            payload: payload.as_object().unwrap().clone(),
        };
        log.push(line.clone());
        app.on_line(crate::link::Line::Session(line));
    };
    feed(
        "turn_started",
        None,
        serde_json::json!({"input": [{"type": "message", "source": "driver", "content": [{"type": "text", "text": "needle prompt"}]}]}),
    );
    for (action, tool, args) in [
        ("a_read", "read", serde_json::json!({"path": "src/read.rs"})),
        ("a_edit", "edit", serde_json::json!({"path": "src/edit.rs"})),
        (
            "a_write",
            "write",
            serde_json::json!({"path": "src/write.rs"}),
        ),
        (
            "a_shell",
            "shell",
            serde_json::json!({"command": "cat src/shell.rs", "path": "not-a-file-call"}),
        ),
        (
            "a_other",
            "other",
            serde_json::json!({"path": "not-a-file-call"}),
        ),
        ("a_missing", "read", serde_json::json!({})),
        ("a_bad", "read", serde_json::json!({"path": 42})),
    ] {
        feed(
            "tool_call_requested",
            Some(action),
            serde_json::json!({"name": tool, "arguments": args}),
        );
        feed(
            "tool_call_completed",
            Some(action),
            serde_json::json!({"status": "completed", "content": [{"type": "text", "text": format!("needle {action}")}]}),
        );
    }
    feed(
        "text_completed",
        Some("a_reply"),
        serde_json::json!({"text": "needle reply"}),
    );
    feed(
        "turn_completed",
        None,
        serde_json::json!({"outcome": "completed"}),
    );
    let check = |found: Vec<(super::Anchor, usize, bool, super::Snippet)>| {
        for (text, path) in [
            ("needle prompt", ""),
            ("needle a_read", "src/read.rs"),
            ("needle a_edit", "src/edit.rs"),
            ("needle a_write", "src/write.rs"),
            ("needle a_shell", ""),
            ("needle a_other", ""),
            ("needle a_missing", ""),
            ("needle a_bad", ""),
            ("needle reply", ""),
        ] {
            let hits: Vec<_> = found
                .iter()
                .filter(|hit| hit.3.line.contains(text))
                .collect();
            assert!(!hits.is_empty(), "a match for {text}");
            for hit in hits {
                assert_eq!(hit.3.path, path, "{text}");
            }
        }
    };
    check(super::resident(app.pages(), 0, "needle").unwrap());
    for _ in 0..16 {
        text_turn(
            &mut log,
            &mut seq,
            &mut app,
            &["filler one", "filler two", "filler three", "filler four"],
        );
    }
    assert!(app.pages().part(0).is_none());
    let (rows, texts, turns, paths) = app.pages().fold_text_turns(0, &log).unwrap();
    let found = super::scratch(app.pages(), 0, &rows, &texts, &turns, &paths, "needle");
    check(found);
}

#[test]
fn a_match_names_the_turn_its_line_sits_in() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0;
    text_turn(&mut log, &mut seq, &mut app, &["needle one"]);
    text_turn(&mut log, &mut seq, &mut app, &["needle two"]);
    let found = super::resident(app.pages(), 0, "needle").unwrap();
    assert_eq!(
        found.iter().map(|hit| hit.3.turn).collect::<Vec<_>>(),
        [Some(0), Some(1)]
    );
    for turn in 0..16 {
        let filler: Vec<String> = (0..4).map(|line| format!("filler {turn} {line}")).collect();
        let refs: Vec<&str> = filler.iter().map(String::as_str).collect();
        text_turn(&mut log, &mut seq, &mut app, &refs);
    }
    assert!(app.pages().part(0).is_none());
    let (rows, texts, turns, paths) = app.pages().fold_text_turns(0, &log).unwrap();
    let found = super::scratch(app.pages(), 0, &rows, &texts, &turns, &paths, "needle");
    assert_eq!(
        found.iter().map(|hit| hit.3.turn).collect::<Vec<_>>(),
        [Some(0), Some(1)]
    );
}

#[test]
fn a_match_on_a_turns_first_and_last_row_is_in_it() {
    let turns = [(3, 2..5)];
    for (row, turn) in [
        (1, None),
        (2, Some(3)),
        (3, Some(3)),
        (4, Some(3)),
        (5, None),
        (6, None),
    ] {
        let line = crate::logical::Logical {
            text: "needle".to_owned(),
            from: Vec::new(),
            row,
            scopes: Vec::new(),
        };
        assert_eq!(
            super::snippet_for(&line, &(0..6), None, None, &turns, &Vec::new()).turn,
            turn,
            "row {row}"
        );
    }
}

#[test]
fn a_match_outside_any_turn_has_no_turn() {
    let mut app = attached(40, 10);
    let line = contract::Envelope {
        kind: "shell_command".to_owned(), session_id: contract::SessionId(super::super::tests::SESSION.to_owned()),
        ts: 0, schema_version: contract::SCHEMA_VERSION, turn_id: None, action_id: None,
        seq: Some(contract::Seq(1)), payload: serde_json::json!({"command": "needle", "process": {"exit_code": 0, "timed_out": false}, "output": "needle"}).as_object().unwrap().clone(),
    };
    app.on_line(crate::link::Line::Session(line));
    let found = super::resident(app.pages(), 0, "needle").unwrap();
    assert!(!found.is_empty());
    assert!(found.iter().all(|hit| hit.3.turn.is_none()));
    assert!(found.iter().all(|hit| hit.3.path.is_empty()));
}

#[test]
fn a_snippet_is_its_line_and_one_line_either_side_cut_to_400() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    let long: String = format!("{}needle{}", "x".repeat(250), "x".repeat(250));
    let exact: String = format!("needle{}", "y".repeat(394));
    text_turn(
        &mut log,
        &mut seq,
        &mut app,
        &["the line before", &long, &exact, "the line after"],
    );
    assert!(search_all(&mut app, &log, "needle").is_empty());
    let flat = app.find.flat();
    assert_eq!(flat.len(), 2);
    // The long line is cut to 400 characters around the match.
    let first = &flat[0].snippet;
    assert_eq!(first.line.chars().count(), super::SNIPPET);
    assert_eq!(first.at, 197..203);
    assert_eq!(&first.line[first.at.start..first.at.end], "needle");
    // Each text is its own block, so the matches sit between blank
    // separator rows: the context skips them to the nearest text.
    assert_eq!(first.before, "the line before");
    assert_eq!(first.after, exact);
    // A line of exactly 400 characters stays whole.
    let second = &flat[1].snippet;
    assert_eq!(second.line, exact);
    assert_eq!(second.at, 0..6);
    assert_eq!(second.before, long.chars().take(400).collect::<String>());
    assert_eq!(second.after, "the line after");
}

#[test]
fn two_matches_on_lines_sharing_their_first_400_characters_stay_distinct() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    let shared = "s".repeat(400);
    let one = format!("{shared}needle one");
    let two = format!("{shared}needle two");
    let end = format!("{}needle", "z".repeat(400));
    text_turn(&mut log, &mut seq, &mut app, &[&one, &two, &end]);
    assert!(search_all(&mut app, &log, "needle").is_empty());
    // Identity is the whole line's hash, never the cut snippet: all
    // three matches are kept, each cut around its own match.
    let flat = app.find.flat();
    assert_eq!(flat.len(), 3);
    assert_ne!(flat[0].anchor, flat[1].anchor);
    for kept in &flat {
        assert_eq!(kept.snippet.line.chars().count(), super::SNIPPET);
        assert_eq!(
            &kept.snippet.line[kept.snippet.at.start..kept.snippet.at.end],
            "needle"
        );
    }
    assert_eq!(flat[0].snippet.at, 390..396);
    assert_eq!(flat[1].snippet.at, 390..396);
    // A match at the line's end cuts the last 400 characters.
    assert_eq!(flat[2].snippet.at, 394..400);
    assert_ne!(flat[0].snippet.line, flat[1].snippet.line);
    assert_eq!(count(&app), "1 of 3");
}

#[test]
fn a_match_at_a_page_edge_has_no_neighbour_across_it() {
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0u64;
    text_turn(
        &mut log,
        &mut seq,
        &mut app,
        &["needle one", "needle two", "needle three"],
    );
    assert!(search_all(&mut app, &log, "needle").is_empty());
    let flat = app.find.flat();
    assert_eq!(flat.len(), 3);
    // The prompt bubble is the page's first logical line, with the time
    // under it; the replies sit between blank separators, which the
    // context skips to the nearest text.
    assert_eq!(flat[0].snippet.before, "you 00:00");
    assert_eq!(flat[0].snippet.after, "needle two");
    assert_eq!(flat[2].snippet.before, "needle two");
    assert_eq!(flat[2].snippet.after, "▣ completed");
    // The bubble is the page's first line and the end marker its last:
    // a match on either has no line past the edge.
    assert!(search_all(&mut app, &log, "go").is_empty());
    let flat = app.find.flat();
    assert_eq!(flat.len(), 1);
    assert_eq!(flat[0].snippet.before, "");
    assert_eq!(flat[0].snippet.after, "you 00:00");
    assert!(search_all(&mut app, &log, "completed").is_empty());
    let flat = app.find.flat();
    assert_eq!(flat.len(), 1);
    assert_eq!(flat[0].snippet.before, "needle three");
    assert_eq!(flat[0].snippet.after, "");
}

#[test]
fn a_scratch_refold_draws_the_current_sender_name() {
    use super::super::tests::numbered;
    let mut app = attached(40, 10);
    let mut log = Vec::new();
    let mut seq = 0;
    app.on_line(crate::app::testkit::sender_status("worker", true));
    let first = numbered(
        &mut log,
        &mut seq,
        "turn_started",
        serde_json::json!({"input": [
            {"type": "message", "source": "driver",
             "content": [{"type": "text", "text": "go"}]},
            {"type": "message", "source": "session",
             "from_session_id": crate::app::testkit::SENDER,
             "content": [{"type": "text", "text": "please check"}]},
        ]}),
        None,
    );
    app.on_line(first);
    app.on_line(numbered(
        &mut log,
        &mut seq,
        "turn_completed",
        serde_json::json!({"outcome": "completed"}),
        None,
    ));
    for turn in 0..16 {
        let filler: Vec<String> = (0..4).map(|line| format!("filler {turn} {line}")).collect();
        let refs: Vec<&str> = filler.iter().map(String::as_str).collect();
        text_turn(&mut log, &mut seq, &mut app, &refs);
    }
    assert!(app.pages().part(0).is_none(), "page 0 is dropped");
    // A rename while the page is dropped reaches its next refold.
    app.on_line(crate::app::testkit::sender_status("renamed", true));
    let (rows, _, _, _) = app.pages().fold_text_turns(0, &log).unwrap();
    let shown: Vec<String> = rows.iter().map(|(line, _)| line.to_string()).collect();
    assert!(
        shown.iter().any(|row| row.contains("renamed · 00:00")),
        "the refold uses the current name: {shown:?}"
    );
    assert!(
        !shown.iter().any(|row| row.contains("worker · 00:00")),
        "{shown:?}"
    );
}
