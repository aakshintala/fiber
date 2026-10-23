//! Tests for `/rules`: the rows by scope, opening the file, and revoking
//! (`docs/tui.md`, "Swapped views"; `docs/configuration.md`, "Standing
//! rules").

use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{Ctx, Rules, fields};
use crate::configure::{Revoked, RuleRow, RulesScope, RulesSection};
use crate::configure_fake::Fake;
use crate::keys::{Edit, Key};
use crate::mouse::{TargetId, hit};
use crate::settings_view::Act;
use crate::swapped::{Ink, Spot, render};
use crate::theme::Role;

/// One rule row: line `line` reading `text`.
fn row(
    line: usize,
    decision: contract::rules::RuleDecision,
    tool: &str,
    prefix: &str,
    added: Option<u64>,
    session: Option<&str>,
) -> RuleRow {
    let rule = contract::rules::Rule {
        decision,
        tool: tool.to_owned(),
        prefix: prefix.to_owned(),
        added,
        session_id: session.map(|session| contract::SessionId(session.to_owned())),
    };
    RuleRow {
        line,
        text: serde_json::to_string(&rule).unwrap_or_default(),
        rule,
    }
}

/// A full rule: allowed `shell`, with prefix, date and session.
fn full() -> RuleRow {
    row(
        1,
        contract::rules::RuleDecision::Allow,
        "shell",
        "npm test",
        Some(1791331200000),
        Some("s_01"),
    )
}

/// A hand-written rule: denied `shell`, with no date or session.
fn hand() -> RuleRow {
    row(
        2,
        contract::rules::RuleDecision::Deny,
        "shell",
        "",
        None,
        None,
    )
}

/// A seam over `global` and `project` sections.
fn fake(global: Vec<RuleRow>, project: Result<Vec<RuleRow>, &str>) -> Fake {
    let fake = Fake::new(Vec::new());
    if let Ok(mut rules) = fake.rules.lock() {
        *rules = Ok((
            RulesSection {
                file: PathBuf::from("/home/rules"),
                rows: Ok(global),
            },
            RulesSection {
                file: PathBuf::from("/home/projects/-w/rules"),
                rows: project
                    .map_err(|message| crate::ConfigureError {
                        code: contract::ErrorCode::ConfigInvalid,
                        message: message.to_owned(),
                    })
                    .map_err(|error| error.message),
            },
        ));
    }
    fake
}

/// A call's context over `fake`, 24 rows tall, in `/w`.
fn ctx(fake: &Fake) -> Ctx<'_> {
    Ctx {
        seam: fake,
        workspace: Path::new("/w"),
        height: 24,
        width: 80,
        usage: None,
    }
}

/// The view over `fake`.
fn open(fake: &Fake) -> Rules {
    Rules::open(&ctx(fake))
}

/// The view with row `at` selected.
fn at(fake: &Fake, at: usize) -> Rules {
    let mut rules = open(fake);
    rules.list.select(at, rules.items.len(), 20);
    rules
}

