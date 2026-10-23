//! The notes template (`docs/permissions.md`, "Setting up the notes"): the
//! command owns the bullets under its topic headings, and text outside them
//! is kept byte for byte.

use contract::notes::Topic;

use super::{DiffRow, NotesLayer, Source, clean_line, default_layer, diff, merge, owned};

fn s(text: &str) -> String {
    text.to_string()
}

fn strike(topic: Topic, text: &str) -> (Topic, String) {
    (topic, s(text))
}

fn add(topic: Topic, text: &str) -> (Topic, String) {
    (topic, s(text))
}

/// The bullets `owned` reports under one topic.
fn bullets(text: &str, topic: Topic) -> Vec<String> {
    owned(text)
        .into_iter()
        .find(|(t, _)| *t == topic)
        .map(|(_, b)| b)
        .unwrap_or_default()
}

/// Texts `merge` must hand back untouched when nothing changes.
const AWKWARD: &[&str] = &[
    "",
    "prose only\n",
    "### Source control\r\n- a\r\n- b\r\nprose\r\n",
    "### Source control\n-  x\n- y   \n",
    "### Source control\n- last has no newline",
    "### Source control\n- a\n### Routine actions",
    "### Source control\n- a\n\nprose\n### Source control\n- b\n",
    "a\rb\n### Source control\n- a\rb\n",
    "### Not one of the five\n- a\n- b\n",
    "### Source control   \n- a\n",
    "### Source control\n- \n- a\n",
    "### Source control\r\n- a\n### Internal hosts\n- b\r\n",
    "before\n### Source control\n- a\nbetween\n### Routine actions\n- b\nafter\n",
];

// ---------------------------------------------------------------- owned

#[test]
fn owned_reads_each_topics_bullets_with_prose_all_around() {
    let text = "Notes by hand\n\
                ### Source control\n\
                - github.com/acme/* are ours\n\
                - my account is mine\n\
                prose between\n\
                ### Routine actions\n\
                - squash my own PRs\n\
                prose below\n";
    assert_eq!(
        bullets(text, Topic::SourceControl),
        ["github.com/acme/* are ours", "my account is mine"]
    );
    assert_eq!(bullets(text, Topic::RoutineActions), ["squash my own PRs"]);
    assert!(bullets(text, Topic::Never).is_empty());
}

#[test]
fn owned_reads_crlf_headings_and_bullets() {
    let text = "### Internal hosts\r\n- corp.example\r\n- registry.example\r\n";
    assert_eq!(
        bullets(text, Topic::InternalHosts),
        ["corp.example", "registry.example"]
    );
}

#[test]
fn owned_joins_a_duplicate_heading_and_counts_a_repeat_once() {
    let text = "### Source control\n- a\n- b\nprose\n### Source control\n- b\n- c\n";
    assert_eq!(bullets(text, Topic::SourceControl), ["a", "b", "c"]);
}

#[test]
fn owned_reports_a_heading_with_no_bullets_as_a_topic_with_none() {
    let found = owned("### Internal tools\nprose\n");
    assert_eq!(found, [(Topic::InternalTools, Vec::<String>::new())]);
}

#[test]
fn a_star_line_ends_a_block() {
    let text = "### Source control\n- a\n* b\n- c\n";
    assert_eq!(bullets(text, Topic::SourceControl), ["a"]);
}

#[test]
fn a_bullet_that_is_exactly_dash_space_is_owned_but_absent() {
    let text = "### Source control\n- \n- a\n";
    assert_eq!(bullets(text, Topic::SourceControl), ["a"]);
}

#[test]
fn extra_spaces_after_the_dash_and_trailing_spaces_are_trimmed() {
    let text = "### Source control\n-  x\n- y   \n";
    assert_eq!(bullets(text, Topic::SourceControl), ["x", "y"]);
}

#[test]
fn a_heading_with_trailing_spaces_is_not_owned() {
    assert!(owned("### Source control   \n- a\n").is_empty());
}

#[test]
fn a_dash_with_no_space_is_not_a_bullet() {
    assert!(bullets("### Source control\n-a\n", Topic::SourceControl).is_empty());
}

// ---------------------------------------------------------------- merge

#[test]
fn nothing_struck_and_nothing_added_returns_every_text_byte_for_byte() {
    for text in AWKWARD {
        assert_eq!(&merge(text, &[], &[]), text, "moved a byte of {text:?}");
    }
}

