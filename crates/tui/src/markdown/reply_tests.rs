//! Tests for a reply's shared render slot: clones share one slot until
//! either changes its text.

use super::super::wrap::visit_wrap_ranges;
use super::Reply;

const TABLE: &str = "| a | b |\n|---|---|\n| c | d |\n";

#[test]
fn wrapping_keeps_the_empty_row_before_a_word_that_fits_the_rest_width() {
    use crate::rows::Join;
    use ratatui::style::Style;

    let cells: Vec<_> = "   abc".chars().map(|ch| (ch, Style::default())).collect();
    let widths = vec![1; cells.len()];
    let mut rows = Vec::new();
    visit_wrap_ranges(&cells, &widths, 2, 5, true, |range, join| {
        rows.push((range, join));
    });

    assert_eq!(rows, [(0..0, Join::Break), (3..6, Join::WrapSpace)]);
}

#[test]
fn rendering_a_clone_fills_the_originals_slot() {
    let reply = Reply::new("hello".to_owned(), 1);
    let clone = reply.clone();
    assert_eq!(reply.cached_at(), None);
    clone.rendered(60);
    assert_eq!(reply.cached_at(), Some(60));
    assert_eq!(clone.cached_at(), Some(60));
}

#[test]
fn a_clones_push_leaves_the_originals_text() {
    let reply = Reply::new("hello".to_owned(), 1);
    let mut clone = reply.clone();
    clone.push(" there");
    assert_eq!(
        clone.rendered(60),
        Reply::new("hello there".to_owned(), 2).rendered(60)
    );
    assert_eq!(
        reply.rendered(60),
        Reply::new("hello".to_owned(), 3).rendered(60)
    );
}

#[test]
fn a_clones_set_leaves_the_originals_text() {
    let reply = Reply::new("hello".to_owned(), 1);
    let mut clone = reply.clone();
    clone.set("goodbye".to_owned());
    assert_eq!(
        clone.rendered(60),
        Reply::new("goodbye".to_owned(), 2).rendered(60)
    );
    assert_eq!(
        reply.rendered(60),
        Reply::new("hello".to_owned(), 3).rendered(60)
    );
}

#[test]
fn a_new_width_renders_again_for_both_clones() {
    let reply = Reply::new("hello".to_owned(), 1);
    let clone = reply.clone();
    reply.rendered(60);
    clone.rendered(80);
    assert_eq!(reply.cached_at(), Some(80));
    assert_eq!(clone.cached_at(), Some(80));
    assert_eq!(reply.rendered(80), clone.rendered(80));
}

#[test]
fn push_empties_only_the_changed_replys_slot() {
    let reply = Reply::new("hello".to_owned(), 1);
    reply.rendered(60);
    let mut clone = reply.clone();
    clone.push("!");
    assert_eq!(clone.cached_at(), None);
    assert_eq!(reply.cached_at(), Some(60));
}

#[test]
fn set_empties_only_the_changed_replys_slot() {
    let reply = Reply::new("hello".to_owned(), 1);
    reply.rendered(60);
    let mut clone = reply.clone();
    clone.set("goodbye".to_owned());
    assert_eq!(clone.cached_at(), None);
    assert_eq!(reply.cached_at(), Some(60));
}

#[test]
fn a_table_in_the_source_reads_as_table_sensitive() {
    let reply = Reply::new(TABLE.to_owned(), 1);
    assert_eq!(reply.table_cached(), None);
    assert!(reply.has_table());
    assert_eq!(reply.table_cached(), Some(true));
    // The answer is cached: parsing runs once.
    assert!(reply.has_table());
    assert_eq!(reply.table_cached(), Some(true));
}

