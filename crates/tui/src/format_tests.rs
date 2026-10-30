//! Tests for a card's figures.

use serde_json::json;

use ratatui::style::Modifier;
use ratatui::text::Line;

use super::{
    Kind, Kinds, content_text, count, cut, duration, heading, headline, kind, money, seconds,
    spend_against_budget, summary, tokens, warning_line, wrap, wrap_joined,
};
use crate::theme::{Role, style};

fn text(kinds: &Kinds) -> String {
    Line::from(kinds.summary()).to_string()
}

#[test]
fn a_protected_warning_line_indents_and_warns() {
    let text = "Protected path .fiber changed while a confined call ran; Fiber restored it.";
    let line = Line::from(warning_line(text));
    assert_eq!(line.to_string(), format!("    {text}"));
    assert_eq!(line.spans[1].style, style(Role::Warning));
}

#[test]
fn shell_kinds_follow_the_command_first_word() {
    let command = |command: &str| json!({"command": command});
    assert!(matches!(
        kind("shell", &command("rg -n foo a")),
        Kind::Search
    ));
    assert!(matches!(
        kind("shell", &command("grep -n foo a")),
        Kind::Search
    ));
    assert!(matches!(
        kind("shell", &command("find . -name x")),
        Kind::List
    ));
    assert!(matches!(kind("shell", &command("ls -la")), Kind::List));
    assert!(matches!(kind("shell", &command("fd foo")), Kind::List));
    assert!(matches!(kind("shell", &command("cargo test")), Kind::Shell));
    assert!(matches!(kind("shell", &json!({})), Kind::Shell));
    assert!(matches!(kind("read", &json!({})), Kind::Read));
    assert!(matches!(kind("edit", &json!({})), Kind::Edit));
    assert!(matches!(kind("write", &json!({})), Kind::Write));
    assert!(matches!(kind("delegate_spawn", &json!({})), Kind::Delegate));
    assert!(matches!(kind("delegate_fork", &json!({})), Kind::Delegate));
    assert!(matches!(kind("jobs", &json!({})), Kind::Jobs));
    assert!(matches!(kind("mystery", &json!({})), Kind::Other));
    // `ask_user` with no questions is no question asked.
    assert!(matches!(
        kind("ask_user", &json!({"questions": []})),
        Kind::Other
    ));
    assert!(matches!(
        kind("ask_user", &json!({"questions": [{}, {}]})),
        Kind::Ask(2)
    ));
}

#[test]
fn headline_of_a_failed_call_is_code_and_message() {
    use contract::events::{CallStatus, ToolCallCompleted};
    use contract::shapes::Failure;
    let done = ToolCallCompleted {
        status: CallStatus::Failed,
        reason: None,
        error: Some(Failure {
            code: contract::ErrorCode::AmbiguousMatch,
            message: "boom".to_owned(),
            retry_after_ms: None,
            provider: None,
        }),
        process: None,
        content: vec![],
        details: None,
        artifact: None,
        kept: None,
        changes: None,
        control: None,
        changed_by: None,
        protected_paths: None,
        hosts_surfaced: Vec::new(),
        provider_item: None,
        loaded: None,
    };
    assert_eq!(headline(&done), "ambiguous_match: boom");
}

#[test]
fn headline_of_a_denied_call_is_its_reason() {
    use contract::events::{CallStatus, ToolCallCompleted};
    let done = ToolCallCompleted {
        status: CallStatus::Denied,
        reason: Some("not allowed".to_owned()),
        error: None,
        process: None,
        content: vec![],
        details: None,
        artifact: None,
        kept: None,
        changes: None,
        control: None,
        changed_by: None,
        protected_paths: None,
        hosts_surfaced: Vec::new(),
        provider_item: None,
        loaded: None,
    };
    assert_eq!(headline(&done), "not allowed");
}

#[test]
fn headline_is_empty_for_a_completed_call() {
    use contract::events::{CallStatus, ToolCallCompleted};
    let done = ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![],
        details: None,
        artifact: None,
        kept: None,
        changes: None,
        control: None,
        changed_by: None,
        protected_paths: None,
        hosts_surfaced: Vec::new(),
        provider_item: None,
        loaded: None,
    };
    assert_eq!(headline(&done), "");
}

#[test]
fn content_text_joins_text_parts_and_skips_images() {
    use contract::events::{CallStatus, ToolCallCompleted};
    use contract::shapes::ContentPart;
    let done = ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![
            ContentPart::Text {
                text: "a".to_owned(),
            },
            ContentPart::Image {
                path: "shot.png".to_owned(),
                mime_type: "image/png".to_owned(),
                width: 1,
                height: 1,
            },
            ContentPart::Text {
                text: "b".to_owned(),
            },
            ContentPart::Unknown,
        ],
        details: None,
        artifact: None,
        kept: None,
        changes: None,
        control: None,
        changed_by: None,
        protected_paths: None,
        hosts_surfaced: Vec::new(),
        provider_item: None,
        loaded: None,
    };
    assert_eq!(content_text(&done), "a\nb");
}