#[test]
fn a_struck_text_nothing_owns_and_an_added_line_already_owned_return_the_text_byte_for_byte() {
    for text in AWKWARD {
        let struck = [strike(Topic::SourceControl, "never written")];
        assert_eq!(&merge(text, &struck, &[]), text, "struck-nothing {text:?}");
    }
    let text = "### Source control\r\n-  x\r\n- y   \r\nprose\r\n";
    let added = [
        add(Topic::SourceControl, "x"),
        add(Topic::SourceControl, "y"),
    ];
    assert_eq!(merge(text, &[], &added), text);
}

#[test]
fn a_struck_bullet_removes_only_its_own_bytes() {
    let text = "intro\n### Source control\r\n- a\r\n- b\n- c   \nprose\n";
    let struck = [strike(Topic::SourceControl, "b")];
    assert_eq!(
        merge(text, &struck, &[]),
        "intro\n### Source control\r\n- a\r\n- c   \nprose\n"
    );
    // The neighbours' CRLF endings stay.
    let struck = [strike(Topic::SourceControl, "a")];
    assert_eq!(
        merge(text, &struck, &[]),
        "intro\n### Source control\r\n- b\n- c   \nprose\n"
    );
}

#[test]
fn a_struck_text_removes_every_bullet_of_it_in_every_block_of_the_topic() {
    let text = "### Source control\n- a\n- b\nprose\n### Source control\n- b\n- c\n";
    let struck = [strike(Topic::SourceControl, "b")];
    assert_eq!(
        merge(text, &struck, &[]),
        "### Source control\n- a\nprose\n### Source control\n- c\n"
    );
}

#[test]
fn a_strike_in_one_topic_leaves_the_same_text_in_another_topic() {
    let text = "### Source control\n- a\n### Internal hosts\n- a\n";
    let struck = [strike(Topic::InternalHosts, "a")];
    assert_eq!(merge(text, &struck, &[]), "### Source control\n- a\n");
}

#[test]
fn a_struck_text_matches_after_trimming() {
    let text = "### Source control\n-  x  \n- y\n";
    let struck = [strike(Topic::SourceControl, "  x")];
    assert_eq!(merge(text, &struck, &[]), "### Source control\n- y\n");
}

#[test]
fn a_block_goes_with_its_heading_when_every_bullet_was_struck() {
    let text = "above\n### Source control\n- a\n- b\nbelow\n";
    let struck = [
        strike(Topic::SourceControl, "a"),
        strike(Topic::SourceControl, "b"),
    ];
    assert_eq!(merge(text, &struck, &[]), "above\nbelow\n");
}

#[test]
fn a_block_that_keeps_a_bullet_keeps_its_heading() {
    let text = "### Source control\n- a\n- b\n";
    let struck = [strike(Topic::SourceControl, "a")];
    assert_eq!(merge(text, &struck, &[]), "### Source control\n- b\n");
}

#[test]
fn a_block_that_keeps_an_empty_bullet_stays() {
    let text = "### Source control\n- \n- a\n";
    let struck = [strike(Topic::SourceControl, "a")];
    assert_eq!(merge(text, &struck, &[]), "### Source control\n- \n");
}

#[test]
fn a_bare_heading_stays_when_a_strike_names_nothing_in_it() {
    let text = "### Source control\nprose\n### Internal hosts\n- h\n";
    let struck = [strike(Topic::SourceControl, "h")];
    assert_eq!(merge(text, &struck, &[]), text);
}

#[test]
fn a_block_emptied_by_strikes_beside_prose_leaves_the_prose() {
    let text = "keep me\n### Routine actions\n- a\nprose directly after\n";
    let struck = [strike(Topic::RoutineActions, "a")];
    assert_eq!(merge(text, &struck, &[]), "keep me\nprose directly after\n");
}

#[test]
fn a_block_emptied_by_strikes_that_receives_an_added_line_stays() {
    let text = "### Source control\n- a\n";
    let struck = [strike(Topic::SourceControl, "a")];
    let added = [add(Topic::SourceControl, "b")];
    assert_eq!(merge(text, &struck, &added), "### Source control\n- b\n");
}

#[test]
fn a_second_block_emptied_by_strikes_goes_while_the_first_receives_the_added_line() {
    let text = "### Source control\n- a\nprose\n### Source control\n- b\n";
    let struck = [strike(Topic::SourceControl, "b")];
    let added = [add(Topic::SourceControl, "c")];
    assert_eq!(
        merge(text, &struck, &added),
        "### Source control\n- a\n- c\nprose\n"
    );
}

#[test]
fn an_added_line_goes_directly_after_the_first_blocks_last_line() {
    let text = "### Source control\n- a\nprose\n### Source control\n- b\n";
    let added = [add(Topic::SourceControl, "c")];
    assert_eq!(
        merge(text, &[], &added),
        "### Source control\n- a\n- c\nprose\n### Source control\n- b\n"
    );
}