#[test]
fn table_looking_text_without_a_table_reads_as_width_stable() {
    // Fenced code with pipes is no table, as the parser knows blocks.
    let fenced = Reply::new("```\n| a | b |\n|---|---|\n```\n".to_owned(), 1);
    assert!(!fenced.has_table());
    assert_eq!(fenced.table_cached(), Some(false));
    // Escaped pipes are no table either.
    let escaped = Reply::new("a \\| b \\| c\n".to_owned(), 2);
    assert!(!escaped.has_table());
    // Strikethrough alone, parsed with the renderer's options, is none.
    let struck = Reply::new("~~gone~~\n".to_owned(), 3);
    assert!(!struck.has_table());
    // Plain prose stays width-stable.
    let prose = Reply::new("just some words\n".to_owned(), 4);
    assert!(!prose.has_table());
}

#[test]
fn a_clone_shares_the_table_answer_until_it_changes_its_text() {
    let reply = Reply::new(TABLE.to_owned(), 1);
    let clone = reply.clone();
    assert!(clone.has_table());
    // The clone's parse filled the shared slot.
    assert_eq!(reply.table_cached(), Some(true));
    let mut clone = reply.clone();
    clone.set("plain words\n".to_owned());
    assert!(!clone.has_table());
    // The original still holds its own answer.
    assert!(reply.has_table());
    assert_eq!(reply.table_cached(), Some(true));
}

#[test]
fn push_and_set_invalidate_the_table_answer() {
    // Pushing a table onto prose introduces one.
    let mut reply = Reply::new("plain words\n".to_owned(), 1);
    assert!(!reply.has_table());
    reply.push(TABLE);
    assert_eq!(reply.table_cached(), None);
    assert!(reply.has_table());
    // Setting prose back removes it.
    reply.set("plain words\n".to_owned());
    assert_eq!(reply.table_cached(), None);
    assert!(!reply.has_table());
    // A push that leaves the answer detaches the clone's slot.
    let mut clone = Reply::new("plain words\n".to_owned(), 2);
    let shared = clone.clone();
    clone.push(" more\n");
    assert_eq!(clone.table_cached(), None);
    assert_eq!(shared.table_cached(), None);
    assert!(!clone.has_table());
    assert!(!shared.has_table());
}

/// The reply's rows through a seam drawing `fake`.
fn seamed(
    reply: &Reply,
    width: u16,
    fake: &std::rc::Rc<crate::ext::tests::SlotRenderer>,
    flat: bool,
) -> Vec<crate::turn::Row> {
    let mut out =
        crate::rows::Rows::default().with_seam(crate::ext::tests::seam(fake, "s", 0, flat));
    reply.rows(width, &mut out);
    out.into_parts().0
}

fn replaced() -> std::rc::Rc<crate::ext::tests::SlotRenderer> {
    use crate::ext::tests::{SlotRenderer, SlotScript, raw};
    std::rc::Rc::new(SlotRenderer::filling(
        &["reply"],
        SlotScript::Replace(vec![vec![raw("custom")]]),
    ))
}

#[test]
fn a_reply_replacement_draws_instead_of_the_text() {
    let reply = Reply::new("Hello.".to_owned(), 7);
    let fake = replaced();
    let rows = seamed(&reply, 40, &fake, false);
    let shown: Vec<String> = rows.iter().map(|(line, _)| line.to_string()).collect();
    // (`docs/tui.md`, "How a TUI extension runs": each drawn item is cached by input and width.)
    assert_eq!(fake.calls.borrow().len(), 1);
    assert_eq!(fake.calls.borrow()[0].1, "reply");
    assert!(
        shown.iter().any(|row| row.starts_with("custom")),
        "{shown:?}"
    );
    assert!(!shown.iter().any(|row| row.contains("Hello")), "{shown:?}");
}

