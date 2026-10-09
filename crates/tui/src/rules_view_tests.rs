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
use crate::swapped::{Spot, render};

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
    let frame = rules.frame();
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
        .frame()
        .rows
        .iter()
        .map(|cells| cells.iter().map(|(text, _, _)| text.clone()).collect())
        .collect()
}

#[test]
fn global_then_project_each_in_line_order_under_its_path() {
    let fake = fake(vec![full(), hand()], Ok(vec![full()]));
    let rules = open(&fake);
    assert_eq!(
        lines(&rules),
        [
            "Global rules  /home/rules",
            "✕ allow  shell  npm test  2026-10-07 00:00 UTC  s_01",
            "✕ deny  shell  (any)",
            "Project rules  /home/projects/-w/rules",
            "✕ allow  shell  npm test  2026-10-07 00:00 UTC  s_01",
        ]
    );
    assert_eq!(fake.reads(), [PathBuf::from("/w")]);
}

#[test]
fn a_rule_row_shows_decision_tool_prefix_date_and_session() {
    assert_eq!(
        fields(&full()),
        "allow  shell  npm test  2026-10-07 00:00 UTC  s_01"
    );
}

#[test]
fn an_absent_added_or_session_is_left_out() {
    let added_only = row(
        1,
        contract::rules::RuleDecision::Ask,
        "shell",
        "x",
        Some(1791331200000),
        None,
    );
    assert_eq!(fields(&added_only), "ask  shell  x  2026-10-07 00:00 UTC");
    let session_only = row(
        1,
        contract::rules::RuleDecision::Ask,
        "shell",
        "x",
        None,
        Some("s_01"),
    );
    assert_eq!(fields(&session_only), "ask  shell  x  s_01");
    // Past the latest instant jiff holds, the row draws with no date.
    let past = row(
        1,
        contract::rules::RuleDecision::Ask,
        "shell",
        "x",
        Some(u64::MAX),
        Some("s_01"),
    );
    assert_eq!(fields(&past), "ask  shell  x  s_01");
}

#[test]
fn an_empty_prefix_shows_any() {
    assert_eq!(fields(&hand()), "deny  shell  (any)");
}

#[test]
fn a_missing_file_says_no_rules() {
    let fake = fake(Vec::new(), Ok(Vec::new()));
    let rules = open(&fake);
    assert_eq!(
        lines(&rules),
        [
            "Global rules  /home/rules",
            "No rules.",
            "Project rules  /home/projects/-w/rules",
            "No rules.",
        ]
    );
}

#[test]
fn a_bad_file_shows_its_error_and_the_other_files_rows() {
    let fake = fake(Vec::new(), Err("/home/projects/-w/rules:2: bad"));
    let rules = open(&fake);
    assert_eq!(
        lines(&rules),
        [
            "Global rules  /home/rules",
            "No rules.",
            "Project rules  /home/projects/-w/rules",
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
    assert_eq!(rules.frame().below, ["/home/rules: gone"]);
    assert!(matches!(rules.key(&Key::CtrlG, &ctx(&fake)), Act::Stay));
}

#[test]
fn ctrl_g_opens_the_selected_sections_file() {
    // The global heading, a global rule, the project heading and the
    // project's note: each opens its own section's file.
    let fake = fake(vec![full()], Ok(Vec::new()));
    for (row, file) in [
        (0, "/home/rules"),
        (1, "/home/rules"),
        (2, "/home/projects/-w/rules"),
        (3, "/home/projects/-w/rules"),
    ] {
        let mut rules = at(&fake, row);
        let Act::Open(opened) = rules.key(&Key::CtrlG, &ctx(&fake)) else {
            panic!("row {row} opened nothing");
        };
        assert_eq!(opened, PathBuf::from(file), "row {row}");
    }
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
    for row in [0, 3] {
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
        rules.frame().below,
        ["Revoked; applies to the next call judged."]
    );
    assert_eq!(
        lines(&rules),
        [
            "Global rules  /home/rules",
            "No rules.",
            "Project rules  /home/projects/-w/rules",
            "No rules.",
        ]
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
        rules.frame().below,
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
    assert_eq!(rules.frame().below, ["locked"]);
    assert_eq!(lines(&rules), before);
    assert_eq!(fake.reads().len(), reads);
}

#[test]
fn revoking_the_last_row_selects_the_new_last() {
    let fake = fake(Vec::new(), Ok(vec![full()]));
    let mut rules = at(&fake, 3);
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
    assert_eq!(rules.list.selected(), 3);
    assert_eq!(lines(&rules)[3], "No rules.");
}

#[test]
fn esc_closes() {
    let fake = fake(vec![full()], Ok(Vec::new()));
    let mut rules = at(&fake, 1);
    assert!(matches!(rules.key(&Key::Esc, &ctx(&fake)), Act::Close));
}

#[test]
fn the_x_stays_clickable_with_a_long_prefix() {
    let long = row(
        1,
        contract::rules::RuleDecision::Allow,
        "shell",
        &"x".repeat(60),
        Some(1791331200000),
        Some("s_01"),
    );
    let fake = fake(vec![long], Ok(Vec::new()));
    let rules = open(&fake);
    let area = Rect::new(0, 0, 30, 10);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    render(&rules.frame(), area, &mut buf, &mut targets);
    let revoke = targets
        .iter()
        .find(|target| target.id == TargetId::View(Spot::Revoke(1)))
        .unwrap_or_else(|| panic!("no revoke target in {targets:?}"));
    assert_eq!((revoke.rect.x, revoke.rect.width), (0, 2));
    assert_eq!(
        hit(&targets, 0, revoke.rect.y),
        Some(TargetId::View(Spot::Revoke(1)))
    );
    assert_eq!(
        hit(&targets, 10, revoke.rect.y),
        Some(TargetId::View(Spot::Row(1)))
    );
}

#[test]
fn moving_down_past_the_first_row_keeps_three_rows_in_view() {
    // Five rows in a three-row window, two past it: moving to row 2
    // keeps the window at the top, while a one-row window scrolls.
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
    rules.key(&Key::Down, &ctx);
    assert_eq!(rules.list.selected(), 2);
    assert_eq!(rules.list.top(), 0);
    let rows: Vec<String> = drawn(&rules, 80, 5).lines().map(str::to_owned).collect();
    assert_eq!(rows[1], "Global rules  /home/rules");
    assert_eq!(
        rows[2],
        "\u{2715} allow  shell  npm test  2026-10-07 00:00 UTC  s_01"
    );
    assert_eq!(rows[3], "\u{2715} deny  shell  (any)");
}

#[test]
fn rules_80x24() {
    let fake = fake(vec![full(), hand()], Err("/home/projects/-w/rules:2: bad"));
    let rules = open(&fake);
    insta::assert_snapshot!("rules_80x24", drawn(&rules, 80, 24));
}
