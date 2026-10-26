//! Tests for the `/` list.

use super::{Row, SHOWN, extensions, filter, is_built_in, rows, window_start};
use crate::app::commands::{TABLE, aliases_of};
use contract::commands::BUILT_IN_COMMANDS;
use contract::events::CommandInfo;

/// A `commands` answer row for the skill `name`, with no hint.
fn skill(name: &str) -> CommandInfo {
    CommandInfo {
        name: name.to_owned(),
        description: format!("The {name} skill."),
        argument_hint: None,
        tag: "skill".to_owned(),
        extension: false,
    }
}

fn extension_row(name: &str) -> CommandInfo {
    CommandInfo {
        name: name.to_owned(),
        description: format!("The {name} extension."),
        argument_hint: Some("[args]".to_owned()),
        tag: "acme".to_owned(),
        extension: true,
    }
}

fn mcp_row(name: &str) -> CommandInfo {
    CommandInfo {
        name: name.to_owned(),
        description: format!("The {name} prompt."),
        argument_hint: None,
        tag: "server".to_owned(),
        extension: false,
    }
}

fn template_row(name: &str) -> CommandInfo {
    CommandInfo {
        name: name.to_owned(),
        description: format!("The {name} template."),
        argument_hint: None,
        tag: "template".to_owned(),
        extension: false,
    }
}

/// The `/` list's built-in order, as drawn: the table keeps this order,
/// and `docs/tui.md`'s "Slash commands" table matches it.
const DRAWN_ORDER: &[&str] = &[
    "home",
    "new",
    "resume",
    "model",
    "thinking",
    "credential",
    "scoped-models",
    "tools",
    "mcp",
    "context",
    "usage",
    "quota",
    "status",
    "doctor",
    "diff",
    "copy",
    "pwd",
    "panel",
    "rail",
    "rules",
    "reviewer-setup",
    "settings",
    "theme",
    "keys",
    "skills",
    "rewind",
    "handoff",
    "name",
    "login",
    "logout",
    "reload",
    "import",
    "clear",
    "close",
    "feedback",
    "quit",
    "approvals",
    "?",
    "help",
    "delegates",
];

#[test]
fn the_list_draws_every_drawn_command_once_in_drawn_order() {
    let all = rows(&[]);
    let names: Vec<&str> = all.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(names, DRAWN_ORDER);
}

// debt: this parser duplicates crates/contract/src/commands_tests.rs:609;
// the dependency runs tui -> contract, so contract cannot name tui's
// DRAWN_ORDER, and inlining the drawn order as a literal in contract
// would make three sync points for one order; the plan fence places
// doc-order tests in slash_tests. Revisit if the doc-table format
// changes (both parsers change together) or a shared test-support
// crate appears.
#[test]
fn the_doc_table_lists_the_drawn_commands_in_drawn_order() {
    let doc = include_str!("../../../docs/tui.md");
    let table = doc.split("### Slash commands").nth(1).unwrap();
    let mut names = Vec::new();
    for line in table.lines().skip_while(|l| !l.starts_with("| `")) {
        if !line.starts_with("| `") {
            break;
        }
        let cell = line.split('|').nth(1).unwrap_or("");
        for part in cell.split(',') {
            let part = part.trim().trim_matches('`');
            // `/handoff [instructions]`, `/?`.
            let name = part
                .trim_start_matches('/')
                .split_whitespace()
                .next()
                .unwrap_or("");
            if !name.is_empty() {
                names.push(name.to_owned());
            }
        }
    }
    let drawn: Vec<String> = names
        .into_iter()
        .filter(|name| DRAWN_ORDER.contains(&name.as_str()))
        .collect();
    assert_eq!(drawn, DRAWN_ORDER);
}