#[test]
fn durations_truncate_to_whole_seconds_at_each_threshold() {
    assert_eq!(duration(0), "0s");
    assert_eq!(duration(59_500), "59s");
    assert_eq!(duration(59_999), "59s");
    assert_eq!(duration(60_000), "1m 00s");
    assert_eq!(duration(245_000), "4m 05s");
    assert_eq!(duration(3_599_999), "59m 59s");
    assert_eq!(duration(3_600_000), "1h 00m");
    assert_eq!(duration(3_720_000), "1h 02m");
    assert_eq!(duration(u64::MAX), "5124095576030h 25m");
}

#[test]
fn tokens_are_exact_under_a_thousand_then_one_decimal_rounded_half_up() {
    assert_eq!(tokens(1), "1 token");
    assert_eq!(tokens(812), "812 tokens");
    assert_eq!(tokens(999), "999 tokens");
    assert_eq!(tokens(1000), "1.0k tokens");
    assert_eq!(tokens(1049), "1.0k tokens");
    assert_eq!(tokens(1050), "1.1k tokens");
    assert_eq!(tokens(18_249), "18.2k tokens");
    assert_eq!(tokens(999_949), "999.9k tokens");
    assert_eq!(tokens(999_950), "1.0M tokens");
    assert_eq!(tokens(1_000_000), "1.0M tokens");
    assert_eq!(tokens(1_249_999), "1.2M tokens");
    assert_eq!(tokens(1_250_000), "1.3M tokens");
    assert_eq!(tokens(u64::MAX), "18446744073709.5M tokens");
}

#[test]
fn money_has_two_decimals_and_a_floor_for_tiny_amounts() {
    assert_eq!(money(0.41), "$0.41");
    assert_eq!(money(1.1), "$1.10");
    assert_eq!(money(0.004), "<$0.01");
    assert_eq!(money(0.005), "$0.01");
    assert_eq!(money(0.0), "$0.00");
    assert_eq!(money(12.345_67), "$12.35");
}

#[test]
fn spend_against_a_budget_reads_billed_cost_and_unknown() {
    // A set budget draws billed spend against it, `$0.00` included; a
    // still-null cost reads "unknown" (`docs/loop.md`, "Spending
    // budget"). With no budget there is no comparison to draw.
    assert_eq!(
        spend_against_budget(Some(1.25), Some(5.0)),
        Some("$1.25 of $5.00".to_owned())
    );
    assert_eq!(
        spend_against_budget(Some(0.0), Some(5.0)),
        Some("$0.00 of $5.00".to_owned())
    );
    assert_eq!(
        spend_against_budget(None, Some(5.0)),
        Some("unknown of $5.00".to_owned())
    );
    assert_eq!(spend_against_budget(Some(1.25), None), None);
    assert_eq!(spend_against_budget(None, None), None);
}

#[test]
fn one_takes_the_singular() {
    assert_eq!(count(0, "call", "calls"), "0 calls");
    assert_eq!(count(1, "call", "calls"), "1 call");
    assert_eq!(count(2, "call", "calls"), "2 calls");
}

#[test]
fn every_kind_in_order_capitalised() {
    let kinds = Kinds {
        read: 34,
        searched: 84,
        listed: 12,
        edited: 24,
        added: 175,
        removed: 83,
        wrote: 5,
        ran: 9,
        delegates: 2,
        jobs: 3,
        asked: 4,
        called: 3,
        thoughts: 4,
    };
    assert_eq!(
        text(&kinds),
        "Read 34 files, searched 84 patterns, listed 12 directories, edited 24 files +175 −83, \
         wrote 5 files, ran 9 commands, started 2 delegates, checked 3 jobs, asked 4 questions, \
         called 3 tools, thought 4 times"
    );
}

#[test]
fn every_kind_singular() {
    let kinds = Kinds {
        read: 1,
        searched: 1,
        listed: 1,
        edited: 1,
        added: 3,
        removed: 1,
        wrote: 1,
        ran: 1,
        delegates: 1,
        jobs: 1,
        asked: 1,
        called: 1,
        thoughts: 1,
    };
    assert_eq!(
        text(&kinds),
        "Read 1 file, searched 1 pattern, listed 1 directory, edited 1 file +3 −1, wrote 1 file, \
         ran 1 command, started 1 delegate, checked 1 job, asked 1 question, called 1 tool, \
         thought once"
    );
}