/// `rules` drawn at `width` x `height`, one string per row.
fn drawn(rules: &Rules, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let frame = rules.frame(usize::from(width), None);
    render(&frame, area, &mut buf, &mut Vec::new());
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The row texts of the frame.
fn lines(rules: &Rules) -> Vec<String> {
    rules
        .frame(80, None)
        .rows
        .iter()
        .map(|cells| cells.iter().map(|(text, _, _)| text.clone()).collect())
        .collect()
}

/// The day, in milliseconds.
const DAY: u64 = 86_400_000;

/// `full()`'s `added`.
const ADDED: u64 = 1_791_331_200_000;

/// The non-blank row texts of the frame: the rows the doc names.
fn texts(rules: &Rules) -> Vec<String> {
    lines(rules)
        .into_iter()
        .filter(|line| !line.is_empty())
        .collect()
}

#[test]
fn global_then_project_each_under_its_heading() {
    let fake = fake(vec![full(), hand()], Ok(vec![full()]));
    let rules = open(&fake);
    assert_eq!(
        texts(&rules),
        [
            "Global",
            "allow shell:npm test · added 2026-10-07 00:00 UTC in s_01 · ✕",
            "deny shell:(any) · ✕",
            "This project",
            "allow shell:npm test · added 2026-10-07 00:00 UTC in s_01 · ✕",
        ]
    );
    assert_eq!(fake.reads(), [PathBuf::from("/w")]);
}

#[test]
fn the_headings_are_bold_and_the_x_is_in_error() {
    let fake = fake(vec![full()], Ok(vec![full()]));
    let rules = open(&fake);
    let frame = rules.frame(80, None);
    // Each row is found by what it says, not by its place among the
    // rows.
    let heading = |text: &str| {
        frame
            .rows
            .iter()
            .find(|cells| cells.first().is_some_and(|cell| cell.0 == text))
            .unwrap_or_else(|| panic!("no {text} row in {frame:?}"))
    };
    assert_eq!(
        heading("Global"),
        &[("Global".to_owned(), None, Ink::Heading)]
    );
    assert_eq!(
        heading("This project"),
        &[("This project".to_owned(), None, Ink::Heading)]
    );
    let (at, rule) = frame
        .rows
        .iter()
        .enumerate()
        .find(|(_, cells)| cells.iter().any(|cell| cell.1.is_some()))
        .expect("a rule row");
    assert_eq!(rule[0].2, Ink::Plain);
    assert_eq!(
        rule[1],
        (
            "✕".to_owned(),
            Some(Spot::Revoke(at)),
            Ink::Role(Role::Error)
        )
    );
    assert_eq!(frame.footer, "Ctrl+G opens the rules file");
    assert!(frame.list.is_gutter());
}

#[test]
fn a_rule_row_shows_decision_tool_prefix_age_and_session() {
    for (wall, said) in [
        (
            Some(ADDED + 2 * DAY),
            "allow shell:npm test · added 2d ago in s_01",
        ),
        (
            Some(ADDED + 5 * 60_000),
            "allow shell:npm test · added 5m ago in s_01",
        ),
        (
            Some(ADDED + 3 * 3_600_000),
            "allow shell:npm test · added 3h ago in s_01",
        ),
        (
            Some(ADDED + 59_999),
            "allow shell:npm test · added 59s ago in s_01",
        ),
        // A rule dated after the wall time is not negative.
        (
            Some(ADDED - 1),
            "allow shell:npm test · added 0s ago in s_01",
        ),
        (
            None,
            "allow shell:npm test · added 2026-10-07 00:00 UTC in s_01",
        ),
    ] {
        assert_eq!(fields(&full(), wall), said, "{wall:?}");
    }
}

#[test]
fn an_absent_added_or_session_is_left_out() {
    let added_only = row(
        1,
        contract::rules::RuleDecision::Ask,
        "shell",
        "x",
        Some(ADDED),
        None,
    );
    assert_eq!(
        fields(&added_only, Some(ADDED + 2 * DAY)),
        "ask shell:x · added 2d ago"
    );
    assert_eq!(
        fields(&added_only, None),
        "ask shell:x · added 2026-10-07 00:00 UTC"
    );
    let session_only = row(
        1,
        contract::rules::RuleDecision::Ask,
        "shell",
        "x",
        None,
        Some("s_01"),
    );
    assert_eq!(
        fields(&session_only, Some(1)),
        "ask shell:x · added in s_01"
    );
    // Past the latest instant jiff holds, the time is left out; the
    // session stays, and with none the whole clause goes.
    let past = row(
        1,
        contract::rules::RuleDecision::Ask,
        "shell",
        "x",
        Some(u64::MAX),
        Some("s_01"),
    );
    assert_eq!(fields(&past, None), "ask shell:x · added in s_01");
    let past_alone = row(
        1,
        contract::rules::RuleDecision::Ask,
        "shell",
        "x",
        Some(u64::MAX),
        None,
    );
    assert_eq!(fields(&past_alone, None), "ask shell:x");
    // With a wall time, any `added` has an age.
    assert_eq!(fields(&past, Some(0)), "ask shell:x · added 0s ago in s_01");
    let epoch = row(
        1,
        contract::rules::RuleDecision::Ask,
        "shell",
        "x",
        Some(0),
        None,
    );
    assert_eq!(fields(&epoch, Some(DAY)), "ask shell:x · added 1d ago");
}

#[test]
fn an_empty_prefix_shows_any() {
    assert_eq!(fields(&hand(), None), "deny shell:(any)");
}

#[test]
fn a_missing_file_says_no_rules() {
    let fake = fake(Vec::new(), Ok(Vec::new()));
    let rules = open(&fake);
    assert_eq!(
        texts(&rules),
        ["Global", "No rules.", "This project", "No rules."]
    );
}

#[test]
fn a_bad_file_shows_its_error_and_the_other_files_rows() {
    let fake = fake(Vec::new(), Err("/home/projects/-w/rules:2: bad"));
    let rules = open(&fake);
    assert_eq!(
        texts(&rules),
        [
            "Global",
            "No rules.",
            "This project",
            "/home/projects/-w/rules:2: bad  Ctrl+G opens it to fix it.",
        ]
    );
}

#[test]
fn a_failed_read_says_why_and_ctrl_g_opens_nothing() {
    let fake = Fake::new(Vec::new());
    if let Ok(mut rules) = fake.rules.lock() {
        *rules = Err(crate::ConfigureError {
            code: contract::ErrorCode::ConfigInvalid,
            message: "/home/rules: gone".to_owned(),
        });
    }
    let mut rules = open(&fake);
    assert!(rules.items.is_empty());
    assert_eq!(rules.frame(80, None).below, ["/home/rules: gone"]);
    assert!(matches!(rules.key(&Key::CtrlG, &ctx(&fake)), Act::Stay));
}

#[test]
fn ctrl_g_opens_the_selected_sections_file() {
    // The global rule and the project's note: each opens its own
    // section's file.
    let fake = fake(vec![full()], Ok(Vec::new()));
    for (row, file) in [(1, "/home/rules"), (4, "/home/projects/-w/rules")] {
        let mut rules = at(&fake, row);
        let Act::Open(opened) = rules.key(&Key::CtrlG, &ctx(&fake)) else {
            panic!("row {row} opened nothing");
        };
        assert_eq!(opened, PathBuf::from(file), "row {row}");
    }
}

/// The text of the selected row.
fn selected_text(rules: &Rules) -> String {
    lines(rules)[rules.list.selected()].clone()
}

#[test]
fn up_and_down_move_only_between_rules_and_notes() {
    // Two global rules and the project's note: headings and blank rows
    // between them are skipped.
    let fake = fake(vec![full(), hand()], Ok(Vec::new()));
    let mut rules = open(&fake);
    let first = "allow shell:npm test · added 2026-10-07 00:00 UTC in s_01 · ✕";
    let second = "deny shell:(any) · ✕";
    let note = "No rules.";
    assert_eq!(selected_text(&rules), first);
    for (key, to) in [
        (Key::Down, second),
        (Key::Down, note),
        (Key::Down, note),
        (Key::Up, second),
        (Key::Up, first),
        (Key::Up, first),
    ] {
        rules.key(&key, &ctx(&fake));
        assert_eq!(selected_text(&rules), to, "{key:?}");
    }
}

#[test]
fn a_click_on_a_heading_or_blank_row_selects_nothing() {
    let fake = fake(vec![full(), hand()], Ok(Vec::new()));
    let mut rules = open(&fake);
    let on = rules.list.selected();
    // Every row that is not a rule or a note: the headings and the blank
    // row.
    let others: Vec<usize> = lines(&rules)
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.ends_with('✕') && line.as_str() != "No rules.")
        .map(|(at, _)| at)
        .collect();
    assert!(others.len() >= 2);
    for row in others {
        rules.click(Spot::Row(row), &ctx(&fake));
        assert_eq!(rules.list.selected(), on, "row {row}");
    }
    let note = lines(&rules)
        .iter()
        .position(|line| line == "No rules.")
        .expect("the project's note");
    rules.click(Spot::Row(note), &ctx(&fake));
    assert_eq!(rules.list.selected(), note);
}

