//! Tests for the markdown renderer.

use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use super::{CopyTarget, Role, render, style};

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

const TINT: Option<ratatui::style::Color> = Some(ratatui::style::Color::Rgb(33, 37, 43));

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
    assert!(style_at(line, 2).add_modifier.contains(Modifier::ITALIC));
    assert!(
        style_at(line, 4)
            .add_modifier
            .contains(Modifier::CROSSED_OUT)
    );
    assert_eq!(style_at(line, 6).fg, fg(Role::CodeText));
    assert_eq!(style_at(line, 6).bg, TINT);
    assert!(
        style_at(line, 8)
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
    assert_eq!(text(line), "b i s c l");
    let plain = style_at(line, 1);
    assert_eq!(plain, style(Role::Text));
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
    assert_eq!(style_at(&rendered.lines[1], 0).fg, fg(Role::Dim));
}

#[test]
fn a_horizontal_rule_spans_the_width_dim() {
    let rendered = render("a\n\n---\n\nb", 5);
    assert_eq!(text(&rendered.lines[2]), "─────");
    assert_eq!(style_at(&rendered.lines[2], 4).fg, fg(Role::Dim));
}

#[test]
fn a_code_block_has_a_header_numbered_lines_and_the_tint() {
    let markdown = "```rust\nfn main() {}\nlet x = 1;\n```";
    let rendered = render(markdown, 20);
    assert_eq!(
        rendered.lines.iter().map(text).collect::<Vec<_>>(),
        vec![
            "rust            copy",
            "1 │ fn main() {}    ",
            "2 │ let x = 1;      ",
        ]
    );
    for line in &rendered.lines {
        assert_eq!(line.width(), 20);
        for col in 0..20 {
            assert_eq!(style_at(line, col).bg, TINT, "{:?} col {col}", text(line));
        }
    }
    assert_eq!(style_at(&rendered.lines[0], 0).fg, fg(Role::Dim));
    assert_eq!(style_at(&rendered.lines[0], 16).fg, fg(Role::Accent));
    assert_eq!(style_at(&rendered.lines[1], 0).fg, fg(Role::Dim));
    assert_eq!(style_at(&rendered.lines[1], 4).fg, fg(Role::Keyword));
    assert_eq!(style_at(&rendered.lines[1], 7).fg, fg(Role::Function));
    assert_eq!(style_at(&rendered.lines[2], 12).fg, fg(Role::Number));
    assert_eq!(
        rendered.targets,
        vec![CopyTarget {
            line: 0,
            cols: 16..20,
            code: "fn main() {}\nlet x = 1;".to_owned(),
        }]
    );
}

#[test]
fn line_numbers_right_align_to_the_widest() {
    let code: String = (1..=10).map(|n| format!("l{n}\n")).collect();
    let lines = texts(&format!("```\n{code}```"), 12);
    assert_eq!(lines[1], " 1 │ l1     ");
    assert_eq!(lines[9], " 9 │ l9     ");
    assert_eq!(lines[10], "10 │ l10    ");
}

#[test]
fn an_unknown_or_empty_fence_draws_plain_code_with_its_tag_as_label() {
    let rendered = render("```brainfuck\n+++\n```", 16);
    assert_eq!(text(&rendered.lines[0]), "brainfuck   copy");
    assert_eq!(style_at(&rendered.lines[1], 4).fg, fg(Role::CodeText));
    assert_eq!(texts("```\nx\n```", 10)[0], "      copy");
    assert_eq!(texts("    indented", 10)[0], "      copy");
}

#[test]
fn an_unclosed_fence_is_code_to_the_end() {
    let rendered = render("text\n\n```py\nx = 1\ny", 12);
    assert_eq!(
        rendered.lines.iter().map(text).collect::<Vec<_>>(),
        vec!["text", "", "py      copy", "1 │ x = 1   ", "2 │ y       "]
    );
    assert_eq!(rendered.targets[0].code, "x = 1\ny");
    assert_eq!(rendered.targets[0].line, 2);
}

