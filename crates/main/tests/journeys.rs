//! Binary-level journeys 3 to 6 (`docs/testing/screens.md`, "Screens"): the
//! real binary in a pseudo-terminal across features, with the settled grid
//! snapshotted as text rows and cursor position at named moments. Colour
//! stays with the in-process snapshots and the look tests.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::Path;

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use support::journey::{
    dismiss_cache_rebuild, grid_text, install_picker_provider, only_session, provider_full,
    provider_heading, reply, shows, shows_row, stalled, tui_off, working_elapsed,
};
use support::pty::{Grid, KITTY_PUSH, Run, WAITING_TITLE, until_hub_exits};
use support::{PIXEL, PIXEL_BASE64, Setup, TRUECOLOUR, write_json};

/// `grid_text` over one row, without its cursor line.
#[cfg(test)]
fn one(row: &str, ids: &[&str]) -> String {
    let ids: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
    let text = grid_text(&[row.to_owned()], (0, 0), false, &ids);
    text.strip_suffix("\ncursor=(0,0)").unwrap().to_owned()
}

#[test]
fn a_session_id_reads_session() {
    assert_eq!(one("ab", &[""]), "ab");
    assert_eq!(
        one("  fiber resume 01jb2kabc", &["01jb2kabc"]),
        "  fiber resume [session]"
    );
    assert_eq!(
        one("  fiber resume other", &["01jb2kabc"]),
        "  fiber resume other"
    );
}

#[test]
fn an_image_line_name_reads_id() {
    assert_eq!(one("  ▣ i_3fz9.png · 1×1", &[]), "  ▣ i_[id].png · 1×1");
    assert_eq!(one("  ▣ i_[id].png · 1×1", &[]), "  ▣ i_[id].png · 1×1");
    assert_eq!(one("  i_.png", &[]), "  i_.png");
}

#[test]
fn a_clock_time_reads_hh_mm_unless_a_digit_or_colon_touches_it() {
    assert_eq!(one("   you 14:12", &[]), "   you HH:MM");
    assert_eq!(one("   you 14:12 and 09:01", &[]), "   you HH:MM and HH:MM");
    assert_eq!(one("10:30:15", &[]), "10:30:15");
    assert_eq!(one("110:30", &[]), "110:30");
    assert_eq!(one("10:305", &[]), "10:305");
    assert_eq!(one("1:30", &[]), "1:30");
    assert_eq!(one("(14:12)", &[]), "(HH:MM)");
}

#[test]
fn a_closing_row_reads_the_same_with_a_duration_or_without() {
    let want = "  ▣ completed · [duration] · 13 tokens";
    assert_eq!(one("  ▣ completed · 13 tokens", &[]), want);
    assert_eq!(one("  ▣ completed · 1s · 13 tokens", &[]), want);
    let want = "  ▣ completed · [duration] · 2 calls";
    assert_eq!(one("  ▣ completed · 2 calls", &[]), want);
    assert_eq!(one("  ▣ completed · 1m 02s · 2 calls", &[]), want);
    assert_eq!(one("  ▣ completed · 1h 02m · 2 calls", &[]), want);
    assert_eq!(one("  ▣ completed", &[]), "  ▣ completed · [duration]");
    assert_eq!(one("  ▣ failed · 3s", &[]), "  ▣ failed · [duration]");
}

#[test]
fn a_framed_closing_row_reads_the_same_whatever_the_duration_width() {
    // The three rows are drawn at the same frame width: a present
    // duration of n bytes leaves n + 3 fewer padding spaces.
    let absent = format!("⋮  ▣ completed · 13 tokens{}⋮", " ".repeat(60));
    let narrow = format!("⋮  ▣ completed · 5s · 13 tokens{}⋮", " ".repeat(55));
    let wide = format!("⋮  ▣ completed · 12s · 13 tokens{}⋮", " ".repeat(54));
    let want = one(&absent, &[]);
    assert_eq!(one(&narrow, &[]), want);
    assert_eq!(one(&wide, &[]), want);
    // The edge: a two-space run still takes the padding back.
    let absent_two = "⋮  ▣ completed · 13 tokens       ⋮";
    let narrow_two = "⋮  ▣ completed · 5s · 13 tokens  ⋮";
    assert_eq!(one(narrow_two, &[]), one(absent_two, &[]));
    // The frame-adjacent edge: one padding space takes five back.
    let absent_one = "⋮  ▣ completed · 13 tokens      ⋮";
    let narrow_one = "⋮  ▣ completed · 5s · 13 tokens ⋮";
    assert_eq!(one(narrow_one, &[]), one(absent_one, &[]));
}