#[test]
fn a_reply_replacement_keeps_link_cells_and_non_text_padding() {
    use crate::ext::tests::{SlotRenderer, SlotScript};
    let reply = Reply::new("original".to_owned(), 8);
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["reply"],
        SlotScript::Replace(vec![vec![crate::ext::RawSpan {
            text: "abc".to_owned(),
            link: Some("https://example.com".to_owned()),
            ..crate::ext::RawSpan::default()
        }]]),
    ));
    let mut out =
        crate::rows::Rows::default().with_seam(crate::ext::tests::seam(&fake, "s", 0, false));
    reply.rows(10, &mut out);
    let (rows, texts) = out.into_parts();

    // (`docs/tui.md`, "What a renderer returns": linked cells stay selectable, and the fit's pad is not text.)
    assert_eq!(rows[0].0.to_string(), "abc       ");
    assert_eq!(texts[0].tail, 7);
    assert_eq!(
        texts[0].links,
        vec![(0..3, "https://example.com".to_owned())]
    );
}

#[test]
fn a_reply_decorator_draws_the_built_in_text_under_its_badge() {
    use crate::ext::tests::{SlotRenderer, SlotScript, raw};
    let reply = Reply::new("Hello.".to_owned(), 7);
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["reply"],
        SlotScript::Decorate {
            badge: vec![vec![raw("BADGE")]],
            trim: 5,
        },
    ));
    let rows = seamed(&reply, 40, &fake, false);
    assert_eq!(fake.builtins.borrow().as_slice(), &[35]);
    let shown: Vec<String> = rows.iter().map(|(line, _)| line.to_string()).collect();
    assert!(
        shown.iter().any(|row| row.starts_with("BADGE")),
        "{shown:?}"
    );
    assert!(shown.iter().any(|row| row.contains("Hello")), "{shown:?}");
}

#[test]
fn count_only_reply_decorators_keep_the_built_in_text() {
    use crate::ext::tests::{SlotRenderer, SlotScript, raw};
    let reply = Reply::new("Hello.".to_owned(), 7);
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["reply"],
        SlotScript::Decorate {
            badge: vec![vec![raw("BADGE")]],
            trim: 5,
        },
    ));
    let mut rows =
        crate::rows::Rows::count_only().with_seam(crate::ext::tests::seam(&fake, "s", 0, false));
    reply.count_rows(40, &mut rows);
    assert_eq!(fake.builtins.borrow().as_slice(), &[35]);
    let shown: Vec<String> = rows
        .into_parts()
        .0
        .iter()
        .map(|(line, _)| line.to_string())
        .collect();
    assert!(
        shown.iter().any(|row| row.starts_with("BADGE")),
        "{shown:?}"
    );
    assert!(shown.iter().any(|row| row.contains("Hello")), "{shown:?}");
}

#[test]
fn a_reply_error_falls_back_with_one_failure() {
    use crate::ext::FailureKind;
    use crate::ext::tests::{SlotRenderer, SlotScript};
    let reply = Reply::new("Hello.".to_owned(), 7);
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["reply"],
        SlotScript::Fail {
            kind: FailureKind::HookFailed,
            message: "boom".into(),
        },
    ));
    let bound = crate::ext::tests::seam(&fake, "s", 0, false);
    for _ in 0..2 {
        let mut out = crate::rows::Rows::default().with_seam(bound.clone());
        reply.rows(40, &mut out);
        let shown: Vec<String> = out
            .into_parts()
            .0
            .iter()
            .map(|(line, _)| line.to_string())
            .collect();
        assert!(shown.iter().any(|row| row.contains("Hello")), "{shown:?}");
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
    assert_eq!(listed[0].0, "reply");
    assert_eq!(listed[0].1.kind, FailureKind::HookFailed);
}

#[test]
fn a_reply_timeout_lists_one_extension_failed() {
    use crate::ext::FailureKind;
    use crate::ext::tests::{SlotRenderer, SlotScript};
    let reply = Reply::new("Hello.".to_owned(), 7);
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["reply"],
        SlotScript::Fail {
            kind: FailureKind::ExtensionFailed,
            message: "slow".into(),
        },
    ));
    let bound = crate::ext::tests::seam(&fake, "s", 0, false);
    let mut out = crate::rows::Rows::default().with_seam(bound.clone());
    reply.rows(40, &mut out);
    let listed = bound.cache.take_failures();
    // (`docs/tui.md`, "When an extension fails": one notice names the extension and slot once until `/reload`.)
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].1.kind, FailureKind::ExtensionFailed);
}

