//! Tests beside [`super::WebFetch`]: the tool against `fakes::ProviderServer`
//! on 127.0.0.1 and a fake clock. Every blocking `run` is on a thread whose
//! result is received with a named deadline.

use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use contract::ErrorCode;
use contract::shapes::{ContentPart, Effect};
use contract::tool::{EffectsError, Output, Tool};
use fakes::clock::FakeClock;
use fakes::{CancelToken, ConnectProxy, ProviderServer, Recorder, Response, TempDir};
use serde_json::{Map, Value, json};

use super::{Kind, WebFetch, kind_of, read_reply, read_up_to};
use crate::web_fetch::http::Head;

/// How long a test waits for a fetch to finish.
const WITHIN: Duration = Duration::from_secs(30);

/// How long a test waits for a fake-clock waiter or a request to arrive.
const SIGNAL: Duration = Duration::from_secs(10);

const TEN_MIB: usize = 10 * 1024 * 1024;

const METADATA: IpAddr = IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254));

fn arguments(url: &str) -> Map<String, Value> {
    let Value::Object(map) = json!({ "url": url }) else {
        unreachable!("an object");
    };
    map
}

fn response(status: u16, content_type: Option<&str>, body: impl Into<Vec<u8>>) -> Response {
    Response {
        status,
        headers: content_type
            .map(|ty| ("content-type".to_owned(), ty.to_owned()))
            .into_iter()
            .collect(),
        body: body.into(),
        drop_connection: false,
        stall: false,
    }
}

fn ok(content_type: &str, body: impl Into<Vec<u8>>) -> Response {
    response(200, Some(content_type), body)
}

fn redirect(status: u16, location: &str) -> Response {
    Response {
        status,
        headers: vec![("location".to_owned(), location.to_owned())],
        body: b"moved".to_vec(),
        drop_connection: false,
        stall: false,
    }
}

struct Rig {
    dir: TempDir,
    clock: Arc<FakeClock>,
    tool: Arc<WebFetch>,
}

impl Rig {
    fn new() -> Self {
        let dir = TempDir::new("fiber-web-fetch");
        let clock = FakeClock::new();
        let tool =
            Arc::new(WebFetch::new(dir.path().join("artifacts"), clock.clone()).with_proxy(None));
        Self { dir, clock, tool }
    }

    fn with(self, change: impl FnOnce(WebFetch) -> WebFetch) -> Self {
        let tool = WebFetch::new(self.dir.path().join("artifacts"), self.clock.clone());
        Self {
            tool: Arc::new(change(tool)),
            ..self
        }
    }

    fn artifacts(&self) -> std::path::PathBuf {
        self.dir.path().join("artifacts")
    }

    /// Starts a fetch of `url` on a thread.
    fn start(&self, url: &str) -> Call {
        let tool = Arc::clone(&self.tool);
        let cancel = CancelToken::new();
        let token = cancel.clone();
        let args = arguments(url);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let output = tool.run(&args, &token, &Recorder::default());
            // The test may have stopped waiting.
            match tx.send(output) {
                Ok(()) | Err(_) => {}
            }
        });
        Call { rx, cancel }
    }

    fn fetch(&self, url: &str) -> Output {
        self.start(url).wait()
    }
}

struct Call {
    rx: Receiver<Output>,
    cancel: CancelToken,
}

impl Call {
    fn wait(&self) -> Output {
        self.rx
            .recv_timeout(WITHIN)
            .expect("the fetch finished within its deadline")
    }
}

fn text(output: &Output) -> String {
    match output.content.first() {
        Some(ContentPart::Text { text }) => text.clone(),
        _ => String::new(),
    }
}

fn code(output: &Output) -> Option<ErrorCode> {
    output.error.as_ref().map(|error| error.code.clone())
}

fn serve(script: impl IntoIterator<Item = Response>) -> ProviderServer {
    ProviderServer::start(script).unwrap()
}

#[test]
fn the_result_begins_with_the_final_url_status_and_content_type() {
    let server = serve([ok("text/plain; charset=utf-8", "hello")]);
    let url = format!("{}/page", server.url());
    let output = Rig::new().fetch(&url);
    assert_eq!(code(&output), None);
    assert_eq!(
        text(&output),
        format!("{url} 200 text/plain; charset=utf-8\n\nhello")
    );
}

#[test]
fn the_content_type_is_shown_as_the_server_sent_it() {
    let server = serve([ok("TEXT/Plain ;Charset=X", "x")]);
    let url = format!("{}/", server.url());
    let output = Rig::new().fetch(&url);
    assert_eq!(
        text(&output),
        format!("{url} 200 TEXT/Plain ;Charset=X\n\nx")
    );
}

#[test]
fn html_comes_back_as_markdown() {
    let body = "<html><head><title>T</title><script>x()</script></head><body><h1>Hi</h1><p>a <a href=\"/b\">b</a></p></body></html>";
    let server = serve([ok("text/html; charset=utf-8", body)]);
    let rig = Rig::new();
    let url = format!("{}/", server.url());
    let output = rig.fetch(&url);
    let text = text(&output);
    let first = text.lines().next().unwrap();
    let prefix = format!("{url} 200 text/html; charset=utf-8; raw page at ");
    let path = first.strip_prefix(&prefix).unwrap();
    assert_eq!(fs::read(path).unwrap(), body.as_bytes());
    assert_eq!(
        text,
        format!("{prefix}{path}\n\n# T\n\n# Hi\n\na [b](/b)\n")
    );
}

#[test]
fn an_html_page_is_saved_raw_byte_for_byte() {
    // Bytes that are not valid UTF-8 survive in the saved page.
    let body: Vec<u8> = b"<p>a\xffb</p>".to_vec();
    let server = serve([ok("text/html", body.clone())]);
    let rig = Rig::new();
    let url = format!("{}/", server.url());
    let output = rig.fetch(&url);
    assert_eq!(code(&output), None);
    let text = text(&output);
    let first = text.lines().next().unwrap();
    assert!(
        first.starts_with(&format!("{url} 200 text/html; raw page at ")),
        "{first}"
    );
    let path = first.rsplit_once("; raw page at ").unwrap().1;
    assert!(path.ends_with(".html"), "{path}");
    assert_eq!(fs::read(path).unwrap(), body);
}