#[test]
fn with_the_rules_unread_no_row_is_marked() {
    let fake = Fake::new(Vec::new());
    if let Ok(mut rules) = fake.rules.lock() {
        *rules = Err(crate::ConfigureError {
            code: contract::ErrorCode::ConfigInvalid,
            message: "gone".to_owned(),
        });
    }
    let rules = open(&fake);
    assert_ne!(rules.frame(80, None).list.selected(), 0);
    let fake = self::fake(vec![full()], Ok(Vec::new()));
    assert_eq!(open(&fake).frame(80, None).list.selected(), 1);
}

#[test]
fn delete_backspace_and_the_x_revoke_the_selected_rule() {
    let text = full().text.clone();
    for act in [
        {
            let fake = fake(vec![full()], Ok(Vec::new()));
            let mut rules = at(&fake, 1);
            let act = rules.key(&Key::Backspace, &ctx(&fake));
            assert_eq!(
                fake.revokes(),
                [(PathBuf::from("/w"), RulesScope::Global, 1, text.clone())]
            );
            act
        },
        {
            let fake = fake(vec![full()], Ok(Vec::new()));
            let mut rules = at(&fake, 1);
            let act = rules.edit_key(&Edit::Delete, &ctx(&fake));
            assert_eq!(
                fake.revokes(),
                [(PathBuf::from("/w"), RulesScope::Global, 1, text.clone())]
            );
            act
        },
        {
            let fake = fake(vec![full()], Ok(Vec::new()));
            let mut rules = at(&fake, 1);
            let act = rules.click(Spot::Revoke(1), &ctx(&fake));
            assert_eq!(
                fake.revokes(),
                [(PathBuf::from("/w"), RulesScope::Global, 1, text.clone())]
            );
            act
        },
    ] {
        assert!(matches!(act, Act::Stay));
    }
}

