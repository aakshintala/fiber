//! Tests beside [`super::to_markdown`]: one per element class, then hostile
//! input.

use super::to_markdown;

#[test]
fn headings_take_their_level() {
    let html = "<h1>One</h1><h2>Two</h2><h3>Three</h3><h4>Four</h4><h5>Five</h5><h6>Six</h6>";
    assert_eq!(
        to_markdown(html),
        "# One\n\n## Two\n\n### Three\n\n#### Four\n\n##### Five\n\n###### Six\n"
    );
}

#[test]
fn a_heading_level_past_six_is_plain_text() {
    assert_eq!(to_markdown("<h7>x</h7>"), "x\n");
}

#[test]
fn block_elements_break_lines() {
    for name in [
        "p", "div", "section", "article", "main", "header", "footer", "nav", "aside",
    ] {
        let html = format!("<{name}>a</{name}><{name}>b</{name}>");
        assert_eq!(to_markdown(&html), "a\n\nb\n", "<{name}>");
    }
}

#[test]
fn a_br_starts_a_new_line_without_a_blank_one() {
    assert_eq!(to_markdown("a<br>b<br/>c<BR />d"), "a\nb\nc\nd\n");
}

#[test]
fn at_most_one_blank_line_separates_blocks() {
    assert_eq!(
        to_markdown("<p>a</p><p></p><div></div><p> </p><p>b</p>"),
        "a\n\nb\n"
    );
}

#[test]
fn blockquote_lines_are_prefixed() {
    assert_eq!(
        to_markdown("<p>x</p><blockquote>one<br>two</blockquote><p>y</p>"),
        "x\n\n> one\n> two\n\ny\n"
    );
}

#[test]
fn nested_blockquotes_double_the_prefix() {
    assert_eq!(
        to_markdown("<blockquote>a<blockquote>b</blockquote></blockquote>"),
        "> a\n\n> > b\n"
    );
}

#[test]
fn a_horizontal_rule_is_three_dashes() {
    assert_eq!(to_markdown("a<hr>b"), "a\n\n---\n\nb\n");
}

#[test]
fn unordered_items_are_dashes() {
    assert_eq!(to_markdown("<ul><li>a</li><li>b</li></ul>"), "- a\n- b\n");
}

#[test]
fn ordered_items_count_from_one() {
    assert_eq!(
        to_markdown("<ol><li>a</li><li>b</li><li>c</li></ol>"),
        "1. a\n2. b\n3. c\n"
    );
}

#[test]
fn nested_lists_indent_two_spaces_a_level() {
    let html = "<ul><li>a<ul><li>b<ol><li>c</li><li>d</li></ol></li></ul></li><li>e</li></ul>";
    assert_eq!(to_markdown(html), "- a\n  - b\n    1. c\n    2. d\n- e\n");
}

#[test]
fn a_second_list_restarts_its_numbers() {
    assert_eq!(
        to_markdown("<ol><li>a</li></ol><ol><li>b</li></ol>"),
        "1. a\n\n1. b\n"
    );
}

#[test]
fn a_list_is_set_off_from_the_paragraphs_around_it() {
    assert_eq!(
        to_markdown("<p>before</p><ul><li>a</li></ul><p>after</p>"),
        "before\n\n- a\n\nafter\n"
    );
}

#[test]
fn an_item_outside_a_list_is_a_dash() {
    assert_eq!(to_markdown("<li>a</li><li>b</li>"), "- a\n- b\n");
}

#[test]
fn a_link_is_text_and_its_href_as_written() {
    assert_eq!(
        to_markdown("see <a href=\"/docs?a=1&amp;b=2\">the docs</a> now"),
        "see [the docs](/docs?a=1&b=2) now\n"
    );
}

#[test]
fn a_link_reads_single_quoted_and_bare_hrefs() {
    assert_eq!(
        to_markdown("<a href='x'>one</a> <a HREF=y>two</a> <a title=\"t\" href = \"z\">three</a>"),
        "[one](x) [two](y) [three](z)\n"
    );
}

