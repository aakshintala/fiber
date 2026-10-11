//! Bounds tests beside [`super::to_markdown`]: link and title text written
//! as they arrive, the hidden-element state at one byte per element, and
//! the output multiple. The link and title tables record the exact
//! output the converter writes; ordinary pages convert the same.
//!
//! `#![allow(..., reason = ...)]` header shared by the test modules in this
//! crate: tests unwrap and index freely.
#![allow(clippy::unwrap_used, reason = "tests unwrap")]
#![allow(clippy::indexing_slicing, reason = "tests index")]

use std::time::Duration;

use html5ever::tokenizer::{Tag, TagKind};
use html5ever::{Attribute, LocalName, QualName, ns};
use proptest::prelude::*;

use super::hidden::Hidden;
use super::{Stream, convert, to_markdown};

/// Link inputs with the exact markdown the converter writes.
fn links() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "<a href=\"u\">  one\n<div>two</div>  </a>",
            "[one two](u)\n",
        ),
        ("<a href=\"u\">\tone</a>", "[one](u)\n"),
        ("<a href=\"u\">one\r\n</a>", "[one](u)\n"),
        ("<a href=\"u\">one \t \n \u{c} \r two</a>", "[one two](u)\n"),
        ("a<a href=\"u\"> \t\n </a>b", "ab\n"),
        (
            "<a href=\"u\"><img src=\"i.png\" alt=\"x\"></a>",
            "[![x](i.png)](u)\n",
        ),
        ("<a href=\"u\">x<br>y</a>", "[x y](u)\n"),
        ("<a href=\"u\">text", "[text](u)\n"),
        ("<a href=\"u\">x<a href=\"v\">y</a>z</a>", "[xy](u)z\n"),
        (
            "<blockquote><p><a href=\"u\">x</a></p></blockquote>",
            "> [x](u)\n",
        ),
        (
            "<blockquote><p>a <a href=\"u\">x</a></p></blockquote>",
            "> a [x](u)\n",
        ),
        (
            "<a href=\"u\">x <b>y</b> <code>e</code></a>",
            "[x **y** `e`](u)\n",
        ),
        ("<a href=\"u\">x</a> tail", "[x](u) tail\n"),
    ]
}

/// Title inputs with the exact markdown the converter writes.
fn titles() -> Vec<(&'static str, &'static str)> {
    vec![
        ("<title> a \t b </title>", "# a b\n"),
        ("<title></title>", ""),
        ("<title>   </title>", ""),
        ("<title>T</title><p>body</p>", "# T\n\nbody\n"),
        ("<title>unclosed words here", "# unclosed words here\n"),
        (
            "<title>Longer Title Here</title><p>b</p>",
            "# Longer Title Here\n\nb\n",
        ),
        (
            "<title>T</title><p>much longer body text here</p>",
            "# T\n\nmuch longer body text here\n",
        ),
        ("<title>Only</title>", "# Only\n"),
        (
            "<noscript><title>N</title></noscript><title>Real</title>",
            "# Real\n",
        ),
    ]
}

#[test]
fn link_text_collapses_its_whitespace_and_keeps_inline_markup() {
    for (html, expected) in links() {
        assert_eq!(to_markdown(html), expected, "{html:?}");
    }
}

#[test]
fn title_text_collapses_its_whitespace_into_one_heading() {
    for (html, expected) in titles() {
        assert_eq!(to_markdown(html), expected, "{html:?}");
    }
}

/// Misnested markup inside a link lands inside the link: `[` takes the
/// quote depth of the link's first visible character, and `pre` and `td`
/// content lands inside the link. Each case names its shape; ordinary
/// pages are unaffected.
#[test]
fn misnested_markup_inside_a_link_lands_inside_the_link() {
    // A `blockquote` end inside a link: `[` keeps the depth of the link's
    // first character, not of its closing tag.
    assert_eq!(
        to_markdown("<blockquote><a href=\"u\">x</blockquote>y</a>"),
        "> [x y](u)\n"
    );
    // A `blockquote` start inside a link, closed after it: same rule the
    // other way, no prefix at the link's start.
    assert_eq!(
        to_markdown("<a href=\"u\">x<blockquote>y</a>z</blockquote>"),
        "[x y](u)z\n"
    );
    // A `pre` inside a link: its fence and text land inside the link
    // rather than before it.
    assert_eq!(
        to_markdown("<a href=\"u\">x<pre>y</pre>z</a>"),
        "[x ```y\n``` z](u)\n"
    );
    // A `td` inside a link after its first character: the space before
    // the link stays, since the pending space clears instead of trimming
    // the output.
    assert_eq!(
        to_markdown("x <a href=\"u\"><td>a<td>b</a>"),
        "x [a | b](u)\n"
    );
}

