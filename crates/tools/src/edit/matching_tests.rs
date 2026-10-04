use super::{Applied, Block, MatchError, apply, at, slice};

fn edited(text: &str, blocks: &[(&str, &str)]) -> Result<Applied, MatchError> {
    let owned: Vec<Block> = blocks
        .iter()
        .map(|(old, new)| Block {
            old_text: (*old).to_owned(),
            new_text: (*new).to_owned(),
        })
        .collect();
    apply(text, &owned)
}

fn must(text: &str, blocks: &[(&str, &str)]) -> Applied {
    edited(text, blocks).unwrap_or_else(|err| panic!("{}", err.message()))
}

fn report(
    index: usize,
    old_start: u64,
    old_end: u64,
    new_span: Option<(u64, u64)>,
    normalised: bool,
) -> super::Report {
    super::Report {
        index,
        old_start,
        old_end,
        new_span,
        normalised,
    }
}

#[test]
fn an_exact_match_replaces_only_the_matched_bytes() {
    let applied = must("hello world\n", &[("world", "there")]);
    assert_eq!(applied.bytes, b"hello there\n");
    assert_eq!(applied.reports, vec![report(0, 1, 1, Some((1, 1)), false)]);
}

#[test]
fn replacing_the_whole_file_leaves_nothing_outside_the_match() {
    let applied = must("abc", &[("abc", "Z")]);
    assert_eq!(applied.bytes, b"Z");
}

#[test]
fn an_old_text_found_twice_is_ambiguous() {
    let err = edited("aa\naa\n", &[("aa", "b")]).unwrap_err();
    assert_eq!(err, MatchError::Ambiguous { index: 0, count: 2 });
    assert_eq!(
        err.message(),
        "edits[0]: old_text matched 2 times. Read the file again and include more surrounding text."
    );
}

#[test]
fn three_matches_report_that_count() {
    let err = edited("aa aa aa", &[("aa", "b")]).unwrap_err();
    assert_eq!(err, MatchError::Ambiguous { index: 0, count: 3 });
    assert!(
        err.message().contains("matched 3 times"),
        "{}",
        err.message()
    );
}

#[test]
fn an_overlapping_needle_is_counted_from_each_match_end() {
    let applied = must("aaa", &[("aa", "X")]);
    assert_eq!(applied.bytes, b"Xa");
    let err = edited("aaaa", &[("aa", "X")]).unwrap_err();
    assert_eq!(err, MatchError::Ambiguous { index: 0, count: 2 });
}

#[test]
fn a_missing_old_text_is_no_match() {
    let err = edited("hello\n", &[("world", "x")]).unwrap_err();
    assert_eq!(err, MatchError::NoMatch { index: 0 });
    assert_eq!(
        err.message(),
        "edits[0]: old_text was not found. Read the file again and copy the text exactly."
    );
}

#[test]
fn no_match_names_the_block_that_missed() {
    let err = edited("aaa\n", &[("aaa", "b"), ("zzz", "c")]).unwrap_err();
    assert_eq!(err, MatchError::NoMatch { index: 1 });
    assert!(err.message().starts_with("edits[1]:"), "{}", err.message());
}

#[test]
fn a_crlf_file_matches_an_lf_old_text_and_is_written_back_as_crlf() {
    let applied = must("a\r\nb\r\n", &[("a\nb", "A\nB")]);
    assert_eq!(applied.bytes, b"A\r\nB\r\n");
    assert!(!applied.reports[0].normalised);
}

#[test]
fn crlf_inside_the_edit_text_is_a_line_ending() {
    let applied = must("a\nb\n", &[("a\r\nb", "A\r\nB")]);
    assert_eq!(applied.bytes, b"A\nB\n");
}

#[test]
fn untouched_lines_keep_their_original_endings() {
    let applied = must("keep\r\nchange\nkeep2\r", &[("change", "CHANGE")]);
    assert_eq!(applied.bytes, b"keep\r\nCHANGE\nkeep2\r");
}

#[test]
fn a_lone_cr_stays_ordinary_text() {
    let applied = must("a\rb\n", &[("b", "B")]);
    assert_eq!(applied.bytes, b"a\rB\n");
    let applied = must("a\rb\r\nc\n", &[("a\rb\nc", "Z")]);
    assert_eq!(applied.bytes, b"Z\n");
}

#[test]
fn a_byte_order_mark_is_set_aside_and_restored() {
    let applied = must("\u{feff}hello\n", &[("hello", "hi")]);
    assert_eq!(applied.bytes, "\u{feff}hi\n".as_bytes());
}