#[test]
fn kinds_with_no_calls_are_left_out() {
    let only = |kinds: Kinds| text(&kinds);
    assert_eq!(only(Kinds::default()), "");
    assert_eq!(
        only(Kinds {
            searched: 2,
            ..Kinds::default()
        }),
        "Searched 2 patterns"
    );
    assert_eq!(
        only(Kinds {
            listed: 2,
            ..Kinds::default()
        }),
        "Listed 2 directories"
    );
    assert_eq!(
        only(Kinds {
            edited: 2,
            ..Kinds::default()
        }),
        "Edited 2 files"
    );
    assert_eq!(
        only(Kinds {
            edited: 1,
            removed: 4,
            ..Kinds::default()
        }),
        "Edited 1 file +0 −4"
    );
    assert_eq!(
        only(Kinds {
            edited: 1,
            added: 4,
            ..Kinds::default()
        }),
        "Edited 1 file +4 −0"
    );
    assert_eq!(
        only(Kinds {
            wrote: 2,
            ..Kinds::default()
        }),
        "Wrote 2 files"
    );
    assert_eq!(
        only(Kinds {
            ran: 2,
            ..Kinds::default()
        }),
        "Ran 2 commands"
    );
    assert_eq!(
        only(Kinds {
            delegates: 2,
            ..Kinds::default()
        }),
        "Started 2 delegates"
    );
    assert_eq!(
        only(Kinds {
            jobs: 2,
            ..Kinds::default()
        }),
        "Checked 2 jobs"
    );
    assert_eq!(
        only(Kinds {
            asked: 2,
            ..Kinds::default()
        }),
        "Asked 2 questions"
    );
    assert_eq!(
        only(Kinds {
            called: 2,
            ..Kinds::default()
        }),
        "Called 2 tools"
    );
    assert_eq!(
        only(Kinds {
            thoughts: 1,
            ..Kinds::default()
        }),
        "Thought once"
    );
    assert_eq!(
        only(Kinds {
            thoughts: 2,
            ..Kinds::default()
        }),
        "Thought 2 times"
    );
    assert_eq!(
        only(Kinds {
            thoughts: 3,
            ..Kinds::default()
        }),
        "Thought 3 times"
    );
    // Totals of 0 and 0 are left out.
    assert_eq!(
        only(Kinds {
            edited: 1,
            ..Kinds::default()
        }),
        "Edited 1 file"
    );
}

#[test]
fn summary_counts_keep_their_added_and_removed_styles() {
    use crate::theme::Role;
    let kinds = Kinds {
        edited: 1,
        added: 2,
        removed: 1,
        ..Kinds::default()
    };
    let spans = kinds.summary();
    let plus = spans
        .iter()
        .find(|span| span.content == "+2")
        .expect("plus");
    let minus = spans
        .iter()
        .find(|span| span.content == "−1")
        .expect("minus");
    for span in [&plus, &minus] {
        assert!(span.style.add_modifier.contains(Modifier::DIM));
    }
    assert_eq!(plus.style.fg, crate::theme::style(Role::Added).fg);
    assert_eq!(minus.style.fg, crate::theme::style(Role::Removed).fg);
    // Every other span is dim.
    for span in &spans {
        if span.content == "+2" || span.content == "−1" {
            continue;
        }
        assert!(
            span.style.add_modifier.contains(Modifier::DIM),
            "{span:?} is dim"
        );
    }
}

#[test]
fn a_heading_is_a_markdown_heading_or_a_bold_line() {
    assert_eq!(
        heading("intro\n## Plan the fix\n**Check**", false).as_deref(),
        Some("Plan the fix")
    );
    assert_eq!(
        heading("intro\n**Check the tests**\n# Later", false).as_deref(),
        Some("Check the tests")
    );
    // Neither: the first non-empty line.
    assert_eq!(
        heading("\n  look at a.rs  \nthen b", false).as_deref(),
        Some("look at a.rs")
    );
    // A bold run that is not the whole line, and bare markers, are no
    // heading.
    assert_eq!(
        heading("**a** b\n####\n****", false).as_deref(),
        Some("**a** b")
    );
    assert_eq!(heading("", false), None);
    assert_eq!(heading("\n \n", false), None);
}

#[test]
fn the_latest_heading_is_the_last_one_so_far() {
    assert_eq!(
        heading("# One\ntext\n**Two**\nmore", true).as_deref(),
        Some("Two")
    );
    assert_eq!(heading("just text", true).as_deref(), Some("just text"));
    assert_eq!(heading("", true), None);
}

