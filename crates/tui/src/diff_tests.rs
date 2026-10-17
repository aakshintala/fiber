//! Tests for the shared diff renderer.

use super::{DiffKind, diff_lines, gutter_prefix, gutter_width};

fn kinds(diff: &str) -> Vec<(Option<u64>, DiffKind, String)> {
    diff_lines(diff)
        .into_iter()
        .map(|line| {
            (
                line.number,
                line.kind,
                line.segments
                    .into_iter()
                    .map(|(text, _)| text)
                    .collect::<String>(),
            )
        })
        .collect()
}

fn marks(diff: &str) -> Vec<Vec<(String, bool)>> {
    diff_lines(diff)
        .into_iter()
        .map(|line| line.segments)
        .collect()
}

#[test]
fn diff_numbers_follow_the_hunk_header() {
    assert_eq!(
        kinds("@@ -12 +13 @@\n-a\n-b\n+c\n+d\n"),
        vec![
            (None, DiffKind::Hunk, "@@ -12 +13 @@".to_owned()),
            (Some(12), DiffKind::Removed, "-a".to_owned()),
            (Some(13), DiffKind::Removed, "-b".to_owned()),
            (Some(13), DiffKind::Added, "+c".to_owned()),
            (Some(14), DiffKind::Added, "+d".to_owned()),
        ]
    );
}

#[test]
fn diff_header_without_counts_parses() {
    assert_eq!(
        kinds("@@ -1 +1 @@\n-before\n+after\n"),
        vec![
            (None, DiffKind::Hunk, "@@ -1 +1 @@".to_owned()),
            (Some(1), DiffKind::Removed, "-before".to_owned()),
            (Some(1), DiffKind::Added, "+after".to_owned()),
        ]
    );
}

#[test]
fn lines_before_the_first_hunk_are_dropped() {
    let diff = "diff --git a/a.rs b/a.rs\nindex 123..456 100644\n--- a/a.rs\n+++ b/a.rs\n@@ -2 +2 @@\n-old\n+new\n";
    assert_eq!(
        kinds(diff),
        vec![
            (None, DiffKind::Hunk, "@@ -2 +2 @@".to_owned()),
            (Some(2), DiffKind::Removed, "-old".to_owned()),
            (Some(2), DiffKind::Added, "+new".to_owned()),
        ]
    );
}

#[test]
fn a_diff_without_a_hunk_draws_plain() {
    let drawn = diff_lines("-old\n+new\nplain");
    assert_eq!(drawn.len(), 3);
    for line in &drawn {
        assert_eq!(line.number, None);
    }
    assert!(drawn.iter().all(|line| line.kind == DiffKind::Context));
    assert_eq!(
        kinds("-old\n+new\nplain"),
        vec![
            (None, DiffKind::Context, "-old".to_owned()),
            (None, DiffKind::Context, "+new".to_owned()),
            (None, DiffKind::Context, "plain".to_owned()),
        ]
    );
}

#[test]
fn one_removed_and_one_added_pair_their_words() {
    let paired = marks("@@ -1 +1 @@\n-call foo(a);\n+call bar(a);\n");
    assert_eq!(paired.len(), 3);
    assert_eq!(
        paired[1],
        vec![("-call ".to_owned(), false), ("foo(a);".to_owned(), true),]
    );
    assert_eq!(
        paired[2],
        vec![("+call ".to_owned(), false), ("bar(a);".to_owned(), true),]
    );
    for (line, raw) in diff_lines("@@ -1 +1 @@\n-call foo(a);\n+call bar(a);\n")
        .iter()
        .zip(["@@ -1 +1 @@", "-call foo(a);", "+call bar(a);"])
    {
        let joined: String = line
            .segments
            .iter()
            .map(|(text, _)| text.as_str())
            .collect();
        assert_eq!(joined, raw);
    }
}

#[test]
fn two_removed_one_added_marks_nothing() {
    for segments in marks("@@ -1,2 +1 @@\n-one\n-two\n+both\n") {
        assert!(segments.iter().all(|(_, marked)| !marked), "{segments:?}");
    }
}

#[test]
fn one_removed_two_added_marks_nothing() {
    for segments in marks("@@ -1 +1,2 @@\n-one\n+uno\n+dos\n") {
        assert!(segments.iter().all(|(_, marked)| !marked), "{segments:?}");
    }
}

#[test]
fn two_by_two_marks_nothing() {
    for segments in marks("@@ -1,2 +1,2 @@\n-one\n-two\n+uno\n+dos\n") {
        assert!(segments.iter().all(|(_, marked)| !marked), "{segments:?}");
    }
}

#[test]
fn a_pair_after_a_longer_run_is_its_own_run() {
    let paired = marks("@@ -1,2 +1 @@\n-a\n-b\n+c\n-d\n+e\n");
    assert_eq!(paired.len(), 6);
    for segments in &paired[1..4] {
        assert!(segments.iter().all(|(_, marked)| !marked), "{segments:?}");
    }
    assert!(paired[4].iter().any(|(_, marked)| *marked));
    assert!(paired[5].iter().any(|(_, marked)| *marked));
}