#[test]
fn an_exact_match_does_not_fold_the_rest_of_its_line() {
    let applied = must("foo \n", &[("foo", "bar")]);
    assert_eq!(applied.bytes, b"bar \n");
    assert!(!applied.reports[0].normalised);
}

#[test]
fn trailing_spaces_match_on_the_second_pass_and_only_those_lines_fold() {
    let applied = must("alpha  \nbeta\nkeep\t\n", &[("alpha\nbeta", "ALPHA\nBETA")]);
    assert_eq!(applied.bytes, b"ALPHA\nBETA\nkeep\t\n");
    assert_eq!(applied.reports, vec![report(0, 1, 2, Some((1, 2)), true)]);
}

#[test]
fn a_trailing_tab_is_ignored_the_same_way() {
    let applied = must("alpha\t\nbeta\n", &[("alpha\nbeta", "ALPHA\nBETA")]);
    assert_eq!(applied.bytes, b"ALPHA\nBETA\n");
    assert!(applied.reports[0].normalised);
}

#[test]
fn a_trailing_nbsp_is_ignored_like_a_trailing_space() {
    let applied = must("foo\u{00A0}\nbar\n", &[("foo\nbar", "FOO\nBAR")]);
    assert_eq!(applied.bytes, b"FOO\nBAR\n");
    assert!(applied.reports[0].normalised);
}

#[test]
fn each_folded_character_matches_its_ascii_form() {
    let folds = [
        ('\u{2018}', "'"),
        ('\u{2019}', "'"),
        ('\u{201A}', "'"),
        ('\u{201B}', "'"),
        ('\u{201C}', "\""),
        ('\u{201D}', "\""),
        ('\u{201E}', "\""),
        ('\u{201F}', "\""),
        ('\u{2010}', "-"),
        ('\u{2013}', "-"),
        ('\u{2015}', "-"),
        ('\u{2212}', "-"),
        ('\u{00A0}', " "),
        ('\u{2002}', " "),
        ('\u{2007}', " "),
        ('\u{200A}', " "),
        ('\u{202F}', " "),
        ('\u{205F}', " "),
        ('\u{3000}', " "),
    ];
    for (ch, ascii) in folds {
        let file = format!("x{ch}y");
        let old = format!("x{ascii}y");
        let applied = must(&file, &[(&old, "Z")]);
        assert_eq!(applied.bytes, b"Z", "{ch:?} should fold to {ascii:?}");
        assert!(applied.reports[0].normalised, "{ch:?}");
    }
}

#[test]
fn characters_just_outside_a_fold_range_do_not_fold() {
    let stays = [
        '\u{200F}', '\u{2016}', '\u{2211}', '\u{2213}', '\u{2001}', '\u{200B}', '\u{202E}',
        '\u{2030}', '\u{205E}', '\u{2060}', '\u{2FFF}', '\u{3001}',
    ];
    for ch in stays {
        let file = format!("x{ch}y");
        for ascii in ["-", " ", "'", "\""] {
            let old = format!("x{ascii}y");
            let err = edited(&file, &[(&old, "Z")]).unwrap_err();
            assert_eq!(
                err,
                MatchError::NoMatch { index: 0 },
                "{ch:?} must not fold to {ascii:?}"
            );
        }
    }
}

#[test]
fn an_em_dash_covers_all_three_bytes_and_the_next_line_keeps_its_ending() {
    let applied = must("\u{2014}\r\nKEEP\n", &[("-", "x")]);
    assert_eq!(applied.bytes, b"x\r\nKEEP\n");
    assert!(applied.reports[0].normalised);
}

#[test]
fn a_nbsp_covers_both_bytes() {
    let applied = must("A\u{00A0}B\r\nTAIL\n", &[("A B", "AB")]);
    assert_eq!(applied.bytes, b"AB\r\nTAIL\n");
}

#[test]
fn a_block_unique_in_the_fold_view_matches_once() {
    let applied = must("it\u{2019}s\n", &[("it's", "it is")]);
    assert_eq!(applied.bytes, b"it is\n");
    assert!(applied.reports[0].normalised);
}

#[test]
fn the_same_curly_quote_matches_exactly_and_is_not_normalised() {
    let applied = must("it\u{2019}s\n", &[("it\u{2019}s", "it is")]);
    assert_eq!(applied.bytes, b"it is\n");
    assert!(!applied.reports[0].normalised);
}

#[test]
fn two_folded_matches_are_ambiguous() {
    let err = edited("it\u{2019}s\nit\u{2019}s\n", &[("it's", "x")]).unwrap_err();
    assert_eq!(err, MatchError::Ambiguous { index: 0, count: 2 });
}

