//! The converter's working memory in bytes (`docs/tools.md`, "HTML to
//! markdown"): the bytes the converting thread holds at once beside the
//! output, on hostile pages of about 8 MiB. This binary installs the
//! counting allocator and holds only these tests, so no other test binary
//! changes allocator.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code, helpers included"
)]

use std::hint::black_box;
use std::time::Duration;

use contract::tool::Tool;
use fakes::alloc::{Bytes, Counting, bytes_during};
use fakes::clock::FakeClock;
use fakes::{CancelToken, ProviderServer, Recorder, Response, TempDir, within};
use serde_json::{Map, Value};
use tools::WebFetch;

#[global_allocator]
static ALLOC: Counting = Counting;

/// The size of each hostile page: under the 10 MiB download cap.
const PAGE: usize = 8 << 20;

/// The size of each page that fills the download cap: the cap minus 1 KiB.
const CAP_PAGE: usize = (10 << 20) - 1024;

/// The working peak a page-sized token may hold beside the output: the
/// measured peaks below, rounded up to a whole MiB. It stays under the
/// 24 MiB the busy session's budget leaves for the fetch.
const HELD_WHOLE: usize = 17 << 20;

/// The converter's own working memory apart from the hidden-element state.
const WORKING: usize = 256 << 10;

/// The output below one slot is counted as working memory.
const SLACK: usize = 64 << 10;

/// How long a fetch may take.
const FETCH: Duration = Duration::from_secs(60);

/// One fetch of a page, measured.
struct Fetched {
    text: String,
    measured: Bytes,
    /// The result buffer's address inside the scope: the clone below has
    /// a different one, allocated past the scope, with no slot.
    addr: usize,
    _dir: TempDir,
    _server: ProviderServer,
}

/// A page holding `count` distinct short attribute names on a tag that
/// never closes: the tokenizer still holds the tag at the end of input.
fn unclosed_tag(count: u32) -> Vec<u8> {
    let mut page = String::from("<a");
    for n in 0..count {
        page.push_str(&format!(" a{n}"));
    }
    page.into_bytes()
}

/// An unclosed tag's attributes held whole: two pages under the cap with
/// different counts of distinct short attribute names, both converting
/// to nothing. The working peaks differ by the attributes alone, so
/// their difference over the count difference is the bytes each
/// attribute holds beside the output: about 40 bytes plus its name.
#[test]
fn an_unclosed_tag_holds_about_forty_five_bytes_per_attribute() {
    let small = unclosed_tag(8_000);
    let large = unclosed_tag(16_000);
    assert!(large.len() < 10 << 20, "the pages are under the cap");
    let first = fetch("text/html; charset=utf-8", small);
    assert_markdown_eq("", markdown(&first), "nothing past an unclosed tag");
    let second = fetch("text/html; charset=utf-8", large);
    assert_markdown_eq("", markdown(&second), "nothing past an unclosed tag");
    // Both peaks hold the tag's attributes beside empty output: the
    // difference over the count difference is the bytes each holds.
    let per = (working(&second) - working(&first)) / 8_000;
    assert!(
        (40..=50).contains(&per),
        "about 45 bytes per attribute, measured {per}"
    );
}

/// Serves `page` as `content_type` and fetches it inside a byte-counting
/// scope, after checking that the allocator counts at all. The scope runs
/// inside the closure `within` runs on its worker thread, so only the
/// fetching thread's bytes count.
fn fetch(content_type: &str, page: Vec<u8>) -> Fetched {
    let server = ProviderServer::start([Response {
        status: 200,
        headers: vec![("content-type".to_owned(), content_type.to_owned())],
        body: page,
        drop_connection: false,
        stall: false,
    }])
    .expect("the page server starts");
    let url = format!("{}/page", server.url());
    let dir = TempDir::new("fiber-web-fetch-bounded");
    let artifacts = dir.path().join("artifacts");
    let tool = WebFetch::new(artifacts, FakeClock::new()).with_proxy(None);
    let mut arguments = Map::new();
    arguments.insert("url".to_owned(), Value::String(url));
    let (result, measured, addr) = within("the fetch", FETCH, move || {
        let ((), calibration) = bytes_during(|| drop(black_box(Vec::<u8>::with_capacity(1024))));
        assert_eq!(
            calibration.peak(),
            1024,
            "the counting allocator is installed"
        );
        let cancel = CancelToken::new();
        let emit = Recorder::default();
        let (output, measured) = bytes_during(|| tool.run(&arguments, &cancel, &emit));
        let addr = match output.content.first() {
            Some(contract::shapes::ContentPart::Text { text }) => text.as_ptr() as usize,
            _ => 0,
        };
        (output, measured, addr)
    });
    let text = match result.content.first() {
        Some(contract::shapes::ContentPart::Text { text }) => text.clone(),
        _ => String::new(),
    };
    assert!(
        result.error.is_none(),
        "fetch failed with {} bytes of text",
        text.len()
    );
    Fetched {
        text,
        measured,
        addr,
        _dir: dir,
        _server: server,
    }
}