#[test]
fn an_interrupted_row_reads_the_same_with_a_duration_or_without() {
    assert_eq!(
        one("  ▣ interrupted · 4s", &[]),
        "  ▣ interrupted · [duration]"
    );
    assert_eq!(one("  ▣ interrupted", &[]), "  ▣ interrupted · [duration]");
    assert_eq!(
        one("  ▣ interrupted · 2s · 5 tokens", &[]),
        "  ▣ interrupted · [duration] · 5 tokens"
    );
}

#[test]
fn a_closing_rule_leaves_other_rows_alone() {
    assert_eq!(one("  ▣ i_[id].png · 1×1", &[]), "  ▣ i_[id].png · 1×1");
    assert_eq!(one("  ▣ completedx", &[]), "  ▣ completedx");
    assert_eq!(
        one("  ▣ completed · 1sx · 3 calls", &[]),
        "  ▣ completed · [duration] · 1sx · 3 calls"
    );
    assert_eq!(
        one("  ▣ completed · 5m 02", &[]),
        "  ▣ completed · [duration] · 5m 02"
    );
    assert_eq!(one("  completed · 3s", &[]), "  completed · 3s");
}

#[test]
fn a_tool_group_summary_reads_its_duration_as_a_placeholder() {
    assert_eq!(
        one("  ● Asked 3 questions 4s ▸", &[]),
        "  ● Asked 3 questions [duration] ▸"
    );
    assert_eq!(
        one("  ● 1 failed model call 0s ▸", &[]),
        "  ● 1 failed model call [duration] ▸"
    );
    assert_eq!(
        one("  ● Ran 2 commands 1m 02s ▾", &[]),
        "  ● Ran 2 commands     [duration] ▾"
    );
    assert_eq!(
        one("  ● Asked 3 questions ▸", &[]),
        "  ● Asked 3 questions ▸"
    );
    assert_eq!(one("  ▸ 4s later", &[]), "  ▸ 4s later");
    assert_eq!(
        one("  ● 4s ran 3 questions ▸", &[]),
        "  ● 4s ran 3 questions ▸"
    );
}

#[test]
fn a_full_width_tool_group_reads_the_same_whatever_the_duration_width() {
    // Both rows fill the same width: the 3-char duration leaves one
    // fewer padding space than the 2-char one.
    let two = format!("  ● Asked 3 questions{}4s ▸", " ".repeat(10));
    let three = format!("  ● Asked 3 questions{}12s ▸", " ".repeat(9));
    assert_eq!(one(&three, &[]), one(&two, &[]));
}

#[test]
fn a_running_tool_group_reads_its_spinner_as_a_placeholder() {
    let want = "  * Asked 3 questions · ask_user 3 questions [duration] ▸";
    assert_eq!(
        one("  ⠇ Asked 3 questions · ask_user 3 questions 4s ▸", &[]),
        want
    );
    assert_eq!(
        one("  ⠙ Asked 3 questions · ask_user 3 questions 0s ▸", &[]),
        want
    );
    assert_eq!(
        one("  ● Asked 3 questions 4s ▸", &[]),
        "  ● Asked 3 questions [duration] ▸"
    );
    assert_eq!(
        one("  ⠇Asked 3 questions 4s ▸", &[]),
        "  ⠇Asked 3 questions [duration] ▸"
    );
    assert_eq!(one("  ⠇ Working on it", &[]), "  ⠇ Working on it");
}