#[test]
fn a_link_text_is_one_trimmed_line() {
    assert_eq!(
        to_markdown("<a href=\"u\">  one\n<div>two</div>  </a>"),
        "[one two](u)\n"
    );
}

#[test]
fn an_anchor_without_an_href_passes_its_text_through() {
    assert_eq!(to_markdown("<a name=\"top\">top</a>"), "top\n");
}

#[test]
fn a_link_with_no_text_is_dropped() {
    assert_eq!(to_markdown("a<a href=\"u\"> </a>b"), "ab\n");
}

#[test]
fn an_unclosed_link_is_still_written() {
    assert_eq!(to_markdown("<a href=\"u\">text"), "[text](u)\n");
}

#[test]
fn an_image_is_alt_and_src() {
    assert_eq!(
        to_markdown("<img src=\"a.png\" alt=\"a &amp; b\">"),
        "![a & b](a.png)\n"
    );
}

#[test]
fn an_image_without_alt_has_empty_brackets() {
    assert_eq!(to_markdown("<img src=\"a.png\">"), "![](a.png)\n");
}

#[test]
fn an_image_without_src_is_its_alt() {
    assert_eq!(to_markdown("<img alt=\"only alt\">"), "only alt\n");
}

#[test]
fn an_image_inside_a_link_is_part_of_its_text() {
    assert_eq!(
        to_markdown("<a href=\"u\"><img src=\"i.png\" alt=\"x\"></a>"),
        "[![x](i.png)](u)\n"
    );
}

#[test]
fn emphasis_and_inline_code() {
    assert_eq!(
        to_markdown("<strong>a</strong> <b>b</b> <em>c</em> <i>d</i> <code>e f</code>"),
        "**a** **b** _c_ _d_ `e f`\n"
    );
}

#[test]
fn pre_is_a_fenced_block_with_its_text_verbatim() {
    assert_eq!(
        to_markdown("<p>x</p><pre>  a\n\n    b &lt; c\n</pre><p>y</p>"),
        "x\n\n```\n  a\n\n    b < c\n```\n\ny\n"
    );
}

#[test]
fn code_inside_pre_has_no_backticks() {
    assert_eq!(
        to_markdown("<pre><code>let x = 1;</code></pre>"),
        "```\nlet x = 1;\n```\n"
    );
}

#[test]
fn tags_inside_pre_leave_their_text_in_place() {
    assert_eq!(
        to_markdown("<pre>a <b>b</b> <a href=\"u\">c</a><br>d</pre>"),
        "```\na b c\nd\n```\n"
    );
}

#[test]
fn an_empty_pre_is_an_empty_fence() {
    assert_eq!(to_markdown("<pre></pre>"), "```\n```\n");
}

#[test]
fn a_pre_never_closed_is_closed_at_the_end() {
    assert_eq!(to_markdown("<pre>a\nb"), "```\na\nb\n```\n");
}

#[test]
fn a_pre_inside_a_pre_does_not_open_a_second_fence() {
    assert_eq!(to_markdown("<pre>a<pre>b</pre>c</pre>"), "```\nabc\n```\n");
}

#[test]
fn whitespace_collapses_outside_pre() {
    assert_eq!(to_markdown("  a \t\n  b\r\n\r\n c  "), "a b c\n");
}

#[test]
fn table_cells_are_separated_by_bars_and_rows_by_lines() {
    let html = "<table><tr><th>Name</th><th>Value</th></tr><tr><td>A</td><td>42</td></tr></table>";
    assert_eq!(to_markdown(html), "Name | Value\nA | 42\n");
}

#[test]
fn a_table_is_set_off_from_its_neighbours() {
    assert_eq!(
        to_markdown("<p>a</p><table><tr><td>x</td></tr></table><p>b</p>"),
        "a\n\nx\n\nb\n"
    );
}

#[test]
fn an_unclosed_cell_starts_the_next_one() {
    assert_eq!(
        to_markdown("<table><tr><td>a<td>b<tr><td>c</table>"),
        "a | b\nc\n"
    );
}