#[test]
fn a_shift_jis_page_named_by_the_header_converts_with_its_text_intact() {
    let mut body = b"<p>".to_vec();
    body.extend_from_slice(&[0x82, 0xa0]);
    body.extend_from_slice(b"</p>");
    let server = serve([ok("text/html; charset=shift_jis", body.clone())]);
    let rig = Rig::new();
    let output = rig.fetch(&server.url());
    let text = text(&output);
    assert!(text.contains("\u{3042}"), "{text}");
    let path = text
        .lines()
        .next()
        .unwrap()
        .rsplit_once("; raw page at ")
        .unwrap()
        .1;
    assert_eq!(fs::read(path).unwrap(), body);
}

#[test]
fn a_latin1_page_named_only_by_its_meta_converts_with_its_text_intact() {
    let mut body = b"<meta charset=\"windows-1252\"><p>".to_vec();
    body.extend_from_slice(&[0xe9]);
    body.extend_from_slice(b"</p>");
    let server = serve([ok("text/html", body.clone())]);
    let rig = Rig::new();
    let output = rig.fetch(&server.url());
    let text = text(&output);
    assert!(text.contains("\u{e9}"), "{text}");
    let path = text
        .lines()
        .next()
        .unwrap()
        .rsplit_once("; raw page at ")
        .unwrap()
        .1;
    assert_eq!(fs::read(path).unwrap(), body);
}

#[test]
fn an_html_page_with_no_charset_reads_as_utf8_with_replacement() {
    let body: Vec<u8> = b"<p>a\xffb</p>".to_vec();
    let server = serve([ok("text/html", body)]);
    let output = Rig::new().fetch(&server.url());
    assert!(text(&output).contains("a\u{fffd}b"), "{}", text(&output));
}

#[test]
fn an_html_download_that_cannot_be_saved_is_a_tool_error() {
    let server = serve([ok("text/html", "<p>x</p>")]);
    let rig = Rig::new();
    fs::write(rig.artifacts(), "a file where the directory goes").unwrap();
    let output = rig.fetch(&server.url());
    assert_eq!(code(&output), Some(ErrorCode::ToolError));
    assert!(
        text(&output).contains("could not save"),
        "{}",
        text(&output)
    );
}

#[test]
fn xhtml_comes_back_as_markdown() {
    let server = serve([ok("application/xhtml+xml", "<p>x</p>")]);
    let output = Rig::new().fetch(&server.url());
    assert!(text(&output).ends_with("\n\nx\n"), "{}", text(&output));
}

#[test]
fn text_json_and_xml_come_back_as_they_are() {
    for (content_type, body) in [
        ("text/plain", "a  <b>\n\n  c"),
        ("application/json", "{\"a\": [1,  2]}"),
        ("application/xml", "<a>  <b/> </a>"),
        ("text/css", "p > a { }"),
        ("application/ld+json", "{}"),
        ("application/atom+xml", "<feed/>"),
        ("image/svg+xml", "<svg><text>hi</text></svg>"),
        ("text/xml", "<a/>"),
    ] {
        let server = serve([ok(content_type, body)]);
        let url = format!("{}/", server.url());
        let output = Rig::new().fetch(&url);
        assert_eq!(
            text(&output),
            format!("{url} 200 {content_type}\n\n{body}"),
            "{content_type}"
        );
    }
}

#[test]
fn text_is_decoded_as_utf8_with_invalid_bytes_replaced() {
    let server = serve([ok("text/plain", b"a\xffb".to_vec())]);
    let output = Rig::new().fetch(&server.url());
    assert!(
        text(&output).ends_with("\n\na\u{fffd}b"),
        "{}",
        text(&output)
    );
}

/// The path a "Saved to" line names.
fn saved_path(output: &Output) -> String {
    let text = text(output);
    let line = text
        .lines()
        .find(|line| line.starts_with("Saved to "))
        .unwrap();
    let rest = line.strip_prefix("Saved to ").unwrap();
    let (path, tail) = rest.rsplit_once(" (").unwrap();
    assert!(tail.ends_with(" bytes). Read it with `read`."), "{line}");
    path.to_owned()
}

#[test]
fn a_pdf_and_each_image_type_are_saved_as_downloaded() {
    let bytes: Vec<u8> = (0..=255u8).chain(0..=255u8).collect();
    for (content_type, extension) in [
        ("application/pdf", "pdf"),
        ("image/png", "png"),
        ("image/jpeg", "jpg"),
        ("image/gif", "gif"),
        ("image/webp", "webp"),
        ("IMAGE/PNG; q=1", "png"),
    ] {
        let server = serve([ok(content_type, bytes.clone())]);
        let rig = Rig::new();
        let url = format!("{}/f", server.url());
        let output = rig.fetch(&url);
        assert_eq!(code(&output), None, "{content_type}: {}", text(&output));
        let path = saved_path(&output);
        let file = std::path::Path::new(&path);
        assert_eq!(file.parent().unwrap(), rig.artifacts(), "{content_type}");
        let name = file.file_name().unwrap().to_str().unwrap();
        let stem = name.strip_suffix(&format!(".{extension}")).unwrap();
        let hex = stem.strip_prefix("w_").unwrap();
        assert_eq!(hex.len(), 16, "{name}");
        assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()), "{name}");
        assert_eq!(fs::read(file).unwrap(), bytes, "{content_type}");
        assert_eq!(
            text(&output),
            format!(
                "{url} 200 {content_type}\n\nSaved to {path} ({} bytes). Read it with `read`.\n",
                bytes.len()
            )
        );
    }
}