/// However a page is cut into pieces, link and title text come out the
/// same: every slice size from 1 to 7 converts like the whole page.
#[test]
fn link_and_title_text_is_chunk_independent() {
    let mut inputs: Vec<&str> = links().iter().map(|(html, _)| *html).collect();
    inputs.extend(titles().iter().map(|(html, _)| *html));
    for html in inputs {
        let whole = to_markdown(html);
        for slice in 1..=7 {
            assert_eq!(convert(html, slice), whole, "{html:?} cut at {slice}");
        }
    }
}

/// A reference model of `Hidden`, kept beside it: both stay observably
/// identical on every sequence.
mod oracle {
    use html5ever::tokenizer::Tag;

    use super::super::attribute;

    const IN_HEAD: [&str; 10] = [
        "base", "basefont", "bgsound", "head", "html", "link", "meta", "noframes", "noscript",
        "template",
    ];

    const OUT_OF_SVG: [&str; 44] = [
        "b",
        "big",
        "blockquote",
        "body",
        "br",
        "center",
        "code",
        "dd",
        "div",
        "dl",
        "dt",
        "em",
        "embed",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "head",
        "hr",
        "i",
        "img",
        "li",
        "listing",
        "menu",
        "meta",
        "nobr",
        "ol",
        "p",
        "pre",
        "ruby",
        "s",
        "small",
        "span",
        "strong",
        "strike",
        "sub",
        "sup",
        "table",
        "tt",
        "u",
        "ul",
        "var",
    ];

    #[derive(Default, Clone, Copy, PartialEq, Eq)]
    enum Head {
        #[default]
        Before,
        In,
        After,
    }

    #[derive(Default)]
    pub(super) struct Oracle {
        hidden: usize,
        svgs: Vec<usize>,
        noscripts: Vec<usize>,
        templates: Vec<usize>,
        head: Head,
    }

    impl Oracle {
        pub(super) fn is_hidden(&self) -> bool {
            self.hidden > 0
        }

        pub(super) fn in_svg(&self) -> bool {
            !self.svgs.is_empty()
        }

        pub(super) fn in_head(&self) -> bool {
            self.head == Head::In
        }

        pub(super) fn templates_open(&self) -> bool {
            !self.templates.is_empty()
        }

        pub(super) fn noscripts_above(&self) -> bool {
            match self.svgs.first() {
                None => !self.noscripts.is_empty(),
                Some(&outer) => self.noscripts.iter().any(|&at| at >= outer),
            }
        }

        pub(super) fn above_counts(&self) -> [usize; 3] {
            match self.svgs.first() {
                None => [self.svgs.len(), self.noscripts.len(), self.templates.len()],
                Some(&outer) => [
                    self.svgs.iter().filter(|&&at| at >= outer).count(),
                    self.noscripts.iter().filter(|&&at| at >= outer).count(),
                    self.templates.iter().filter(|&&at| at >= outer).count(),
                ],
            }
        }

        pub(super) fn open(&mut self, name: &str, tag: &Tag) {
            if name == "head" {
                if self.head == Head::Before {
                    self.head = Head::In;
                }
            } else if name != "html" && (self.head == Head::Before || !IN_HEAD.contains(&name)) {
                self.head = Head::After;
            }
            if breaks_out_of_svg(name, tag) {
                self.close_svg();
            }
            let depth = self.hidden;
            if let Some(depths) = self.depths(name)
                && !tag.self_closing
            {
                depths.push(depth);
                self.hidden += 1;
            }
        }

        pub(super) fn end(&mut self, name: &str) -> bool {
            if matches!(name, "head" | "body" | "html" | "br") {
                self.head = Head::After;
            }
            if matches!(name, "p" | "br") {
                self.close_svg();
            }
            if matches!(name, "svg" | "noscript" | "template") {
                self.end_hidden(name);
                true
            } else {
                false
            }
        }

