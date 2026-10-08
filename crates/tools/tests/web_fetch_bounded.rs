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