#[test]
fn every_fetch_saves_under_a_fresh_name() {
    let server = serve([ok("image/png", "a"), ok("image/png", "b")]);
    let rig = Rig::new();
    let first = saved_path(&rig.fetch(&server.url()));
    let second = saved_path(&rig.fetch(&server.url()));
    assert_ne!(first, second);
    assert_eq!(fs::read(first).unwrap(), b"a");
    assert_eq!(fs::read(second).unwrap(), b"b");
}

#[test]
fn a_download_that_cannot_be_saved_is_a_tool_error() {
    let server = serve([ok("application/pdf", "x")]);
    let rig = Rig::new();
    fs::write(rig.artifacts(), "a file where the directory goes").unwrap();
    let output = rig.fetch(&server.url());
    assert_eq!(code(&output), Some(ErrorCode::ToolError));
    assert!(
        text(&output).contains("could not save"),
        "{}",
        text(&output)
    );
}

#[test]
fn another_content_type_fails_unsupported_with_its_type_and_size() {
    let server = serve([ok("application/octet-stream", vec![0u8; 1234])]);
    let output = Rig::new().fetch(&server.url());
    assert_eq!(code(&output), Some(ErrorCode::UnsupportedFile));
    let message = text(&output);
    assert!(message.contains("`application/octet-stream`"), "{message}");
    assert!(message.contains("1234 bytes"), "{message}");
}

#[test]
fn a_missing_content_type_fails_unsupported_with_its_size() {
    let server = serve([response(200, None, "abc")]);
    let output = Rig::new().fetch(&server.url());
    assert_eq!(code(&output), Some(ErrorCode::UnsupportedFile));
    let message = text(&output);
    assert!(message.contains("no content type"), "{message}");
    assert!(message.contains("3 bytes"), "{message}");
}

#[test]
fn a_page_past_the_loops_cut_comes_back_whole() {
    let paragraph = "<p>0123456789 0123456789 0123456789</p>";
    let html = paragraph.repeat(20 * 1024 / paragraph.len() + 1);
    assert!(html.len() > 20 * 1024);
    let server = serve([ok("text/html", html)]);
    let output = Rig::new().fetch(&server.url());
    let expected = "0123456789 0123456789 0123456789\n\n".repeat(20 * 1024 / paragraph.len() + 1);
    let body = text(&output);
    let body = body.split_once("\n\n").unwrap().1;
    assert_eq!(body, format!("{}\n", expected.trim_end()));
    assert!(body.len() > 16 * 1024);
    assert_eq!(
        super::WebFetch::new(std::path::PathBuf::new(), FakeClock::new()).bound(),
        contract::tool::Bound::DEFAULT
    );
}

#[test]
fn a_download_of_exactly_ten_mib_succeeds() {
    let server = serve([ok("text/plain", vec![b'a'; TEN_MIB])]);
    let output = Rig::new().fetch(&server.url());
    assert_eq!(code(&output), None);
    assert!(text(&output).len() > TEN_MIB);
}

#[test]
fn a_download_one_byte_past_ten_mib_fails_too_large() {
    let server = serve([ok("text/plain", vec![b'a'; TEN_MIB + 1])]);
    let output = Rig::new().fetch(&server.url());
    assert_eq!(code(&output), Some(ErrorCode::TooLarge));
    assert!(text(&output).contains("10485760"), "{}", text(&output));
}

#[test]
fn a_download_too_large_for_a_saved_type_is_too_large_too() {
    let server = serve([ok("application/pdf", vec![0u8; TEN_MIB + 1])]);
    let rig = Rig::new();
    let output = rig.fetch(&server.url());
    assert_eq!(code(&output), Some(ErrorCode::TooLarge));
    assert!(!rig.artifacts().exists(), "nothing was saved");
}

#[test]
fn a_status_outside_2xx_fails_http_error_with_the_start_of_the_body() {
    let server = serve([response(404, Some("text/plain"), "no such page")]);
    let url = format!("{}/missing", server.url());
    let output = Rig::new().fetch(&url);
    assert_eq!(code(&output), Some(ErrorCode::HttpError));
    assert_eq!(
        text(&output),
        format!("HTTP 404 from {url}. The body begins:\nno such page\n")
    );
}

#[test]
fn statuses_at_the_edges_of_2xx() {
    for (status, succeeds) in [
        (200, true),
        (204, true),
        (299, true),
        (300, false),
        (400, false),
        (500, false),
        (503, false),
    ] {
        let server = serve([response(status, Some("text/plain"), "b")]);
        let output = Rig::new().fetch(&server.url());
        assert_eq!(
            code(&output).is_none(),
            succeeds,
            "{status}: {}",
            text(&output)
        );
    }
}

#[test]
fn the_classification_of_a_status_at_the_edges_of_2xx() {
    let reply = |status: u16| {
        let head = Head {
            status,
            content_type: None,
            location: None,
            content_length: None,
        };
        match read_reply(head, &mut &b"body"[..]).unwrap() {
            super::Reply::Page(..) => "page",
            super::Reply::Refused(..) => "refused",
            super::Reply::Redirect(_) => "redirect",
        }
    };
    assert_eq!(reply(199), "refused");
    assert_eq!(reply(200), "page");
    assert_eq!(reply(299), "page");
    assert_eq!(reply(300), "refused");
    assert_eq!(reply(301), "refused");
}

#[test]
fn a_body_is_read_into_room_reserved_for_its_stated_length_never_past_the_limit() {
    let body = b"0123456789";
    let read = |limit: u64, hint: Option<u64>| read_up_to(&mut &body[..], limit, hint).unwrap();
    let exact = read(20, Some(10));
    assert_eq!(exact, body);
    assert_eq!(exact.capacity(), 10, "the stated length is reserved");
    let over = read(5, Some(100));
    assert_eq!(over, b"01234");
    assert_eq!(over.capacity(), 5, "never more than the limit is reserved");
    let none = read(20, None);
    assert_eq!(none, body);
    assert!(none.capacity() >= 10);
    let short = read(5, Some(3));
    assert_eq!(
        short, b"01234",
        "a body longer than stated is still cut at the limit"
    );
}