#[test]
fn the_provider_heading_reads_the_same_in_every_freshness_state() {
    for row in [
        "  fake",
        "  fake ⟳ refreshing ⠙",
        "  fake · updated 3s ago",
        "  fake · updated 1m 05s ago",
    ] {
        assert_eq!(one(row, &[]), "  fake [freshness]", "{row}");
    }
    assert_eq!(one("  alpha-1", &[]), "  alpha-1");
    assert_eq!(one("  fake/alpha-1", &[]), "  fake/alpha-1");
    assert_eq!(one("  fake ⟳ refreshing ", &[]), "  fake ⟳ refreshing");
    assert_eq!(one("  fake · updated ago", &[]), "  fake · updated ago");
    assert_eq!(one("  fake · updated  ago", &[]), "  fake · updated  ago");
    // The row is trimmed before the rule runs, so the empty spinner is
    // reached directly.
    assert_eq!(
        provider_heading("  fake ⟳ refreshing "),
        "  fake ⟳ refreshing "
    );
}

#[test]
fn the_working_line_reads_its_spinner_and_elapsed_time_as_placeholders() {
    let want_bare = "* Working [elapsed] · Esc to interrupt";
    let want = "  * Working [elapsed] · Esc to interrupt";
    assert_eq!(one("  ⠙ Working 11m 14s · Esc to interrupt", &[]), want);
    assert_eq!(one("  ● Working 3s · Esc to interrupt", &[]), want);
    assert_eq!(one("  ⠏ Working 0s · Esc to interrupt", &[]), want);
    assert_eq!(one("⠙ Working 3s · Esc to interrupt", &[]), want_bare);
}

#[test]
fn a_reply_that_says_working_is_left_alone() {
    assert_eq!(one("  Working", &[]), "  Working");
    assert_eq!(one("  Working 3s", &[]), "  Working 3s");
    assert_eq!(
        one("  x Working 3s · Esc to interrupt", &[]),
        "  x Working 3s · Esc to interrupt"
    );
    assert_eq!(one("  ⠙ Working 3s", &[]), "  ⠙ Working 3s");
    assert_eq!(
        one("  ⠙Working 3s · Esc to interrupt", &[]),
        "  ⠙Working 3s · Esc to interrupt"
    );
}

#[test]
fn rows_are_trimmed_and_the_cursor_line_is_last() {
    let rows = ["a   ".to_owned(), String::new(), "  b ".to_owned()];
    assert_eq!(
        grid_text(&rows, (29, 4), false, &[]),
        "a\n\n  b\ncursor=(29,4)"
    );
}

#[test]
fn a_hidden_cursor_reads_hidden_wherever_it_is_parked() {
    let rows = ["a".to_owned()];
    assert_eq!(grid_text(&rows, (29, 4), true, &[]), "a\ncursor=hidden");
    assert_eq!(grid_text(&rows, (0, 7), true, &[]), "a\ncursor=hidden");
    assert_ne!(
        grid_text(&rows, (29, 4), false, &[]),
        grid_text(&rows, (0, 7), false, &[])
    );
}