/// The markdown of a fetch: the result past its first line.
fn markdown(fetched: &Fetched) -> &str {
    fetched
        .text
        .split_once("\n\n")
        .map(|(_, rest)| rest)
        .unwrap_or_default()
}

/// The working peak of a fetch: the peak without the result buffer.
fn working(fetched: &Fetched) -> usize {
    fetched.measured.peak_without(fetched.addr as *const u8)
}

/// The bound every working peak is checked against: the empty page's peak
/// without its result, measured the same way, plus working memory and the
/// slack for output below one slot.
fn bound() -> usize {
    let empty = fetch("text/html; charset=utf-8", Vec::new());
    assert_eq!(markdown(&empty), "");
    working(&empty) + WORKING + SLACK
}

/// Asserts `actual` is `expected`, reporting only the lengths and the
/// first differing offset: both strings can be page-sized.
fn assert_markdown_eq(expected: &str, actual: &str, what: &str) {
    let offset = expected
        .bytes()
        .zip(actual.bytes())
        .position(|(a, b)| a != b);
    let same = expected.len() == actual.len() && offset.is_none();
    assert!(
        same,
        "{what}: lengths {} vs {}, first difference at {}",
        expected.len(),
        actual.len(),
        offset.unwrap_or(expected.len().min(actual.len()))
    );
}

#[test]
fn a_page_long_comment_holds_it_whole_within_the_measured_peak() {
    let overhead = "<!--".len() + "-->".len() + "<p>x</p>".len();
    let body = "c".repeat(CAP_PAGE - overhead);
    let html = format!("<!--{body}--><p>x</p>");
    assert_eq!(html.len(), CAP_PAGE);
    let fetched = fetch("text/html; charset=utf-8", html.into_bytes());
    assert_markdown_eq("x\n", markdown(&fetched), "the text past the comment");
    assert!(
        working(&fetched) <= HELD_WHOLE,
        "working {}",
        working(&fetched)
    );
}

#[test]
fn a_page_long_attribute_value_holds_it_whole_within_the_measured_peak() {
    let overhead = "<a href=\"".len() + "\">t</a>".len();
    let value = "v".repeat(CAP_PAGE - overhead);
    let html = format!("<a href=\"{value}\">t</a>");
    assert_eq!(html.len(), CAP_PAGE);
    let fetched = fetch("text/html; charset=utf-8", html.into_bytes());
    let expected = format!("[t]({value})\n");
    assert_markdown_eq(&expected, markdown(&fetched), "the link with its href");
    assert!(
        working(&fetched) <= HELD_WHOLE,
        "working {}",
        working(&fetched)
    );
}

#[test]
fn a_page_long_link_holds_no_page_sized_buffer() {
    let words = "ab ".repeat(PAGE / 3);
    let html = format!("<a href=\"u\">{words}</a>");
    assert!(html.len() > PAGE - 16);
    let fetched = fetch("text/html; charset=utf-8", html.into_bytes());
    let expected = format!("[{}](u)\n", words.trim_end());
    assert_markdown_eq(&expected, markdown(&fetched), "the link markdown");
    assert!(
        working(&fetched) <= bound(),
        "working {}",
        working(&fetched)
    );
}

#[test]
fn a_link_of_short_words_and_mixed_whitespace_holds_no_word_vec() {
    let mut raw = String::new();
    for separator in [" ", "\t", "\n", "\r", "\x0C", "  \t\n"] {
        raw.push('a');
        raw.push_str(separator);
        if raw.len() >= PAGE {
            break;
        }
    }
    while raw.len() < PAGE {
        raw.push_str("a ");
    }
    raw.truncate(PAGE);
    let html = format!("<a href=\"u\">{raw}</a>");
    let fetched = fetch("text/html; charset=utf-8", html.into_bytes());
    let collapsed = raw.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
    let expected = format!("[{collapsed}](u)\n");
    assert_markdown_eq(&expected, markdown(&fetched), "the link markdown");
    assert!(
        working(&fetched) <= bound(),
        "working {}",
        working(&fetched)
    );
}