        pub(super) fn text_is_in_head(&mut self, text: &str) -> bool {
            if !text.chars().all(|c| c.is_ascii_whitespace()) {
                self.head = Head::After;
            }
            self.head == Head::In
        }

        fn end_hidden(&mut self, name: &str) {
            let foreign = self.svgs.first().copied().unwrap_or(self.hidden);
            let innermost = self.depths(name).and_then(|depths| depths.last().copied());
            let at = innermost.filter(|&at| at >= foreign).or_else(|| {
                if name == "template" {
                    innermost
                } else {
                    foreign
                        .checked_sub(1)
                        .filter(|&below| innermost == Some(below))
                }
            });
            if let Some(at) = at {
                self.close_hidden(at);
            }
        }

        fn close_svg(&mut self) {
            if let Some(&svg) = self.svgs.first() {
                self.close_hidden(svg);
            }
        }

        fn close_hidden(&mut self, at: usize) {
            self.hidden = at;
            for depths in [&mut self.svgs, &mut self.noscripts, &mut self.templates] {
                while depths.last().is_some_and(|&depth| depth >= at) {
                    depths.pop();
                }
            }
        }

        fn depths(&mut self, name: &str) -> Option<&mut Vec<usize>> {
            match name {
                "svg" => Some(&mut self.svgs),
                "noscript" => Some(&mut self.noscripts),
                "template" => Some(&mut self.templates),
                _ => None,
            }
        }
    }

    fn breaks_out_of_svg(name: &str, tag: &Tag) -> bool {
        OUT_OF_SVG.contains(&name)
            || name == "font"
                && ["color", "face", "size"]
                    .iter()
                    .any(|wanted| attribute(tag, wanted).is_some())
    }
}

/// A start tag for the differential test.
fn start(name: &str, self_closing: bool, attrs: Vec<(&str, &str)>) -> Tag {
    Tag {
        kind: TagKind::StartTag,
        name: LocalName::from(name),
        self_closing,
        attrs: attrs
            .into_iter()
            .map(|(key, value)| Attribute {
                name: QualName::new(None, ns!(html), LocalName::from(key)),
                value: value.into(),
            })
            .collect(),
        had_duplicate_attributes: false,
    }
}

/// One differential step: opens (self-closing or not), end tags,
/// break-out tags, head tags and text, driven through both states and
/// compared after each step.
fn drive(both: &mut (Hidden, oracle::Oracle), step: u8) {
    let (hidden, oracle) = &mut *both;
    match step {
        0 => {
            let tag = start("svg", false, vec![]);
            hidden.open("svg", &tag);
            oracle.open("svg", &tag);
        }
        1 => {
            let tag = start("svg", true, vec![]);
            hidden.open("svg", &tag);
            oracle.open("svg", &tag);
        }
        2 => {
            let tag = start("noscript", false, vec![]);
            hidden.open("noscript", &tag);
            oracle.open("noscript", &tag);
        }
        3 => {
            let tag = start("noscript", true, vec![]);
            hidden.open("noscript", &tag);
            oracle.open("noscript", &tag);
        }
        4 => {
            let tag = start("template", false, vec![]);
            hidden.open("template", &tag);
            oracle.open("template", &tag);
        }
        5 => {
            let tag = start("template", true, vec![]);
            hidden.open("template", &tag);
            oracle.open("template", &tag);
        }
        6 => {
            hidden.end("svg");
            oracle.end("svg");
        }
        7 => {
            hidden.end("noscript");
            oracle.end("noscript");
        }
        8 => {
            hidden.end("template");
            oracle.end("template");
        }
        9 => {
            let tag = start("div", false, vec![]);
            hidden.open("div", &tag);
            oracle.open("div", &tag);
        }
        10 => {
            let tag = start("p", false, vec![]);
            hidden.open("p", &tag);
            oracle.open("p", &tag);
        }
        11 => {
            let tag = start("font", false, vec![]);
            hidden.open("font", &tag);
            oracle.open("font", &tag);
        }
        12 => {
            let tag = start("font", false, vec![("color", "x")]);
            hidden.open("font", &tag);
            oracle.open("font", &tag);
        }
        13 => {
            let tag = start("font", false, vec![("face", "f")]);
            hidden.open("font", &tag);
            oracle.open("font", &tag);
        }
        14 => {
            let tag = start("font", false, vec![("size", "3")]);
            hidden.open("font", &tag);
            oracle.open("font", &tag);
        }
        15 => {
            hidden.end("p");
            oracle.end("p");
        }
        16 => {
            hidden.end("br");
            oracle.end("br");
        }
        17 => {
            let tag = start("head", false, vec![]);
            hidden.open("head", &tag);
            oracle.open("head", &tag);
        }
        18 => {
            let tag = start("body", false, vec![]);
            hidden.open("body", &tag);
            oracle.open("body", &tag);
        }
        19 => {
            let tag = start("html", false, vec![]);
            hidden.open("html", &tag);
            oracle.open("html", &tag);
        }
        20 => {
            hidden.end("head");
            oracle.end("head");
        }
        21 => {
            hidden.end("body");
            oracle.end("body");
        }
        22 => {
            hidden.end("html");
            oracle.end("html");
        }
        23 => {
            let tag = start("br", false, vec![]);
            hidden.open("br", &tag);
            oracle.open("br", &tag);
        }
        24 => {
            hidden.text_is_in_head("   ");
            oracle.text_is_in_head("   ");
        }
        _ => {
            hidden.text_is_in_head("x");
            oracle.text_is_in_head("x");
        }
    }
    assert_eq!(hidden.is_hidden(), oracle.is_hidden(), "step {step}");
    assert_eq!(hidden.in_svg(), oracle.in_svg(), "step {step}");
    assert_eq!(hidden.in_head(), oracle.in_head(), "step {step}");
}