#[test]
fn wrapping_breaks_at_spaces_and_inside_long_words() {
    assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
    assert_eq!(wrap("one two", 7), vec!["one two"]);
    // The space between two words counts toward the row.
    assert_eq!(wrap("ab cd", 5), vec!["ab cd"]);
    assert_eq!(wrap("ab cd", 4), vec!["ab", "cd"]);
    assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    assert_eq!(wrap("a\nb", 10), vec!["a", "b"]);
    // A wide character takes two columns.
    assert_eq!(wrap("世界世界", 4), vec!["世界", "世界"]);
    assert_eq!(wrap("x", 0), vec!["x"]);
}

#[test]
fn a_span_under_a_second_is_left_out() {
    assert_eq!(seconds(999), None);
    assert_eq!(seconds(1000).as_deref(), Some("1s"));
}

#[test]
fn cutting_keeps_what_fits_in_columns() {
    assert_eq!(cut("abcdef", 4), "abcd");
    assert_eq!(cut("abc", 3), "abc");
    assert_eq!(cut("abc", 0), "");
    // A wide character that would cross the edge is left out.
    assert_eq!(cut("a界b", 2), "a");
    assert_eq!(cut("a界b", 3), "a界");
}

#[test]
fn cutting_keeps_graphemes_whole() {
    // A ZWJ emoji is one grapheme two columns wide: a width landing
    // mid-grapheme keeps the whole grapheme when it fits and drops it
    // when it does not, never splitting it.
    assert_eq!(cut("a👩\u{200d}💻b", 3), "a👩\u{200d}💻");
    assert_eq!(cut("a👩\u{200d}💻b", 2), "a");
    // A flag pair is one grapheme: a lone regional indicator never
    // stands alone.
    assert_eq!(cut("🇫🇷x", 1), "");
    assert_eq!(cut("🇫🇷x", 2), "🇫🇷");
    // A base letter with a combining accent is one grapheme.
    assert_eq!(cut("e\u{301}x", 1), "e\u{301}");
    assert_eq!(cut("e\u{301}x", 2), "e\u{301}x");
    // A piece past a cut that does not fit stays out, even when it is
    // zero columns wide.
    assert_eq!(cut("ab\u{301}", 1), "a");
    // Control characters keep their old one-char measure: a grapheme
    // walk alone would drop them.
    assert_eq!(cut("a\tb", 10), "a\tb");
    assert_eq!(cut("a\nb", 10), "a\nb");
}

#[test]
fn wrapping_keeps_graphemes_whole() {
    // A width landing mid-grapheme breaks around the grapheme.
    assert_eq!(wrap("a👩\u{200d}💻b", 3), vec!["a👩\u{200d}💻", "b"]);
    assert_eq!(wrap("🇫🇷x", 1), vec!["🇫🇷", "x"]);
    assert_eq!(wrap("e\u{301}x", 1), vec!["e\u{301}", "x"]);
    // A tab inside a word is kept, never dropped.
    assert_eq!(wrap("a\tb", 10), vec!["a\tb"]);
}

#[test]
fn wrapping_keeps_a_space_with_a_combining_mark_whole() {
    // `" \u{301}"` is one grapheme, not a word separator: the space
    // stays with its combining mark instead of being discarded.
    assert_eq!(wrap("a \u{301}b", 1), vec!["a", " \u{301}", "b"]);
    assert_eq!(wrap("a \u{301}b", 3), vec!["a \u{301}b"]);
}

#[test]
fn an_over_wide_grapheme_takes_its_own_row_or_is_dropped() {
    // A grapheme wider than `max` is emitted whole on its own row by
    // `wrap_joined` and dropped by `cut`.
    use crate::rows::Join::Break;
    assert_eq!(wrap_joined("界", 1), vec![("界".to_owned(), Break)]);
    assert_eq!(cut("a界b", 2), "a");
}

#[test]
fn wrap_joined_marks_word_and_character_breaks() {
    use crate::rows::Join::{Break, Wrap, WrapSpace};
    let cases = [
        (
            "a word break",
            "one two",
            4,
            vec![("one", Break), ("two", WrapSpace)],
        ),
        (
            "a long word",
            "abcdef",
            4,
            vec![("abcd", Break), ("ef", Wrap)],
        ),
        ("a newline", "a\nb", 10, vec![("a", Break), ("b", Break)]),
        (
            "an empty line",
            "a\n\nb",
            10,
            vec![("a", Break), ("", Break), ("b", Break)],
        ),
    ];
    for (name, text, max, expected) in cases {
        let expected: Vec<(String, crate::rows::Join)> = expected
            .into_iter()
            .map(|(row, join)| (row.to_owned(), join))
            .collect();
        assert_eq!(super::wrap_joined(text, max), expected, "{name}");
    }
}