#[test]
fn built_ins_are_the_drawn_commands_their_aliases_and_rewind() {
    for name in table_names() {
        assert!(
            BUILT_IN_COMMANDS.contains(&name),
            "{name} drawn but not in the contract list"
        );
    }
    let extra: Vec<&&str> = BUILT_IN_COMMANDS
        .iter()
        .filter(|name| !table_names().contains(name))
        .collect();
    // This terminal draws `rewind` among the built-in rows.
    let wanted = [
        &"effort",
        &"cost",
        &"agents",
        &"cwd",
        &"permissions",
        &"config",
        &"keybindings",
        &"keymap",
        &"compact",
        &"rename",
        &"stop",
        &"exit",
        &"bug",
    ];
    // The table enumerates the complete alias set (`docs/tui.md:1920–1959,1965`).
    assert_eq!(extra, wanted);
    // An extension or skill named like an alias cannot take it silently
    // (`docs/tui.md:1974`).
    for alias in TABLE
        .iter()
        .flat_map(|entry| aliases_of(entry.name).iter().copied())
    {
        assert!(is_built_in(alias), "{alias}");
    }
    let rows = rows(&[]);
    let credential = rows.iter().find(|row| row.name == "credential").unwrap();
    assert!(credential.line().contains("/credential [<label>]"));
    assert!(is_built_in("credential"));
    assert!(is_built_in("rewind"));
    assert!(is_built_in("delegates"));
    assert!(is_built_in("agents"));
    assert!(!is_built_in("tdd"));
}

/// The table's names, in order.
fn table_names() -> Vec<&'static str> {
    TABLE.iter().map(|entry| entry.name).collect()
}

fn names(rows: &[&Row]) -> Vec<String> {
    rows.iter().map(|row| row.name.clone()).collect()
}

#[test]
fn built_ins_come_in_table_order_then_the_answer_rows() {
    let all = rows(&[skill("tdd"), skill("review")]);
    let all: Vec<&Row> = all.iter().collect();
    let built_ins = rows(&[]).len();
    let listed = table_names();
    assert_eq!(listed.len(), built_ins);
    let (table, answered) = all.split_at(built_ins);
    assert_eq!(names(table), listed);
    assert!(table.iter().all(|row| is_built_in(&row.name)));
    assert_eq!(names(answered), ["tdd", "review"]);
    assert!(table.iter().all(|row| row.tag == "command"));
    assert!(answered.iter().all(|row| row.tag == "skill"));
    assert_eq!(
        all.iter()
            .find(|row| row.name == "tdd")
            .map(|row| &row.description),
        Some(&"The tdd skill.".to_owned())
    );
    assert_eq!(
        all.iter()
            .find(|row| row.name == "context")
            .map(|row| row.line()),
        Some("/context  Opens the context breakdown.  command".to_owned())
    );
}

#[test]
fn a_skill_named_like_a_built_in_is_left_out() {
    for answer in [
        skill("reload"),
        template_row("reload"),
        mcp_row("reload"),
        CommandInfo {
            tag: "acme".to_owned(),
            ..skill("reload")
        },
    ] {
        let all = rows(&[answer, skill("tdd")]);
        let reloads: Vec<&Row> = all.iter().filter(|row| row.name == "reload").collect();
        assert_eq!(reloads.len(), 1);
        assert_eq!(reloads.first().map(|row| row.tag.as_str()), Some("command"));
    }
    assert!(is_built_in("reload"));
    assert!(!is_built_in("tdd"));
}

#[test]
fn an_extension_row_takes_its_built_in_slot() {
    let mut model = extension_row("model");
    model.description = "Picks a fast model.".to_owned();
    model.argument_hint = Some("[speed]".to_owned());
    let all = rows(&[model, skill("tdd")]);
    let models: Vec<&Row> = all.iter().filter(|row| row.name == "model").collect();
    assert_eq!(models.len(), 1);
    let at = table_names()
        .iter()
        .position(|name| *name == "model")
        .unwrap();
    assert_eq!(all[at].name, "model");
    assert_eq!(all[at].tag, "acme");
    assert_eq!(all[at].line(), "/model [speed]  Picks a fast model.  acme");
    // Every drawn slot, including the `?`/`help` shared-handler rows,
    // keeps its position with the extension's tag.
    let plain = rows(&[]);
    for name in DRAWN_ORDER {
        let at = plain.iter().position(|row| row.name == *name).unwrap();
        let with = rows(&[extension_row(name)]);
        assert_eq!(with.len(), plain.len(), "{name}");
        assert_eq!(with[at].name, *name);
        assert_eq!(with[at].tag, "acme");
    }
}