#[test]
fn the_title_becomes_a_leading_heading() {
    assert_eq!(
        to_markdown(
            "<html><head><title> News &amp;\n Updates </title></head><body><p>Body</p></body></html>"
        ),
        "# News & Updates\n\nBody\n"
    );
}

#[test]
fn a_title_alone_is_a_heading_line() {
    assert_eq!(to_markdown("<title>T</title>"), "# T\n");
}

#[test]
fn only_the_first_title_counts() {
    assert_eq!(
        to_markdown("<title>One</title><p>x</p><title>Two</title>"),
        "# One\n\nx\n"
    );
}

#[test]
fn an_empty_title_adds_nothing() {
    assert_eq!(to_markdown("<title> </title><p>x</p>"), "x\n");
}

#[test]
fn an_unclosed_title_does_not_swallow_the_page() {
    // The spec reads the rest of the page as the title's Rcdata text, so
    // its words, whitespace collapsed, become the leading heading.
    assert_eq!(to_markdown("<title>T<p>x</p>"), "# T<p>x</p>\n");
}

#[test]
fn a_title_tag_is_case_insensitive() {
    assert_eq!(to_markdown("<TITLE>T</TITLE>"), "# T\n");
}

#[test]
fn head_contents_other_than_the_title_are_dropped() {
    assert_eq!(
        to_markdown(
            "<head><meta charset=\"utf-8\"><link href=\"x\">stray<base href=\"/\"></head><p>x</p>"
        ),
        "x\n"
    );
}

#[test]
fn a_head_never_closed_ends_at_the_body() {
    assert_eq!(
        to_markdown("<html><head><title>T</title><body><p>x</p></body></html>"),
        "# T\n\nx\n"
    );
}

#[test]
fn script_and_style_text_is_dropped_whatever_it_holds() {
    let html = "a<script>if (1 < 2 && x > 0) { document.write('<p>hi</p>'); }</script>b\
        <style>p > a { color: red }</style>c<SCRIPT type=\"x\">1</SCRIPT>d";
    assert_eq!(to_markdown(html), "abcd\n");
}

#[test]
fn a_script_never_closed_drops_the_rest() {
    assert_eq!(to_markdown("a<script>b<p>c"), "a\n");
}

#[test]
fn a_script_with_a_tag_like_close_prefix_keeps_looking() {
    assert_eq!(to_markdown("a<script>x</scripty>y</script>b"), "ab\n");
}

#[test]
fn an_end_tag_closes_before_whitespace_or_a_slash() {
    assert_eq!(to_markdown("a<script>x</script >b"), "ab\n");
    assert_eq!(to_markdown("a<script>x</script\n>b"), "ab\n");
    assert_eq!(to_markdown("a<script>x</script/>b"), "ab\n");
}

#[test]
fn noscript_template_and_svg_are_dropped() {
    assert_eq!(
        to_markdown(
            "a<noscript>no</noscript>b<template><p>t</p></template>c<svg><g><text>s</text></g></svg>d"
        ),
        "abcd\n"
    );
}

#[test]
fn a_self_closing_svg_hides_nothing() {
    assert_eq!(to_markdown("a<svg/>b"), "ab\n");
}

#[test]
fn nested_dropped_elements_end_with_the_outer_one() {
    assert_eq!(to_markdown("a<svg><svg>x</svg>y</svg>b"), "ab\n");
}

#[test]
fn comments_doctype_and_instructions_are_dropped() {
    assert_eq!(
        to_markdown("<!DOCTYPE html><?xml version=\"1.0\"?>a<!-- <p>no</p> -->b<![CDATA[c]]>d"),
        "abd\n"
    );
}

#[test]
fn an_unterminated_comment_drops_the_rest() {
    assert_eq!(to_markdown("a<!-- b <p>c"), "a\n");
}