#[test]
fn a_long_code_line_wraps_with_unnumbered_continuations() {
    let lines = texts("```\nabcdefghij\n```", 10);
    assert_eq!(lines[1..], ["1 │ abcdef", "  │ ghij  "]);
}

#[test]
fn tabs_in_code_expand_to_stops_of_four() {
    let lines = texts("```\n\tx\nab\ty\n```", 16);
    assert_eq!(lines[1], "1 │     x       ");
    assert_eq!(lines[2], "2 │ ab  y       ");
}

#[test]
fn the_header_truncates_its_label_and_drops_it_then_copy_when_narrow() {
    let header = |width| render("```typescript\nx\n```", width);
    assert_eq!(text(&header(10).lines[0]), "type… copy");
    assert_eq!(header(10).targets[0].cols, 6..10);
    assert_eq!(text(&header(6).lines[0]), "… copy");
    assert_eq!(text(&header(5).lines[0]), " copy");
    assert_eq!(header(5).targets[0].cols, 1..5);
    assert_eq!(text(&header(4).lines[0]), "copy");
    assert_eq!(header(4).targets[0].cols, 0..4);
    assert_eq!(text(&header(3).lines[0]), "   ");
    assert!(header(3).targets.is_empty());
    for width in 1..30 {
        assert_eq!(
            header(width).lines[0].width(),
            usize::from(width),
            "{width}"
        );
    }
}

#[test]
fn a_wide_label_truncates_by_cells() {
    assert_eq!(texts("```語語語語\nx\n```", 10)[0], "語語… copy");
}

#[test]
fn copy_targets_point_at_each_blocks_header_line() {
    let rendered = render("```\na\n```\n\ntext\n\n```sh\nb\n```", 20);
    let lines: Vec<usize> = rendered.targets.iter().map(|t| t.line).collect();
    assert_eq!(lines, vec![0, 5]);
    assert_eq!(text(&rendered.lines[5]), "sh              copy");
    assert_eq!(rendered.targets[1].code, "b");
}

