//! Property tests beside [`super::to_markdown`]: generated pages mix nesting,
//! broken markup, entities, scripts and huge attributes, and each is checked
//! against the converter's promises: no visible text lost outside the dropped
//! elements, no panic, and output in proportion to the input.
//!
//! `#![allow(..., reason = ...)]` header shared by the test modules in this
//! crate: tests unwrap and index freely.
#![allow(clippy::unwrap_used, reason = "tests unwrap")]
#![allow(clippy::indexing_slicing, reason = "tests index")]

use proptest::prelude::*;

use super::{convert, to_markdown};

/// Whether `word` appears as a whole word in `output`: split on
/// non-alphanumerics, so `v1` never matches inside `v12`.
fn contains_word(output: &str, word: &str) -> bool {
    output
        .split(|c: char| !c.is_alphanumeric())
        .any(|candidate| candidate == word)
}

/// One piece of a generated page. Visible words (`v` + digits) go only where
/// a reader would see them; hidden words (`h` + digits) only inside dropped
/// elements; attribute values use neither prefix.
fn piece(visible: Vec<String>, hidden: Vec<String>) -> impl Strategy<Value = String> {
    (0..20usize, 0..8usize, 0..8usize).prop_map(move |(kind, a, b)| {
        let first = |n: usize| n % visible.len();
        let veil = |n: usize| n % hidden.len();
        match kind {
            0 => format!(" {} ", visible[first(a)]),
            1 => format!("<p>{}</p>", visible[first(a)]),
            2 => format!("<h2>{}</h2>", visible[first(a)]),
            3 => format!(
                "<ul><li>{} <b>{}</b></li></ul>",
                visible[first(a)],
                visible[first(b)]
            ),
            4 => format!(
                "<div><p>{} <em>{}</em></p><blockquote>{}</blockquote></div>",
                visible[first(a)],
                visible[first(b)],
                visible[first(a)]
            ),
            5 => format!(
                "<table><tr><th>{}</th></tr><tr><td>{}</td></tr></table>",
                visible[first(a)],
                visible[first(b)]
            ),
            6 => format!("<a href=\"/p{a}?x=1&y=2\">{}</a>", visible[first(a)]),
            7 => format!("<img src=\"/i{a}.png\" alt=\"{}\">", visible[first(a)]),
            8 => format!("<pre>code {} &lt;tag&gt;</pre>", visible[first(a)]),
            9 => format!("<h3>{}</h3>", visible[first(a)]),
            10 => format!(
                "<script>var {} = '<p>{}</p>'; if (1 < 2) {{ f(); }}</script>",
                hidden[veil(a)],
                hidden[veil(a)]
            ),
            11 => format!("<style>.{} {{ color: red; }}</style>", hidden[veil(a)]),
            12 => format!("<svg><text>{}</text></svg>", hidden[veil(a)]),
            13 => format!("<noscript>{}</noscript>", hidden[veil(a)]),
            14 => format!("<template><p>{}</p></template>", hidden[veil(a)]),
            15 => format!("<!-- {} -->", hidden[veil(a)]),
            16 => "&amp; &mdash; &#65; &#x42;".to_owned(),
            17 => "<div><span>unclosed <b>bold <i>both".to_owned(),
            18 => "1 < 2 and a trailing <".to_owned(),
            _ => format!(
                "<span title=\"{}\">{}</span>",
                "q".repeat(1000),
                visible[first(a)]
            ),
        }
    })
}

/// A generated page: its visible words, its hidden words, and its HTML. The
/// first piece always shows a visible word, so the oracle below never runs
/// on a page with nothing to find. At most one `<title>` appears, since only
/// the first counts.
fn page() -> impl Strategy<Value = (Vec<String>, Vec<String>, String)> {
    (1usize..6, 1usize..6).prop_flat_map(|(nv, nh)| {
        let visible: Vec<String> = (0..nv).map(|i| format!("v{i}")).collect();
        let hidden: Vec<String> = (0..nh).map(|i| format!("h{i}")).collect();
        let first = format!("<p>{}</p>", visible[0]);
        (
            Just(visible.clone()),
            Just(hidden.clone()),
            (0..2usize, 0..visible.len()),
            (
                Just(first),
                proptest::collection::vec(piece(visible, hidden), 0..25),
            ),
        )
            .prop_map(|(visible, hidden, (titled, ti), (first, rest))| {
                // Spaced apart, so two visible words never abut into one:
                // the oracle matches whole words.
                let mut html = String::new();
                if titled == 0 {
                    html.push_str(&format!("<title>{}</title> ", visible[ti]));
                }
                html.push_str(&first);
                for part in rest {
                    html.push(' ');
                    html.push_str(&part);
                }
                (visible, hidden, html)
            })
    })
}

proptest! {
    /// Every visible word survives as a whole word, no hidden word leaks,
    /// and the output stays in proportion to the input.
    #[test]
    fn generated_pages_lose_no_visible_text((visible, hidden, html) in page()) {
        let markdown = to_markdown(&html);
        for word in &visible {
            // Only words the page shows can be lost: pieces pick words at
            // random, so a word may never appear in the input.
            if contains_word(&html, word) {
                prop_assert!(
                    contains_word(&markdown, word),
                    "visible {word} lost in {markdown:?} from {html:?}"
                );
            }
        }
        for word in &hidden {
            prop_assert!(
                !contains_word(&markdown, word),
                "hidden {word} leaked into {markdown:?} from {html:?}"
            );
        }
        prop_assert!(
            markdown.len() <= 8 * html.len() + 64,
            "output {} bytes from {} input bytes",
            markdown.len(),
            html.len()
        );
    }

    /// Feeding the page one byte at a time, up to seven, converts exactly
    /// like feeding it whole: a tag, entity, multibyte char or `</script>`
    /// cut across slices changes nothing.
    #[test]
    fn every_slice_size_converts_like_the_whole_page(html in "<p>é &mdash; <a href=\"/x\">v</a></p><script>y</script><title>t</title>|[a-z<>/&;é ]{0,200}") {
        let whole = convert(&html, html.len().max(1));
        for slice in 1..8usize {
            prop_assert_eq!(convert(&html, slice), whole.clone());
        }
    }
}
