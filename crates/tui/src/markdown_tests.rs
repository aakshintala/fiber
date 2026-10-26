//! Tests for the markdown renderer.

use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use super::{CopyTarget, render};
use crate::theme::{Role, style};

/// A line's text.
fn text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// Every line's text.
fn texts(markdown: &str, width: u16) -> Vec<String> {
    render(markdown, width).lines.iter().map(text).collect()
}

/// The style of the cell at `col` on `line`.
fn style_at(line: &Line<'_>, col: usize) -> Style {
    let mut at = 0;
    for span in &line.spans {
        let width = span.width();
        if col < at + width {
            return span.style;
        }
        at += width;
    }
    panic!("no cell {col} on {:?}", text(line));
}

/// One row's link columns and destinations.
type RowLinks = Vec<(std::ops::Range<u16>, String)>;

fn fg(role: Role) -> Option<ratatui::style::Color> {
    Some(role.color())
}

const TINT: Option<ratatui::style::Color> = Some(Role::Code.color());

#[test]
fn empty_text_renders_nothing() {
    assert_eq!(render("", 40), super::Rendered::default());
}

#[test]
fn a_paragraph_carries_the_full_text_colour() {
    let rendered = render("hello", 40);
    assert_eq!(
        rendered.lines,
        vec![Line::from(ratatui::text::Span::styled(
            "hello",
            style(Role::Text)
        ))]
    );
}

#[test]
fn a_heading_drops_its_markers_and_is_bold_in_the_heading_colour() {
    let rendered = render("# Title\n\nbody", 40);
    assert_eq!(texts("# Title\n\nbody", 40), vec!["Title", "", "body"]);
    let heading = style_at(&rendered.lines[0], 0);
    assert_eq!(heading.fg, fg(Role::Heading));
    assert!(heading.add_modifier.contains(Modifier::BOLD));
    assert_eq!(style_at(&rendered.lines[2], 0).fg, fg(Role::Text));
}

#[test]
fn bullets_are_accent_dots_and_nested_lists_indent_two_columns() {
    let markdown = "- one\n- two\n  - inner\n- three";
    assert_eq!(
        texts(markdown, 40),
        vec!["• one", "• two", "  • inner", "• three"]
    );
    let rendered = render(markdown, 40);
    assert_eq!(style_at(&rendered.lines[0], 0).fg, fg(Role::Accent));
    assert_eq!(style_at(&rendered.lines[0], 2).fg, fg(Role::Text));
    assert_eq!(style_at(&rendered.lines[2], 2).fg, fg(Role::Accent));
}

#[test]
fn ordered_lists_keep_their_numbers_in_the_accent() {
    let markdown = "3. three\n4. four";
    assert_eq!(texts(markdown, 40), vec!["3. three", "4. four"]);
    let rendered = render(markdown, 40);
    assert_eq!(style_at(&rendered.lines[1], 1).fg, fg(Role::Accent));
}

#[test]
fn a_wrapped_item_hangs_under_its_text() {
    assert_eq!(
        texts("- alpha beta gamma", 10),
        vec!["• alpha", "  beta", "  gamma"]
    );
}

#[test]
fn a_blank_line_separates_blocks() {
    assert_eq!(
        texts("one\n\ntwo\n\n- a\n\nthree", 40),
        vec!["one", "", "two", "", "• a", "", "three"]
    );
}