#[test]
fn the_x_revokes_its_own_row_not_the_selection() {
    let fake = fake(vec![full(), hand()], Ok(Vec::new()));
    let mut rules = at(&fake, 1);
    rules.click(Spot::Revoke(2), &ctx(&fake));
    assert_eq!(
        fake.revokes(),
        [(
            PathBuf::from("/w"),
            RulesScope::Global,
            2,
            hand().text.clone()
        )]
    );
    assert_eq!(rules.list.selected(), 2);
}

#[test]
fn revoke_on_a_heading_or_note_does_nothing() {
    let fake = fake(vec![full()], Ok(Vec::new()));
    // The project's note is selectable; the heading and blank row are not.
    for row in [0, 2, 3, 4] {
        let mut rules = at(&fake, row);
        assert!(matches!(rules.key(&Key::Backspace, &ctx(&fake)), Act::Stay));
        assert!(matches!(
            rules.edit_key(&Edit::Delete, &ctx(&fake)),
            Act::Stay
        ));
        assert!(matches!(
            rules.click(Spot::Revoke(row), &ctx(&fake)),
            Act::Stay
        ));
    }
    assert!(fake.revokes().is_empty());
}

#[test]
fn other_edits_do_nothing() {
    let fake = fake(vec![full()], Ok(Vec::new()));
    let mut rules = at(&fake, 1);
    assert!(matches!(
        rules.edit_key(&Edit::Paste("x".to_owned()), &ctx(&fake)),
        Act::Stay
    ));
    assert!(matches!(
        rules.edit_key(&Edit::Left, &ctx(&fake)),
        Act::Stay
    ));
    assert!(fake.revokes().is_empty());
}