#[test]
fn named_entities() {
    // `&nbsp;` decodes to U+00A0 per the spec, which the converter keeps:
    // only ASCII whitespace collapses.
    assert_eq!(
        to_markdown("&amp; &lt; &gt; &quot; &apos;|&nbsp;|"),
        "& < > \" '|\u{a0}|\n"
    );
}

#[test]
fn non_breaking_spaces_are_kept_as_written() {
    // The spec decodes `&nbsp;` to U+00A0, which is not ASCII whitespace,
    // so it is kept rather than collapsed.
    assert_eq!(to_markdown("a&nbsp;&nbsp; b"), "a\u{a0}\u{a0} b\n");
}

#[test]
fn numeric_entities_decimal_and_hex() {
    assert_eq!(
        to_markdown("&#65;&#x42;&#X43;&#169;&#x1F600;"),
        "ABC\u{a9}\u{1f600}\n"
    );
}

#[test]
fn invalid_entities_are_kept_as_written() {
    for kept in ["&unknown;", "&#;", "&#xZ;", "&;"] {
        assert_eq!(
            to_markdown(&format!("a{kept}b")),
            format!("a{kept}b\n"),
            "{kept}"
        );
    }
}

#[test]
fn a_numeric_reference_without_its_semicolon_still_decodes() {
    // Unlike a named reference, `&#12` followed by `a` flushes U+000C and
    // leaves the `a;` as text per the spec; the form feed then collapses
    // to a space.
    assert_eq!(to_markdown("a&#12a;b"), "a a;b\n");
}

#[test]
fn out_of_range_numeric_entities_become_the_replacement_character() {
    // Zero, surrogates and code points past U+10FFFF decode to U+FFFD
    // per the spec, instead of being kept as written.
    for invalid in ["&#0;", "&#xD800;", "&#x110000;", "&#99999999999;"] {
        assert_eq!(
            to_markdown(&format!("a{invalid}b")),
            "a\u{fffd}b\n",
            "{invalid}"
        );
    }
}

#[test]
fn an_ampersand_that_is_not_an_entity_stays() {
    // A legacy entity without its semicolon still decodes before a
    // character that cannot continue the name, including the end of input.
    assert_eq!(
        to_markdown("AT&T said a & b; ok &amp"),
        "AT&T said a & b; ok &\n"
    );
}

#[test]
fn an_entity_is_decoded_inside_pre() {
    assert_eq!(
        to_markdown("<pre>&lt;a&gt;&#10;b</pre>"),
        "```\n<a>\nb\n```\n"
    );
}

#[test]
fn entities_have_no_length_cap() {
    // The hand-written tokenizer looked at 32 bytes at most; the spec
    // sets no limit, so a longer numeric reference decodes too.
    let fits = format!("&#x{}41;", "0".repeat(26));
    assert_eq!(fits.len(), 32);
    assert_eq!(to_markdown(&fits), "A\n");
    let long = format!("&#x{}41;", "0".repeat(27));
    assert_eq!(long.len(), 33);
    assert_eq!(to_markdown(&long), "A\n");
}

#[test]
fn unknown_tags_pass_their_text_through() {
    assert_eq!(
        to_markdown("<blink>a</blink><my-tag>b</my-tag><x:y>c</x:y>"),
        "abc\n"
    );
}

#[test]
fn text_with_a_less_than_that_is_not_a_tag_is_kept() {
    assert_eq!(to_markdown("1 < 2 and 3 <4> 5 <"), "1 < 2 and 3 <4> 5 <\n");
}

#[test]
fn a_tag_with_no_closing_bracket_is_dropped_at_eof() {
    // The spec drops a tag the end of input cuts off, instead of keeping
    // it as text.
    assert_eq!(to_markdown("a <b c=d"), "a\n");
}

#[test]
fn unclosed_tags_do_not_stop_the_text() {
    assert_eq!(
        to_markdown("<div><span>one <b>two <i>three"),
        "one **two _three\n"
    );
}

#[test]
fn stray_closing_tags_are_harmless() {
    assert_eq!(
        to_markdown("a</a></ul></li></pre></blockquote></td></svg>b"),
        "a\n\nb\n"
    );
}