#[test]
fn cutting_measures_each_kept_piece_and_at_most_the_first_rejected_one() {
    // One-piece examples: a kept piece and a rejected one each count one.
    crate::work::take();
    assert_eq!(cut("a", 1), "a");
    assert_eq!(crate::work::take().grapheme_widths, 1);
    crate::work::take();
    assert_eq!(cut("界", 1), "");
    assert_eq!(crate::work::take().grapheme_widths, 1);
    // Kept pieces plus the first rejected one, never the rest.
    crate::work::take();
    assert_eq!(cut("abcdef", 4), "abcd");
    assert_eq!(crate::work::take().grapheme_widths, 5);
    // Everything fits: no rejected piece to measure.
    crate::work::take();
    assert_eq!(cut("abc", 3), "abc");
    assert_eq!(crate::work::take().grapheme_widths, 3);
    // Nothing fits: only the first piece is ever measured.
    crate::work::take();
    assert_eq!(cut("abcdef", 0), "");
    assert_eq!(crate::work::take().grapheme_widths, 1);
    // A wide kept piece counts once, like any other.
    crate::work::take();
    assert_eq!(cut("a界b", 3), "a界");
    assert_eq!(crate::work::take().grapheme_widths, 3);
    // A zero-width piece still measures once and never forces a cut.
    crate::work::take();
    assert_eq!(cut("a\u{200b}b", 1), "a\u{200b}");
    assert_eq!(crate::work::take().grapheme_widths, 3);
}

#[test]
fn wrapping_measures_each_piece_once_at_any_length() {
    // One width measurement per drawn grapheme, whatever the row's
    // length: doubling the input doubles the count, nothing more.
    for len in [80usize, 160, 320] {
        let text = "a".repeat(len);
        crate::work::take();
        let rows = wrap(&text, 80);
        assert_eq!(crate::work::take().grapheme_widths, len);
        assert_eq!(rows.len(), len.div_ceil(80));
        assert!(rows.iter().all(|row| row.len() <= 80));
    }
    // The separator space is never sized: only the words' pieces are.
    crate::work::take();
    assert_eq!(wrap("ab cd", 5), vec!["ab cd"]);
    assert_eq!(crate::work::take().grapheme_widths, 4);
}

#[test]
fn cut_and_wrap_hold_their_edges() {
    // `cut`: a piece fits while what is taken plus its width stays at
    // or under the cap. `>=` would drop the last fitting piece.
    assert_eq!(cut("ab", 0), "");
    assert_eq!(cut("ab", 1), "a");
    assert_eq!(cut("ab", 2), "ab");
    // A wide grapheme at the edge: `>` keeps it when it lands on the cap.
    assert_eq!(cut("a界", 2), "a");
    assert_eq!(cut("a界", 3), "a界");
    // `wrap_joined`: the space before a word counts only after another
    // word. At cap − 1 the word wraps, at cap it joins with its space.
    assert_eq!(wrap("ab cd", 4), vec!["ab", "cd"]);
    assert_eq!(wrap("ab cd", 5), vec!["ab cd"]);
    assert_eq!(wrap("ab cd", 6), vec!["ab cd"]);
    // Inside a word the same edge breaks only past the cap.
    assert_eq!(wrap("abcde", 4), vec!["abcd", "e"]);
    assert_eq!(wrap("abcdef", 4), vec!["abcd", "ef"]);
    assert_eq!(wrap("abcdefg", 4), vec!["abcd", "efg"]);
    // A too-wide first piece still takes its own row, never nothing.
    assert_eq!(wrap("界x", 1), vec!["界", "x"]);
}

#[test]
fn wrapping_keeps_empty_lines_and_spacing_whole() {
    assert_eq!(wrap("", 10), vec![""]);
    assert_eq!(wrap("a\n\nb", 10), vec!["a", "", "b"]);
    // Repeated and trailing spaces survive as drawn rows.
    assert_eq!(wrap("a  b", 10), vec!["a  b"]);
    assert_eq!(wrap("a ", 10), vec!["a "]);
    assert_eq!(wrap("  ", 10), vec![""]);
    // Controls stay one piece each, measured as drawn.
    assert_eq!(wrap("a\tb", 10), vec!["a\tb"]);
    crate::work::take();
    assert_eq!(wrap("a\tb", 10), vec!["a\tb"]);
    assert_eq!(crate::work::take().grapheme_widths, 3);
}

#[test]
fn ask_user_counts_its_questions_when_it_has_any() {
    let questions = |arguments| match kind("ask_user", &arguments) {
        Kind::Ask(n) => Some(n),
        Kind::Read
        | Kind::Search
        | Kind::List
        | Kind::Edit
        | Kind::Write
        | Kind::Shell
        | Kind::Delegate
        | Kind::Jobs
        | Kind::Other => None,
    };
    assert_eq!(questions(json!({"questions": [{}]})), Some(1));
    assert_eq!(questions(json!({"questions": [{}, {}, {}]})), Some(3));
    assert_eq!(questions(json!({"questions": []})), None);
    assert_eq!(questions(json!({"questions": "x"})), None);
    assert_eq!(questions(json!({})), None);
    assert!(matches!(
        kind("ask_user", &json!({"questions": []})),
        Kind::Other
    ));
    assert!(matches!(
        kind("other", &json!({"questions": [{}]})),
        Kind::Other
    ));
}