#[test]
fn an_added_line_goes_after_a_bare_heading() {
    let text = "### Source control\nprose\n";
    let added = [add(Topic::SourceControl, "a")];
    assert_eq!(merge(text, &[], &added), "### Source control\n- a\nprose\n");
}

#[test]
fn an_added_line_after_a_last_line_with_no_ending_takes_the_headings_ending() {
    let added = [add(Topic::SourceControl, "b")];
    assert_eq!(
        merge("### Source control\n- a", &[], &added),
        "### Source control\n- a\n- b\n"
    );
    assert_eq!(
        merge("### Source control\r\n- a", &[], &added),
        "### Source control\r\n- a\r\n- b\n"
    );
    assert_eq!(
        merge("### Source control", &[], &added),
        "### Source control\n- b\n"
    );
    assert_eq!(
        merge("### Source control\r\n", &[], &added),
        "### Source control\r\n- b\n"
    );
}

#[test]
fn an_added_line_goes_after_the_last_bullet_left_when_the_last_was_struck() {
    let text = "### Source control\n- a\n- b\nprose\n";
    let struck = [strike(Topic::SourceControl, "b")];
    let added = [add(Topic::SourceControl, "c")];
    assert_eq!(
        merge(text, &struck, &added),
        "### Source control\n- a\n- c\nprose\n"
    );
}

#[test]
fn a_topic_with_no_block_gets_one_appended_after_a_blank_line_in_topic_order() {
    let added = [
        add(Topic::Never, "touch infra/prod"),
        add(Topic::SourceControl, "acme"),
    ];
    assert_eq!(
        merge("prose\n", &[], &added),
        "prose\n\n### Source control\n- acme\n\n### What must never happen\n- touch infra/prod\n"
    );
}

#[test]
fn a_new_block_after_text_with_no_final_newline_adds_the_newline_first() {
    let added = [add(Topic::SourceControl, "acme")];
    assert_eq!(
        merge("prose", &[], &added),
        "prose\n\n### Source control\n- acme\n"
    );
    assert_eq!(
        merge("prose\r\n", &[], &added),
        "prose\r\n\n### Source control\n- acme\n"
    );
}

#[test]
fn a_new_block_in_an_empty_text_has_no_blank_line_above() {
    let added = [add(Topic::InternalHosts, "corp.example")];
    assert_eq!(
        merge("", &[], &added),
        "### Internal hosts\n- corp.example\n"
    );
}

#[test]
fn a_confirmed_line_equal_to_an_unstruck_owned_bullet_of_its_topic_is_not_added() {
    let text = "### Source control\n- a\n";
    let added = [add(Topic::SourceControl, "a")];
    assert_eq!(merge(text, &[], &added), text);
    // Another topic's bullet does not cover it.
    let added = [add(Topic::InternalHosts, "a")];
    assert_eq!(
        merge(text, &[], &added),
        "### Source control\n- a\n\n### Internal hosts\n- a\n"
    );
}

#[test]
fn a_repeated_confirmed_line_is_added_once() {
    let added = [
        add(Topic::SourceControl, "b"),
        add(Topic::SourceControl, "b"),
        add(Topic::SourceControl, " b "),
    ];
    assert_eq!(
        merge("### Source control\n- a\n", &[], &added),
        "### Source control\n- a\n- b\n"
    );
}

#[test]
fn a_line_struck_then_confirmed_again_is_removed_once_and_added_once() {
    let text = "### Source control\n- a\n- b\n";
    let struck = [strike(Topic::SourceControl, "a")];
    let added = [add(Topic::SourceControl, "a")];
    assert_eq!(
        merge(text, &struck, &added),
        "### Source control\n- b\n- a\n"
    );
}

#[test]
fn a_newline_or_tab_inside_an_added_line_becomes_a_space() {
    let added = [add(Topic::SourceControl, "a\nb\tc\r\nd")];
    assert_eq!(
        merge("### Source control\n", &[], &added),
        "### Source control\n- a b c  d\n"
    );
}

#[test]
fn an_added_line_that_cleans_to_nothing_is_dropped() {
    let added = [add(Topic::SourceControl, "\u{200b} \u{202e}")];
    assert_eq!(merge("prose\n", &[], &added), "prose\n");
}

#[test]
fn an_added_line_is_stored_cleaned() {
    let added = [add(Topic::SourceControl, "ac\u{200b}me")];
    assert_eq!(merge("", &[], &added), "### Source control\n- acme\n");
}