#[test]
fn a_reply_replacement_carries_no_copy_target() {
    let reply = Reply::new("```sh\necho hi\n```".to_owned(), 7);
    // The built-in places one copy target on the code block's header.
    let mut plain = crate::rows::Rows::default();
    reply.rows(40, &mut plain);
    let (built, _) = plain.into_parts();
    assert!(
        built
            .iter()
            .any(|(_, target)| matches!(target, Some(crate::app::Target::Copy { .. }))),
        "the built-in names its code block"
    );
    let fake = replaced();
    let rows = seamed(&reply, 40, &fake, false);
    assert!(!rows.is_empty());
    for (line, target) in &rows {
        assert_eq!(*target, None, "{line:?} keeps no copy target");
    }
}

#[test]
fn a_flat_reply_replacement_drops_every_style() {
    use crate::ext::tests::{SlotRenderer, SlotScript};
    use ratatui::style::Style;
    let reply = Reply::new("Hello.".to_owned(), 7);
    let fake = std::rc::Rc::new(SlotRenderer::filling(
        &["reply"],
        SlotScript::Replace(vec![vec![crate::ext::RawSpan {
            text: "custom".into(),
            bold: true,
            fg: Some("accent".into()),
            ..crate::ext::RawSpan::default()
        }]]),
    ));
    let rows = seamed(&reply, 40, &fake, true);
    // (`docs/tui.md`, "How a TUI extension runs": in screen-reader mode the terminal drops every style.)
    let shown: Vec<String> = rows.iter().map(|(line, _)| line.to_string()).collect();
    assert!(shown.iter().any(|row| row.contains("custom")), "{shown:?}");
    for (line, _) in &rows {
        for span in &line.spans {
            assert_eq!(span.style, Style::default(), "{span:?}");
        }
    }
}

fn count_and_draw(text: &str, width: u16) -> (Vec<crate::turn::Row>, Vec<crate::turn::Row>) {
    let reply = Reply::new(text.to_owned(), 17);
    let rendered = reply.rendered(width);
    let counted_result = reply.counted(width);
    assert_eq!(
        counted_result.rows,
        rendered.lines.len(),
        "logical row count: {text:?} at {width}"
    );
    assert_eq!(
        counted_result.targets,
        rendered
            .targets
            .iter()
            .map(|target| target.line)
            .collect::<Vec<_>>(),
        "copy target offsets: {text:?} at {width}"
    );

    let mut shown = crate::rows::Rows::default();
    reply.rows(width, &mut shown);
    let shown = shown.into_parts().0;
    crate::work::take();
    let mut counted = crate::rows::Rows::count_only();
    reply.count_rows(width, &mut counted);
    let reply_renders = crate::work::take().reply_renders;
    assert_eq!(
        reply_renders > 0,
        counted_result.over_width,
        "fallback decision: {text:?} at {width}"
    );
    let counted = counted.into_parts().0;
    (shown, counted)
}

#[test]
fn markdown_exact_row_width_matches_the_text_width_helper() {
    for code in 0..=0x7f {
        let text = char::from(code).to_string();
        assert_eq!(
            crate::markdown::exact_width(&text),
            crate::text::width(&text),
            "ASCII character {code}"
        );
    }
    for text in ["ordinary ASCII", "界", "❤️", "👩‍💻", "e\u{301}", "a\u{1}"] {
        assert_eq!(
            crate::markdown::exact_width(text),
            crate::text::width(text),
            "row {text:?}"
        );
    }
}