/// The model picker mid-session: opened after a first answer, narrowed by
/// a display name only the name carries, switched to the other model,
/// answered on it, and quit from an open picker.
#[test]
fn journey_model_picker_by_name_switch_answer_quit_from_picker() {
    let setup = Setup::new();
    // The first reply carries no usage, so opening the picker draws no
    // cache-rebuild notice.
    let server = ProviderServer::start([reply("Hello."), reply("Beta here.")]).unwrap();
    install_picker_provider(&setup, &server);
    let mut run = Run::spawn(
        &setup,
        120,
        32,
        &[],
        &[
            ("TERM", "xterm-256color"),
            ("FIBER_TEST_FAKE_KEY", "sk-test"),
        ],
    );
    let ids: Vec<String> = Vec::new();
    // The end of the first frame proves the input reader runs before the
    // prompt goes out.
    run.ready();
    let from = run.output().len();
    run.write(b"say hi\r");
    run.wait_screen("the reply", |screen| shows(screen, "Hel"));
    run.turn_finished(from);
    // Ctrl+L opens the picker: the open picker's catalogue proves the key
    // is taken. Both models list before any filter.
    run.write(b"\x0c");
    let opened = run.wait_screen("the open picker with its catalogue", |screen| {
        shows(screen, "Type to search")
            && shows(screen, "alpha-1")
            && shows(screen, "beta-2")
            && !shows(screen, "refreshing…")
    });
    assert!(
        shows(&opened, "Type to search"),
        "the picker shows its filter"
    );
    assert!(shows(&opened, "alpha-1"), "alpha-1 lists before the filter");
    assert!(shows(&opened, "beta-2"), "beta-2 lists before the filter");
    insta::assert_snapshot!(
        "model_picker_open",
        grid_text(&opened.rows, opened.cursor, opened.hide_cursor, &ids)
    );
    // What only the display name carries narrows the list to `alpha-1`.
    run.write(b"opus 5.5");
    let filtered = run.wait_screen("the name-filtered list", |screen| {
        shows(screen, "1 of 2 models") && shows(screen, "alpha-1") && !shows(screen, "beta-2")
    });
    assert!(
        shows(&filtered, "1 of 2 models"),
        "the count names one of two"
    );
    assert!(shows(&filtered, "alpha-1"), "the named model stays");
    assert!(!shows(&filtered, "beta-2"), "beta-2 dropped out");
    insta::assert_snapshot!(
        "model_picker_by_name",
        grid_text(&filtered.rows, filtered.cursor, filtered.hide_cursor, &ids)
    );
    // Clear the query, find `beta-2` and choose it: the picker closes.
    run.write(b"\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7fbeta");
    run.wait_screen("the beta-filtered list", |screen| {
        shows(screen, "1 of 2 models") && shows(screen, "beta-2") && !shows(screen, "opus 5.5")
    });
    run.write(b"\r");
    run.wait_screen("the closed picker", |screen| {
        !shows(screen, "Type to search") && shows(screen, "›")
    });
    // The switch after a counted turn warns of a cache rebuild; click the
    // notice away before the next prompt so no snapshot holds it.
    let notice = run.wait_screen("the cache-rebuild notice", |screen| {
        shows(screen, "cache_rebuild") && shows(screen, "✕")
    });
    dismiss_cache_rebuild(&mut run, &notice);
    run.wait_screen("the dismissed notice", |screen| {
        !shows(screen, "cache_rebuild")
    });
    // The next prompt is answered on the chosen model.
    let from = run.output().len();
    run.write(b"again\r");
    run.wait_screen("the second reply", |screen| shows(screen, "Beta here."));
    run.turn_finished(from);
    let answered = run.wait_screen("the second closed turn", |screen| {
        shows(screen, "Beta here.")
            && screen
                .rows
                .iter()
                .filter(|row| row.contains("▣ completed"))
                .count()
                == 2
            && !shows(screen, "Esc to interrupt")
    });
    insta::assert_snapshot!(
        "model_picker_answered_on_beta",
        grid_text(&answered.rows, answered.cursor, answered.hide_cursor, &ids)
    );
    let requests = server.requests();
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(second["model"], "beta-2", "the second turn ran on beta-2");
    // Quit from an open picker filtered by the name.
    run.write(b"\x0c");
    run.wait_screen("the reopened picker", |screen| {
        shows(screen, "Type to search") && shows(screen, "alpha-1")
    });
    run.write(b"opus 5.5");
    run.wait_screen("the name-filtered list again", |screen| {
        shows(screen, "1 of 2 models") && shows(screen, "alpha-1")
    });
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored terminal", |screen| {
        !screen.alternate_screen && !screen.hide_cursor
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// The scripted `ask_user` step: one call with three questions.
fn form_step() -> Value {
    json!({"tool_calls": [{"name": "ask_user", "arguments": {"questions": [
        {"header": "Scope", "question": "Which crates should the change touch?",
         "multiSelect": true, "options": [
            {"label": "hub", "description": "the session host"},
            {"label": "tui", "description": "the terminal"}]},
        {"header": "Tests", "question": "Which tests cover it?",
         "options": [{"label": "unit"}, {"label": "pty"}]},
        {"header": "Docs", "question": "Which docs change?"},
    ]}}]})
}

/// Whether the grid draws the three-question form's title and first
/// question.
fn form_drawn(screen: &Grid) -> bool {
    shows(
        screen,
        "Question from run it · 3 questions, any can be skipped",
    ) && shows(screen, "Which crates should the change touch?")
}

/// A question form answered through its tabs, then the session's second
/// form declined with Esc.
#[test]
fn journey_question_form_answered_then_declined() {
    let setup = Setup::new();
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            form_step(),
            {"text": ["Thanks."]},
            form_step(),
            {"text": ["Thanks after decline."]},
        ]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200}, "tui": tui_off()}),
    );
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    let ids: Vec<String> = Vec::new();
    run.ready();
    let from = run.output().len();
    run.write(b"run it\r");
    run.wait_screen("the question form", form_drawn);
    // The waiting title orders the session's Waiting state before the
    // grid with no working line.
    run.wait_bytes(from, WAITING_TITLE, "the question turn waiting");
    let open = run.wait_screen("the open form without a working line", |screen| {
        form_drawn(screen) && !shows(screen, "Esc to interrupt")
    });
    insta::assert_snapshot!(
        "question_form_open",
        grid_text(&open.rows, open.cursor, open.hide_cursor, &ids)
    );
    // Answer the first form: a digit chooses an option, → and Tab move
    // between questions, Enter on Submit answers.
    run.write(b"2");
    run.wait_screen("the toggled option", |screen| shows(screen, "2 [x] tui"));
    run.write(b"\x1b[C");
    run.wait_screen("the Tests tab", |screen| {
        shows(screen, "Which tests cover it?")
    });
    run.write(b"\t");
    run.wait_screen("the Docs words row", |screen| {
        shows(screen, "Which docs change?") && shows(screen, "Type an answer")
    });
    run.write(b"2");
    run.wait_screen("the typed answer", |screen| shows(screen, "✎ 2"));
    run.write(b"\t");
    run.wait_screen("the Submit tab", |screen| shows(screen, "Review"));
    let from = run.output().len();
    run.write(b"\r");
    let card = run.wait_screen("the answered card", |screen| {
        shows(screen, "you answered") && shows(screen, "Thanks.")
    });
    assert!(shows(&card, "you answered"), "the answers land in the card");
    assert!(shows(&card, "Thanks."), "the turn goes on to its text");
    run.turn_finished(from);
    let answered = run.wait_screen("the closed turn", |screen| {
        shows(screen, "Thanks.")
            && shows(screen, "▣ completed")
            && !shows(screen, "Esc to interrupt")
    });
    insta::assert_snapshot!(
        "question_form_answered",
        grid_text(&answered.rows, answered.cursor, answered.hide_cursor, &ids)
    );
    // The session's second form, snapshotted open before Esc declines it.
    let decline_from = run.output().len();
    run.write(b"again\r");
    run.wait_screen("the second form", |screen| {
        form_drawn(screen) && shows(screen, "Thanks.")
    });
    run.wait_bytes(
        decline_from,
        WAITING_TITLE,
        "the second question turn waiting",
    );
    let second = run.wait_screen("the second form without a working line", |screen| {
        form_drawn(screen) && !shows(screen, "Esc to interrupt")
    });
    insta::assert_snapshot!(
        "question_form_second_open",
        grid_text(&second.rows, second.cursor, second.hide_cursor, &ids)
    );
    // The declined answer starts a free-text model call; quitting right
    // after the decline leaves that call's reply unwaited and unsnapshotted.
    run.write(b"\x1b[27u");
    let declined = run.wait_screen("the declined form", |screen| {
        shows(screen, "declined") && !shows(screen, "Question from")
    });
    assert!(
        declined.contents.contains("declined"),
        "{}",
        declined.contents
    );
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |screen| {
        !screen.alternate_screen && !screen.hide_cursor && shows(screen, "fiber resume")
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// Whether the grid's text holds the answer to the pasted image, drawn
/// with its image line.
fn pixel_answered(screen: &Grid) -> bool {
    shows(screen, "A pixel.") && shows(screen, "· 1×1")
}

/// An image pasted from the clipboard, answered, the session closed and
/// resumed from the list: the image line and the answer draw again from
/// the log.
#[test]
fn journey_image_paste_answer_close_resume() {
    let setup = Setup::new();
    let server = ProviderServer::start([reply("A pixel.")]).unwrap();
    provider_full(
        &setup,
        &json!({"input": ["text", "image"]}),
        &server,
        &json!({}),
    );
    // A fake clipboard program first on the child's PATH, printing the
    // pixel in its real program's form.
    let bin = setup.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let (name, body) = if cfg!(target_os = "macos") {
        let hex: String = PIXEL.iter().map(|byte| format!("{byte:02X}")).collect();
        (
            "osascript",
            format!("printf '\\302\\253data PNGf{hex}\\302\\273\\n'"),
        )
    } else {
        let octal: String = PIXEL.iter().map(|byte| format!("\\{byte:03o}")).collect();
        ("wl-paste", format!("printf '{octal}'"))
    };
    fakes::script(&bin, name, &body);
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let mut env: Vec<(&str, &str)> = vec![
        ("FIBER_TEST_FAKE_KEY", "sk-test"),
        ("PATH", path.to_str().unwrap()),
    ];
    if !cfg!(target_os = "macos") {
        env.push(("WAYLAND_DISPLAY", "fiber-test"));
    }
    let mut run = Run::spawn(&setup, 120, 32, &[], &env);
    let mut ids: Vec<String> = Vec::new();
    run.wait_screen("the first frame", |screen| shows(screen, "›"));
    run.ready();
    run.write(&[0x16]);
    run.wait_screen("the pasted image", |screen| shows(screen, "[Image #1]"));
    run.write(b" what is this");
    let pasted = run.wait_screen("the image beside its text", |screen| {
        shows(screen, "[Image #1] what is this")
    });
    insta::assert_snapshot!(
        "image_paste_pasted",
        grid_text(&pasted.rows, pasted.cursor, pasted.hide_cursor, &ids)
    );
    let from = run.output().len();
    run.write(b"\r");
    run.wait_screen("the answer with its image line", pixel_answered);
    run.turn_finished(from);
    let answered = run.wait_screen("the closed turn", |screen| {
        pixel_answered(screen) && shows(screen, "▣ completed") && !shows(screen, "Esc to interrupt")
    });
    insta::assert_snapshot!(
        "image_paste_answered",
        grid_text(&answered.rows, answered.cursor, answered.hide_cursor, &ids)
    );
    // The request carries the pasted image as an image part.
    let requests = server.requests();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let image = json!({"type": "input_image", "image_url": format!("data:image/png;base64,{PIXEL_BASE64}")});
    assert!(
        body["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["content"].as_array())
            .flatten()
            .any(|part| *part == image),
        "the request carries the pasted image: {body}"
    );
    // `/close` stops the session; nothing live remains to print a resume
    // line, so the restored primary screen is the quit.
    let id = only_session(&setup);
    ids.push(id.clone());
    run.write(b"/close\r");
    until_hub_exits(
        setup.deadline,
        &setup.session_socket(&id),
        Path::new(""),
        "the closed session to exit",
    );
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored primary screen", |screen| {
        !screen.alternate_screen && !screen.hide_cursor
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    // Resume lists the closed session by its first prompt; Enter opens it,
    // and the image line, the answer and the close draw from the log.
    let mut run = Run::spawn(&setup, 120, 32, &["resume"], &env);
    run.wait_screen("the first frame", |screen| shows(screen, "›"));
    run.ready();
    run.wait_screen("the session list", |screen| shows(screen, "what is this"));
    let attached = run.output().len();
    run.write(b"\r");
    let drawn = |screen: &Grid| {
        screen.alternate_screen && pixel_answered(screen) && shows(screen, "▣ completed")
    };
    run.wait_screen("the resumed conversation", drawn);
    // The attach title orders the open before the grid is read.
    run.wait_bytes(
        attached,
        support::pty::TITLE,
        "the attached session's title",
    );
    let resumed = run.wait_screen("the resumed conversation, attached", drawn);
    insta::assert_snapshot!(
        "image_paste_resumed",
        grid_text(&resumed.rows, resumed.cursor, resumed.hide_cursor, &ids)
    );
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |screen| {
        !screen.alternate_screen && !screen.hide_cursor && shows(screen, "fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn working_elapsed_needs_the_count() {
    assert!(working_elapsed("Working 0s · Esc to interrupt"));
    assert!(working_elapsed("Working 39s · Esc to interrupt"));
    assert!(working_elapsed("Working 1m 2s · Esc to interrupt"));
    assert!(!working_elapsed("Working · Esc to interrupt"));
    assert!(!working_elapsed("Working"));
    assert!(!working_elapsed("idle"));
}

/// An interrupted turn, then a turn whose first model call fails with a
/// 500 that Fiber retries to an answer.
#[test]
fn journey_interrupt_then_retried_answer() {
    let setup = Setup::new();
    let server =
        ProviderServer::start([stalled(), Response::status(500, "{}"), reply("Recovered.")])
            .unwrap();
    provider_full(
        &setup,
        &json!({}),
        &server,
        &json!({"retry": {"attempts": 3, "initial_delay_ms": 0, "max_delay_ms": 0}}),
    );
    let mut run = Run::spawn(&setup, 120, 32, &[], &[("FIBER_TEST_FAKE_KEY", "sk-test")]);
    let ids: Vec<String> = Vec::new();
    run.wait_screen("the first frame", |screen| shows(screen, "›"));
    run.ready();
    // The loop pushes kitty's flags only after it processes the harness's
    // reply, so this also keeps the responder's pty write ahead of input.
    run.wait_bytes(0, KITTY_PUSH, "kitty keyboard flags pushed");
    run.write(b"say hi\r");
    // The elapsed count proves `turn_started` folded, which opens the
    // turn: Esc goes out as `cancel` only while the turn is busy.
    run.wait_screen("the running turn", |screen| {
        working_elapsed(&screen.contents)
    });
    // The stalled response has reached the fake server before it is
    // cancelled.
    assert!(
        server.await_partial(1, setup.deadline),
        "the stalled response reached the server"
    );
    // The partial send above proves the server wrote the delta, not that
    // the terminal drew it: Esc races that path, so wait for the reply
    // row itself before interrupting. The match is a whole trimmed row,
    // so the working status line never satisfies it.
    run.wait_screen("the partial reply Working", |screen| {
        shows_row(screen, "Working")
    });
    run.write(b"\x1b[27u");
    let interrupted = run.wait_screen("the interrupted turn", |screen| {
        screen.alternate_screen
            && shows(screen, "▣ interrupted")
            && !shows(screen, "Esc to interrupt")
    });
    insta::assert_snapshot!(
        "interrupt_retry_interrupted",
        grid_text(
            &interrupted.rows,
            interrupted.cursor,
            interrupted.hide_cursor,
            &ids
        )
    );
    // The cancelled response has closed, and the close folded, so the
    // terminal is idle and Enter sends a prompt.
    assert!(
        server.await_closed(1, setup.deadline),
        "the cancelled response closed"
    );
    let from = run.output().len();
    run.write(b"again\r");
    run.wait_screen("the recovered answer", |screen| shows(screen, "Recovered."));
    run.turn_finished(from);
    let answered = run.wait_screen("the closed turn", |screen| {
        shows(screen, "Recovered.")
            && shows(screen, "▣ completed")
            && !shows(screen, "Esc to interrupt")
    });
    insta::assert_snapshot!(
        "interrupt_retry_answered",
        grid_text(&answered.rows, answered.cursor, answered.hide_cursor, &ids)
    );
    // The stalled call, the 500 and its retry make three requests, and
    // the log holds the one failed call: the retry, not the turn, took the
    // 500.
    assert_eq!(server.requests().len(), 3);
    let id = only_session(&setup);
    let log = fs::read_to_string(support::session_dir(&setup, &id).join("events.jsonl")).unwrap();
    let lines = support::complete_lines(&log);
    let failed: Vec<&Value> = lines
        .iter()
        .filter(|line| {
            line["kind"] == "assistant_message_completed" && line["payload"]["outcome"] == "failed"
        })
        .collect();
    assert_eq!(
        failed.len(),
        1,
        "one failed model call in the log: {lines:#?}"
    );
    assert_eq!(
        failed[0]["payload"]["error"]["code"],
        "provider_unavailable"
    );
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |screen| {
        !screen.alternate_screen && !screen.hide_cursor && shows(screen, "fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}