#[test]
fn an_exact_match_wins_when_the_fold_view_has_it_twice() {
    let applied = must("foo\nfoo \n", &[("foo\n", "bar\n")]);
    assert_eq!(applied.bytes, b"bar\nfoo \n");
    assert!(!applied.reports[0].normalised);
}

#[test]
fn a_pass_two_line_folds_the_text_outside_the_match() {
    let applied = must(
        "\u{201c}say\u{201d} it\u{2019}s\nNEXT\n",
        &[("it's", "it is")],
    );
    assert_eq!(
        String::from_utf8(applied.bytes).unwrap(),
        "\"say\" it is\nNEXT\n"
    );
}

#[test]
fn a_folded_match_keeps_the_folded_context_on_its_lines() {
    let applied = must("abc \ndef\nghi\n", &[("c\ndef\ng", "X")]);
    assert_eq!(applied.bytes, b"abXhi\n");
    assert!(applied.reports[0].normalised);
    assert_eq!(applied.reports[0].old_start, 1);
    assert_eq!(applied.reports[0].old_end, 3);
}

#[test]
fn overlapping_blocks_name_both_indexes() {
    let err = edited("abcd", &[("abc", "X"), ("cd", "Y")]).unwrap_err();
    assert_eq!(
        err,
        MatchError::Invalid("edits[0] and edits[1] overlap.".to_owned())
    );
}

#[test]
fn overlap_names_the_lower_index_first() {
    let err = edited("abcd", &[("a", "A"), ("bcd", "Y"), ("abc", "X")]).unwrap_err();
    assert_eq!(
        err,
        MatchError::Invalid("edits[0] and edits[2] overlap.".to_owned())
    );
}

#[test]
fn two_pass_two_blocks_on_the_same_line_overlap() {
    let err = edited(
        "\u{201c}a\u{201d} \u{201c}b\u{201d}\n",
        &[("\"a\"", "A"), ("\"b\"", "B")],
    )
    .unwrap_err();
    assert_eq!(
        err,
        MatchError::Invalid("edits[0] and edits[1] overlap.".to_owned())
    );
}

#[test]
fn adjacent_exact_blocks_are_both_applied() {
    let applied = must("abcd\n", &[("ab", "AB"), ("cd", "CD")]);
    assert_eq!(applied.bytes, b"ABCD\n");
}

#[test]
fn adjacent_pass_two_lines_do_not_overlap() {
    let applied = must(
        "\u{201c}a\u{201d}\n\u{201c}b\u{201d}\n",
        &[("\"a\"", "A"), ("\"b\"", "B")],
    );
    assert_eq!(String::from_utf8(applied.bytes).unwrap(), "A\nB\n");
}

#[test]
fn an_empty_old_text_is_invalid() {
    let err = edited("abc", &[("", "x")]).unwrap_err();
    assert_eq!(
        err,
        MatchError::Invalid("edits[0]: old_text is empty.".to_owned())
    );
}

#[test]
fn an_empty_old_text_names_its_index() {
    let err = edited("abc", &[("a", "A"), ("", "x")]).unwrap_err();
    assert_eq!(
        err,
        MatchError::Invalid("edits[1]: old_text is empty.".to_owned())
    );
}

#[test]
fn an_old_text_that_folds_away_is_not_found() {
    let err = edited("abc\n", &[("  ", "x")]).unwrap_err();
    assert_eq!(err, MatchError::NoMatch { index: 0 });
}

#[test]
fn no_blocks_is_invalid() {
    let err = apply("abc", &[]).unwrap_err();
    assert_eq!(
        err,
        MatchError::Invalid("`edits` must contain at least one block.".to_owned())
    );
}

#[test]
fn an_edit_that_changes_nothing_is_invalid() {
    let err = edited("abc\n", &[("abc", "abc")]).unwrap_err();
    assert_eq!(
        err,
        MatchError::Invalid("The edits leave the file unchanged.".to_owned())
    );
}

#[test]
fn a_crlf_edit_that_only_restates_the_file_is_invalid() {
    let err = edited("a\r\nb\r\n", &[("a\nb", "a\r\nb")]).unwrap_err();
    assert_eq!(
        err,
        MatchError::Invalid("The edits leave the file unchanged.".to_owned())
    );
}

#[test]
fn blocks_are_matched_against_the_original_in_any_order() {
    let applied = must("aaa\nbbb\nccc\n", &[("ccc", "CCC"), ("aaa", "AAA")]);
    assert_eq!(applied.bytes, b"AAA\nbbb\nCCC\n");
}