proptest! {
    /// Random open, close, break-out, head and text steps keep both
    /// observably identical on every sequence.
    #[test]
    fn hidden_matches_the_reference_model_on_every_sequence(steps in proptest::collection::vec(0..26u8, 0..300)) {
        let mut both = (Hidden::default(), oracle::Oracle::default());
        for step in steps {
            drive(&mut both, step);
        }
    }
}

/// How long the linear hidden-close cases may take on the wall clock.
const HIDDEN_LINEAR: Duration = Duration::from_secs(10);

/// An end tag that matches no open hidden element is consumed and leaves
/// the open elements and their counts as they were. The deadline is a hang
/// guard, not a timing assertion: a per-close rescan at this size takes
/// minutes, linear work a fraction of a second in debug.
#[test]
fn half_a_million_unmatched_hidden_closes_stay_linear() {
    let n = 500_000;
    for (svg, open, close) in [
        (true, "noscript", "template"),
        (true, "template", "noscript"),
        (false, "noscript", "svg"),
        (false, "template", "svg"),
    ] {
        fakes::within("the hidden closes", HIDDEN_LINEAR, move || {
            let mut hidden = Hidden::default();
            if svg {
                hidden.open("svg", &start("svg", false, vec![]));
            }
            let tag = start(open, false, vec![]);
            for _ in 0..n {
                hidden.open(open, &tag);
            }
            let before = hidden.above_counts();
            for _ in 0..n {
                assert!(hidden.end(close), "{close}");
            }
            assert!(hidden.is_hidden(), "{open} {close}");
            assert_eq!(hidden.above_counts(), before, "{open} {close}");
        });
    }
}

#[test]
fn the_hidden_stack_shrinks_back_after_deep_nesting() {
    let depth = 524_289;
    let mut stream = Stream::default();
    stream.push(&"<template>".repeat(depth));
    stream.push(&"</template>".repeat(depth));
    let converter = stream.tokenizer.sink.cell.borrow();
    assert!(
        converter.hidden.open_capacity() <= 64,
        "capacity {}",
        converter.hidden.open_capacity()
    );
}

/// Closing the element above the outermost `svg` must not spend the
/// `noscript` below it: popping the template decrements only its own
/// above-`svg` count, so a later `</noscript>` with the `svg` still open
/// closes nothing.
#[test]
fn a_pop_above_the_svg_keeps_the_noscript_below_it() {
    let mut hidden = Hidden::default();
    let mut oracle = oracle::Oracle::default();
    for (name, self_closing) in [
        ("noscript", false),
        ("template", false),
        ("svg", false),
        ("template", false),
    ] {
        let tag = start(name, self_closing, vec![]);
        hidden.open(name, &tag);
        oracle.open(name, &tag);
    }
    hidden.end("template");
    oracle.end("template");
    hidden.end("noscript");
    oracle.end("noscript");
    assert_eq!(hidden.is_hidden(), oracle.is_hidden());
    assert_eq!(hidden.in_svg(), oracle.in_svg());
    assert!(hidden.is_hidden());
    assert!(hidden.in_svg());
}

