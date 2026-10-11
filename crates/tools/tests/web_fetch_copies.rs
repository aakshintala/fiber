//! How many copies of a fetched page `web_fetch` holds at once
//! (`docs/tools.md`, "web_fetch"): the large blocks, 1 MiB or more, alive
//! together on the fetching thread while a 4 MiB page is fetched, with the
//! result still held. This binary installs the counting allocator and holds
//! only these tests, so no other test binary changes allocator.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code, helpers included"
)]

use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::Duration;

use contract::ErrorCode;
use contract::shapes::ContentPart;
use contract::tool::{Output, Tool};
use fakes::alloc::{Counting, large_blocks_during};
use fakes::clock::FakeClock;
use fakes::{CancelToken, ProviderServer, Recorder, Response, TempDir, within};
use serde_json::{Map, Value};
use tools::WebFetch;

#[global_allocator]
static ALLOC: Counting = Counting;

/// The size of each page: four large blocks' worth.
const PAGE: usize = 4 << 20;

/// How long a fetch may take.
const FETCH: Duration = Duration::from_secs(60);

/// One fetch of a page served as `content_type`, measured.
struct Fetched {
    url: String,
    output: Output,
    /// The most large blocks alive at once on the fetching thread.
    peak: usize,
    artifacts: PathBuf,
    _dir: TempDir,
    _server: ProviderServer,
}

/// Serves `page` as `content_type` and fetches it inside a counting scope,
/// after checking that the allocator counts at all.
fn fetch(content_type: &str, page: Vec<u8>) -> Fetched {
    let ((), calibration) =
        large_blocks_during(|| drop(black_box(Vec::<u8>::with_capacity(2 << 20))));
    assert_eq!(calibration, 1, "the counting allocator is installed");
    let server = ProviderServer::start([Response {
        status: 200,
        headers: vec![("content-type".to_owned(), content_type.to_owned())],
        body: page,
        drop_connection: false,
        stall: false,
    }])
    .expect("the page server starts");
    let url = format!("{}/page", server.url());
    let dir = TempDir::new("fiber-web-fetch-copies");
    let artifacts = dir.path().join("artifacts");
    let tool = WebFetch::new(artifacts.clone(), FakeClock::new()).with_proxy(None);
    let mut arguments = Map::new();
    arguments.insert("url".to_owned(), Value::String(url.clone()));
    let (output, peak) = within("the fetch", FETCH, move || {
        let cancel = CancelToken::new();
        let emit = Recorder::default();
        large_blocks_during(|| tool.run(&arguments, &cancel, &emit))
    });
    Fetched {
        url,
        output,
        peak,
        artifacts,
        _dir: dir,
        _server: server,
    }
}

impl Fetched {
    fn text(&self) -> &str {
        match self.output.content.first() {
            Some(ContentPart::Text { text }) => text,
            _ => "",
        }
    }

    fn code(&self) -> Option<ErrorCode> {
        self.output.error.as_ref().map(|error| error.code.clone())
    }

    /// The artifact the first line names after `marker`, checked to be a
    /// fresh `w_<16 hex>.<extension>` file in `artifacts/`.
    fn artifact(&self, line: &str, marker: &str, extension: &str) -> String {
        let path = line
            .rsplit_once(marker)
            .map(|(_, path)| path.to_owned())
            .unwrap_or_default();
        let file = std::path::Path::new(&path);
        assert_eq!(file.parent(), Some(self.artifacts.as_path()), "{line}");
        let name = file
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let hex = name
            .strip_suffix(&format!(".{extension}"))
            .and_then(|stem| stem.strip_prefix("w_"))
            .unwrap_or_default();
        assert_eq!(hex.len(), 16, "{name}");
        assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()), "{name}");
        path
    }
}

/// An HTML page of `PAGE` bytes: `start`, then `block` repeated as often as
/// it fits, then spaces, which convert to nothing.
fn html(start: &[u8], block: &[u8]) -> (Vec<u8>, usize) {
    let mut page = [b"<html><body>".as_slice(), start].concat();
    let mut blocks = 0;
    while page.len() + block.len() + "</body></html>".len() <= PAGE {
        page.extend_from_slice(block);
        blocks += 1;
    }
    page.resize(PAGE - "</body></html>".len(), b' ');
    page.extend_from_slice(b"</body></html>");
    (page, blocks)
}

/// The markdown of `blocks` paragraphs reading `text`.
fn paragraphs(text: &str, blocks: usize) -> String {
    let mut markdown = format!("{text}\n\n").repeat(blocks);
    markdown.truncate(markdown.len() - 1);
    markdown
}

fn assert_html(fetched: &Fetched, content_type: &str, page: &[u8], markdown: &str) {
    assert_eq!(fetched.code(), None, "{}", fetched.text());
    let text = fetched.text();
    let (first, _) = text.split_once("\n\n").unwrap_or_default();
    let path = fetched.artifact(first, "; raw page at ", "html");
    assert_eq!(
        text,
        format!(
            "{} 200 {content_type}; raw page at {path}\n\n{markdown}",
            fetched.url
        )
    );
    assert!(
        fs::read(&path).expect("the raw page is saved") == page,
        "the raw page is saved byte for byte"
    );
}

#[test]
fn an_html_page_with_large_markdown_holds_one_large_block() {
    let paragraph = "turn tool call file session context model log";
    let (page, blocks) = html(b"", format!("<p>{paragraph}</p>").as_bytes());
    let markdown = paragraphs(paragraph, blocks);
    assert!(markdown.len() > 1 << 20);
    let content_type = "text/html; charset=utf-8";
    let fetched = fetch(content_type, page.clone());
    assert_html(&fetched, content_type, &page, &markdown);
    assert_eq!(fetched.peak, 1, "the markdown only");
}

#[test]
fn a_text_page_holds_one_large_block() {
    let page = "turn tool call file session context model log\n"
        .repeat(PAGE / 46)
        .into_bytes();
    let content_type = "text/plain; charset=utf-8";
    let fetched = fetch(content_type, page.clone());
    assert_eq!(fetched.code(), None, "{}", fetched.text());
    let expected = [
        format!("{} 200 {content_type}\n\n", fetched.url).as_bytes(),
        &page,
    ]
    .concat();
    assert!(
        fetched.text().as_bytes() == expected,
        "the result is the first line and the page"
    );
    assert_eq!(fetched.peak, 1, "the page once, as the result");
}