#[test]
fn only_a_redirect_status_with_a_location_redirects() {
    let reply = |status: u16, location: Option<&str>| {
        let head = Head {
            status,
            content_type: None,
            location: location.map(str::to_owned),
            content_length: None,
        };
        matches!(
            read_reply(head, &mut &b""[..]).unwrap(),
            super::Reply::Redirect(_)
        )
    };
    for status in [301, 302, 303, 307, 308] {
        assert!(reply(status, Some("/x")), "{status}");
        assert!(!reply(status, None), "{status} without a location");
    }
    for status in [200, 204, 300, 304, 305, 306, 309, 404] {
        assert!(!reply(status, Some("/x")), "{status} is not a redirect");
    }
}

#[test]
fn the_error_body_is_cut_at_2048_bytes() {
    let server = serve([response(500, Some("text/plain"), "x".repeat(3000))]);
    let output = Rig::new().fetch(&server.url());
    let message = text(&output);
    let body = message
        .split_once("begins:\n")
        .unwrap()
        .1
        .trim_end_matches('\n');
    assert_eq!(body.len(), 2048);
}

#[test]
fn an_error_body_cut_inside_a_character_ends_before_it() {
    let server = serve([response(500, Some("text/plain"), "\u{e9}".repeat(1500))]);
    let output = Rig::new().fetch(&server.url());
    let message = text(&output);
    let body = message
        .split_once("begins:\n")
        .unwrap()
        .1
        .trim_end_matches('\n');
    assert_eq!(body.len(), 2048);
    assert!(!body.contains('\u{fffd}'));
}

#[test]
fn only_the_start_of_an_error_body_is_read() {
    // Two connections would need two responses: the second hop is none.
    let server = serve([response(500, Some("text/plain"), "y".repeat(1_000_000))]);
    let output = Rig::new().fetch(&server.url());
    assert_eq!(code(&output), Some(ErrorCode::HttpError));
    assert!(text(&output).len() < 4096);
}

#[test]
fn a_3xx_without_a_location_is_an_http_error() {
    for status in [300, 304, 305] {
        let server = serve([response(status, Some("text/plain"), "meh")]);
        let output = Rig::new().fetch(&server.url());
        assert_eq!(code(&output), Some(ErrorCode::HttpError), "{status}");
        assert!(text(&output).starts_with(&format!("HTTP {status} from ")));
    }
}

#[test]
fn every_redirect_status_is_followed_with_a_get() {
    for status in [301, 302, 303, 307, 308] {
        let server = serve([redirect(status, "/there"), ok("text/plain", "arrived")]);
        let output = Rig::new().fetch(&format!("{}/here", server.url()));
        assert_eq!(code(&output), None, "{status}");
        assert!(text(&output).ends_with("\n\narrived"), "{status}");
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert!(
            requests
                .iter()
                .all(|r| r.method == "GET" && r.body.is_empty())
        );
        assert_eq!(requests[1].path, "/there");
    }
}

#[test]
fn the_final_url_is_the_one_after_the_redirects() {
    let server = serve([redirect(302, "/b"), ok("text/plain", "x")]);
    let output = Rig::new().fetch(&format!("{}/a", server.url()));
    assert!(
        text(&output).starts_with(&format!("{}/b 200 text/plain", server.url())),
        "{}",
        text(&output)
    );
}

#[test]
fn a_relative_location_is_joined_to_the_current_url() {
    let server = serve([
        redirect(302, "next"),
        redirect(302, "../up"),
        redirect(302, "?q=1"),
        ok("text/plain", "x"),
    ]);
    Rig::new().fetch(&format!("{}/a/b/c", server.url()));
    let paths: Vec<_> = server.requests().into_iter().map(|r| r.path).collect();
    assert_eq!(
        paths,
        ["/a/b/c", "/a/b/next", "/a/b/../up", "/a/b/../up?q=1"]
    );
}

#[test]
fn a_location_header_name_is_matched_without_regard_to_case() {
    let mut moved = redirect(302, "/z");
    moved.headers = vec![("LoCaTiOn".to_owned(), "/z".to_owned())];
    let server = serve([moved, ok("text/plain", "x")]);
    let output = Rig::new().fetch(&server.url());
    assert_eq!(code(&output), None);
}

#[test]
fn ten_redirects_are_followed_and_the_eleventh_fails() {
    let ten: Vec<_> = (1..=10).map(|n| redirect(302, &format!("/{n}"))).collect();
    let server = serve(ten.into_iter().chain([ok("text/plain", "end")]));
    let output = Rig::new().fetch(&format!("{}/0", server.url()));
    assert_eq!(code(&output), None, "{}", text(&output));
    assert!(text(&output).ends_with("\n\nend"));
    assert_eq!(server.requests().len(), 11);

    let eleven: Vec<_> = (1..=11).map(|n| redirect(302, &format!("/{n}"))).collect();
    let server = serve(eleven.into_iter().chain([ok("text/plain", "never")]));
    let output = Rig::new().fetch(&format!("{}/0", server.url()));
    assert_eq!(code(&output), Some(ErrorCode::HttpError));
    assert!(
        text(&output).contains("more than 10 redirects"),
        "{}",
        text(&output)
    );
    assert_eq!(
        server.requests().len(),
        11,
        "the twelfth request is never sent"
    );
}

#[test]
fn a_redirect_to_another_scheme_is_an_http_error_naming_it() {
    for location in [
        "ftp://example.com/x",
        "mailto:a@b.c",
        "file:///etc/passwd",
        "http://",
    ] {
        let server = serve([redirect(302, location)]);
        let output = Rig::new().fetch(&server.url());
        assert_eq!(code(&output), Some(ErrorCode::HttpError), "{location}");
        assert!(text(&output).contains(location), "{}", text(&output));
        assert_eq!(server.requests().len(), 1);
    }
}