#[test]
fn an_unclosed_title_holds_no_title_copy() {
    let words = "word ".repeat(PAGE / 5);
    let html = format!("<title>{words}");
    let fetched = fetch("text/html; charset=utf-8", html.into_bytes());
    let collapsed = words.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
    let expected = format!("# {collapsed}\n");
    assert_markdown_eq(&expected, markdown(&fetched), "the title heading");
    assert!(
        working(&fetched) <= bound(),
        "working {}",
        working(&fetched)
    );
}

#[test]
fn deeply_nested_templates_hold_four_bytes_per_element() {
    let depth = 400_000;
    let mut html = "<template>".repeat(depth);
    assert_eq!(html.len(), 4_000_000);
    html.push('x');
    let fetched = fetch("text/html; charset=utf-8", html.into_bytes());
    assert_eq!(markdown(&fetched), "");
    let empty = fetch("text/html; charset=utf-8", Vec::new());
    let peak = fetched.measured.peak();
    assert!(
        peak <= working(&empty) + WORKING + 4 * depth,
        "plain peak {peak}"
    );
}

/// The body length the finishing tests use: `String` doubling lands
/// exactly on it, so one title byte more doubles the title's capacity.
const FINISH_BODY: usize = 131_072;

/// Fetches a page of a plain title of `title_len` characters over a
/// plain body of `FINISH_BODY`, with the whole result checked exactly:
/// both lengths are exact, so each side of the shorter-part choice in
/// `Writer::finish` measures differently.
fn finish_fetch(title_len: usize) -> Fetched {
    let title = "t".repeat(title_len);
    let body = "x".repeat(FINISH_BODY);
    let html = format!("<title>{title}</title>{body}");
    let fetched = fetch("text/html; charset=utf-8", html.into_bytes());
    let expected = format!("# {title}\n\n{body}\n");
    assert_markdown_eq(&expected, markdown(&fetched), "the titled page");
    fetched
}

/// The empty page's working peak, measured the same way.
fn empty_working() -> usize {
    working(&fetch("text/html; charset=utf-8", Vec::new()))
}

#[test]
fn finishing_a_shorter_title_copies_only_the_title() {
    let baseline = empty_working();
    let fetched = finish_fetch(FINISH_BODY - 1);
    // The title is the shorter part: the working peak holds it, not the
    // body, which became the result.
    assert!(
        working(&fetched) <= baseline + (FINISH_BODY - 1) + WORKING,
        "working {}",
        working(&fetched)
    );
}

#[test]
fn finishing_an_equal_title_copies_either_part() {
    let baseline = empty_working();
    let fetched = finish_fetch(FINISH_BODY);
    // Equal lengths take the title branch: either copy costs the same,
    // so the peak only pins the bound, not the branch.
    assert!(
        working(&fetched) <= baseline + FINISH_BODY + WORKING,
        "working {}",
        working(&fetched)
    );
}

#[test]
fn finishing_a_longer_title_copies_only_the_body() {
    let baseline = empty_working();
    let fetched = finish_fetch(FINISH_BODY + 1);
    // The body is the shorter part: the working peak holds it, not the
    // title, which became the result. The title's capacity doubled past
    // the body's, so copying it instead would cost 128 KiB more.
    assert!(
        working(&fetched) <= baseline + FINISH_BODY + WORKING,
        "working {}",
        working(&fetched)
    );
}

#[test]
fn finishing_a_tiny_title_copies_only_the_title() {
    let baseline = empty_working();
    let title_len = 16;
    let title = "t".repeat(title_len);
    let body = "x".repeat(PAGE);
    let html = format!("<title>{title}</title>{body}");
    let fetched = fetch("text/html; charset=utf-8", html.into_bytes());
    let expected = format!("# {title}\n\n{body}\n");
    assert_markdown_eq(&expected, markdown(&fetched), "the titled page");
    // The title is much shorter than the body: the body buffer becomes
    // the result in place, so the working peak holds only the title.
    // The wrong branch copies the whole body into the title's buffer
    // and holds both at once, about a page over the bound.
    assert!(
        working(&fetched) <= baseline + title_len + WORKING,
        "working {}",
        working(&fetched)
    );
}