#[test]
fn spacing_only_change_marks_nothing() {
    for segments in marks("@@ -1 +1 @@\n-a  b\n+a b\n") {
        assert!(segments.iter().all(|(_, marked)| !marked), "{segments:?}");
    }
}

#[test]
fn no_newline_marker_is_dim_and_unnumbered() {
    assert_eq!(
        kinds("@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n"),
        vec![
            (None, DiffKind::Hunk, "@@ -1 +1 @@".to_owned()),
            (Some(1), DiffKind::Removed, "-old".to_owned()),
            (
                None,
                DiffKind::Note,
                "\\ No newline at end of file".to_owned()
            ),
            (Some(1), DiffKind::Added, "+new".to_owned()),
        ]
    );
}

#[test]
fn crlf_lines_draw_as_lf() {
    assert_eq!(
        kinds("@@ -1 +1 @@\r\n-old\r\n+new\r\n"),
        vec![
            (None, DiffKind::Hunk, "@@ -1 +1 @@".to_owned()),
            (Some(1), DiffKind::Removed, "-old".to_owned()),
            (Some(1), DiffKind::Added, "+new".to_owned()),
        ]
    );
}

#[test]
fn a_hunk_at_u64_max_saturates() {
    let diff = "@@ -18446744073709551615,3 +18446744073709551615,3 @@\na\nb\nc\n";
    let drawn = diff_lines(diff);
    assert_eq!(drawn.len(), 4);
    for line in &drawn[1..] {
        assert_eq!(line.kind, DiffKind::Context);
        assert_eq!(line.number, Some(u64::MAX));
    }
}

#[test]
fn a_hunk_number_past_u64_draws_unnumbered_lines() {
    let drawn = diff_lines("@@ -99999999999999999999 +1 @@\n-a\n+b\n");
    assert_eq!(drawn.len(), 3);
    assert_eq!(drawn[0].kind, DiffKind::Hunk);
    assert_eq!(drawn[1].kind, DiffKind::Removed);
    assert_eq!(drawn[1].number, None);
    assert_eq!(drawn[2].kind, DiffKind::Added);
    assert_eq!(drawn[2].number, None);
}

#[test]
fn gutter_prefix_is_none_without_a_gutter_and_blanks_without_a_number() {
    assert_eq!(gutter_prefix(Some(7), 0), None);
    assert_eq!(gutter_prefix(Some(7), 2), Some(" 7 ".to_owned()));
    assert_eq!(gutter_prefix(None, 2), Some("   ".to_owned()));
}

#[test]
fn gutter_width_of_zero_is_one_and_of_u64_max_is_twenty() {
    assert_eq!(gutter_width(vec![0].into_iter()), 1);
    assert_eq!(gutter_width(vec![u64::MAX].into_iter()), 20);
    assert_eq!(gutter_width(vec![9].into_iter()), 1);
    assert_eq!(gutter_width(vec![10].into_iter()), 2);
    assert_eq!(gutter_width(Vec::<u64>::new().into_iter()), 0);
}

#[test]
fn two_word_tail_marks_one_word() {
    let paired = marks("@@ -1 +1 @@\n-p a b c\n+p X b c\n");
    assert_eq!(
        paired[1],
        vec![
            ("-p ".to_owned(), false),
            ("a ".to_owned(), true),
            ("b ".to_owned(), false),
            ("c".to_owned(), false),
        ]
    );
    assert_eq!(
        paired[2],
        vec![
            ("+p ".to_owned(), false),
            ("X ".to_owned(), true),
            ("b ".to_owned(), false),
            ("c".to_owned(), false),
        ]
    );
}

#[test]
fn prefix_words_mark_their_tail() {
    let paired = marks("@@ -1 +1 @@\n-a\n+a b\n");
    assert_eq!(paired[1], vec![("-a".to_owned(), false)]);
    assert_eq!(
        paired[2],
        vec![("+a ".to_owned(), false), ("b".to_owned(), true)]
    );
}

#[test]
fn added_overlap_marks_its_tail() {
    let paired = marks("@@ -1 +1 @@\n-a\n+a a\n");
    assert_eq!(paired[1], vec![("-a".to_owned(), false)]);
    assert_eq!(
        paired[2],
        vec![("+a ".to_owned(), false), ("a".to_owned(), true)]
    );
}

#[test]
fn trailing_edge_marks_one_word() {
    let paired = marks("@@ -1 +1 @@\n-a b\n+c b\n");
    assert_eq!(
        paired[1],
        vec![("-a ".to_owned(), true), ("b".to_owned(), false)]
    );
    assert_eq!(
        paired[2],
        vec![("+c ".to_owned(), true), ("b".to_owned(), false)]
    );
}