#[test]
fn nothing_in_gives_nothing_out() {
    assert_eq!(to_markdown(""), "");
    assert_eq!(to_markdown("   \n "), "");
    assert_eq!(to_markdown("<p></p><script>x</script>"), "");
}

#[test]
fn the_output_ends_with_one_newline() {
    assert_eq!(to_markdown("a\n\n\n"), "a\n");
    assert_eq!(to_markdown("<p>a</p><p>b</p>\n\n"), "a\n\nb\n");
}

#[test]
fn multibyte_text_survives_every_path() {
    assert_eq!(
        to_markdown("<h1>é世</h1><p>日本語 &amp; ü<a href=\"é\">ñ</a></p><pre>ß\n</pre>"),
        "# é世\n\n日本語 & ü[ñ](é)\n\n```\nß\n```\n"
    );
}

#[test]
fn a_tag_name_is_case_insensitive() {
    assert_eq!(
        to_markdown("<H1>a</H1><P>b</P><UL><LI>c</LI></UL>"),
        "# a\n\nb\n\n- c\n"
    );
}

#[test]
fn deep_nesting_of_every_kind_is_handled_without_recursion() {
    let depth = 100_000;
    for (open, close) in [
        ("<div>", "</div>"),
        ("<ul><li>", "</li></ul>"),
        ("<blockquote>", "</blockquote>"),
        ("<b>", "</b>"),
        ("<svg>", "</svg>"),
        ("<table><tr><td>", "</td></tr></table>"),
    ] {
        let html = format!("{}x{}", open.repeat(depth), close.repeat(depth));
        assert!(to_markdown(&html).len() < 8 * html.len(), "{open}");
    }
}

#[test]
fn deep_lists_and_quotes_cap_their_indentation() {
    let depth = 5_000;
    let html = format!("{}x", "<ul><li>".repeat(depth));
    let markdown = to_markdown(&html);
    assert!(markdown.len() < 64 * depth, "{} bytes", markdown.len());
    let html = format!("{}x{}", "<blockquote>".repeat(depth), "<br>y".repeat(depth));
    let markdown = to_markdown(&html);
    assert!(markdown.len() < 64 * depth, "{} bytes", markdown.len());
}

#[test]
fn a_huge_attribute_value_is_read_once() {
    let value = "a".repeat(2_000_000);
    let html = format!("<a href=\"{value}\">x</a><img alt=\"{value}\" src=\"s\">");
    let markdown = to_markdown(&html);
    assert_eq!(markdown, format!("[x]({value})![{value}](s)\n"));
}

#[test]
fn many_unterminated_openers_stay_linear() {
    // The spec drops a tag the end of input cuts off, so `<a ` alone
    // converts to nothing; the point here is that each still converts in
    // proportion to its size.
    let html = "<a ".repeat(300_000);
    assert_eq!(to_markdown(&html), "");
    let html = "<title>".repeat(300_000);
    let _ = to_markdown(&html);
    let html = "<!-- ".repeat(300_000);
    let _ = to_markdown(&html);
    let html = "<script>".repeat(300_000);
    let _ = to_markdown(&html);
    let html = "&#".repeat(300_000);
    assert_eq!(to_markdown(&html).len(), 600_001);
}

#[test]
fn excess_list_closes_pop_the_counter_first() {
    let over = 5;
    let total = super::MAX_LEVELS + over;
    let mut html = "<ul>".repeat(total);
    html.push_str("<li>deep");
    html.push_str(&"</ul>".repeat(over));
    html.push_str("<li>still-deep");
    html.push_str(&"</ul>".repeat(super::MAX_LEVELS));
    let markdown = to_markdown(&html);
    let capped = format!("{}- ", " ".repeat((super::MAX_LEVELS - 1) * 2));
    assert!(markdown.contains(&format!("{capped}deep")), "{markdown}");
    assert!(
        markdown.contains(&format!("{capped}still-deep")),
        "{markdown}"
    );
}