#[test]
fn summaries_show_parsed_arguments_never_json() {
    let questions = |n: usize| {
        (0..n)
            .map(|i| json!({"header": format!("H{i}"), "question": format!("q{i}?")}))
            .collect::<Vec<_>>()
    };
    let cases = [
        // File tools and the shell keep their path and command.
        ("read", json!({"path": "a.rs"}), "a.rs"),
        ("edit", json!({"path": "a.rs"}), "a.rs"),
        ("write", json!({"path": "n.rs"}), "n.rs"),
        ("shell", json!({"command": "cargo test"}), "cargo test"),
        // `ask_user` counts its questions: one is singular.
        ("ask_user", json!({"questions": questions(1)}), "1 question"),
        (
            "ask_user",
            json!({"questions": questions(2)}),
            "2 questions",
        ),
        (
            "ask_user",
            json!({"questions": questions(4)}),
            "4 questions",
        ),
        // Other tools show their first string field, one line.
        ("search", json!({"pattern": "foo"}), "foo"),
        (
            "web_fetch",
            json!({"url": "https://example.com/x"}),
            "https://example.com/x",
        ),
        ("web_search", json!({"query": "rust"}), "rust"),
        ("search", json!({"query": "a\nb"}), "a b"),
        ("mystery", json!({"name": "x"}), "x"),
        // The first key in `query, url, pattern, ...` order wins, and
        // empty values are skipped.
        ("search", json!({"url": "u", "query": "q"}), "q"),
        ("search", json!({"query": "", "url": "u"}), "u"),
        ("search", json!({"query": "  ", "url": "u"}), "u"),
        // An array field reads as a short count; a nested object, an
        // empty object and unparsed raw text read as nothing.
        ("jobs", json!({"items": [1, 2, 3]}), "3 items"),
        ("jobs", json!({"items": []}), ""),
        ("mystery", json!({"filter": {"a": 1}}), ""),
        ("mystery", json!({}), ""),
    ];
    for (name, arguments, expected) in &cases {
        let found = summary(name, arguments);
        assert_eq!(found, *expected, "{name} {arguments}");
        assert!(!found.contains('{'), "{name} {arguments}");
    }
    // Raw text that was not JSON shows nothing: the running line already
    // holds it until the call is requested.
    let raw = summary("ask_user", &serde_json::Value::String("{\"q".to_owned()));
    assert_eq!(raw, "");
}

#[test]
fn kinds_name_the_ledgers_kind_word() {
    let word = |name: &str, arguments: serde_json::Value| kind(name, &arguments).word();
    assert_eq!(word("read", json!({})), "read");
    assert_eq!(word("shell", json!({"command": "rg x"})), "search");
    assert_eq!(word("shell", json!({"command": "fd x"})), "list");
    assert_eq!(word("edit", json!({})), "edit");
    assert_eq!(word("write", json!({})), "write");
    assert_eq!(word("shell", json!({"command": "cargo t"})), "shell");
    assert_eq!(word("delegate_spawn", json!({})), "delegate");
    assert_eq!(word("delegate_fork", json!({})), "delegate");
    assert_eq!(word("jobs", json!({})), "jobs");
    assert_eq!(word("ask_user", json!({"questions": [{}]})), "ask");
    assert_eq!(word("mystery", json!({})), "other");
    // Every word fits the nine-column kind cell.
    for name in [
        "read",
        "shell",
        "edit",
        "write",
        "delegate_spawn",
        "jobs",
        "ask_user",
        "mystery",
    ] {
        assert!(word(name, json!({"command": "cargo t", "questions": [{}]})).len() <= 9);
    }
}

#[test]
fn opened_notes_draw_every_line_uncapped() {
    use super::opened;
    use crate::rows::Rows;
    let text = (1..=30)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = Rows::default();
    opened(&text, "  ", &mut out);
    let (rows, _) = out.into_parts();
    assert_eq!(rows.len(), 30);
    assert_eq!(rows[0].0.to_string(), "  line 1");
    assert_eq!(rows[29].0.to_string(), "  line 30");
}