#[test]
fn two_word_overlap_marks_one_word() {
    let paired = marks("@@ -1 +1 @@\n-b a\n+c b a\n");
    assert_eq!(
        paired[1],
        vec![("-b ".to_owned(), false), ("a".to_owned(), false)]
    );
    assert_eq!(
        paired[2],
        vec![
            ("+c ".to_owned(), true),
            ("b ".to_owned(), false),
            ("a".to_owned(), false),
        ]
    );
}

#[test]
fn removed_overlap_leaves_the_added_line_plain() {
    let paired = marks("@@ -1 +1 @@\n-a b\n+b\n");
    assert_eq!(
        paired[1],
        vec![("-a ".to_owned(), true), ("b".to_owned(), false)]
    );
    assert_eq!(paired[2], vec![("+b".to_owned(), false)]);
}

#[test]
fn shared_overlap_marks_the_diverging_word() {
    let paired = marks("@@ -1 +1 @@\n-a a\n+a\n");
    assert_eq!(
        paired[1],
        vec![("-a ".to_owned(), false), ("a".to_owned(), true)]
    );
    assert_eq!(paired[2], vec![("+a".to_owned(), false)]);
}

#[test]
fn reentrant_overlap_leaves_the_middle_plain() {
    let paired = marks("@@ -1 +1 @@\n-x z a\n+z a\n");
    assert_eq!(
        paired[1],
        vec![
            ("-x ".to_owned(), true),
            ("z ".to_owned(), false),
            ("a".to_owned(), false),
        ]
    );
    assert_eq!(
        paired[2],
        vec![("+z ".to_owned(), false), ("a".to_owned(), false)]
    );
}

#[test]
fn full_overlap_marks_the_extra_word() {
    let paired = marks("@@ -1 +1 @@\n-a x\n+a x x\n");
    assert_eq!(
        paired[1],
        vec![("-a ".to_owned(), false), ("x".to_owned(), false)]
    );
    assert_eq!(
        paired[2],
        vec![
            ("+a ".to_owned(), false),
            ("x ".to_owned(), false),
            ("x".to_owned(), true),
        ]
    );
}

#[test]
fn fully_common_words_draw_as_one_segment() {
    let paired = marks("@@ -1 +1 @@\n-a  b\n+a b\n");
    assert_eq!(paired[1], vec![("-a  b".to_owned(), false)]);
    assert_eq!(paired[2], vec![("+a b".to_owned(), false)]);
}

#[test]
fn leading_space_survives_pairing() {
    let paired = marks("@@ -1 +1 @@\n-  foo\n+  bar\n");
    assert_eq!(
        paired[1],
        vec![("-  ".to_owned(), false), ("foo".to_owned(), true)]
    );
    assert_eq!(
        paired[2],
        vec![("+  ".to_owned(), false), ("bar".to_owned(), true)]
    );
}

#[test]
fn trailing_edge_added_side_marks_one_word() {
    let paired = marks("@@ -1 +1 @@\n-b\n+c b\n");
    assert_eq!(paired[1], vec![("-b".to_owned(), false)]);
    assert_eq!(
        paired[2],
        vec![("+c ".to_owned(), true), ("b".to_owned(), false)]
    );
}

#[test]
fn three_word_tail_marks_two_words() {
    let paired = marks("@@ -1 +1 @@\n-p X a b c d\n+p Y a b c d\n");
    assert_eq!(
        paired[1],
        vec![
            ("-p ".to_owned(), false),
            ("X ".to_owned(), true),
            ("a ".to_owned(), false),
            ("b ".to_owned(), false),
            ("c ".to_owned(), false),
            ("d".to_owned(), false),
        ]
    );
    assert_eq!(
        paired[2],
        vec![
            ("+p ".to_owned(), false),
            ("Y ".to_owned(), true),
            ("a ".to_owned(), false),
            ("b ".to_owned(), false),
            ("c ".to_owned(), false),
            ("d".to_owned(), false),
        ]
    );
}

#[test]
fn tail_sum_and_product_differ() {
    let paired = marks("@@ -1 +1 @@\n-a b c d\n+a b X d\n");
    assert_eq!(
        paired[1],
        vec![
            ("-a ".to_owned(), false),
            ("b ".to_owned(), false),
            ("c ".to_owned(), true),
            ("d".to_owned(), false),
        ]
    );
    assert_eq!(
        paired[2],
        vec![
            ("+a ".to_owned(), false),
            ("b ".to_owned(), false),
            ("X ".to_owned(), true),
            ("d".to_owned(), false),
        ]
    );
}

#[test]
fn trailing_stops_at_the_short_sides_leading_edge() {
    let paired = marks("@@ -1 +1 @@\n-q x q a b\n+q a b\n");
    assert_eq!(
        paired[1],
        vec![
            ("-q ".to_owned(), false),
            ("x ".to_owned(), true),
            ("q ".to_owned(), true),
            ("a ".to_owned(), false),
            ("b".to_owned(), false),
        ]
    );
    assert_eq!(
        paired[2],
        vec![
            ("+q ".to_owned(), false),
            ("a ".to_owned(), false),
            ("b".to_owned(), false),
        ]
    );
}