#[test]
fn a_table_has_a_bold_header_a_rule_and_right_aligned_numbers() {
    let markdown = "| name | size |\n|---|---|\n| a | 1,200 |\n| bb | 7 |";
    let rendered = render(markdown, 40);
    assert_eq!(
        rendered.lines.iter().map(text).collect::<Vec<_>>(),
        vec!["name   size", "───────────", "a     1,200", "bb        7"]
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
    assert_eq!(style_at(&rendered.lines[1], 0).fg, fg(Role::Dim));
}

#[test]
fn numeric_columns_take_signs_and_percentages_but_not_units() {
    let lines = texts(
        "| a | b | c |\n|---|---|---|\n| -1.5 | 3ms | |\n| +20% | 4 | x |",
        40,
    );
    assert_eq!(lines[2], "-1.5  3ms   ");
    assert_eq!(lines[3], "+20%  4    x");
    let lines = texts("| a | b |\n|---|---|\n| 1 | - |", 40);
    assert_eq!(lines[2], "1  -");
}

#[test]
fn the_alignment_row_is_honoured() {
    let lines = texts("| left | right | mid |\n|:--|--:|:-:|\n| a | b | c |", 40);
    assert_eq!(lines[2], "a         b   c ");
}

#[test]
fn a_wide_table_shrinks_its_widest_column_and_wraps_its_cells() {
    let markdown = "| key | description |\n|---|---|\n| k | one two three four |";
    let rendered = render(markdown, 15);
    assert_eq!(
        rendered.lines.iter().map(text).collect::<Vec<_>>(),
        vec![
            "key  descriptio",
            "     n         ",
            "───────────────",
            "k    one two   ",
            "     three four",
        ]
    );
}

#[test]
fn shrinking_stops_at_six_columns_each() {
    let markdown = "| aaaaaaaa | bbbbbbbb |\n|---|---|\n| x | y |";
    let lines = texts(markdown, 4);
    assert_eq!(lines[0], "aaaaaa  bbbbbb");
    assert_eq!(lines[1], "aa      bb    ");
    assert_eq!(lines[2], "──────────────");
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
    assert_eq!(lines[1..], ["1 │ 語語", "  │ 語  "]);
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
    assert_eq!(lines[1], "        copy");
    let lines = texts("- a\n  | x | y |\n  |---|---|\n  | 1 | 2 |", 20);
    assert_eq!(lines[0], "• a");
    assert_eq!(lines[1], "x  y");
}

#[test]
fn a_soft_break_is_a_space() {
    assert_eq!(texts("one\ntwo", 40), vec!["one two"]);
}

#[test]
fn the_label_is_the_info_strings_first_word() {
    assert_eq!(texts("```rust,ignore\nx\n```", 14)[0], "rust      copy");
    assert_eq!(texts("```py title\nx\n```", 14)[0], "py        copy");
}

#[test]
fn a_tie_for_widest_shrinks_the_first_column() {
    let lines = texts("| aaaaaaaa | bbbbbbbb |\n|---|---|\n| x | y |", 17);
    assert_eq!(lines[0], "aaaaaaa  bbbbbbbb");
    assert_eq!(lines[1], "a                ");
}

#[test]
fn a_tab_in_prose_is_a_space_and_a_quote_ends() {
    assert_eq!(texts("a\tb", 40), vec!["a b"]);
    assert_eq!(texts("> b\n\nc", 40), vec!["│ b", "", "c"]);
}

#[test]
fn an_item_that_opens_with_a_code_block_shows_its_marker_first() {
    let lines = texts("- ```\n  x\n  ```\n- b", 12);
    assert_eq!(lines, vec!["• ", "        copy", "1 │ x       ", "• b"]);
}

#[test]
fn the_widest_of_unequal_columns_shrinks_first() {
    let lines = texts("| aaaaaaaaaa | bbbbbbb |\n|---|---|\n| x | y |", 17);
    assert_eq!(lines[0], "aaaaaaaa  bbbbbbb");
}

/// `text` wrapped by [`super::wrap_cells`], each row as a string.
fn wrapped(text: &str, first: usize, rest: usize, words: bool) -> Vec<String> {
    let cells: Vec<(char, Style)> = text.chars().map(|ch| (ch, Style::default())).collect();
    super::wrap_cells(&cells, first, rest, words)
        .iter()
        .map(|row| row.iter().map(|(ch, _)| ch).collect())
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
        assert_eq!(lines.len(), 2, "{lines:?}");
        for line in &lines {
            assert!(line.starts_with(bars), "{line:?}");
            assert_eq!(line.chars().count(), 24, "{line:?}");
        }
        let tail = 24usize.saturating_sub(bars.chars().count());
        assert_eq!(lines[0], format!("{bars}{:<w$}copy", "rust", w = tail - 4));
        assert!(lines[1].starts_with(&format!("{bars}1 │ let a = 1;")));
        assert_eq!(style_at(&rendered.lines[0], 0).fg, fg(Role::Dim));
        let target = rendered.target(0).expect("a copy target");
        assert_eq!(target.line, 0);
        assert_eq!(target.cols, 20..24);
        assert_eq!(cells(&rendered.lines[0], target.cols), "copy");
    }
}

#[test]
fn a_table_in_a_quote_keeps_the_bars_and_shrinks_inside_them() {
    let markdown = "> | aaaaaaaaaa | bbbbbbb |\n> |---|---|\n> | x | y |";
    assert_eq!(
        texts(markdown, 19),
        vec![
            "│ aaaaaaaa  bbbbbbb",
            "│ aa               ",
            "│ ─────────────────",
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