#[test]
fn answer_parts_pads_the_header_to_13_cells_with_one_space_least() {
    use super::answer_parts;
    // 4 cells: 9 spaces pad to 13.
    let parts = answer_parts("Base", None);
    assert_eq!(parts.header, "Base         ");
    assert_eq!(parts.text, "skipped");
    assert!(parts.skipped);
    // 12 cells: 1 space. 13 cells: still 1 space. 14 cells: still 1 space.
    assert_eq!(answer_parts("123456789012", None).header, "123456789012 ");
    assert_eq!(answer_parts("1234567890123", None).header, "1234567890123 ");
    assert_eq!(
        answer_parts("12345678901234", None).header,
        "12345678901234 "
    );
    // A wide header counts cells, not bytes: 2 cells each.
    assert_eq!(answer_parts("名前", None).header, "名前         ");
    // The padded header's cells are 13, or the header's cells plus one.
    for (header, cells) in [("Base", 13), ("1234567890123", 14), ("12345678901234", 15)] {
        assert_eq!(
            super::width(&answer_parts(header, None).header),
            cells,
            "{header}"
        );
    }
}

#[test]
fn answer_parts_carries_labels_quoted_words_or_skipped() {
    use super::answer_parts;
    let labels = ["main (Recommended)".to_owned(), "dev".to_owned()];
    let parts = answer_parts("Base", Some((&labels, Some("and tags"))));
    assert_eq!(parts.header, "Base         ");
    assert_eq!(parts.text, r#"main (Recommended), dev, "and tags""#);
    assert!(!parts.skipped);
    let parts = answer_parts("Name", Some((&[], Some("fiber-cli"))));
    assert_eq!(parts.text, r#""fiber-cli""#);
    assert!(!parts.skipped);
    let parts = answer_parts("Name", Some((&labels, None)));
    assert_eq!(parts.text, "main (Recommended), dev");
    assert!(!parts.skipped);
    // Escapes without quotes, one line.
    let labels = ["a \"b\"".to_owned()];
    let parts = answer_parts("Pick\nnow", Some((&labels, Some("x\ny"))));
    assert_eq!(parts.header, "Pick\\nnow    ");
    assert_eq!(parts.text, r#"a \"b\", "x\ny""#);
    assert!(!parts.skipped);
}

/// A failure's rows through a seam drawing `fake`.
fn failed_seamed(
    error: &contract::shapes::Failure,
    width: u16,
    fake: &std::rc::Rc<crate::ext::tests::SlotRenderer>,
    flat: bool,
) -> Vec<crate::turn::Row> {
    let mut out =
        crate::rows::Rows::default().with_seam(crate::ext::tests::seam(fake, "s", 0, flat));
    super::failure(error, width, &mut out);
    out.into_parts().0
}

fn failed(code: contract::ErrorCode) -> contract::shapes::Failure {
    contract::shapes::Failure {
        code,
        message: "sign in".to_owned(),
        retry_after_ms: None,
        provider: Some(contract::shapes::ProviderFailure {
            name: "fake".to_owned(),
            status: Some(401),
            message: "bad key".to_owned(),
        }),
    }
}

fn replaced() -> std::rc::Rc<crate::ext::tests::SlotRenderer> {
    use crate::ext::tests::{SlotRenderer, SlotScript, raw};
    std::rc::Rc::new(SlotRenderer::filling(
        &["error_line"],
        SlotScript::Replace(vec![vec![raw("custom")]]),
    ))
}

#[test]
fn an_error_line_replacement_draws_instead_of_the_failure() {
    let error = failed(contract::ErrorCode::IoFailed);
    let fake = replaced();
    let rows = failed_seamed(&error, 40, &fake, false);
    // (`docs/tui.md`, "When an extension fails": the failure is cached against its input.)
    assert_eq!(fake.calls.borrow().len(), 1);
    assert_eq!(fake.calls.borrow()[0].1, "error_line");
    let shown: Vec<String> = rows.iter().map(|(line, _)| line.to_string()).collect();
    assert!(
        shown.iter().any(|row| row.starts_with("custom")),
        "{shown:?}"
    );
    assert!(!shown.iter().any(|row| row.contains('✗')), "{shown:?}");
    assert!(
        !shown.iter().any(|row| row.contains("bad key")),
        "{shown:?}"
    );
}

#[test]
fn a_login_replacement_keeps_the_login_target_on_every_line() {
    use crate::ext::tests::{SlotRenderer, SlotScript, raw};
    let error = failed(contract::ErrorCode::AuthenticationFailed);
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["error_line"],
        SlotScript::Replace(vec![vec![raw("one")], vec![raw("two")]]),
    ));
    let rows = failed_seamed(&error, 40, &fake, false);
    // (`docs/tui.md`, "The slots", `error_line`: the failed-login target stays on each line.)
    assert!(!rows.is_empty());
    for (line, target) in &rows {
        assert_eq!(*target, Some(crate::app::Target::Login), "{line:?}");
    }
}

#[test]
fn a_non_login_replacement_carries_no_target() {
    let error = failed(contract::ErrorCode::IoFailed);
    let fake = replaced();
    let rows = failed_seamed(&error, 40, &fake, false);
    // (`docs/tui.md`, "The slots", `error_line`: only the failed-login row carries a target.)
    let shown: Vec<String> = rows.iter().map(|(line, _)| line.to_string()).collect();
    assert!(
        shown.iter().any(|row| row.starts_with("custom")),
        "{shown:?}"
    );
    for (line, target) in &rows {
        assert_eq!(*target, None, "{line:?}");
    }
}

#[test]
fn the_login_target_sits_on_the_built_in_failure_line_only() {
    // (`docs/tui.md`, "The slots", `error_line`: the built-in draws the
    // ✗ line with the login target and the provider's words without one.)
    let error = failed(contract::ErrorCode::AuthenticationFailed);
    let fake = std::rc::Rc::new(crate::ext::tests::SlotRenderer::filling(
        &[],
        crate::ext::tests::SlotScript::default(),
    ));
    let rows = failed_seamed(&error, 40, &fake, false);
    let shown: Vec<String> = rows.iter().map(|(line, _)| line.to_string()).collect();
    assert!(
        shown.iter().any(|row| row.starts_with("✗ sign in")),
        "{shown:?}"
    );
    assert!(shown.iter().any(|row| row.contains("bad key")), "{shown:?}");
    let login_lines = rows
        .iter()
        .filter(|(_, target)| *target == Some(crate::app::Target::Login))
        .count();
    assert_eq!(login_lines, 1, "{shown:?}");
}

#[test]
fn an_error_line_decorator_draws_the_built_in_failure_under_its_badge() {
    use crate::ext::tests::{SlotRenderer, SlotScript, raw};
    let error = failed(contract::ErrorCode::IoFailed);
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["error_line"],
        SlotScript::Decorate {
            badge: vec![vec![raw("BADGE")]],
            trim: 5,
        },
    ));
    let rows = failed_seamed(&error, 40, &fake, false);
    assert_eq!(fake.builtins.borrow().as_slice(), &[35]);
    let shown: Vec<String> = rows.iter().map(|(line, _)| line.to_string()).collect();
    assert!(
        shown.iter().any(|row| row.starts_with("BADGE")),
        "{shown:?}"
    );
    assert!(shown.iter().any(|row| row.contains('✗')), "{shown:?}");
}