#[test]
fn http_is_fetched_as_written_and_the_fragment_is_not_sent() {
    let server = serve([ok("text/plain", "x")]);
    let url = format!("{}/p?q=1", server.url());
    let output = Rig::new().fetch(&format!("{url}#frag"));
    assert!(
        text(&output).starts_with(&format!("{url} 200 ")),
        "{}",
        text(&output)
    );
    assert!(url.starts_with("http://"));
    assert_eq!(server.requests()[0].path, "/p?q=1");
}

#[test]
fn a_bad_url_in_run_is_invalid_arguments() {
    let rig = Rig::new();
    for url in ["example.com", "ftp://example.com/", "http://", ""] {
        let output = rig.fetch(url);
        assert_eq!(code(&output), Some(ErrorCode::InvalidArguments), "{url:?}");
    }
    let tool = Arc::clone(&rig.tool);
    for args in [json!({}), json!({"url": 5})] {
        let Value::Object(args) = args else {
            unreachable!()
        };
        let output = tool.run(&args, &CancelToken::new(), &Recorder::default());
        assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    }
}

/// The addresses a resolver hands out: `answers` by name, else loopback.
fn resolver_for(name: &'static str, answer: IpAddr) -> super::Resolve {
    Arc::new(move |host, port| {
        if host == name {
            Ok(vec![SocketAddr::new(answer, port)])
        } else {
            Ok(vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)])
        }
    })
}