#[test]
fn an_extension_row_for_an_undrawn_built_in_stays_in_answer_order() {
    let all = rows(&[skill("tdd"), extension_row("effort")]);
    let efforts: Vec<&Row> = all.iter().filter(|row| row.name == "effort").collect();
    assert_eq!(efforts.len(), 1);
    assert_eq!(efforts[0].tag, "acme");
    let built_ins = rows(&[]).len();
    let (_, answered) = all.split_at(built_ins);
    let answered: Vec<&Row> = answered.iter().collect();
    assert_eq!(names(&answered), ["tdd", "effort"]);
}

#[test]
fn extensions_lists_only_extension_rows_in_answer_order() {
    let answer = vec![
        skill("tdd"),
        extension_row("deploy"),
        mcp_row("greet"),
        extension_row("model"),
    ];
    assert_eq!(extensions(&answer), ["deploy", "model"]);
    assert!(extensions(&[]).is_empty());
}

#[test]
fn an_empty_query_shows_every_row() {
    let all = rows(&[skill("tdd")]);
    assert_eq!(filter(&all, "").len(), all.len());
}

#[test]
fn prefix_matches_come_before_contains_matches() {
    let all = rows(&[skill("prereview"), skill("Review")]);
    // `re`: resume starts with it, then reviewer-setup, then name by its alias `rename`,
    // reload, and the skill `Review` (case ignored); `prereview`
    // contains it later on.
    assert_eq!(
        names(&filter(&all, "re")),
        [
            "resume",
            "reviewer-setup",
            "rewind",
            "name",
            "reload",
            "Review",
            "credential",
            "prereview"
        ]
    );
    assert_eq!(names(&filter(&all, "RE")), names(&filter(&all, "re")));
    // Contains only: no row starts with `ose`.
    assert_eq!(names(&filter(&all, "ose")), ["close"]);
    assert!(filter(&all, "zz").is_empty());
}

#[test]
fn a_row_draws_name_hint_description_and_tag() {
    // The table orders `/compact` after `/handoff` (`docs/tui.md:1947,1965`).
    let all = rows(&[]);
    let handoff = all.iter().find(|row| row.name == "handoff");
    assert_eq!(
        handoff.map(Row::line),
        Some("/handoff /compact [instructions]  Starts a handoff.  command".to_owned())
    );
    let home = all.iter().find(|row| row.name == "home");
    assert_eq!(
        home.map(Row::line),
        Some("/home  Goes home, focused on the session on screen's row.  command".to_owned())
    );
}

#[test]
fn the_window_scrolls_only_past_the_last_shown_row() {
    assert_eq!(SHOWN, 8);
    assert_eq!(window_start(0), 0);
    assert_eq!(window_start(SHOWN - 1), 0);
    assert_eq!(window_start(SHOWN), 1);
    assert_eq!(window_start(SHOWN + 3), 4);
}

#[test]
fn an_answer_row_keeps_its_hint_and_tag() {
    let template = CommandInfo {
        name: "review".to_owned(),
        description: "Review a diff.".to_owned(),
        argument_hint: Some("[base]".to_owned()),
        tag: "template".to_owned(),
        extension: false,
    };
    let from_extension = CommandInfo {
        tag: "acme".to_owned(),
        extension: true,
        ..skill("deploy")
    };
    let all = rows(&[template, from_extension]);
    let lines: Vec<String> = all
        .iter()
        .filter(|row| !is_built_in(&row.name))
        .map(Row::line)
        .collect();
    assert_eq!(
        lines,
        [
            "/review [base]  Review a diff.  template",
            "/deploy  The deploy skill.  acme",
        ]
    );
}

