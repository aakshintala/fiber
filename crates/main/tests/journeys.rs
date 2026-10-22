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

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use support::pty::{Grid, Run};
use support::{Setup, write_json};

/// The spinner glyphs of the working line: braille frames, or `●` under
/// reduced motion.
const SPINNERS: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏●";

/// The byte length of the `format::duration` shape at the start of
/// `text`: `<n>s`, `<n>m <nn>s` or `<n>h <nn>m`.
fn duration_len(text: &str) -> Option<usize> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = &text[digits..];
    let two_then = |tail: &str, unit: u8| {
        let bytes = tail.strip_prefix(' ')?.as_bytes();
        (bytes.len() >= 3
            && bytes[0].is_ascii_digit()
            && bytes[1].is_ascii_digit()
            && bytes[2] == unit)
            .then_some(1 + 3)
    };
    match rest.as_bytes().first()? {
        b's' => Some(digits + 1),
        b'm' => Some(digits + 1 + two_then(&rest[1..], b's')?),
        b'h' => Some(digits + 1 + two_then(&rest[1..], b'm')?),
        _ => None,
    }
}

/// Each `i_<id>.png` image line name reads `i_[id].png`.
fn image_ids(row: &str) -> String {
    let mut out = String::new();
    let mut rest = row;
    while let Some(at) = rest.find("i_") {
        let after = &rest[at + 2..];
        let id_len = after.find([' ', '.']).unwrap_or(after.len());
        if id_len > 0 && after[id_len..].starts_with(".png") {
            out.push_str(&rest[..at]);
            out.push_str("i_[id].png");
            rest = &after[id_len + ".png".len()..];
        } else {
            out.push_str(&rest[..at + 2]);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Each `dd:dd` with no digit or `:` on either side reads `HH:MM`.
fn clock_times(row: &str) -> String {
    let cells: Vec<char> = row.chars().collect();
    let live = |cell: Option<&char>| cell.is_some_and(|c| c.is_ascii_digit() || *c == ':');
    let mut out = String::new();
    let mut at = 0;
    while at < cells.len() {
        let shape = cells.get(at..at + 5).is_some_and(|w| {
            w[0].is_ascii_digit()
                && w[1].is_ascii_digit()
                && w[2] == ':'
                && w[3].is_ascii_digit()
                && w[4].is_ascii_digit()
        });
        let before = at.checked_sub(1).and_then(|p| cells.get(p));
        if shape && !live(before) && !live(cells.get(at + 5)) {
            out.push_str("HH:MM");
            at += 5;
        } else {
            out.push(cells[at]);
            at += 1;
        }
    }
    out
}

/// A closing row: `▣ ` then `completed`, `interrupted` or `failed`. Its
/// duration, absent under a second and present after, reads
/// ` · [duration]` after the outcome word either way.
fn closing_row(row: &str) -> String {
    let Some(at) = row.find("▣ ") else {
        return row.to_owned();
    };
    let from = at + "▣ ".len();
    let Some(word) = ["completed", "interrupted", "failed"]
        .into_iter()
        .find(|word| row[from..].starts_with(word))
    else {
        return row.to_owned();
    };
    let (head, after) = row.split_at(from + word.len());
    if !after.is_empty() && !after.starts_with(" · ") {
        return row.to_owned();
    }
    let kept = after
        .strip_prefix(" · ")
        .and_then(|tail| {
            let n = duration_len(tail)?;
            (tail[n..].is_empty() || tail[n..].starts_with(" · ")).then_some(&tail[n..])
        })
        .unwrap_or(after);
    format!("{head} · [duration]{kept}")
}

/// The working line: `Working ` and `Esc to interrupt` on a row whose
/// cell two before `Working` is a spinner glyph. The glyph reads `*` and
/// the elapsed time `[elapsed]`.
fn working_line(row: &str) -> String {
    let Some(at) = row.find("Working ") else {
        return row.to_owned();
    };
    let before: Vec<char> = row[..at].chars().collect();
    let spinner = before
        .len()
        .checked_sub(2)
        .filter(|cell| row.contains("Esc to interrupt") && SPINNERS.contains(before[*cell]));
    let Some(cell) = spinner else {
        return row.to_owned();
    };
    let tail = &row[at + "Working ".len()..];
    let Some(n) = duration_len(tail) else {
        return row.to_owned();
    };
    let mut out: String = before[..cell].iter().collect();
    out.push('*');
    out.extend(&before[cell + 1..]);
    format!("{out}Working [elapsed]{}", &tail[n..])
}

/// A tool-group summary: a row ending in ` ▸` or ` ▾` whose last word
/// before it is a duration.
fn tool_group(row: &str) -> String {
    let Some(body) = row.strip_suffix(" ▸").or_else(|| row.strip_suffix(" ▾")) else {
        return row.to_owned();
    };
    let suffix = &row[body.len()..];
    for (space, _) in body.match_indices(' ') {
        let tail = &body[space + 1..];
        if duration_len(tail) == Some(tail.len()) {
            return format!("{} [duration]{suffix}", &body[..space]);
        }
    }
    row.to_owned()
}

/// The picker's `fake` provider heading, with no suffix, `⟳ refreshing`
/// and its spinner, or `· updated <age> ago`, reads `fake [freshness]`.
fn provider_heading(row: &str) -> String {
    let indent = row.len() - row.trim_start().len();
    let Some(rest) = row[indent..].strip_prefix("fake") else {
        return row.to_owned();
    };
    let dynamic = rest.is_empty()
        || rest
            .strip_prefix(" ⟳ refreshing ")
            .is_some_and(|spinner| !spinner.is_empty() && !spinner.contains(' '))
        || rest
            .strip_prefix(" · updated ")
            .is_some_and(|age| age.len() > " ago".len() && age.ends_with(" ago"));
    if dynamic {
        format!("{}fake [freshness]", &row[..indent])
    } else {
        row.to_owned()
    }
}

/// What changes every run, replaced before a snapshot. The settled grid's
/// text: each row right-trimmed, rows joined by `\n`, then
/// `\ncursor=(<row>,<col>)`, after the replacements, in order: `ids`,
/// image names, clock times, the closing row, the working line, a
/// tool-group summary, the provider heading.
fn grid_text(rows: &[String], cursor: (u16, u16), ids: &[String]) -> String {
    let mut text = Vec::with_capacity(rows.len() + 1);
    for row in rows {
        let mut row = row.trim_end().to_owned();
        for id in ids.iter().filter(|id| !id.is_empty()) {
            row = row.replace(id.as_str(), "[session]");
        }
        let row = image_ids(&row);
        let row = clock_times(&row);
        let row = closing_row(&row);
        let row = working_line(&row);
        let row = tool_group(&row);
        text.push(provider_heading(&row).trim_end().to_owned());
    }
    text.push(format!("cursor=({},{})", cursor.0, cursor.1));
    text.join("\n")
}

/// `grid_text` over one row, without its cursor line.
#[cfg(test)]
fn one(row: &str, ids: &[&str]) -> String {
    let ids: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
    let text = grid_text(&[row.to_owned()], (0, 0), &ids);
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
        "  ● Ran 2 commands [duration] ▾"
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
    assert_eq!(grid_text(&rows, (29, 4), &[]), "a\n\n  b\ncursor=(29,4)");
}

/// The one `tui` config object every journey writes: no rail and no panel
/// cards, so no live figure or temp path reaches a snapshot.
fn tui_off() -> Value {
    json!({"rail": {"shown": false}, "panel": {"cards": []}})
}

/// A reply saying `text` in two deltas.
fn reply(text: &str) -> Response {
    let at = text.len() / 2;
    let (first, rest) = text.split_at(at);
    support::stream(&[
        json!({"type": "response.output_text.delta", "delta": first}),
        json!({"type": "response.output_text.delta", "delta": rest}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": text}]
        }}),
    ])
}

/// Whether any grid row holds `needle`.
fn shows(screen: &Grid, needle: &str) -> bool {
    screen.rows.iter().any(|row| row.contains(needle))
}

/// Installs a provider `fake` with models `alpha-1` (named
/// "Claude Opus 5.5") and `beta-2` (no name) on `openai-responses` at the
/// fake server, reading its key from `FIBER_TEST_FAKE_KEY`, and makes
/// `fake/alpha-1` the configured model. Neither the provider nor either id
/// contains "opus": only the display name can carry the picker's
/// "opus 5.5" query.
fn install_picker_provider(setup: &Setup, server: &ProviderServer) {
    let source = setup.root.path().join("src");
    write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
    );
    write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [
                {"id": "alpha-1", "protocol": "openai-responses",
                 "base_url": format!("{}/v1", server.url()),
                 "context_window": 100000, "name": "Claude Opus 5.5"},
                {"id": "beta-2", "protocol": "openai-responses",
                 "base_url": format!("{}/v1", server.url()),
                 "context_window": 100000},
            ],
        }),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/alpha-1", "hub": {"idle_exit_ms": 200}, "tui": tui_off()}),
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
        grid_text(&opened.rows, opened.cursor, &ids)
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
        grid_text(&filtered.rows, filtered.cursor, &ids)
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
        grid_text(&answered.rows, answered.cursor, &ids)
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