#[test]
fn a_later_block_does_not_see_an_earlier_blocks_new_text() {
    let err = edited("aaa\n", &[("aaa", "bbb"), ("bbb", "ccc")]).unwrap_err();
    assert_eq!(err, MatchError::NoMatch { index: 1 });
}

#[test]
fn replacing_a_whole_line_reports_that_line_only() {
    let applied = must("a\nb\nc\n", &[("b\n", "B\n")]);
    assert_eq!(applied.bytes, b"a\nB\nc\n");
    assert_eq!(applied.reports, vec![report(0, 2, 2, Some((2, 2)), false)]);
}

#[test]
fn a_match_on_a_last_line_without_a_newline_stays_on_that_line() {
    let applied = must("a\nb", &[("b", "B")]);
    assert_eq!(applied.bytes, b"a\nB");
    assert_eq!(applied.reports, vec![report(0, 2, 2, Some((2, 2)), false)]);
}

#[test]
fn deleting_a_whole_line_occupies_no_lines() {
    let applied = must("a\nb\nc\n", &[("b\n", "")]);
    assert_eq!(applied.bytes, b"a\nc\n");
    assert_eq!(applied.reports, vec![report(0, 2, 2, None, false)]);
}

#[test]
fn a_growing_block_shifts_the_lines_after_it() {
    let applied = must("a\nb\nc\n", &[("a\n", "a\nA\n"), ("c", "C")]);
    assert_eq!(applied.bytes, b"a\nA\nb\nC\n");
    assert_eq!(
        applied.reports,
        vec![
            report(0, 1, 1, Some((1, 2)), false),
            report(1, 3, 3, Some((4, 4)), false),
        ]
    );
}

#[test]
fn a_block_that_touches_the_next_still_shifts_it() {
    let applied = must("ab\ncd\n", &[("ab\n", "ab\nAB\n"), ("cd", "CD")]);
    assert_eq!(applied.bytes, b"ab\nAB\nCD\n");
    assert_eq!(applied.reports[1].old_start, 2);
    assert_eq!(applied.reports[1].new_span, Some((3, 3)));
}

#[test]
fn a_shrinking_block_shifts_the_lines_after_it() {
    let applied = must("a\nb\nc\n", &[("a\n", ""), ("c", "C")]);
    assert_eq!(applied.bytes, b"b\nC\n");
    assert_eq!(
        applied.reports,
        vec![
            report(0, 1, 1, None, false),
            report(1, 3, 3, Some((2, 2)), false),
        ]
    );
}

#[test]
fn a_growing_and_a_shrinking_block_both_shift_a_later_one() {
    let applied = must(
        "one\ntwo\nthree\nfour\nfive\n",
        &[("two\n", "two\nTWO\n"), ("four\n", ""), ("five", "FIVE")],
    );
    assert_eq!(applied.bytes, b"one\ntwo\nTWO\nthree\nFIVE\n");
    assert_eq!(
        applied.reports,
        vec![
            report(0, 2, 2, Some((2, 3)), false),
            report(1, 4, 4, None, false),
            report(2, 5, 5, Some((5, 5)), false),
        ]
    );
}

#[test]
fn line_numbers_follow_the_written_file_when_blocks_are_out_of_order() {
    let applied = must("aaa\nbbb\nccc\n", &[("ccc", "CCC\nX"), ("aaa", "AAA\nY")]);
    assert_eq!(applied.bytes, b"AAA\nY\nbbb\nCCC\nX\n");
    assert_eq!(
        applied.reports,
        vec![
            report(0, 3, 3, Some((4, 5)), false),
            report(1, 1, 1, Some((1, 2)), false),
        ]
    );
}

#[test]
fn a_later_block_does_not_shift_an_earlier_line() {
    let applied = must("aaa\nbbb\n", &[("bbb", "BBB\nB"), ("aaa", "AAA")]);
    assert_eq!(applied.bytes, b"AAA\nBBB\nB\n");
    assert_eq!(applied.reports[1].new_span, Some((1, 1)));
    assert_eq!(applied.reports[0].new_span, Some((2, 3)));
}

#[test]
fn an_origin_miss_is_zero_and_a_hit_is_the_entry() {
    assert_eq!(at(&[], 0), 0);
    assert_eq!(at(&[4], 1), 0);
    assert_eq!(at(&[4, 9], 0), 4);
    assert_eq!(at(&[4, 9], 1), 9);
}

#[test]
fn a_slice_is_the_range_and_empty_past_the_end() {
    assert_eq!(slice("ab", 0, 1), "a");
    assert_eq!(slice("ab", 0, 2), "ab");
    assert_eq!(slice("ab", 2, 2), "");
    assert_eq!(slice("ab", 3, 4), "");
}