#[test]
fn inline_styles() {
    let rendered = render("**b** *i* ~~s~~ `c` [l](http://x)", 40);
    let line = &rendered.lines[0];
    assert!(style_at(line, 0).add_modifier.contains(Modifier::BOLD));
    assert_eq!(style_at(line, 0).fg, fg(Role::Text));
    assert!(style_at(line, 2).add_modifier.contains(Modifier::ITALIC));
    assert!(
        style_at(line, 4)
            .add_modifier
            .contains(Modifier::CROSSED_OUT)
    );
    assert_eq!(style_at(line, 6).fg, fg(Role::Info));
    assert_eq!(style_at(line, 6).bg, None);
    assert!(
        style_at(line, 8)
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
    assert_eq!(style_at(line, 8).fg, fg(Role::Accent));
    assert_eq!(text(line), "b i s c l");
    let plain = style_at(line, 1);
    assert_eq!(plain, style(Role::Text));
}

#[test]
fn each_run_of_one_style_is_one_span_and_a_style_change_starts_another() {
    let rendered = render("a **b** c **d** e", 40);
    let spans: Vec<&str> = rendered.lines[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(spans, ["a ", "b", " c ", "d", " e"]);
}

#[test]
fn links_draw_in_accent_while_plain_text_stays_text() {
    let rendered = render("see [l](http://x) now", 40);
    let line = &rendered.lines[0];
    assert_eq!(text(line), "see l now");
    // The link's cell in the accent colour, underlined.
    let link = style_at(line, 4);
    assert_eq!(link.fg, fg(Role::Accent));
    assert!(link.add_modifier.contains(Modifier::UNDERLINED));
    // The cells on either side stay in the text colour, not underlined.
    for col in [0, 3, 5, 8] {
        let plain = style_at(line, col);
        assert_eq!(plain.fg, fg(Role::Text), "cell {col}");
        assert!(
            !plain.add_modifier.contains(Modifier::UNDERLINED),
            "cell {col}"
        );
    }
}

#[test]
fn images_draw_as_alt_text_and_html_as_literal_text() {
    assert_eq!(texts("![alt text](x.png)", 40), vec!["alt text"]);
    assert_eq!(texts("a <b>c</b>", 40), vec!["a <b>c</b>"]);
}

#[test]
fn a_block_quote_is_prefixed_with_a_dim_bar() {
    let rendered = render("> quoted words here", 12);
    assert_eq!(
        rendered.lines.iter().map(text).collect::<Vec<_>>(),
        vec!["│ quoted", "│ words here"]
    );
    assert_eq!(style_at(&rendered.lines[1], 0).fg, fg(Role::Muted));
}

#[test]
fn a_horizontal_rule_spans_the_width_dim() {
    let rendered = render("a\n\n---\n\nb", 5);
    assert_eq!(text(&rendered.lines[2]), "─────");
    assert_eq!(style_at(&rendered.lines[2], 4).fg, fg(Role::Muted));
}

#[test]
fn a_code_block_has_a_header_numbered_lines_and_the_tint() {
    let markdown = "```rust\nfn main() {}\nlet x = 1;\n```";
    let rendered = render(markdown, 20);
    assert_eq!(
        rendered.lines.iter().map(text).collect::<Vec<_>>(),
        vec![
            "▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄",
            " rust  click to copy",
            "   1  fn main() {}  ",
            "   2  let x = 1;    ",
            "▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀",
        ]
    );
    for line in &rendered.lines[1..4] {
        assert_eq!(line.width(), 20);
        for col in 0..20 {
            assert_eq!(style_at(line, col).bg, TINT, "{:?} col {col}", text(line));
        }
    }
    // The edge rows are half blocks in the code colour over the card.
    for (row, edge) in [(0, "▄"), (4, "▀")] {
        assert_eq!(text(&rendered.lines[row]), edge.repeat(20));
        assert_eq!(style_at(&rendered.lines[row], 0).fg, fg(Role::Code));
        assert_eq!(style_at(&rendered.lines[row], 0).bg, None);
    }
    assert_eq!(style_at(&rendered.lines[1], 0).fg, fg(Role::Muted));
    assert_eq!(style_at(&rendered.lines[1], 7).fg, fg(Role::Muted));
    // The numbers are right-aligned in four columns in the rule colour.
    assert_eq!(style_at(&rendered.lines[2], 0).fg, fg(Role::Rule));
    assert_eq!(style_at(&rendered.lines[2], 3).fg, fg(Role::Rule));
    assert_eq!(style_at(&rendered.lines[2], 6).fg, fg(Role::Keyword));
    assert_eq!(style_at(&rendered.lines[2], 9).fg, fg(Role::Function));
    assert_eq!(style_at(&rendered.lines[3], 14).fg, fg(Role::Number));
    assert_eq!(
        rendered.targets,
        vec![CopyTarget {
            line: 1,
            cols: 7..20,
            code: "fn main() {}\nlet x = 1;".to_owned(),
        }]
    );
}

#[test]
fn a_comment_in_code_is_italic_in_the_comment_colour() {
    let rendered = render("```rust\n// one holder\nlet x = 1;\n```", 30);
    let comment = &rendered.lines[2];
    assert!(text(comment).contains("// one holder"));
    let comment_style = style_at(comment, 8);
    assert_eq!(comment_style.fg, fg(Role::Comment));
    assert!(comment_style.add_modifier.contains(Modifier::ITALIC));
    let code = &rendered.lines[3];
    assert!(!style_at(code, 6).add_modifier.contains(Modifier::ITALIC));
}

#[test]
fn line_numbers_are_right_aligned_in_four_columns() {
    let code: String = (1..=10).map(|n| format!("l{n}\n")).collect();
    let lines = texts(&format!("```\n{code}```"), 14);
    assert_eq!(lines[2], "   1  l1      ");
    assert_eq!(lines[10], "   9  l9      ");
    assert_eq!(lines[11], "  10  l10     ");
}

#[test]
fn an_unknown_or_empty_fence_draws_plain_code_with_its_tag_as_label() {
    let rendered = render("```brainfuck\n+++\n```", 16);
    assert_eq!(text(&rendered.lines[1]), " … click to copy");
    assert_eq!(style_at(&rendered.lines[2], 6).fg, fg(Role::CodeText));
    assert_eq!(texts("```\nx\n```", 10)[1], "          ");
    assert_eq!(texts("    indented", 10)[1], "          ");
}

#[test]
fn an_unclosed_fence_is_code_to_the_end() {
    let rendered = render("text\n\n```py\nx = 1\ny", 20);
    assert_eq!(
        rendered.lines.iter().map(text).collect::<Vec<_>>(),
        vec![
            "text",
            "",
            "▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄",
            " py    click to copy",
            "   1  x = 1         ",
            "   2  y             ",
            "▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀",
        ]
    );
    assert_eq!(rendered.targets[0].code, "x = 1\ny");
    assert_eq!(rendered.targets[0].line, 3);
}

#[test]
fn a_long_code_line_wraps_with_unnumbered_continuations() {
    let lines = texts("```\nabcdefghij\n```", 10);
    assert_eq!(
        lines,
        vec![
            "▄▄▄▄▄▄▄▄▄▄",
            "          ",
            "   1  abcd",
            "      efgh",
            "      ij  ",
            "▀▀▀▀▀▀▀▀▀▀",
        ]
    );
}

#[test]
fn tabs_in_code_expand_to_stops_of_four() {
    let lines = texts("```\n\tx\nab\ty\n```", 16);
    assert_eq!(lines[2], "   1      x     ");
    assert_eq!(lines[3], "   2  ab  y     ");
}

#[test]
fn the_header_truncates_its_label_and_drops_it_then_copy_when_narrow() {
    let header = |width| render("```typescript\nx\n```", width);
    assert_eq!(text(&header(18).lines[1]), " ty… click to copy");
    assert_eq!(header(18).targets[0].cols, 5..18);
    assert_eq!(text(&header(15).lines[1]), "  click to copy");
    assert_eq!(header(15).targets[0].cols, 2..15);
    assert_eq!(text(&header(14).lines[1]), " click to copy");
    assert_eq!(header(14).targets[0].cols, 1..14);
    assert_eq!(text(&header(13).lines[1]), "click to copy");
    assert_eq!(header(13).targets[0].cols, 0..13);
    assert_eq!(text(&header(12).lines[1]), "            ");
    assert!(header(12).targets.is_empty());
    for width in 1..30 {
        assert_eq!(
            header(width).lines[1].width(),
            usize::from(width),
            "{width}"
        );
    }
}

#[test]
fn a_wide_label_truncates_by_cells() {
    assert_eq!(texts("```語語語語\nx\n```", 20)[1], " 語語… click to copy");
}

#[test]
fn copy_targets_point_at_each_blocks_header_line() {
    let rendered = render("```\na\n```\n\ntext\n\n```sh\nb\n```", 20);
    let lines: Vec<usize> = rendered.targets.iter().map(|t| t.line).collect();
    assert_eq!(lines, vec![1, 8]);
    assert_eq!(text(&rendered.lines[8]), " sh    click to copy");
    assert_eq!(rendered.targets[1].code, "b");
}

#[test]
fn a_table_has_a_bold_header_a_rule_and_right_aligned_numbers() {
    let markdown = "| name | size |\n|---|---|\n| a | 1,200 |\n| bb | 7 |";
    let rendered = render(markdown, 40);
    assert_eq!(
        rendered.lines.iter().map(text).collect::<Vec<_>>(),
        vec![
            "name    size",
            "────   ─────",
            "a      1,200",
            "bb         7"
        ]
    );
    assert!(
        style_at(&rendered.lines[0], 0)
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert!(
        !style_at(&rendered.lines[2], 0)
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert_eq!(style_at(&rendered.lines[1], 0).fg, fg(Role::Rule));
    // The rule breaks at the gaps.
    assert_eq!(style_at(&rendered.lines[1], 4).fg, None);
    assert_eq!(
        text(&rendered.lines[1])
            .chars()
            .skip(4)
            .take(3)
            .collect::<String>(),
        "   ".to_owned()
    );
}

#[test]
fn a_number_columns_body_cells_are_plain_text() {
    let rendered = render("| n | t |\n|---|---|\n| `41` | `xy` |", 40);
    let row = &rendered.lines[2];
    assert_eq!(text(row), "41   xy");
    // The number column's body cell keeps the text colour only.
    assert_eq!(style_at(row, 1), style(Role::Text));
    // Any other column keeps its inline style.
    assert_eq!(style_at(row, 5).fg, fg(Role::Info));
}

#[test]
fn numeric_columns_take_signs_and_percentages_but_not_units() {
    let lines = texts(
        "| a | b | c |\n|---|---|---|\n| -1.5 | 3ms | |\n| +20% | 4 | x |",
        40,
    );
    assert_eq!(lines[2], "-1.5   3ms    ");
    assert_eq!(lines[3], "+20%   4     x");
    let lines = texts("| a | b |\n|---|---|\n| 1 | - |", 40);
    assert_eq!(lines[2], "1   -");
}

#[test]
fn the_alignment_row_is_honoured() {
    let lines = texts("| left | right | mid |\n|:--|--:|:-:|\n| a | b | c |", 40);
    assert_eq!(lines[2], "a          b    c ");
}

#[test]
fn a_wide_table_shrinks_its_widest_column_and_wraps_its_cells() {
    let markdown = "| key | description |\n|---|---|\n| k | one two three four |";
    let rendered = render(markdown, 15);
    assert_eq!(
        rendered.lines.iter().map(text).collect::<Vec<_>>(),
        vec![
            "key   descripti",
            "      on       ",
            "───   ─────────",
            "k     one two  ",
            "      three    ",
            "      four     ",
        ]
    );
}

#[test]
fn shrinking_stops_at_five_columns_each() {
    let markdown = "| aaaaaaaa | bbbbbbbb |\n|---|---|\n| x | y |";
    let lines = texts(markdown, 4);
    assert_eq!(lines[0], "aaaaa   bbbbb");
    assert_eq!(lines[1], "aaa     bbb  ");
    assert_eq!(lines[2], "─────   ─────");
}

#[test]
fn a_table_header_without_its_rule_yet_is_a_paragraph() {
    assert_eq!(texts("| a | b |", 40), vec!["| a | b |"]);
}

#[test]
fn rendering_is_pure() {
    let markdown = "# H\n\n- a\n\n```rust\nfn x() {}\n```\n\n| a | 1 |\n|---|---|\n| b | 2 |";
    assert_eq!(render(markdown, 30), render(markdown, 30));
}

#[test]
fn every_line_fits_the_width() {
    let markdown = "# A heading that is long\n\n- item with words that wrap\n  - nested item that wraps too\n\n> quote that wraps around\n\n```rust\nlet a_long_line = \"with a string\";\n```\n\n| a | b |\n|---|---|\n| 語語語語 | wide |";
    // The table's minimum is 12: its columns stop shrinking at five and
    // four with a three-space gap, so narrower widths overflow by design.
    for width in 12..40u16 {
        for line in render(markdown, width).lines {
            assert!(
                line.width() <= usize::from(width),
                "{width}: {:?}",
                text(&line)
            );
        }
    }
}

#[test]
fn wide_characters_wrap_by_cells() {
    assert_eq!(texts("語語語語語", 4), vec!["語語", "語語", "語"]);
    let lines = texts("```\n語語語\n```", 8);
    assert_eq!(
        lines,
        vec![
            "▄▄▄▄▄▄▄▄",
            "        ",
            "   1  語",
            "      語",
            "      語",
            "▀▀▀▀▀▀▀▀",
        ]
    );
}

#[test]
fn hard_breaks_end_lines() {
    assert_eq!(texts("one  \ntwo", 40), vec!["one", "two"]);
}

#[test]
fn a_loose_list_has_no_blank_lines_and_its_second_paragraph_hangs() {
    assert_eq!(
        texts("- a\n\n  more\n- b\n\nafter", 40),
        vec!["• a", "  more", "• b", "", "after"]
    );
}

#[test]
fn an_empty_item_shows_its_marker() {
    assert_eq!(texts("-\n- b", 40), vec!["• ", "• b"]);
}

#[test]
fn a_wide_number_marker_hangs_by_its_width() {
    assert_eq!(texts("10. alpha beta", 10), vec!["10. alpha", "    beta"]);
}

#[test]
fn a_block_after_a_blank_gets_one_blank() {
    assert_eq!(texts("a\n\n> b", 40), vec!["a", "", "│ b"]);
}

#[test]
fn an_items_text_ends_before_a_block_inside_it() {
    assert_eq!(texts("- a\n  > q", 20), vec!["• a", "│   q"]);
    assert_eq!(texts("- a\n  ***", 6), vec!["• a", "──────"]);
    let lines = texts("- a\n  ```\n  x\n  ```", 12);
    assert_eq!(lines[0], "• a");
    assert_eq!(lines[1], "▄▄▄▄▄▄▄▄▄▄▄▄");
    assert_eq!(lines[2], "            ");
    let lines = texts("- a\n  | x | y |\n  |---|---|\n  | 1 | 2 |", 20);
    assert_eq!(lines[0], "• a");
    assert_eq!(lines[1], "x   y");
}

#[test]
fn a_soft_break_is_a_space() {
    assert_eq!(texts("one\ntwo", 40), vec!["one two"]);
}

#[test]
fn the_label_is_the_info_strings_first_word() {
    assert_eq!(texts("```rust,ignore\nx\n```", 14)[1], " click to copy");
    assert_eq!(texts("```py title\nx\n```", 14)[1], " click to copy");
}

#[test]
fn a_tie_for_widest_shrinks_the_first_column() {
    let lines = texts("| aaaaaaaa | bbbbbbbb |\n|---|---|\n| x | y |", 17);
    assert_eq!(lines[0], "aaaaaaa   bbbbbbb");
    assert_eq!(lines[1], "a         b      ");
}

#[test]
fn a_tab_in_prose_is_a_space_and_a_quote_ends() {
    assert_eq!(texts("a\tb", 40), vec!["a b"]);
    assert_eq!(texts("> b\n\nc", 40), vec!["│ b", "", "c"]);
}

#[test]
fn an_item_that_opens_with_a_code_block_shows_its_marker_first() {
    let lines = texts("- ```\n  x\n  ```\n- b", 12);
    assert_eq!(
        lines,
        vec![
            "• ",
            "▄▄▄▄▄▄▄▄▄▄▄▄",
            "            ",
            "   1  x     ",
            "▀▀▀▀▀▀▀▀▀▀▀▀",
            "• b"
        ]
    );
}

#[test]
fn the_widest_of_unequal_columns_shrinks_first() {
    let lines = texts("| aaaaaaaaaa | bbbbbbb |\n|---|---|\n| x | y |", 17);
    assert_eq!(lines[0], "aaaaaaa   bbbbbbb");
}

/// `text` wrapped by [`super::wrap_joined_indices`], each row as a string.
fn wrapped(text: &str, first: usize, rest: usize, words: bool) -> Vec<String> {
    let cells: Vec<(char, Style)> = text.chars().map(|ch| (ch, Style::default())).collect();
    super::wrap_joined_indices(&cells, first, rest, words)
        .iter()
        .map(|(row, _, _)| row.iter().map(|(ch, _)| ch).collect())
        .collect()
}

#[test]
fn wrapping_by_words_breaks_before_a_word_that_fits_the_next_row() {
    assert_eq!(wrapped("ab cd ef", 5, 5, true), vec!["ab cd", "ef"]);
    assert_eq!(wrapped("abcde f", 5, 5, true), vec!["abcde", "f"]);
    assert_eq!(wrapped("ab   cd", 3, 3, true), vec!["ab", "cd"]);
}

#[test]
fn a_word_longer_than_a_row_splits_from_where_the_row_stands() {
    assert_eq!(
        wrapped("ab cdefghij", 5, 5, true),
        vec!["ab cd", "efghi", "j"]
    );
    assert_eq!(wrapped("abcdef", 3, 10, true), vec!["abc", "def"]);
}

#[test]
fn leading_spaces_stay_on_the_first_row_only() {
    assert_eq!(wrapped("  a b", 10, 10, true), vec!["  a b"]);
    assert_eq!(wrapped("a\n  b", 10, 10, true), vec!["a", "b"]);
}

#[test]
fn wrapping_by_cells_ignores_words() {
    assert_eq!(wrapped("ab cd", 3, 3, false), vec!["ab ", "cd"]);
    assert_eq!(wrapped("a\n\nb", 3, 3, false), vec!["a", "", "b"]);
}

/// The text of `line` in the cells `cols`.
fn cells(line: &Line<'_>, cols: std::ops::Range<u16>) -> String {
    text(line)
        .chars()
        .skip(usize::from(cols.start))
        .take(usize::from(cols.end.saturating_sub(cols.start)))
        .collect()
}

#[test]
fn a_code_block_in_a_quote_keeps_the_bars_and_its_copy_cells() {
    for (markdown, bars) in [
        ("> ```rust\n> let a = 1;\n> ```", "│ "),
        ("> > ```rust\n> > let a = 1;\n> > ```", "│ │ "),
    ] {
        let rendered = render(markdown, 24);
        let lines: Vec<String> = rendered.lines.iter().map(text).collect();
        assert_eq!(lines.len(), 4, "{lines:?}");
        for line in &lines {
            assert!(line.starts_with(bars), "{line:?}");
            assert_eq!(line.chars().count(), 24, "{line:?}");
        }
        let tail = 24usize.saturating_sub(bars.chars().count());
        assert_eq!(
            lines[1],
            format!("{bars} {:<w$}click to copy", "rust", w = tail - 14)
        );
        assert!(lines[2].starts_with(&format!("{bars}   1  let a = 1;")));
        assert_eq!(style_at(&rendered.lines[0], 0).fg, fg(Role::Muted));
        let target = rendered.target(0).expect("a copy target");
        assert_eq!(target.line, 1);
        assert_eq!(target.cols, 11..24);
        assert_eq!(cells(&rendered.lines[1], target.cols), "click to copy");
    }
}

#[test]
fn a_table_in_a_quote_keeps_the_bars_and_shrinks_inside_them() {
    let markdown = "> | aaaaaaaaaa | bbbbbbb |\n> |---|---|\n> | x | y |";
    assert_eq!(
        texts(markdown, 19),
        vec![
            "│ aaaaaaa   bbbbbbb",
            "│ aaa              ",
            "│ ───────   ───────",
            "│ x         y      ",
        ]
    );
}

#[test]
fn a_rule_in_a_quote_fills_the_width_inside_the_bars() {
    assert_eq!(texts("> ***", 8), vec!["│ ──────"]);
    assert_eq!(texts("> > ***", 8), vec!["│ │ ────"]);
}

#[test]
fn a_link_in_a_table_cell_is_a_link() {
    let rendered = render("| a | b |\n|---|---|\n| [x](http://x.example) | y |", 40);
    assert_eq!(rendered.text.len(), rendered.lines.len());
    let links: RowLinks = rendered
        .text
        .iter()
        .flat_map(|text| text.links.clone())
        .collect();
    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(links[0].1, "http://x.example");
}

#[test]
fn a_link_in_a_table_cell_draws_in_accent() {
    let rendered = render("| a | b |\n|---|---|\n| [x](http://x.example) | y |", 40);
    let row = &rendered.lines[2];
    assert_eq!(text(row), "x   y");
    // The link cell in the accent colour, underlined; the plain cell
    // in the text colour, not underlined.
    let link = style_at(row, 0);
    assert_eq!(link.fg, fg(Role::Accent));
    assert!(link.add_modifier.contains(Modifier::UNDERLINED));
    let plain = style_at(row, 4);
    assert_eq!(plain.fg, fg(Role::Text));
    assert!(!plain.add_modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn a_link_in_a_wrapped_table_cell_covers_both_rows() {
    let markdown = "| key | description |\n|---|---|\n| k | [a very long link text that wraps](http://example.com/long) |";
    let rendered = render(markdown, 24);
    assert_eq!(rendered.text.len(), rendered.lines.len());
    let rows: Vec<(String, RowLinks)> = rendered
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .zip(rendered.text.iter().map(|text| text.links.clone()))
        .collect();
    let hits: Vec<&(String, RowLinks)> = rows
        .iter()
        .filter(|(_, links)| {
            links
                .iter()
                .any(|(_, url)| url == "http://example.com/long")
        })
        .collect();
    assert!(hits.len() >= 2, "{rows:?}");
}

#[test]
fn a_link_with_no_cells_to_draw_has_no_link_entry() {
    // A zero-width character draws no cell, so the link has no columns.
    let rendered = render("[\u{200b}](http://x.example)", 40);
    assert!(
        rendered.text.iter().all(|text| text.links.is_empty()),
        "{:?}",
        rendered.text
    );
}

/// Boxed mode on through the shared seam: `NO_COLOR` set and not empty,
/// agreeing with `look::depth` (`docs/tui.md`, "Look").
fn no_color() {
    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    assert!(crate::surface::boxed());
}

/// Coloured mode on through the shared seam.
fn coloured() {
    crate::surface::init(&|name: &str| (name == "COLORTERM").then(|| "truecolor".to_owned()));
    assert!(!crate::surface::boxed());
}

/// The NO_COLOR look: every role paints to the terminal's default.
fn no_color_look() -> crate::look::Look {
    let (look, notice) = crate::look::Look::new(crate::look::ThemeSetting::Dark, &|name: &str| {
        (name == "NO_COLOR").then(|| "1".to_owned())
    });
    assert_eq!(notice, None);
    look
}

/// The rendered lines on a buffer, painted without colour.
fn painted(rendered: &super::Rendered) -> ratatui::buffer::Buffer {
    use ratatui::layout::Rect;
    let width = rendered
        .lines
        .iter()
        .map(ratatui::text::Line::width)
        .max()
        .unwrap_or(0);
    let mut buf = ratatui::buffer::Buffer::empty(Rect::new(
        0,
        0,
        u16::try_from(width).unwrap_or(u16::MAX),
        u16::try_from(rendered.lines.len()).unwrap_or(u16::MAX),
    ));
    for (y, line) in rendered.lines.iter().enumerate() {
        buf.set_line(
            0,
            u16::try_from(y).unwrap_or(u16::MAX),
            line,
            buf.area.width,
        );
    }
    no_color_look().paint(&mut buf);
    buf
}

/// The code block's source rebuilt from its body rows' text metadata:
/// each row's text without its decoration, joined as its joins say. The
/// reserved sides are decoration, so they never reach the logical copy.
fn code_text(rendered: &super::Rendered, rows: std::ops::Range<usize>) -> String {
    use crate::rows::Join;
    let mut out = String::new();
    for at in rows {
        let meta = &rendered.text[at];
        let (_, row) = crate::logical::line_text(&rendered.lines[at], meta);
        match meta.join {
            Join::Break if !out.is_empty() => out.push('\n'),
            Join::WrapSpace if !out.is_empty() && !row.is_empty() => out.push(' '),
            Join::Break | Join::Wrap | Join::WrapSpace => {}
        }
        out.push_str(&row);
    }
    out
}

#[test]
fn no_color_code_block_reserves_side_columns() {
    no_color();
    let markdown = "```rust\nfn main() {}\nlet x = 1;\n```";
    let rendered = render(markdown, 30);
    assert_eq!(
        rendered.lines.iter().map(text).collect::<Vec<_>>(),
        vec![
            "┌────────────────────────────┐",
            "│ rust          click to copy│",
            "│   1  fn main() {}          │",
            "│   2  let x = 1;            │",
            "└────────────────────────────┘",
        ]
    );
    for line in &rendered.lines {
        assert_eq!(line.width(), 30);
    }
    // The edge rows box through the shared helper; the header and the
    // body keep their rows with one reserved column on each side.
    assert!(rendered.text[0].decoration);
    assert!(rendered.text[1].decoration);
    assert!(!rendered.text[2].decoration);
    assert!(!rendered.text[3].decoration);
    assert!(rendered.text[4].decoration);
    // The skip covers the leading side past the bars and the number
    // gutter, which is kept; the tail covers the trailing side, so the
    // logical copy keeps the source bytes without the border.
    assert_eq!(rendered.text[2].skip, 7);
    assert_eq!(rendered.text[3].skip, 7);
    assert_eq!(rendered.text[2].tail, 1);
    assert_eq!(rendered.text[3].tail, 1);
    // The sides draw dim with no role marker; the interior keeps the
    // tint markers until the paint pass, and the edge rows carry no
    // role marker.
    for line in &rendered.lines[1..4] {
        for col in [0, 29] {
            let side = style_at(line, col);
            assert_eq!(text(line).chars().nth(col), Some('│'), "{line:?}");
            assert_eq!(side.bg, None, "col {col} of {:?}", text(line));
            assert_eq!(side.fg, None, "col {col} of {:?}", text(line));
            assert!(
                side.add_modifier.contains(Modifier::DIM),
                "col {col} of {:?}",
                text(line)
            );
        }
        for col in 1..29 {
            assert_eq!(
                style_at(line, col).bg,
                TINT,
                "col {col} of {:?}",
                text(line)
            );
        }
    }
    assert!(
        rendered.lines[0]
            .spans
            .iter()
            .all(|span| span.style.fg.is_none())
    );
    // The number gutter and the highlight roles are kept, one column in.
    assert_eq!(style_at(&rendered.lines[2], 7).fg, fg(Role::Keyword));
    assert_eq!(style_at(&rendered.lines[2], 10).fg, fg(Role::Function));
    assert_eq!(style_at(&rendered.lines[3], 15).fg, fg(Role::Number));
    // The copy target moves with the narrowed interior: its cells spell
    // the label exactly, never a border, and the code is unchanged.
    assert_eq!(
        rendered.targets,
        vec![CopyTarget {
            line: 1,
            cols: 16..29,
            code: "fn main() {}\nlet x = 1;".to_owned(),
        }]
    );
    assert_eq!(cells(&rendered.lines[1], 16..29), "click to copy");
    // Every readable source glyph survives wrapping.
    assert_eq!(code_text(&rendered, 2..4), "fn main() {}\nlet x = 1;");
}

#[test]
fn no_color_quoted_code_keeps_its_bars_inside_the_box() {
    no_color();
    for (markdown, bars, cols) in [
        ("> ```rust\n> let a = 1;\n> ```", "│ ", 10..23),
        ("> > ```rust\n> > let a = 1;\n> > ```", "│ │ ", 10..23),
    ] {
        let rendered = render(markdown, 24);
        let lines: Vec<String> = rendered.lines.iter().map(text).collect();
        assert_eq!(lines.len(), 4, "{lines:?}");
        for line in &lines {
            assert!(line.starts_with(bars), "{line:?}");
            assert_eq!(line.chars().count(), 24, "{line:?}");
        }
        // The bars stay outside the slab's own rows: the edge rows box
        // inside them and the slab draws its own dim sides past them.
        assert!(lines[0].starts_with(&format!("{bars}┌")));
        assert!(lines[0].ends_with('┐'));
        assert!(lines[3].starts_with(&format!("{bars}└")));
        assert!(lines[3].ends_with('┘'));
        assert!(lines[1].starts_with(&format!("{bars}│")));
        assert!(lines[1].ends_with('│'));
        assert!(lines[2].starts_with(&format!("{bars}│")));
        assert!(lines[2].ends_with('│'));
        // The slab's sides draw dim with no role marker, past the bars.
        let bar_cells = u16::try_from(bars.chars().count()).unwrap_or(u16::MAX);
        for line in &rendered.lines[1..3] {
            for col in [bar_cells, 23] {
                let side = style_at(line, usize::from(col));
                assert_eq!(side.bg, None, "col {col} of {line:?}");
                assert_eq!(side.fg, None, "col {col} of {line:?}");
                assert!(
                    side.add_modifier.contains(Modifier::DIM),
                    "col {col} of {line:?}"
                );
            }
        }
        assert_eq!(cells(&rendered.lines[1], cols.clone()), "click to copy");
        let target = rendered.target(0).expect("a copy target");
        assert_eq!(target.cols, cols);
        assert_eq!(target.code, "let a = 1;");
        assert!(rendered.text[2].skip > u16::try_from(bars.chars().count()).unwrap_or(u16::MAX));
        assert_eq!(rendered.text[2].tail, 1);
        assert_eq!(code_text(&rendered, 2..3), "let a = 1;");
    }
}

#[test]
fn no_color_long_wide_and_tabbed_code_wrap_inside_the_sides() {
    no_color();
    let lines = texts("```\nabcdefghij\n```", 10);
    assert_eq!(
        lines,
        vec![
            "┌────────┐",
            "│        │",
            "│   1  ab│",
            "│      cd│",
            "│      ef│",
            "│      gh│",
            "│      ij│",
            "└────────┘",
        ]
    );
    let rendered = render("```\nabcdefghij\n```", 10);
    assert_eq!(code_text(&rendered, 2..7), "abcdefghij");
    let lines = texts("```\n語語語\n```", 10);
    assert_eq!(
        lines,
        vec![
            "┌────────┐",
            "│        │",
            "│   1  語│",
            "│      語│",
            "│      語│",
            "└────────┘",
        ]
    );
    let rendered = render("```\n語語語\n```", 10);
    for line in &rendered.lines {
        assert_eq!(line.width(), 10);
    }
    assert_eq!(code_text(&rendered, 2..5), "語語語");
    // Tabs expand to stops of four as with colour, inside the sides.
    let lines = texts("```\n\tx\nab\ty\n```", 16);
    assert_eq!(lines[2], "│   1      x   │");
    assert_eq!(lines[3], "│   2  ab  y   │");
    let rendered = render("```\n\tx\nab\ty\n```", 16);
    assert_eq!(code_text(&rendered, 2..4), "    x\nab  y");
}

#[test]
fn no_color_header_clips_its_label_and_copy_needs_the_interior() {
    no_color();
    // Too narrow for the thirteen-cell label in the interior: no copy
    // target and neither label nor copy.
    let narrow = render("```typescript\nx\n```", 14);
    assert!(narrow.targets.is_empty());
    assert_eq!(text(&narrow.lines[1]), "│            │");
    // Just enough interior: the label fits exactly between the sides.
    let exact = render("```typescript\nx\n```", 15);
    assert_eq!(exact.targets[0].cols, 1..14);
    assert_eq!(text(&exact.lines[1]), "│click to copy│");
    assert_eq!(cells(&exact.lines[1], 1..14), "click to copy");
    assert_eq!(exact.targets[0].code, "x");
    // One more column: the lead blank fits too, still no label room.
    let lead = render("```typescript\nx\n```", 16);
    assert_eq!(lead.targets[0].cols, 2..15);
    assert_eq!(text(&lead.lines[1]), "│ click to copy│");
    assert_eq!(cells(&lead.lines[1], 2..15), "click to copy");
    // A label without room truncates with `…`, one column in.
    assert_eq!(
        text(&render("```typescript\nx\n```", 18).lines[1]),
        "│ … click to copy│"
    );
    assert_eq!(render("```typescript\nx\n```", 18).targets[0].cols, 4..17);
    // A wide label truncates by cells, inside the sides: the label is
    // the thirteen interior cells before the reserved side (`cells`
    // counts characters, so a wide row ends the check by cells).
    let wide = render("```語語語語\nx\n```", 22);
    assert_eq!(text(&wide.lines[1]), "│ 語語… click to copy│");
    assert_eq!(wide.targets[0].cols, 8..21);
    assert!(text(&wide.lines[1]).ends_with("click to copy│"));
    for width in [14, 15, 16, 18, 22] {
        for line in render("```typescript\nx\n```", width).lines {
            assert_eq!(line.width(), usize::from(width), "{width}");
        }
    }
}

#[test]
fn no_color_empty_and_unclosed_fences_box() {
    no_color();
    let rendered = render("```\nx\n```", 10);
    assert_eq!(
        rendered.lines.iter().map(text).collect::<Vec<_>>(),
        vec!["┌────────┐", "│        │", "│   1  x │", "└────────┘",]
    );
    assert!(rendered.targets.is_empty());
    assert!(rendered.text[1].decoration);
    assert_eq!(rendered.text[2].skip, 7);
    assert_eq!(code_text(&rendered, 2..3), "x");
    let rendered = render("text\n\n```py\nx = 1\ny", 20);
    assert_eq!(
        rendered.lines.iter().map(text).collect::<Vec<_>>(),
        vec![
            "text",
            "",
            "┌──────────────────┐",
            "│ py  click to copy│",
            "│   1  x = 1       │",
            "│   2  y           │",
            "└──────────────────┘",
        ]
    );
    assert_eq!(rendered.targets[0].line, 3);
    assert_eq!(rendered.targets[0].cols, 6..19);
    assert_eq!(rendered.targets[0].code, "x = 1\ny");
    assert_eq!(code_text(&rendered, 4..6), "x = 1\ny");
}

#[test]
fn no_color_tiny_widths_keep_the_text_first_fallback() {
    coloured();
    let coloured_renders: Vec<super::Rendered> = (0..6u16)
        .map(|width| render("```rust\nfn main() {}\n```", width))
        .collect();
    no_color();
    // Too narrow for two sides and text: the colourless render is the
    // coloured one, half-block edges included.
    for (at, width) in (0..3u16).enumerate() {
        assert_eq!(
            render("```rust\nfn main() {}\n```", width),
            coloured_renders[at]
        );
    }
    // A box fits from three columns: the edges box and every readable
    // source glyph survives. Spaces at a wrap edge trim the way they do
    // with colour, so tiny widths compare without them, boxed and
    // coloured alike.
    let kept = |rendered: &super::Rendered| {
        let body = 2..rendered.lines.len().saturating_sub(1);
        code_text(rendered, body)
            .chars()
            .filter(|ch| *ch != ' ')
            .collect::<String>()
    };
    for width in 3..6u16 {
        let rendered = render("```rust\nfn main() {}\n```", width);
        assert!(text(&rendered.lines[0]).starts_with('┌'), "{width}");
        assert!(text(&rendered.lines[0]).ends_with('┐'), "{width}");
        assert!(text(rendered.lines.last().expect("a bottom edge")).starts_with('└'));
        assert_eq!(kept(&rendered), "fnmain(){}");
        assert_eq!(kept(&rendered), kept(&coloured_renders[width as usize]));
    }
}

#[test]
fn no_color_copy_cells_spell_the_label_and_exclude_borders() {
    no_color();
    let rendered = render("```\na\n```\n\ntext\n\n```sh\nb\n```", 20);
    // Border rows stay at their existing positions: the headers are the
    // same rows as with colour.
    let lines: Vec<usize> = rendered.targets.iter().map(|t| t.line).collect();
    assert_eq!(lines, vec![1, 8]);
    assert_eq!(text(&rendered.lines[1]), "│     click to copy│");
    assert_eq!(text(&rendered.lines[8]), "│ sh  click to copy│");
    assert_eq!(
        rendered.targets,
        vec![
            CopyTarget {
                line: 1,
                cols: 6..19,
                code: "a".to_owned(),
            },
            CopyTarget {
                line: 8,
                cols: 6..19,
                code: "b".to_owned(),
            },
        ]
    );
    for target in &rendered.targets {
        assert_eq!(
            cells(&rendered.lines[target.line], target.cols.clone()),
            "click to copy"
        );
        assert!(target.cols.start >= 1, "{target:?}");
        assert!(target.cols.end <= 19, "{target:?}");
    }
    assert_eq!(rendered.targets[0].code, "a");
    assert_eq!(rendered.targets[1].code, "b");
    assert_eq!(code_text(&rendered, 2..3), "a");
    assert_eq!(code_text(&rendered, 9..10), "b");
}

#[test]
fn no_color_code_paints_dim_borders_and_default_text() {
    use ratatui::style::{Color, Modifier};
    no_color();
    let rendered = render("```rust\n// one holder\nlet x = 1;\n```", 30);
    // Colourless decoration keeps its role markers until the paint pass.
    assert_eq!(style_at(&rendered.lines[2], 7).bg, TINT);
    assert!(
        rendered.lines[0]
            .spans
            .iter()
            .all(|span| span.style.fg.is_none())
    );
    assert!(
        rendered.lines[0]
            .spans
            .iter()
            .all(|span| { span.style.add_modifier.contains(Modifier::DIM) })
    );
    let buf = painted(&rendered);
    // The box edges paint dim on the terminal's default colours.
    for (x, symbol) in [(0, "┌"), (15, "─"), (29, "┐")] {
        let cell = &buf[(x, 0)];
        assert_eq!(cell.symbol(), symbol, "({x}, 0)");
        assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset), "({x}, 0)");
        assert!(cell.modifier.contains(Modifier::DIM), "({x}, 0)");
    }
    let bottom = &buf[(0, 4)];
    assert_eq!(bottom.symbol(), "└");
    assert_eq!((bottom.fg, bottom.bg), (Color::Reset, Color::Reset));
    assert!(bottom.modifier.contains(Modifier::DIM));
    // The slab's own sides paint as dim │ on the terminal's default
    // colours; the body stays readable on the default colours.
    for y in 1..4u16 {
        for x in [0, 29] {
            let cell = &buf[(x, y)];
            assert_eq!(cell.symbol(), "│", "({x}, {y})");
            assert_eq!(
                (cell.fg, cell.bg),
                (Color::Reset, Color::Reset),
                "({x}, {y})"
            );
            assert!(cell.modifier.contains(Modifier::DIM), "({x}, {y})");
        }
    }
    assert_eq!(buf[(8, 2)].symbol(), "/");
    assert_eq!(
        (buf[(8, 2)].fg, buf[(8, 2)].bg),
        (Color::Reset, Color::Reset)
    );
    // Modifiers survive the paint: the comment stays italic, the muted
    // header stays dim.
    assert!(buf[(8, 2)].modifier.contains(Modifier::ITALIC));
    assert!(!buf[(15, 3)].modifier.contains(Modifier::DIM));
    assert!(buf[(2, 1)].modifier.contains(Modifier::DIM));
    assert_eq!(code_text(&rendered, 2..4), "// one holder\nlet x = 1;");
}

/// A finished turn holding `text`, rendered through the conversation
/// and painted without colour.
fn conversation(text: &str) -> ratatui::buffer::Buffer {
    use std::path::PathBuf;

    use crate::link::Line as LinkLine;

    const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    let mut app = crate::app::App::new(PathBuf::from("/w"));
    app.set_size(80, 24);
    app.attach(contract::SessionId(SESSION.to_owned()));
    let feed = |app: &mut crate::app::App,
                kind: &str,
                action: Option<&str>,
                payload: serde_json::Value| {
        app.on_line(LinkLine::Session(contract::Envelope {
            kind: kind.to_owned(),
            session_id: contract::SessionId(SESSION.to_owned()),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: action.map(|id| contract::ActionId(id.to_owned())),
            seq: None,
            payload: payload.as_object().cloned().unwrap_or_default(),
        }));
    };
    feed(
        &mut app,
        "turn_started",
        None,
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    );
    feed(
        &mut app,
        "text_completed",
        Some("a_1"),
        serde_json::json!({"text": text}),
    );
    feed(
        &mut app,
        "turn_completed",
        None,
        serde_json::json!({"outcome": "completed"}),
    );
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    no_color_look().paint(&mut buf);
    buf
}

/// A dim box side on the terminal's default colours.
fn assert_side(buf: &ratatui::buffer::Buffer, x: u16, y: u16) {
    use ratatui::style::{Color, Modifier};
    let cell = &buf[(x, y)];
    assert_eq!(cell.symbol(), "│", "({x}, {y})");
    assert_eq!(
        (cell.fg, cell.bg),
        (Color::Reset, Color::Reset),
        "({x}, {y})"
    );
    assert!(cell.modifier.contains(Modifier::DIM), "({x}, {y})");
}

#[test]
fn no_color_code_slabs_draw_their_own_sides_in_the_conversation() {
    use ratatui::style::{Color, Modifier};
    no_color();
    for markdown in ["```rust\nlet a = 1;\n```", "> ```rust\n> let a = 1;\n> ```"] {
        let buf = conversation(markdown);
        let rows: Vec<String> = (0..24u16)
            .map(|y| (0..80u16).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let y = u16::try_from(
            rows.iter()
                .position(|row| row.contains("let a = 1;"))
                .expect("the code's row"),
        )
        .expect("a row");
        let row = &rows[usize::from(y)];
        let glyphs: Vec<char> = row.chars().collect();
        let needle: Vec<char> = "let a = 1;".chars().collect();
        let at = glyphs
            .windows(needle.len())
            .position(|window| window == needle.as_slice())
            .expect("the code");
        // The number gutter and the slab's own leading side sit before
        // the code: `│   1  `, nested inside the turn's outer side.
        let left = u16::try_from(at).expect("a column").saturating_sub(7);
        assert_side(&buf, left, y);
        assert!(left > 0, "the slab nests inside the turn's side");
        assert_eq!(buf[(0, y)].symbol(), "│", "{row:?}");
        // The slab's trailing side ends the code row before the turn's
        // own right side: the last box side inside it.
        let right = glyphs[..78]
            .iter()
            .rposition(|glyph| *glyph == '│')
            .map(|at| u16::try_from(at).expect("a column"))
            .expect("the slab's side");
        assert_side(&buf, right, y);
        assert!(right < 78, "the slab nests inside the turn's side");
        assert_eq!(buf[(78, y)].symbol(), "│", "{row:?}");
        // The slab's edge rows box at the slab's own columns, inside
        // the turn's edges.
        let top = (0..y)
            .rev()
            .find(|top| buf[(left, *top)].symbol() == "┌")
            .expect("the slab's top edge");
        assert_eq!(buf[(right, top)].symbol(), "┐");
        let bottom = (y + 1..24)
            .find(|bottom| buf[(left, *bottom)].symbol() == "└")
            .expect("the slab's bottom edge");
        assert_eq!(buf[(right, bottom)].symbol(), "┘");
        for x in [left, right] {
            for edge in [top, bottom] {
                let cell = &buf[(x, edge)];
                assert_eq!(
                    (cell.fg, cell.bg),
                    (Color::Reset, Color::Reset),
                    "({x}, {edge})"
                );
                assert!(cell.modifier.contains(Modifier::DIM), "({x}, {edge})");
            }
        }
        if markdown.starts_with('>') {
            // The quote bars stay outside the slab's own sides: the
            // turn's side, the bar, then the slab's side.
            assert!(row.starts_with("││ │"), "{row:?}");
        } else {
            assert!(row.starts_with("││   1  "), "{row:?}");
        }
    }
}