#[test]
fn markdown_ascii_cell_widths_match_the_text_width_helper() {
    for code in 0..=0x7f {
        let ch = char::from(code);
        assert_eq!(
            crate::markdown::wrap::char_width(ch),
            crate::text::char_width(ch),
            "ASCII character {code}"
        );
    }
    for ch in ['界', '❤', '\u{fe0f}', '👩', '\u{301}', '\u{200d}'] {
        assert_eq!(
            crate::markdown::wrap::char_width(ch),
            crate::text::char_width(ch),
            "Unicode character {ch:?}"
        );
    }
}

#[test]
fn count_only_matches_each_markdown_row_kind_and_copy_target() {
    let samples = [
        "plain paragraph with a very long word abcdefghijklmnopqrstuvwxyz\nnext line",
        "abcdefghijklmnopqrstuvwxyz",
        "12345",
        "        a word that must wrap after a spaces-only prefix",
        "- one\n  - nested item with a long unbroken sequence abcdefghijklmnopqrstuvwxyz\n  - sibling",
        "- \n- item",
        "## heading\n\n- first item\n- second item",
        "> quoted text with words that wrap at narrow widths",
        "```\n\twide界 code\n\nlast\n```",
        "```rust\nfn main() { println!(\"ok\"); }\n```",
        "```diff\n+added\n-removed\n```",
        "```md\n**marked**\n```",
        "| name | value |\n|---|---:|\n| alpha | 123 |",
        "| emoji | value |\n|---|---|\n| ❤️ | exact |",
        "| a | b | c | d | e | f |\n|---|---|---|---|---|---|\n| 1 | 2 | 3 | 4 | 5 | 6 |",
        "```rust\nlet emoji = \"👩‍💻\";\n```",
        "**e**\u{301} and \u{1}",
        "---\n\ntext  \nnext",
        "a ❤️ and 👩‍💻 plus e\u{301}",
        "\n\nempty blocks\n",
    ];
    for text in samples {
        for width in [1, 2, 5, 10, 20, 40, 80] {
            let (shown, counted) = count_and_draw(text, width);
            assert_eq!(counted.len(), shown.len(), "{text:?} at {width}");
            let shown_targets: Vec<_> = shown.iter().map(|row| row.1).collect();
            let count_targets: Vec<_> = counted.iter().map(|row| row.1).collect();
            assert_eq!(count_targets, shown_targets, "targets: {text:?} at {width}");
        }
    }
}

#[test]
fn fitting_ascii_count_does_not_render_and_count_cache_is_shared_by_key() {
    let reply = Reply::new("ordinary fitting paragraph".to_owned(), 1);
    let clone = reply.clone();
    crate::work::take();
    let mut rows = crate::rows::Rows::count_only();
    reply.count_rows(40, &mut rows);
    assert_eq!(rows.len(), 1);
    assert_eq!(crate::work::take().reply_renders, 0);
    let first = reply.count_cached_at(40).expect("count was cached");
    let same = clone.count_cached_at(40).expect("clone shares count slot");
    assert!(std::sync::Arc::ptr_eq(&first, &same));
    let mut rows = crate::rows::Rows::count_only();
    clone.count_rows(39, &mut rows);
    let changed_width = clone.count_cached_at(39).expect("new width cached");
    assert!(!std::sync::Arc::ptr_eq(&first, &changed_width));
    assert!(reply.count_cached_at(40).is_none());
}

#[test]
fn a_table_whose_minimum_columns_exceed_the_viewport_falls_back() {
    let reply = Reply::new(
        "| alpha | bravo | charlie | delta | echo | foxtrot |\n|---|---|---|---|---|---|\n| one | two | three | four | five | six |"
            .to_owned(),
        1,
    );
    assert!(reply.counted(20).over_width);
    let mut shown = crate::rows::Rows::default();
    reply.rows(20, &mut shown);
    let shown = shown.into_parts().0;
    crate::work::take();
    let mut counted = crate::rows::Rows::count_only();
    reply.count_rows(20, &mut counted);
    assert_eq!(counted.into_parts().0, shown);
    assert_eq!(crate::work::take().reply_renders, 1);
}