#[test]
fn a_name_the_resolver_maps_is_fetched_at_the_address_it_gave() {
    let server = serve([ok("text/plain", "x")]);
    let port = server.url().rsplit(':').next().unwrap().to_owned();
    let rig = Rig::new().with(|tool| {
        tool.with_proxy(None)
            .with_resolver(resolver_for("mapped.test", IpAddr::V4(Ipv4Addr::LOCALHOST)))
    });
    let url = format!("http://mapped.test:{port}/x");
    let output = rig.fetch(&url);
    assert_eq!(code(&output), None, "{}", text(&output));
    assert!(text(&output).starts_with(&format!("{url} 200 ")));
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_redirect_to_the_metadata_address_is_blocked_before_any_request() {
    for target in [
        "http://169.254.169.254/latest/meta-data/",
        "http://169.254.0.1/",
        "http://[fe80::1]/",
        "http://[::ffff:169.254.169.254]/",
        "http://[fd00:ec2::254]/",
        "http://metadata.google.internal/computeMetadata/v1/",
        "http://METADATA.GOOGLE.INTERNAL./",
        "http://metadata.goog/",
    ] {
        let server = serve([redirect(302, target), ok("text/plain", "never")]);
        let output = Rig::new().fetch(&server.url());
        assert_eq!(
            code(&output),
            Some(ErrorCode::BlockedHost),
            "{target}: {}",
            text(&output)
        );
        assert_eq!(server.requests().len(), 1, "{target}");
    }
}

#[test]
fn a_url_written_as_a_blocked_address_is_blocked_too() {
    let rig = Rig::new();
    for url in [
        "http://169.254.169.254/",
        "http://[fe80::1]/",
        "http://[::ffff:a9fe:a9fe]/",
        "http://[::ffff:169.254.169.254]:8080/x",
        "http://metadata.google.internal/",
        "http://metadata.google.internal:80/",
    ] {
        let output = rig.fetch(url);
        assert_eq!(
            code(&output),
            Some(ErrorCode::BlockedHost),
            "{url}: {}",
            text(&output)
        );
    }
}

#[test]
fn a_decimal_form_of_the_metadata_address_is_blocked_after_resolution() {
    let output = Rig::new().fetch("http://2852039166/");
    assert_eq!(
        code(&output),
        Some(ErrorCode::BlockedHost),
        "{}",
        text(&output)
    );
}

#[test]
fn a_name_that_resolves_to_a_blocked_address_is_blocked_and_nothing_is_sent() {
    let server = serve([ok("text/plain", "never")]);
    let port = server.url().rsplit(':').next().unwrap().to_owned();
    let rig = Rig::new().with(|tool| {
        tool.with_proxy(None)
            .with_resolver(resolver_for("evil.test", METADATA))
    });
    let output = rig.fetch(&format!("http://evil.test:{port}/"));
    assert_eq!(code(&output), Some(ErrorCode::BlockedHost));
    assert!(server.requests().is_empty());
}

#[test]
fn one_blocked_address_among_several_blocks_the_name() {
    let server = serve([ok("text/plain", "never")]);
    let port = server.url().rsplit(':').next().unwrap().to_owned();
    let rig = Rig::new().with(|tool| {
        tool.with_proxy(None).with_resolver(Arc::new(|_, port| {
            Ok(vec![
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
                SocketAddr::new(METADATA, port),
            ])
        }))
    });
    let output = rig.fetch(&format!("http://mixed.test:{port}/"));
    assert_eq!(code(&output), Some(ErrorCode::BlockedHost));
    assert!(server.requests().is_empty());
}

#[test]
fn a_redirect_to_a_name_that_resolves_to_a_blocked_address_is_blocked() {
    let server = serve([
        redirect(302, "http://evil.test:9/"),
        ok("text/plain", "never"),
    ]);
    let rig = Rig::new().with(|tool| {
        tool.with_proxy(None)
            .with_resolver(resolver_for("evil.test", METADATA))
    });
    let output = rig.fetch(&server.url());
    assert_eq!(code(&output), Some(ErrorCode::BlockedHost));
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_name_is_resolved_once_a_hop_so_a_second_answer_cannot_swap_the_address() {
    let server = serve([ok("text/plain", "x")]);
    let port = server.url().rsplit(':').next().unwrap().to_owned();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let rig = Rig::new().with(|tool| {
        tool.with_proxy(None)
            .with_resolver(Arc::new(move |_, port| {
                // The first answer passes; any later lookup would be the
                // metadata address.
                let ip = if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                    IpAddr::V4(Ipv4Addr::LOCALHOST)
                } else {
                    METADATA
                };
                Ok(vec![SocketAddr::new(ip, port)])
            }))
    });
    let output = rig.fetch(&format!("http://rebind.test:{port}/"));
    assert_eq!(code(&output), None, "{}", text(&output));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn loopback_and_internal_names_are_allowed() {
    let rig = Rig::new().with(|tool| {
        tool.with_proxy(None)
            .with_resolver(resolver_for("nothing", METADATA))
    });
    for host in ["intranet.test", "localhost", "127.0.0.1"] {
        let server = serve([ok("text/plain", "y")]);
        let output = rig.fetch(&format!("http://{host}:{}/", port_of(&server)));
        assert_eq!(code(&output), None, "{host}: {}", text(&output));
    }
}

#[test]
fn a_failed_lookup_is_connection_failed() {
    let rig = Rig::new().with(|tool| {
        tool.with_proxy(None)
            .with_resolver(Arc::new(|_, _| Err(std::io::Error::other("no such name"))))
    });
    let output = rig.fetch("http://nowhere.test/");
    assert_eq!(code(&output), Some(ErrorCode::ConnectionFailed));
    assert!(text(&output).contains("no such name"), "{}", text(&output));
}

#[test]
fn a_lookup_with_no_address_is_connection_failed() {
    let rig = Rig::new().with(|tool| {
        tool.with_proxy(None)
            .with_resolver(Arc::new(|_, _| Ok(Vec::new())))
    });
    let output = rig.fetch("http://empty.test/");
    assert_eq!(code(&output), Some(ErrorCode::ConnectionFailed));
}

#[test]
fn a_refused_connection_is_connection_failed() {
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let output = Rig::new().fetch(&format!("http://127.0.0.1:{port}/"));
    assert_eq!(code(&output), Some(ErrorCode::ConnectionFailed));
}

#[test]
fn a_dropped_connection_is_connection_failed() {
    let server = serve([Response::drop_connection()]);
    let output = Rig::new().fetch(&server.url());
    assert_eq!(code(&output), Some(ErrorCode::ConnectionFailed));
}

/// Runs a held fetch: the server holds every response, the fetch starts, and
/// the watcher parks at `origin + deadline`.
fn held(rig: &Rig, server: &ProviderServer, url: &str, deadline: Duration) -> Call {
    server.hold();
    let call = rig.start(url);
    assert!(
        rig.clock
            .await_parked(rig.clock.origin() + deadline, SIGNAL),
        "the watcher waits for the deadline"
    );
    call
}

#[test]
fn a_request_that_takes_over_sixty_seconds_times_out() {
    let server = serve([ok("text/plain", "late")]);
    let rig = Rig::new();
    let call = held(&rig, &server, &server.url(), Duration::from_secs(60));
    assert!(server.await_requests(1, SIGNAL), "the request is in flight");
    rig.clock.advance(Duration::from_secs(60));
    let output = call.wait();
    assert_eq!(code(&output), Some(ErrorCode::Timeout));
    assert!(
        text(&output).contains("the request took over 60 seconds"),
        "{}",
        text(&output)
    );
}

#[test]
fn a_request_one_second_short_of_sixty_is_still_waiting() {
    let server = serve([ok("text/plain", "late")]);
    let rig = Rig::new();
    let call = held(&rig, &server, &server.url(), Duration::from_secs(60));
    assert!(server.await_requests(1, SIGNAL));
    let mark = rig.clock.advance_marked(Duration::from_secs(59));
    assert!(
        rig.clock.await_parked_since(
            &mark,
            Some(rig.clock.origin() + Duration::from_secs(60)),
            SIGNAL
        ),
        "the watcher waits again a second before its deadline"
    );
    assert!(
        call.rx.try_recv().is_err(),
        "the fetch ended a second before its deadline"
    );
    server.release();
    let output = call.wait();
    assert_eq!(code(&output), None, "{}", text(&output));
}

/// A response that sends its head and the first bytes of a 100-byte body,
/// then holds the connection until the client closes it: the body read
/// blocks past the headers.
fn stall() -> Response {
    Response::stall(200, b"partial".to_vec(), 100).header("content-type", "text/plain")
}

#[test]
fn a_deadline_that_passes_while_the_body_is_read_times_out() {
    let server = serve([stall()]);
    let rig = Rig::new();
    let call = rig.start(&server.url());
    assert!(
        rig.clock
            .await_parked(rig.clock.origin() + Duration::from_secs(60), SIGNAL)
    );
    // Past headers: the partial body is what the read is blocked on.
    assert!(
        server.await_partial(1, SIGNAL),
        "the partial response was sent"
    );
    rig.clock.advance(Duration::from_secs(60));
    let output = call.wait();
    assert_eq!(code(&output), Some(ErrorCode::Timeout), "{}", text(&output));
    assert!(
        server.await_closed(1, SIGNAL),
        "the fetch closed the socket"
    );
}

#[test]
fn a_cancel_while_the_body_is_read_stops_the_call() {
    let server = serve([stall()]);
    let rig = Rig::new();
    let call = rig.start(&server.url());
    assert!(
        rig.clock
            .await_parked(rig.clock.origin() + Duration::from_secs(60), SIGNAL)
    );
    assert!(
        server.await_partial(1, SIGNAL),
        "the partial response was sent"
    );
    call.cancel.cancel();
    let output = call.wait();
    assert_eq!(code(&output), None);
    assert_eq!(text(&output), "Cancelled and stopped.\n");
    assert!(
        server.await_closed(1, SIGNAL),
        "the fetch closed the socket"
    );
}

#[test]
fn a_cancel_during_a_held_response_returns_promptly_and_says_it_stopped() {
    let server = serve([ok("text/plain", "late")]);
    let rig = Rig::new();
    let call = held(&rig, &server, &server.url(), Duration::from_secs(60));
    assert!(server.await_requests(1, SIGNAL));
    call.cancel.cancel();
    let output = call.wait();
    assert_eq!(code(&output), None);
    assert_eq!(text(&output), "Cancelled and stopped.\n");
}

#[test]
fn a_call_cancelled_before_it_starts_sends_nothing() {
    let server = serve([ok("text/plain", "never")]);
    let rig = Rig::new();
    let cancel = CancelToken::new();
    cancel.cancel();
    let output = rig
        .tool
        .run(&arguments(&server.url()), &cancel, &Recorder::default());
    assert_eq!(text(&output), "Cancelled and stopped.\n");
    assert!(server.requests().is_empty());
}

#[test]
fn the_whole_fetch_times_out_after_five_minutes_whatever_each_hop_has_left() {
    for hop in [55u64, 48u64] {
        let hops = 5;
        let script: Vec<_> = (1..=hops)
            .map(|n| redirect(302, &format!("/{n}")))
            .collect();
        let server = serve(script.into_iter().chain([ok("text/plain", "never")]));
        let rig = Rig::new();
        server.hold();
        let call = rig.start(&format!("{}/0", server.url()));
        let origin = rig.clock.origin();
        let mut now = 0u64;
        // Each hop is held short of its own 60-second deadline.
        for _ in 0..hops {
            assert!(
                rig.clock
                    .await_parked(origin + Duration::from_secs(now + 60), SIGNAL),
                "hop deadline {} s",
                now + 60
            );
            rig.clock.advance(Duration::from_secs(hop));
            now += hop;
            server.release_one();
        }
        // The sixth hop starts at `now`, so its own deadline would be
        // `now + 60`; the fetch's 300 s is the earlier one.
        assert_eq!(now, 5 * hop);
        assert!(
            rig.clock
                .await_parked(origin + Duration::from_secs(300), SIGNAL),
            "the sixth hop waits for the fetch's deadline"
        );
        rig.clock.advance(Duration::from_secs(300 - now));
        let output = call.wait();
        assert_eq!(code(&output), Some(ErrorCode::Timeout), "hop {hop}s");
        assert!(
            text(&output).contains("the fetch took over 5 minutes"),
            "hop {hop}s: {}",
            text(&output)
        );
    }
}

#[test]
fn a_hop_whose_own_deadline_is_earlier_than_the_fetchs_is_a_request_timeout() {
    let server = serve([redirect(302, "/1"), ok("text/plain", "late")]);
    let rig = Rig::new();
    server.hold();
    let call = rig.start(&format!("{}/0", server.url()));
    let origin = rig.clock.origin();
    assert!(
        rig.clock
            .await_parked(origin + Duration::from_secs(60), SIGNAL)
    );
    rig.clock.advance(Duration::from_secs(40));
    server.release_one();
    assert!(
        rig.clock
            .await_parked(origin + Duration::from_secs(100), SIGNAL)
    );
    rig.clock.advance(Duration::from_secs(60));
    let output = call.wait();
    assert!(
        text(&output).contains("the request took over 60 seconds"),
        "{}",
        text(&output)
    );
}

/// A proxy value pointing at `proxy`, bypassing nothing.
fn proxy_through(proxy: &ConnectProxy) -> ureq::Proxy {
    ureq::Proxy::builder(ureq::ProxyProtocol::Http)
        .host("127.0.0.1")
        .port(proxy.port())
        .build()
        .unwrap()
}

fn port_of(server: &ProviderServer) -> String {
    server.url().rsplit(':').next().unwrap().to_owned()
}

#[test]
fn a_fetch_through_a_proxy_tunnels_through_it() {
    let server = serve([ok("text/plain", "via proxy")]);
    let proxy = ConnectProxy::start().unwrap();
    let rig = Rig::new().with(|tool| tool.with_proxy(Some(proxy_through(&proxy))));
    let output = rig.fetch(&server.url());
    assert_eq!(code(&output), None, "{}", text(&output));
    assert!(text(&output).ends_with("via proxy"));
    assert!(proxy.await_connects(1, SIGNAL));
    assert_eq!(
        proxy.connects(),
        [format!("127.0.0.1:{}", port_of(&server))]
    );
}

#[test]
fn a_host_past_no_proxy_bypasses_the_proxy() {
    let server = serve([ok("text/plain", "direct")]);
    let proxy = ConnectProxy::start().unwrap();
    let bypass = ureq::Proxy::builder(ureq::ProxyProtocol::Http)
        .host("127.0.0.1")
        .port(proxy.port())
        .no_proxy("127.0.0.1")
        .build()
        .unwrap();
    let rig = Rig::new().with(|tool| tool.with_proxy(Some(bypass)));
    let output = rig.fetch(&server.url());
    assert_eq!(code(&output), None, "{}", text(&output));
    assert!(proxy.connects().is_empty());
}

#[test]
fn a_blocked_address_is_refused_even_through_a_proxy() {
    let server = serve([ok("text/plain", "never")]);
    let proxy = ConnectProxy::start().unwrap();
    let rig = Rig::new().with(|tool| {
        tool.with_proxy(Some(proxy_through(&proxy)))
            .with_resolver(resolver_for("evil.test", METADATA))
    });
    let output = rig.fetch(&format!("http://evil.test:{}/", port_of(&server)));
    assert_eq!(code(&output), Some(ErrorCode::BlockedHost));
    assert!(proxy.connects().is_empty());
}

#[test]
fn a_redirect_to_a_blocked_name_is_refused_through_a_proxy() {
    let server = serve([
        redirect(302, "http://evil.test:9/"),
        ok("text/plain", "never"),
    ]);
    let proxy = ConnectProxy::start().unwrap();
    let rig = Rig::new().with(|tool| {
        tool.with_proxy(Some(proxy_through(&proxy)))
            .with_resolver(resolver_for("evil.test", METADATA))
    });
    let target = format!("localhost:{}", port_of(&server));
    let output = rig.fetch(&format!("http://{target}/"));
    assert_eq!(code(&output), Some(ErrorCode::BlockedHost));
    assert_eq!(proxy.connects(), [target]);
}

#[test]
fn the_metadata_name_is_refused_through_a_proxy_resolving_or_not() {
    for resolves in [true, false] {
        let proxy = ConnectProxy::start().unwrap();
        let rig = Rig::new().with(|tool| {
            tool.with_proxy(Some(proxy_through(&proxy)))
                .with_resolver(Arc::new(move |_, port| {
                    if resolves {
                        Ok(vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)])
                    } else {
                        Err(std::io::Error::other("no local name"))
                    }
                }))
        });
        let output = rig.fetch("http://metadata.google.internal/");
        assert_eq!(code(&output), Some(ErrorCode::BlockedHost), "{resolves}");
        assert!(proxy.connects().is_empty(), "{resolves}");
    }
}