/// Popping two `noscript`s above the outermost `svg` spends the
/// above-`svg` count twice: after the first pop one stays above, after
/// the second none does. Two stay open so decrementing (`-= 1`) and
/// dividing (`/= 1`, a no-op) differ, and skipping the decrement (`<`
/// for `>=`) leaves the count high: each pop is checked against the
/// oracle, down to `len == svg_base + 1` and back to one above.
#[test]
fn popping_noscripts_above_the_svg_spends_the_above_count_twice() {
    let mut hidden = Hidden::default();
    let mut oracle = oracle::Oracle::default();
    for name in ["svg", "noscript", "noscript"] {
        let tag = start(name, false, vec![]);
        hidden.open(name, &tag);
        oracle.open(name, &tag);
    }
    assert_eq!(hidden.above_counts(), oracle.above_counts());
    assert_eq!(hidden.above_counts(), [1, 2, 0]);
    hidden.end("noscript");
    oracle.end("noscript");
    assert_eq!(hidden.is_hidden(), oracle.is_hidden());
    assert_eq!(hidden.in_svg(), oracle.in_svg());
    assert_eq!(hidden.noscripts_above(), oracle.noscripts_above());
    assert_eq!(hidden.templates_open(), oracle.templates_open());
    assert_eq!(hidden.above_counts(), oracle.above_counts());
    assert_eq!(hidden.above_counts(), [1, 1, 0]);
    hidden.end("noscript");
    oracle.end("noscript");
    assert_eq!(hidden.is_hidden(), oracle.is_hidden());
    assert_eq!(hidden.in_svg(), oracle.in_svg());
    assert_eq!(hidden.noscripts_above(), oracle.noscripts_above());
    assert_eq!(hidden.templates_open(), oracle.templates_open());
    assert_eq!(hidden.above_counts(), oracle.above_counts());
    assert_eq!(hidden.above_counts(), [1, 0, 0]);
    assert!(hidden.in_svg());
    assert!(!hidden.noscripts_above());
}

/// Closing the `noscript` below the outermost `svg` pops exactly to
/// `len == svg_base - 1` and stops: with nothing above one, the end tag
/// closes the `svg`, what it hides, and the `noscript` below it, leaving
/// the stack empty like the oracle.
#[test]
fn closing_the_noscript_below_the_svg_stops_at_base_minus_one() {
    let mut hidden = Hidden::default();
    let mut oracle = oracle::Oracle::default();
    for name in ["noscript", "svg", "template", "template"] {
        let tag = start(name, false, vec![]);
        hidden.open(name, &tag);
        oracle.open(name, &tag);
    }
    assert_eq!(hidden.above_counts(), oracle.above_counts());
    assert_eq!(hidden.above_counts(), [1, 0, 2]);
    hidden.end("noscript");
    oracle.end("noscript");
    assert_eq!(hidden.is_hidden(), oracle.is_hidden());
    assert_eq!(hidden.in_svg(), oracle.in_svg());
    assert_eq!(hidden.noscripts_above(), oracle.noscripts_above());
    assert_eq!(hidden.templates_open(), oracle.templates_open());
    assert_eq!(hidden.above_counts(), oracle.above_counts());
    assert!(!hidden.is_hidden());
    assert!(!hidden.in_svg());
}

/// How long the inflated-above-count close may take before it fails.
const HIDDEN_POP_HANG: Duration = Duration::from_secs(10);

/// Growing the above-`svg` count instead of spending it (`+=` for `-=`)
/// leaves a `noscript` above one after both close: the next
/// `</noscript>` then looks for one that is no longer there and never
/// finds it. The close runs under a wall-clock limit so the mutant fails
/// the test within seconds instead of hanging the job.
#[test]
fn growing_the_above_count_hangs_the_next_noscript_close() {
    fakes::within("the inflated noscript close", HIDDEN_POP_HANG, move || {
        let mut hidden = Hidden::default();
        let mut oracle = oracle::Oracle::default();
        for name in ["svg", "noscript", "noscript"] {
            let tag = start(name, false, vec![]);
            hidden.open(name, &tag);
            oracle.open(name, &tag);
        }
        hidden.end("noscript");
        oracle.end("noscript");
        hidden.end("noscript");
        oracle.end("noscript");
        assert_eq!(hidden.above_counts(), oracle.above_counts());
        hidden.end("noscript");
        oracle.end("noscript");
        assert_eq!(hidden.is_hidden(), oracle.is_hidden());
        assert_eq!(hidden.in_svg(), oracle.in_svg());
        assert!(hidden.is_hidden());
        assert!(hidden.in_svg());
    });
}