// ----------------------------------------------------------- clean_line

#[test]
fn clean_line_strips_zero_width_and_bidirectional_characters() {
    assert_eq!(clean_line("a\u{200b}b\u{202e}c\u{feff}d\u{00ad}e"), "abcde");
}

#[test]
fn clean_line_turns_whitespace_controls_into_spaces_and_drops_other_controls() {
    assert_eq!(clean_line("a\tb\nc\rd"), "a b c d");
    assert_eq!(clean_line("a\u{7}b"), "ab");
    assert_eq!(clean_line("a\u{1b}[31mb"), "a[31mb");
}

#[test]
fn clean_line_trims() {
    assert_eq!(clean_line("  a b \t"), "a b");
    assert_eq!(clean_line("\u{200b}  a  \u{200b}"), "a");
}

#[test]
fn clean_line_is_empty_for_a_line_with_nothing_left() {
    assert_eq!(clean_line(""), "");
    assert_eq!(clean_line("\u{200b}\u{feff} \t"), "");
}

#[test]
fn clean_line_is_idempotent() {
    for line in ["a\u{200b} b", "  x\ty ", "\u{202e}z\u{7}"] {
        let once = clean_line(line);
        assert_eq!(clean_line(&once), once);
    }
}

/// Unicode 15's format characters, as the table lists them.
const CF: &[(u32, u32)] = &[
    (0x00AD, 0x00AD),
    (0x0600, 0x0605),
    (0x061C, 0x061C),
    (0x06DD, 0x06DD),
    (0x070F, 0x070F),
    (0x0890, 0x0891),
    (0x08E2, 0x08E2),
    (0x180E, 0x180E),
    (0x200B, 0x200F),
    (0x202A, 0x202E),
    (0x2060, 0x2064),
    (0x2066, 0x206F),
    (0xFEFF, 0xFEFF),
    (0xFFF9, 0xFFFB),
    (0x110BD, 0x110BD),
    (0x110CD, 0x110CD),
    (0x13430, 0x1343F),
    (0x1BCA0, 0x1BCA3),
    (0x1D173, 0x1D17A),
    (0xE0001, 0xE0001),
    (0xE0020, 0xE007F),
];

fn inside(code: u32) -> bool {
    CF.iter().any(|(lo, hi)| (*lo..=*hi).contains(&code))
}

fn wrapped(code: u32) -> String {
    let c = char::from_u32(code).expect("a scalar value");
    format!("a{c}b")
}

#[test]
fn clean_line_removes_the_first_and_last_character_of_every_format_range() {
    for (lo, hi) in CF {
        assert_eq!(clean_line(&wrapped(*lo)), "ab", "U+{lo:04X} stays");
        assert_eq!(clean_line(&wrapped(*hi)), "ab", "U+{hi:04X} stays");
    }
}

#[test]
fn clean_line_keeps_the_character_just_outside_every_format_range() {
    for (lo, hi) in CF {
        for code in [lo - 1, hi + 1] {
            if inside(code) {
                continue;
            }
            assert_eq!(
                clean_line(&wrapped(code)),
                wrapped(code),
                "U+{code:04X} is removed"
            );
        }
    }
    // The neighbours the plan names.
    for code in [0x200A, 0x2010, 0x202F, 0x2070, 0xFEFE, 0xE0080] {
        assert_eq!(clean_line(&wrapped(code)), wrapped(code), "U+{code:04X}");
    }
}

#[test]
fn clean_line_removes_every_character_inside_every_format_range() {
    for (lo, hi) in CF {
        for code in *lo..=*hi {
            assert_eq!(clean_line(&wrapped(code)), "ab", "U+{code:04X} stays");
        }
    }
}

// -------------------------------------------------------- default_layer

#[test]
fn a_files_line_goes_per_project() {
    assert_eq!(default_layer(Source::Files), NotesLayer::PerProject);
}

#[test]
fn an_answer_line_goes_global() {
    assert_eq!(default_layer(Source::Answer), NotesLayer::Global);
}

#[test]
fn a_session_line_goes_per_project_only_when_every_escalation_came_from_here() {
    assert_eq!(
        default_layer(Source::Session { all_here: true }),
        NotesLayer::PerProject
    );
    assert_eq!(
        default_layer(Source::Session { all_here: false }),
        NotesLayer::Global
    );
}

#[test]
fn the_layer_tags_name_the_layers() {
    assert_eq!(NotesLayer::PerProject.tag(), "[per-project]");
    assert_eq!(NotesLayer::Global.tag(), "[global]");
}