#[test]
fn matched_is_the_first_case_folded_occurrence_on_char_boundaries() {
    use super::matched;
    assert_eq!(matched("reload", "re"), Some(0..2));
    assert_eq!(matched("reload", "load"), Some(2..6));
    assert_eq!(matched("reload", "RE"), Some(0..2));
    assert_eq!(matched("close", "ose"), Some(2..5));
    assert_eq!(matched("abcb", "bc"), Some(1..3));
    assert_eq!(matched("caf\u{e9}", "F\u{c9}"), Some(2..5));
    assert_eq!(matched("reload", "zzz"), None);
    assert_eq!(matched("reload", ""), None);
    assert_eq!(matched("", "re"), None);
    // The same fold the filter uses: a final sigma folds contextually,
    // which per-character folding misses, so the filter accepts these
    // and the match agrees with it.
    assert_eq!(matched("\u{39f}\u{3a3}", "\u{3bf}\u{3c2}"), Some(0..4));
    assert_eq!(matched("\u{39f}\u{3a3}", "\u{39f}\u{3a3}"), Some(0..4));
    // Folding can expand past the original string: İ lowercases to
    // two code points, so folded offsets map back to the name's own
    // boundaries.
    assert_eq!(matched("\u{130}\u{130}abc", "c"), Some(6..7));
    assert_eq!(matched("\u{130}\u{130}abc", "bc"), Some(5..7));
    assert_eq!(matched("\u{130}\u{130}abc", "ab"), Some(4..6));
    assert_eq!(matched("\u{130}\u{130}abc", "d"), None);
    // A query landing mid-expansion bolds the characters its match
    // touches: the whole name here.
    assert_eq!(matched("\u{130}x", "\u{307}x"), Some(0..3));
    // A query matching a fold's head bolds the whole character.
    assert_eq!(matched("\u{130}", "i"), Some(0..2));
}

#[test]
fn matched_agrees_with_the_filter_fold_wherever_it_matches() {
    // Wherever the whole-string fold contains the query, the match maps
    // to whole characters covering it: agreement holds by construction,
    // pinned here over the fold's tricky cases, with every range on
    // char boundaries and never empty.
    for (name, query) in [
        ("\u{39f}\u{3a3}", "\u{3bf}\u{3c2}"),
        (
            "\u{39f}\u{394}\u{3a5}\u{3a3}\u{3a3}\u{395}\u{38e}\u{3a3}",
            "\u{3c5}\u{3c3}\u{3c3}",
        ),
        ("\u{130}\u{130}abc", "c"),
        ("\u{130}\u{130}abc", "i"),
        ("Straße", "STRASSE"),
        ("maße", "MASSE"),
        ("reload", "re"),
        ("reload", "zzz"),
    ] {
        let accepted = name.to_lowercase().contains(&query.to_lowercase());
        let found = super::matched(name, query);
        assert_eq!(found.is_some(), accepted, "{name:?} / {query:?}");
        if let Some(found) = found {
            assert!(name.is_char_boundary(found.start));
            assert!(name.is_char_boundary(found.end));
            assert!(found.start < found.end, "{name:?} / {query:?}");
        }
    }
}

#[test]
fn quota_opens_the_view_from_the_list() {
    let all = rows(&[]);
    let quota = all.iter().find(|row| row.name == "quota").unwrap();
    assert_eq!(
        quota.line(),
        "/quota  Opens the quota view and fetches every label's quota.  command"
    );
}

#[test]
fn doctor_opens_the_report_from_the_list() {
    let all = rows(&[]);
    let doctor = all.iter().find(|row| row.name == "doctor").unwrap();
    assert_eq!(doctor.line(), "/doctor  Opens the doctor report.  command");
    let names: Vec<&str> = all.iter().map(|row| row.name.as_str()).collect();
    let (status, doctor) = (
        names.iter().position(|name| *name == "status").unwrap(),
        names.iter().position(|name| *name == "doctor").unwrap(),
    );
    assert_eq!(doctor, status + 1);
}