#[test]
fn a_name_only_the_proxy_can_resolve_is_not_refused() {
    let server = serve([ok("text/plain", "resolved by the proxy")]);
    let proxy = ConnectProxy::start().unwrap();
    let rig = Rig::new().with(|tool| {
        tool.with_proxy(Some(proxy_through(&proxy)))
            .with_resolver(Arc::new(|_, _| Err(std::io::Error::other("no local name"))))
    });
    let target = format!("localhost:{}", port_of(&server));
    let output = rig.fetch(&format!("http://{target}/"));
    assert_eq!(code(&output), None, "{}", text(&output));
    assert_eq!(proxy.connects(), [target]);
}

#[test]
fn the_effects_are_network_and_irreversible_with_the_url_as_parsed() {
    let tool = Rig::new().tool;
    let effects = tool.effects(&arguments("HTTPS://Example.com#top")).unwrap();
    assert_eq!(effects.declared.effects, vec![Effect::Network]);
    assert!(!effects.declared.reversible);
    assert_eq!(effects.declared.paths, None);
    assert_eq!(effects.subject.as_deref(), Some("https://example.com/"));
    assert_eq!(effects.prefix.as_deref(), Some("https://example.com/"));
    let effects = tool
        .effects(&arguments("http://localhost:8080/a/b?x=1"))
        .unwrap();
    assert_eq!(
        effects.subject.as_deref(),
        Some("http://localhost:8080/a/b?x=1")
    );
    assert_eq!(effects.prefix.as_deref(), Some("http://localhost:8080/"));
}