#[test]
fn an_error_line_error_falls_back_with_one_failure() {
    use crate::ext::FailureKind;
    use crate::ext::tests::{SlotRenderer, SlotScript};
    let error = failed(contract::ErrorCode::IoFailed);
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["error_line"],
        SlotScript::Fail {
            kind: FailureKind::HookFailed,
            message: "boom".into(),
        },
    ));
    let bound = crate::ext::tests::seam(&fake, "s", 0, false);
    for _ in 0..2 {
        let mut out = crate::rows::Rows::default().with_seam(bound.clone());
        super::failure(&error, 40, &mut out);
        let shown: Vec<String> = out
            .into_parts()
            .0
            .iter()
            .map(|(line, _)| line.to_string())
            .collect();
        assert!(shown.iter().any(|row| row.contains('✗')), "{shown:?}");
    }
    // (`docs/tui.md`, "When an extension fails": one notice names the extension and slot once until `/reload`.)
    assert_eq!(
        fake.calls.borrow().len(),
        1,
        "the failure calls nothing again"
    );
    let listed = bound.cache.take_failures();
    // (`docs/tui.md`, "When an extension fails": one notice names the extension and slot once until `/reload`.)
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, "error_line");
    assert_eq!(listed[0].1.kind, FailureKind::HookFailed);
}

#[test]
fn a_flat_error_line_replacement_drops_every_style() {
    use crate::ext::tests::{SlotRenderer, SlotScript};
    use ratatui::style::Style;
    let error = failed(contract::ErrorCode::IoFailed);
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["error_line"],
        SlotScript::Replace(vec![vec![crate::ext::RawSpan {
            text: "custom".into(),
            bold: true,
            fg: Some("accent".into()),
            ..crate::ext::RawSpan::default()
        }]]),
    ));
    let rows = failed_seamed(&error, 40, &fake, true);
    // (`docs/tui.md`, "How a TUI extension runs": in screen-reader mode the terminal drops every style.)
    let shown: Vec<String> = rows.iter().map(|(line, _)| line.to_string()).collect();
    assert!(shown.iter().any(|row| row.contains("custom")), "{shown:?}");
    for (line, _) in &rows {
        for span in &line.spans {
            assert_eq!(span.style, Style::default(), "{span:?}");
        }
    }
}