#[test]
fn a_revoke_rereads_and_says_it_applies_to_the_next_call() {
    let fake = fake(vec![full()], Ok(Vec::new()));
    let mut rules = at(&fake, 1);
    if let Ok(mut sections) = fake.rules.lock() {
        *sections = Ok((
            RulesSection {
                file: PathBuf::from("/home/rules"),
                rows: Ok(Vec::new()),
            },
            RulesSection {
                file: PathBuf::from("/home/projects/-w/rules"),
                rows: Ok(Vec::new()),
            },
        ));
    }
    rules.key(&Key::Backspace, &ctx(&fake));
    assert_eq!(
        rules.frame(80, None).below,
        ["Revoked; applies to the next call judged."]
    );
    assert_eq!(
        texts(&rules),
        ["Global", "No rules.", "This project", "No rules."]
    );
}

#[test]
fn stale_rereads_and_says_nothing_was_revoked() {
    let fake = fake(vec![full()], Ok(Vec::new()));
    if let Ok(mut revoked) = fake.revoked.lock() {
        *revoked = Ok(Revoked::Stale);
    }
    let mut rules = at(&fake, 1);
    rules.key(&Key::Backspace, &ctx(&fake));
    assert_eq!(
        rules.frame(80, None).below,
        ["The rules file changed; nothing was revoked."]
    );
    assert_eq!(fake.reads().len(), 2);
}

#[test]
fn a_revoke_error_is_shown_and_the_rows_stay() {
    let fake = fake(vec![full()], Ok(Vec::new()));
    if let Ok(mut revoked) = fake.revoked.lock() {
        *revoked = Err(crate::ConfigureError {
            code: contract::ErrorCode::Usage,
            message: "locked".to_owned(),
        });
    }
    let mut rules = at(&fake, 1);
    let before = lines(&rules);
    let reads = fake.reads().len();
    rules.key(&Key::Backspace, &ctx(&fake));
    assert_eq!(rules.frame(80, None).below, ["locked"]);
    assert_eq!(lines(&rules), before);
    assert_eq!(fake.reads().len(), reads);
}

#[test]
fn revoking_the_last_row_selects_the_new_last() {
    let fake = fake(Vec::new(), Ok(vec![full()]));
    // Global heading and note, a blank row, the project heading and rule.
    let mut rules = at(&fake, 4);
    if let Ok(mut sections) = fake.rules.lock() {
        *sections = Ok((
            RulesSection {
                file: PathBuf::from("/home/rules"),
                rows: Ok(Vec::new()),
            },
            RulesSection {
                file: PathBuf::from("/home/projects/-w/rules"),
                rows: Ok(Vec::new()),
            },
        ));
    }
    rules.key(&Key::Backspace, &ctx(&fake));
    assert_eq!(rules.list.selected(), 4);
    assert_eq!(lines(&rules)[4], "No rules.");
}

#[test]
fn a_reread_moves_a_selection_off_a_heading_to_the_next_row_that_takes_a_key() {
    let fake = fake(vec![full()], Ok(Vec::new()));
    let mut rules = open(&fake);
    rules.list.select(3, rules.items.len(), 20);
    rules.reread(&ctx(&fake));
    assert_eq!(rules.list.selected(), 4);
    // Past the last such row, it falls back to the last.
    rules.list.select(0, rules.items.len(), 20);
    rules.items.truncate(3);
    rules.list.select(3, 5, 20);
    rules.reread(&ctx(&fake));
    assert_eq!(rules.list.selected(), 4);
}

#[test]
fn esc_closes() {
    let fake = fake(vec![full()], Ok(Vec::new()));
    let mut rules = at(&fake, 1);
    assert!(matches!(rules.key(&Key::Esc, &ctx(&fake)), Act::Close));
}

/// The view over one rule with a prefix of `len` columns and no date or
/// session, so its text is `allow shell:` and the prefix.
fn with_prefix(len: usize) -> Rules {
    let long = row(
        1,
        contract::rules::RuleDecision::Allow,
        "shell",
        &"x".repeat(len),
        None,
        None,
    );
    open(&fake(vec![long], Ok(Vec::new())))
}