#[test]
fn a_url_the_effects_cannot_parse_is_an_arguments_error() {
    let tool = Rig::new().tool;
    for url in ["example.com", "ftp://example.com/", "http://"] {
        assert!(
            matches!(
                tool.effects(&arguments(url)),
                Err(EffectsError::Arguments(_))
            ),
            "{url}"
        );
    }
    let Value::Object(wrong) = json!({"url": 5}) else {
        unreachable!()
    };
    assert!(matches!(
        tool.effects(&wrong),
        Err(EffectsError::Arguments(_))
    ));
    assert!(matches!(
        tool.effects(&Map::new()),
        Err(EffectsError::Arguments(_))
    ));
}

#[test]
fn the_definition_takes_a_url_only() {
    let definition = Rig::new().tool.definition();
    assert_eq!(definition.name, "web_fetch");
    assert!(!definition.deferred);
    assert_eq!(
        definition.input_schema,
        json!({
            "type": "object",
            "properties": {"url": {"type": "string", "description": definition.input_schema["properties"]["url"]["description"].clone()}},
            "required": ["url"],
            "additionalProperties": false
        })
    );
    assert!(Rig::new().tool.guidelines().is_none());
}

fn kind_name(content_type: &str) -> &'static str {
    match kind_of(content_type) {
        Kind::Markdown => "markdown",
        Kind::Text => "text",
        Kind::Saved(extension) => extension,
        Kind::Unsupported => "unsupported",
    }
}

#[test]
fn a_content_type_is_classified_by_its_media_type_alone() {
    for (content_type, expected) in [
        ("text/html", "markdown"),
        ("TEXT/HTML; charset=UTF-8", "markdown"),
        ("  text/html  ;charset=x", "markdown"),
        ("application/xhtml+xml", "markdown"),
        ("text/plain", "text"),
        ("text/markdown; variant=GFM", "text"),
        ("text/x-anything", "text"),
        ("application/json", "text"),
        ("Application/JSON;charset=utf-8", "text"),
        ("application/xml", "text"),
        ("application/vnd.api+json", "text"),
        ("application/rss+xml", "text"),
        ("image/svg+xml", "text"),
        ("application/pdf", "pdf"),
        ("application/PDF ; x=y", "pdf"),
        ("image/png", "png"),
        ("image/jpeg", "jpg"),
        ("image/gif", "gif"),
        ("image/webp", "webp"),
        ("application/octet-stream", "unsupported"),
        ("application/zip", "unsupported"),
        ("image/bmp", "unsupported"),
        ("video/mp4", "unsupported"),
        ("audio/mpeg", "unsupported"),
        ("font/woff2", "unsupported"),
        ("video/vnd+json", "unsupported"),
        ("image/x+json", "unsupported"),
        ("model/x+xml", "unsupported"),
        ("application/json-seq", "unsupported"),
        ("textual/plain", "unsupported"),
        ("", "unsupported"),
        (";text/plain", "unsupported"),
    ] {
        assert_eq!(kind_name(content_type), expected, "{content_type:?}");
    }
}

#[test]
fn the_requests_carry_no_body_and_a_get() {
    let server = serve([ok("text/plain", "x")]);
    Rig::new().fetch(&server.url());
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert!(requests[0].body.is_empty());
}
