//! Tests for the `/` list.

use super::{BUILT_INS, Row, SHOWN, filter, is_built_in, rows, window_start};
use contract::events::CommandInfo;

/// A `commands` answer row for the skill `name`, with no hint.
fn skill(name: &str) -> CommandInfo {
    CommandInfo {
        name: name.to_owned(),
        description: format!("The {name} skill."),
        argument_hint: None,
        tag: "skill".to_owned(),
    }
}

fn names(rows: &[&Row]) -> Vec<String> {
    rows.iter().map(|row| row.name.clone()).collect()
}

#[test]
fn built_ins_come_in_table_order_then_the_answer_rows() {
    let all = rows(&[skill("tdd"), skill("review")]);
    let all: Vec<&Row> = all.iter().collect();
    let built_ins = rows(&[]).len();
    let table_names: Vec<&str> = BUILT_INS.iter().map(|(name, _, _)| *name).collect();
    assert_eq!(table_names.len(), built_ins);
    let (table, answered) = all.split_at(built_ins);
    assert_eq!(names(table), table_names);
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
fn an_answer_row_named_like_a_built_in_shows_once_as_the_built_in() {
    let all = rows(&[skill("reload"), skill("tdd")]);
    let reloads: Vec<&Row> = all.iter().filter(|row| row.name == "reload").collect();
    assert_eq!(reloads.len(), 1);
    assert_eq!(reloads.first().map(|row| row.tag.as_str()), Some("command"));
    assert!(is_built_in("reload"));
    assert!(!is_built_in("tdd"));
}

#[test]
fn an_empty_query_shows_every_row() {
    let all = rows(&[skill("tdd")]);
    assert_eq!(filter(&all, "").len(), all.len());
}

#[test]
fn prefix_matches_come_before_contains_matches() {
    let all = rows(&[skill("prereview"), skill("Review")]);
    // `re`: resume starts with it, then reload, then the skill `Review`
    // (case ignored); `prereview` contains it later on.
    assert_eq!(
        names(&filter(&all, "re")),
        ["resume", "reload", "Review", "prereview"]
    );
    assert_eq!(names(&filter(&all, "RE")), names(&filter(&all, "re")));
    // Contains only: no row starts with `ose`.
    assert_eq!(names(&filter(&all, "ose")), ["close"]);
    assert!(filter(&all, "zz").is_empty());
}

#[test]
fn a_row_draws_name_hint_description_and_tag() {
    let all = rows(&[]);
    let handoff = all.iter().find(|row| row.name == "handoff");
    assert_eq!(
        handoff.map(Row::line),
        Some("/handoff [instructions]  Starts a handoff.  command".to_owned())
    );
    let home = all.iter().find(|row| row.name == "home");
    assert_eq!(
        home.map(Row::line),
        Some("/home  Goes home.  command".to_owned())
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
    };
    let from_extension = CommandInfo {
        tag: "acme".to_owned(),
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
    assert_eq!(matched("caf\u{e9}", "F\u{c9}"), Some(2..5));
    // A fold that widens past the match leaves no range to bold: İ
    // lowercases to two code points, so the query's end falls inside
    // the name's first character.
    assert_eq!(matched("\u{130}", "i"), None);
    assert_eq!(matched("reload", "zzz"), None);
    assert_eq!(matched("reload", ""), None);
    assert_eq!(matched("", "re"), None);
}