// ----------------------------------------------------------------- diff

fn rows(before: &str, after: &str) -> Vec<DiffRow> {
    diff(before, after)
}

fn context(text: &str) -> DiffRow {
    DiffRow::Context(s(text))
}
fn removed(text: &str) -> DiffRow {
    DiffRow::Removed(s(text))
}
fn added(text: &str) -> DiffRow {
    DiffRow::Added(s(text))
}

#[test]
fn an_unchanged_layer_has_no_rows() {
    for text in AWKWARD {
        assert!(rows(text, text).is_empty(), "{text:?}");
    }
}

#[test]
fn an_added_bullet_shows_under_its_heading_with_the_kept_bullets_as_context() {
    let before = "### Source control\n- a\n";
    let after = merge(before, &[], &[add(Topic::SourceControl, "b")]);
    assert_eq!(
        rows(before, &after),
        [context("### Source control"), context("- a"), added("- b")]
    );
}

#[test]
fn a_struck_bullet_shows_removed_among_its_kept_neighbours() {
    let before = "### Source control\n- a\n- b\n- c\n";
    let after = merge(before, &[strike(Topic::SourceControl, "b")], &[]);
    assert_eq!(
        rows(before, &after),
        [
            context("### Source control"),
            context("- a"),
            removed("- b"),
            context("- c")
        ]
    );
}

#[test]
fn a_new_heading_is_itself_an_added_line() {
    let before = "prose\n";
    let after = merge(before, &[], &[add(Topic::RoutineActions, "squash")]);
    assert_eq!(
        rows(before, &after),
        [added("### Routine actions"), added("- squash")]
    );
}

#[test]
fn a_removed_block_shows_its_heading_removed() {
    let before = "### Source control\n- a\n";
    let after = merge(before, &[strike(Topic::SourceControl, "a")], &[]);
    assert_eq!(
        rows(before, &after),
        [removed("### Source control"), removed("- a")]
    );
}

#[test]
fn a_topic_with_no_change_is_omitted_and_so_is_prose() {
    let before = "intro\n### Source control\n- a\nbetween\n### Internal hosts\n- h\n- i\nend\n";
    let after = merge(before, &[strike(Topic::InternalHosts, "h")], &[]);
    assert_eq!(
        rows(before, &after),
        [
            context("### Internal hosts"),
            removed("- h"),
            context("- i")
        ]
    );
}

#[test]
fn rows_hold_lines_without_their_endings() {
    let before = "### Source control\r\n- a\r\n";
    let after = merge(before, &[], &[add(Topic::SourceControl, "b")]);
    assert_eq!(
        rows(before, &after),
        [context("### Source control"), context("- a"), added("- b")]
    );
}

#[test]
fn a_fifty_thousand_line_text_with_one_bullet_changed_diffs_to_its_rows() {
    // A table over the whole text, 50,000 by 50,000 cells, would not fit in
    // memory.
    let mut before = String::from("### Source control\n- a\n- b\n");
    for n in 0..50_000 {
        before.push_str(&format!("prose line {n}\n"));
    }
    before.push_str("### Routine actions\n- c\n");
    let after = merge(&before, &[strike(Topic::RoutineActions, "c")], &[]);
    assert_eq!(
        rows(&before, &after),
        [removed("### Routine actions"), removed("- c")]
    );
}

#[test]
fn a_change_at_the_top_of_a_fifty_thousand_line_text_diffs_to_its_rows() {
    let mut before = String::from("### Source control\n- a\n- b\n");
    for n in 0..50_000 {
        before.push_str(&format!("prose line {n}\n"));
    }
    before.push_str("### Routine actions\n- c\n");
    let after = merge(&before, &[strike(Topic::SourceControl, "a")], &[]);
    assert_eq!(
        rows(&before, &after),
        [
            context("### Source control"),
            removed("- a"),
            context("- b")
        ]
    );
}

#[test]
fn changes_at_both_ends_of_a_large_text_diff_to_their_rows() {
    let mut before = String::from("### Source control\n- a\n- b\n");
    for n in 0..1_500 {
        before.push_str(&format!("prose line {n}\n"));
    }
    before.push_str("### Routine actions\n- c\n- d\n");
    let struck = [
        strike(Topic::SourceControl, "a"),
        strike(Topic::RoutineActions, "d"),
    ];
    let after = merge(&before, &struck, &[]);
    assert_eq!(
        rows(&before, &after),
        [
            context("### Source control"),
            removed("- a"),
            context("- b"),
            context("### Routine actions"),
            context("- c"),
            removed("- d"),
        ]
    );
}