#[test]
fn an_exact_width_count_does_not_render() {
    let reply = Reply::new("12345".to_owned(), 1);
    assert!(!reply.counted(5).over_width);
    crate::work::take();
    let mut rows = crate::rows::Rows::count_only();
    reply.count_rows(5, &mut rows);
    assert_eq!(rows.len(), 1);
    assert_eq!(crate::work::take().reply_renders, 0);
}

#[test]
fn markdown_wrapping_keeps_word_moves_trailing_newlines_and_empty_input() {
    use ratatui::style::Style;

    let cells = |text: &str| {
        text.chars()
            .map(|ch| (ch, Style::default()))
            .collect::<Vec<_>>()
    };
    let text = cells("ab  cd");
    let rows = crate::markdown::wrap::wrap_joined_indices(&text, 5, 5, true);
    assert_eq!(
        rows.iter()
            .map(|(row, _, _)| row.iter().map(|(ch, _)| *ch).collect::<String>())
            .collect::<Vec<_>>(),
        ["ab", "cd"]
    );
    assert_eq!(
        rows.iter().map(|(_, join, _)| *join).collect::<Vec<_>>(),
        [crate::rows::Join::Break, crate::rows::Join::WrapSpace]
    );
    assert_eq!(
        rows.iter()
            .map(|(_, _, indices)| indices.clone())
            .collect::<Vec<_>>(),
        [vec![0, 1], vec![4, 5]]
    );

    let trailing_newline = cells("x\n");
    let rows = crate::markdown::wrap::wrap_joined_indices(&trailing_newline, 5, 5, false);
    assert_eq!(
        rows.len(),
        1,
        "a final newline closes the row; it adds no empty row"
    );
    assert_eq!(rows[0].2, [0]);
    assert!(crate::markdown::wrap::wrap_joined_indices(&[], 5, 5, true).is_empty());
}

#[test]
fn code_block_counts_keep_tab_width_and_language_fallback_boundaries() {
    for text in ["```\na\tb\n```", "```\nabc\tdef\n```", "```\nrust\n```"] {
        for width in [5, 8, 9, 10, 11, 15, 16, 17, 20] {
            let (shown, counted) = count_and_draw(text, width);
            assert_eq!(counted.len(), shown.len(), "{text:?} at {width}");
        }
    }

    for (text, expected_over_width) in [
        ("```rust\nx界\n```", true),
        ("```\nx界\n```", false),
        ("```   \nx界\n```", false),
        ("```rust\nx\n```", false),
    ] {
        assert_eq!(
            Reply::new(text.to_owned(), 1).counted(80).over_width,
            expected_over_width,
            "{text:?}"
        );
    }
}

#[test]
fn a_table_count_distinguishes_exact_fit_from_overflow() {
    let table = "| a | b |\n|---|---|\n| 1 | 2 |";
    assert!(!Reply::new(table.to_owned(), 1).counted(5).over_width);
    assert!(Reply::new(table.to_owned(), 1).counted(4).over_width);
    for width in [4, 5, 6, 8] {
        let (shown, counted) = count_and_draw(table, width);
        assert_eq!(counted.len(), shown.len(), "table at {width}");
    }

    let wide_cell = Reply::new("| 界 | a |\n|---|---|\n| b | c |".to_owned(), 1);
    assert!(
        wide_cell.counted(5).over_width,
        "a wide cell cannot fit a one-column minimum"
    );
}

#[test]
fn an_over_width_count_falls_back_to_the_drawn_rows() {
    let reply = Reply::new("界".to_owned(), 1);
    let mut shown = crate::rows::Rows::default();
    reply.rows(1, &mut shown);
    let shown = shown.into_parts().0;
    crate::work::take();
    let mut counted = crate::rows::Rows::count_only();
    reply.count_rows(1, &mut counted);
    assert_eq!(counted.into_parts().0, shown);
    assert_eq!(crate::work::take().reply_renders, 1);
}