/// Asserts `markdown` is at most 14 times its page, naming the ratio.
fn assert_bounded(html: &str, markdown: &str, what: &str) {
    assert!(
        markdown.len() <= 14 * html.len(),
        "{what}: {} bytes of markdown for {} of page, ratio {:.2}",
        markdown.len(),
        html.len(),
        markdown.len() as f64 / html.len().max(1) as f64
    );
}

#[test]
fn every_output_shape_stays_within_fourteen_times_its_page() {
    let quotes = "<blockquote>".repeat(8);
    let unquote = "</blockquote>".repeat(8);
    let shapes = [
        (
            "nested quotes",
            format!("{quotes}{}{unquote}", "<br>x".repeat(250_000)),
        ),
        (
            "pre",
            format!("{quotes}<pre>{}</pre>{unquote}", "\nx".repeat(500_000)),
        ),
        (
            "deep list",
            format!(
                "{quotes}{}{}{unquote}",
                "<ol>".repeat(8),
                "<li>x".repeat(200_000)
            ),
        ),
        (
            "rules",
            format!("{quotes}{}{unquote}", "<hr>".repeat(200_000)),
        ),
        (
            "headings",
            format!("{quotes}{}{unquote}", "<h6>x</h6>".repeat(110_000)),
        ),
        (
            "bold breaks",
            format!("{quotes}{}{unquote}", "<br><b>x".repeat(125_000)),
        ),
        (
            "paragraphs",
            format!("{quotes}{}{unquote}", "<p>x".repeat(330_000)),
        ),
        (
            "table rows",
            format!("{quotes}{}{unquote}", "<tr>x".repeat(250_000)),
        ),
    ];
    for (what, html) in shapes {
        assert!(html.len() > 800_000, "{what}: {} bytes", html.len());
        let markdown = to_markdown(&html);
        assert_bounded(&html, &markdown, what);
    }
}

proptest! {
    /// Random mixes of the heavy pieces inside random quote and list
    /// nesting stay within fourteen times the page.
    #[test]
    fn mixed_pieces_stay_within_fourteen_times_the_page(
        quotes in 0..9usize,
        lists in 0..4usize,
        pieces in proptest::collection::vec(0..8usize, 0..40),
    ) {
        let mut html = "<blockquote>".repeat(quotes) + &"<ul><li>".repeat(lists);
        for piece in pieces {
            html.push_str(match piece {
                0 => "<br>x",
                1 => "<pre>\nx</pre>",
                2 => "<li>y",
                3 => "<hr>",
                4 => "<h6>z</h6>",
                5 => "<b>w",
                6 => "<p>v",
                _ => "<tr>u",
            });
        }
        let markdown = to_markdown(&html);
        assert_bounded(&html, &markdown, "mixed pieces");
    }
}

#[test]
fn every_character_reference_expands_less_than_twice() {
    // The public table is keyed without the `&` and holds every name
    // prefix, mapped to no code point: only real entities count.
    for (key, &(first, second)) in html5ever::data::NAMED_ENTITIES.entries() {
        if (first, second) == (0, 0) {
            continue;
        }
        let name = format!("&{key}");
        let source = format!("{name}z");
        let markdown = to_markdown(&source);
        let decoded = markdown.len() - "z\n".len();
        assert!(
            decoded < 2 * name.len(),
            "{name}: {decoded} bytes for {}",
            name.len()
        );
    }
    for reference in [
        "&#0;",
        "&#38;",
        "&#x26;",
        "&#233;",
        "&#8212;",
        "&#10;",
        "&#x10FFFF;",
        "&#xD800;",
        "&#xFFFE;",
        "&#159;",
    ] {
        let markdown = to_markdown(reference);
        let decoded = markdown.trim_end().len();
        assert!(
            decoded < 2 * reference.len(),
            "{reference}: {decoded} bytes"
        );
    }
}