/// The ✕ target's x when `rules` draws at `width`.
fn x_column(rules: &Rules, width: u16) -> u16 {
    let area = Rect::new(0, 0, width, 10);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    render(
        &rules.frame(usize::from(width), None),
        area,
        &mut buf,
        &mut targets,
    );
    let revoke = targets
        .iter()
        .find(|target| target.id == TargetId::View(Spot::Revoke(1)))
        .unwrap_or_else(|| panic!("no revoke target in {targets:?}"));
    assert_eq!(revoke.rect.width, 1);
    assert_eq!(
        hit(&targets, revoke.rect.x, revoke.rect.y),
        Some(TargetId::View(Spot::Revoke(1)))
    );
    revoke.rect.x
}

#[test]
fn the_x_stays_clickable_with_a_long_prefix() {
    // At width 80 the text has 80 - 6 = 74 columns: `allow shell:`
    // takes 12, so a prefix of 62 fills them.
    for (len, x, cut) in [(61, 78, false), (62, 79, false), (63, 79, true)] {
        let rules = with_prefix(len);
        assert_eq!(x_column(&rules, 80), x, "{len}");
        let text = &rules.frame(80, None).rows[1][0].0;
        assert_eq!(text.contains('…'), cut, "{len}");
    }
    // The cut ends in `…` and keeps the room.
    let cells = &with_prefix(63).frame(80, None).rows[1];
    assert_eq!(cells[0].0, format!("allow shell:{}… · ", "x".repeat(61)));
}

#[test]
fn the_x_is_in_reach_at_the_floor_and_clips_below_it() {
    // At 22 columns the text keeps 16, the ` · ` and the ✕ follow.
    let rules = with_prefix(40);
    assert_eq!(x_column(&rules, 22), 21);
    assert_eq!(x_column(&rules, 23), 22);
    // Narrower than the floor nothing panics; the row clips.
    for width in [0, 1, 6, 10, 21] {
        let area = Rect::new(0, 0, width, 4);
        render(
            &rules.frame(usize::from(width), None),
            area,
            &mut Buffer::empty(area),
            &mut Vec::new(),
        );
    }
}

#[test]
fn a_wide_prefix_is_cut_by_columns() {
    let long = row(
        1,
        contract::rules::RuleDecision::Allow,
        "shell",
        &"\u{3042}".repeat(40),
        None,
        None,
    );
    let rules = open(&fake(vec![long], Ok(Vec::new())));
    let x = x_column(&rules, 80);
    assert!(x < 80, "{x}");
}

#[test]
fn moving_down_past_the_first_row_keeps_three_rows_in_view() {
    // Six rows in a three-row window: moving to the second rule keeps
    // the window at the top.
    let fake = fake(vec![full(), hand()], Ok(vec![full()]));
    let ctx = Ctx {
        seam: &fake,
        workspace: Path::new("/w"),
        height: 5,
        width: 80,
        usage: None,
    };
    let mut rules = Rules::open(&ctx);
    rules.key(&Key::Down, &ctx);
    assert_eq!(rules.list.selected(), 2);
    assert_eq!(rules.list.top(), 0);
    let rows: Vec<String> = drawn(&rules, 80, 5).lines().map(str::to_owned).collect();
    assert_eq!(rows[1], "  Global");
    assert_eq!(
        rows[2],
        "  allow shell:npm test · added 2026-10-07 00:00 UTC in s_01 · \u{2715}"
    );
    assert_eq!(rows[3], "› deny shell:(any) · \u{2715}");
}

#[test]
fn rules_80x24() {
    let fake = fake(vec![full(), hand()], Err("/home/projects/-w/rules:2: bad"));
    let rules = open(&fake);
    insta::assert_snapshot!("rules_80x24", drawn(&rules, 80, 24));
}