#[test]
fn count_cache_text_changes_detach_only_the_changed_reply() {
    let reply = Reply::new("before".to_owned(), 1);
    let mut rows = crate::rows::Rows::count_only();
    reply.count_rows(40, &mut rows);
    let original = reply.count_cached_at(40).expect("count was cached");
    let mut clone = reply.clone();
    clone.push(" after");
    assert!(clone.count_cached_at(40).is_none());
    assert!(std::sync::Arc::ptr_eq(
        &original,
        &reply
            .count_cached_at(40)
            .expect("unchanged clone keeps cache")
    ));
    let mut rows = crate::rows::Rows::count_only();
    clone.count_rows(40, &mut rows);
    let pushed = clone.count_cached_at(40).expect("pushed text counted");
    assert!(!std::sync::Arc::ptr_eq(&original, &pushed));
    clone.set("set text".to_owned());
    assert!(clone.count_cached_at(40).is_none());
}

#[test]
fn count_and_draw_agree_at_markdown_table_and_code_boundaries() {
    let samples = [
        "| left | numeric | right |\n|:---|---:|---|\n| a very long cell | 123456789 | ❤️ and 👩‍💻 |\n| x | 0 | e\u{301} |",
        "| short | centered |\n|:---|:---:|\n| x | a long centered cell |",
        "| a | b | c | d | e | f |\n|---|---|---|---|---|---|\n| one two three | 99 | 界 | | | end |",
        "```\nalpha beta gamma delta\n\twide界\n\nend\n```",
        "```rust\nlet value = 123456789;\nfn main() { println!(\"👩‍💻\"); }\n```",
        "```diff\n+alpha beta gamma\n-removed 界\n```",
        "```md\n**bold** and a longwordwithoutspaces\n```",
    ];
    for text in samples {
        for width in 1..=80 {
            let (shown, counted) = count_and_draw(text, width);
            assert_eq!(counted.len(), shown.len(), "{text:?} at {width}");
            assert_eq!(
                counted.iter().map(|row| row.1).collect::<Vec<_>>(),
                shown.iter().map(|row| row.1).collect::<Vec<_>>(),
                "targets: {text:?} at {width}"
            );
        }
    }
}

// (`docs/tui.md`, "History and paging": row counts are exact at each width.)
fn assert_count_rows_match_drawn_rows(text: &str) {
    for width in 0..=48 {
        let reply = Reply::new(text.to_owned(), 17);
        let rendered = reply.rendered(width);
        let counted = reply.counted(width);
        let drawn_needs_split = rendered
            .lines
            .iter()
            .any(|line| crate::rows::fitted(line, width) > 1);
        assert_eq!(
            counted.rows,
            rendered.lines.len(),
            "logical rows at width {width}: {text:?}"
        );
        assert_eq!(
            counted.over_width, drawn_needs_split,
            "fallback at width {width}: {text:?}"
        );

        let mut drawn = crate::rows::Rows::default();
        reply.rows(width, &mut drawn);
        drawn.pad(0, 0, width);
        let mut counted = crate::rows::Rows::count_only();
        reply.count_rows(width, &mut counted);
        counted.pad(0, 0, width);
        assert_eq!(
            counted.len(),
            drawn.len(),
            "padded rows at width {width}: {text:?}"
        );
    }
}

#[test]
fn row_chunk_counts_match_drawn_rows_at_fit_boundaries() {
    assert_count_rows_match_drawn_rows("plain text with a word at the edge");
}

#[test]
fn code_block_counts_match_drawn_rows_at_fit_boundaries() {
    assert_count_rows_match_drawn_rows("```\nalpha beta gamma\n\nwide 界\nend\n```");
}

#[test]
fn table_counts_match_drawn_rows_at_fit_boundaries() {
    assert_count_rows_match_drawn_rows(
        "| left | right |\n|---|---|\n| a longer cell | 123456789 |\n| x | 界 |",
    );
}