#[test]
fn a_million_nested_lists_keep_the_stack_at_its_cap() {
    let html = "<ul>".repeat(1_000_000);
    let markdown = to_markdown(&html);
    assert!(markdown.len() < 64 * 1_000_000, "{} bytes", markdown.len());
}

#[test]
fn a_close_of_a_non_element_is_dropped() {
    // `</` followed by a digit is ignored per the spec, instead of being
    // kept as text.
    assert_eq!(to_markdown("a</2x>b"), "ab\n");
}

#[test]
fn a_script_close_is_not_an_opener() {
    assert_eq!(to_markdown("a</script>b"), "ab\n");
    assert_eq!(to_markdown("a</style>b"), "ab\n");
}

#[test]
fn a_title_close_is_not_an_opener() {
    assert_eq!(to_markdown("</title><title>T</title>"), "# T\n");
}

#[test]
fn a_body_close_does_not_end_the_head() {
    assert_eq!(to_markdown("<head></body><p>x</p>"), "");
}

#[test]
fn a_title_inside_a_dropped_element_is_dropped() {
    assert_eq!(to_markdown("a<svg><title>T</title></svg>b"), "ab\n");
}

#[test]
fn an_image_close_writes_nothing_even_with_attributes() {
    assert_eq!(to_markdown("</img src=\"x\">"), "");
}

#[test]
fn a_table_breaks_around_nothing() {
    assert_eq!(to_markdown("a<table></table>b"), "a\n\nb\n");
}

#[test]
fn a_nested_link_keeps_the_outer_href() {
    assert_eq!(
        to_markdown("<a href=\"u\">x<a href=\"v\">y</a>z</a>"),
        "[xy](u)z\n"
    );
}

#[test]
fn a_link_opened_inside_pre_does_not_capture_what_follows_it() {
    assert_eq!(
        to_markdown("<pre><a href=\"u\">x</pre>y"),
        "```\nx\n```\n\ny\n"
    );
}

#[test]
fn a_line_break_inside_pre_keeps_the_space_before_it() {
    assert_eq!(to_markdown("<pre>a <br>b</pre>"), "```\na \nb\n```\n");
}

#[test]
fn trailing_spaces_are_trimmed_before_a_block_break() {
    assert_eq!(to_markdown("a <p>b</p>"), "a\n\nb\n");
}

#[test]
fn blocks_inside_a_link_are_spaces() {
    assert_eq!(to_markdown("<a href=\"u\">x<div></div>y</a>"), "[x y](u)\n");
}

#[test]
fn named_entities_decode_per_the_spec() {
    assert_eq!(to_markdown("&mdash;"), "—\n");
    assert_eq!(to_markdown("&eacute;"), "é\n");
    // A legacy entity without its semicolon still decodes in text.
    assert_eq!(to_markdown("&notit"), "¬it\n");
    assert_eq!(to_markdown("&Ouml;"), "Ö\n");
    // Some entities name two code points.
    assert_eq!(to_markdown("&NotEqualTilde;"), "≂̸\n");
}

#[test]
fn an_entity_that_could_continue_a_name_is_kept_in_attributes() {
    assert_eq!(
        to_markdown("<a href=\"?a=1&copy=2\">x</a>"),
        "[x](?a=1&copy=2)\n"
    );
}

#[test]
fn script_and_style_drop_markup_like_text() {
    assert_eq!(to_markdown("a<script>x</p><b>y</b></script>b"), "ab\n");
    assert_eq!(to_markdown("a<style>p</p><b>x</b></style>b"), "ab\n");
}

#[test]
fn a_title_reads_markup_as_text() {
    // Rcdata keeps markup literally: the tags become part of the title.
    assert_eq!(to_markdown("<title>a <b>b</b></title>"), "# a <b>b</b>\n");
}

#[test]
fn a_dropped_first_title_leaves_a_later_one_counting() {
    assert_eq!(
        to_markdown("<noscript><title>N</title></noscript><title>Real</title>"),
        "# Real\n"
    );
}
