//! `web_fetch` (`docs/tools.md`, "web_fetch"): one GET of a URL the model
//! names, redirects followed hop by hop, each hop judged and bounded, and the
//! page returned as text or saved to the session's `artifacts/`.

mod charset;
mod download;
mod http;
mod markdown;
mod target;

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::io::{self, Read};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::emit::Emit;
use contract::provider::ToolDefinition;
use contract::shapes::{DeclaredEffects, Effect};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};
use ureq::http::Uri;

use crate::files::{failed, string_argument, text_output};
use download::{Artifact, Html, Sink, Wrap};
use http::{Ended, Get, Head, Hop, Limit, Stop, guarded};

const MISSING: &str = "Give the page's address as `url`.";

/// The most a body may hold: a download past it fails `too_large`.
const MAX_BODY: u64 = 10 * 1024 * 1024;

/// How much of an error response's body the failure message keeps.
const ERROR_BODY: u64 = 2048;

/// How many redirects are followed; the next one fails.
const MAX_REDIRECTS: u32 = 10;

/// How long one request may take, and how long the whole fetch.
const REQUEST_LIMIT: Duration = Duration::from_secs(60);
const FETCH_LIMIT: Duration = Duration::from_secs(5 * 60);

/// Looks a name up. Tests inject their own.
type Resolve = Arc<dyn Fn(&str, u16) -> io::Result<Vec<SocketAddr>> + Send + Sync>;

/// Fetches a page over HTTP or HTTPS, on this machine, for every provider.
pub struct WebFetch {
    artifacts: PathBuf,
    clock: Arc<dyn Clock>,
    resolve: Resolve,
    /// `Some` fixes the proxy: tests, so a developer's environment does not
    /// reach them. `None` reads it from the environment on every call.
    proxy: Option<Option<ureq::Proxy>>,
    /// Wraps each artifact's file: tests only.
    wrap: Option<Wrap>,
}

impl WebFetch {
    /// A fetch that saves what it downloads under `artifacts` and measures
    /// its deadlines on `clock`.
    pub fn new(artifacts: PathBuf, clock: Arc<dyn Clock>) -> Self {
        Self {
            artifacts,
            clock,
            resolve: Arc::new(|host, port| {
                (host, port)
                    .to_socket_addrs()
                    .map(|addresses| addresses.collect())
            }),
            proxy: None,
            wrap: None,
        }
    }

    /// Looks names up with `resolve`.
    #[cfg(test)]
    pub(crate) fn with_resolver(mut self, resolve: Resolve) -> Self {
        self.resolve = resolve;
        self
    }

    /// Writes each artifact through `wrap`'s writer around its file.
    #[cfg(test)]
    pub(crate) fn with_artifact_writer(mut self, wrap: Wrap) -> Self {
        self.wrap = Some(wrap);
        self
    }

    /// Fixes the proxy, `None` meaning a direct connection, so the proxy
    /// environment is not read.
    #[must_use]
    pub fn with_proxy(mut self, proxy: Option<ureq::Proxy>) -> Self {
        self.proxy = Some(proxy);
        self
    }
}

impl Tool for WebFetch {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "web_fetch".to_owned(),
            description: "Fetches a web page and returns it as text. There is no prompt: you \
                 read the page yourself. The result begins with one line giving the final \
                 URL after redirects, the HTTP status and the content type, and, for HTML, \
                 the path of the raw page in `artifacts/`. HTML is \
                 converted to markdown; other text, JSON and XML come back as they are. A \
                 PDF, or a PNG, JPEG, GIF or WebP image, is saved to `artifacts/` and the \
                 result gives its path, which you read with `read`. A long page is cut, and \
                 the whole page is in an artifact you can `read`."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The address to fetch, `http://` or `https://`, as written."
                    }
                },
                "required": ["url"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        let url = string_argument(arguments, "url", MISSING).map_err(EffectsError::Arguments)?;
        let uri = target::parse(&url).map_err(EffectsError::Arguments)?;
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Network],
                // A request can change state on the server.
                reversible: false,
                paths: None,
            },
            subject: Some(target::subject(&uri)),
            prefix: Some(target::prefix(&uri)),
        })
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, _emit: &dyn Emit) -> Output {
        let parsed = string_argument(arguments, "url", MISSING).and_then(|url| target::parse(&url));
        match parsed {
            Ok(uri) => self.fetch(uri, cancel),
            Err(message) => failed(ErrorCode::InvalidArguments, message),
        }
    }
}

/// A 2xx response's body as it was read, by what its content type says to
/// do with it. Only a text page is held whole: it is the result. An HTML
/// page went to its artifact and its converter as it was read, a saved type
/// to its artifact, and anything else was only counted.
enum Body {
    Text(Vec<u8>),
    Html {
        artifact: Artifact,
        html: Box<Html>,
        length: u64,
    },
    Saved {
        artifact: Artifact,
        length: u64,
    },
    Unsupported {
        length: u64,
    },
}

/// What one hop's response came to.
enum Reply {
    /// A redirect to follow, with its `location`.
    Redirect(String),
    /// A 2xx response: the head and the body read, one byte past the limit
    /// at most.
    Page(Head, Body),
    /// Any other response, with the start of its body.
    Refused(Head, Vec<u8>),
}

impl WebFetch {
    fn fetch(&self, mut uri: Uri, cancel: &dyn Cancel) -> Output {
        let proxy = self.proxy.clone().unwrap_or_else(ureq::Proxy::try_from_env);
        let start = self.clock.now();
        let overall = start.checked_add(FETCH_LIMIT).unwrap_or(start);
        let mut redirects = 0;
        loop {
            let now = self.clock.now();
            let request = now.checked_add(REQUEST_LIMIT).unwrap_or(now);
            let (deadline, limit) = if overall <= request {
                (overall, Limit::Fetch)
            } else {
                (request, Limit::Request)
            };
            let reply = guarded(&self.clock, cancel, deadline, limit, |hop| {
                self.hop(hop, &uri, proxy.as_ref())
            });
            match reply {
                Ok(Reply::Redirect(location)) => {
                    if redirects == MAX_REDIRECTS {
                        return failed(
                            ErrorCode::HttpError,
                            format!("more than {MAX_REDIRECTS} redirects, the last from {uri}."),
                        );
                    }
                    redirects += 1;
                    match target::parse(&target::join(&uri, &location)) {
                        Ok(next) => uri = next,
                        Err(why) => {
                            return failed(
                                ErrorCode::HttpError,
                                format!("{uri} redirected to `{location}`: {why}"),
                            );
                        }
                    }
                }
                Ok(Reply::Page(head, body)) => return self.page(&uri, &head, body),
                Ok(Reply::Refused(head, bytes)) => return refused(&uri, &head, &bytes),
                Err(Ended::Stopped(stop)) => return stopped_output(&uri, stop),
                Err(Ended::Failed(output)) => return *output,
                Err(Ended::NoWatcher(error)) => {
                    return failed(
                        ErrorCode::ToolError,
                        format!("could not start the fetch's watcher: {error}."),
                    );
                }
            }
        }
    }

    /// One hop: the host is judged after its name is resolved, then the
    /// request goes to exactly the addresses that passed.
    fn hop(
        &self,
        hop: &Arc<Hop>,
        uri: &Uri,
        proxy: Option<&ureq::Proxy>,
    ) -> Result<Reply, Box<Output>> {
        let (host, port) = target::host_and_port(uri);
        if target::blocked_name(&host) {
            return Err(Box::new(blocked(&host)));
        }
        let via_proxy = proxy.is_some_and(|proxy| !proxy.is_no_proxy(uri));
        // debt: a name lookup blocked in the OS is not interruptible; a stop
        // lands as soon as it returns. Look up from an interruptible thread
        // if a stop stuck on a lookup is reported.
        let addresses = match (self.resolve)(&host, port) {
            Ok(addresses) => addresses,
            // The proxy resolves the target; only an address found here can
            // refuse it.
            Err(_) if via_proxy => Vec::new(),
            Err(error) => {
                return Err(Box::new(failed(
                    ErrorCode::ConnectionFailed,
                    format!("could not resolve {host}: {error}."),
                )));
            }
        };
        if addresses
            .iter()
            .any(|address| target::blocked_addr(address.ip()))
        {
            return Err(Box::new(blocked(&host)));
        }
        if addresses.is_empty() && !via_proxy {
            return Err(Box::new(failed(
                ErrorCode::ConnectionFailed,
                format!("{host} resolved to no address."),
            )));
        }
        let request = Get {
            uri,
            pinned: (!via_proxy).then_some(addresses.as_slice()),
            proxy: proxy.cloned(),
        };
        let stopped = || hop.stopped().is_some();
        hop.get(&request, |head, body| self.read_reply(head, body, &stopped))
            .map_err(|message| {
                Box::new(failed(
                    ErrorCode::ConnectionFailed,
                    format!("could not fetch {uri}: {message}."),
                ))
            })
    }

    /// A 2xx response, as the result. A page past the limit is too large,
    /// whatever else went wrong; then a page that could not be saved fails.
    /// An HTML page's markdown, and a text page's body, become the result
    /// itself, the first line put in front of it.
    fn page(&self, uri: &Uri, head: &Head, body: Body) -> Output {
        let length = match &body {
            Body::Text(bytes) => u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            Body::Html { length, .. }
            | Body::Saved { length, .. }
            | Body::Unsupported { length } => *length,
        };
        if length > MAX_BODY {
            return failed(
                ErrorCode::TooLarge,
                format!("{uri} is larger than 10 MiB ({MAX_BODY} bytes); nothing was kept."),
            );
        }
        let first = format!(
            "{uri} {} {}",
            head.status,
            head.content_type.as_deref().unwrap_or_default()
        );
        match body {
            Body::Html { artifact, html, .. } => match artifact.keep() {
                Ok(path) => {
                    let mut markdown = html.finish();
                    markdown.insert_str(0, &format!("{first}; raw page at {path}\n\n"));
                    text_output(markdown)
                }
                Err(message) => failed(ErrorCode::ToolError, message),
            },
            Body::Text(bytes) => {
                let mut text = String::from_utf8(bytes).unwrap_or_else(|invalid| {
                    String::from_utf8_lossy(invalid.as_bytes()).into_owned()
                });
                text.insert_str(0, &format!("{first}\n\n"));
                text_output(text)
            }
            Body::Saved { artifact, length } => match artifact.keep() {
                Ok(path) => text_output(format!(
                    "{first}\n\nSaved to {path} ({length} bytes). Read it with `read`.\n"
                )),
                Err(message) => failed(ErrorCode::ToolError, message),
            },
            Body::Unsupported { length } => failed(
                ErrorCode::UnsupportedFile,
                format!(
                    "{uri} is {}, which `web_fetch` cannot read ({length} bytes).",
                    head.content_type
                        .as_deref()
                        .map_or("of no content type".to_owned(), |ty| format!("`{ty}`"))
                ),
            ),
        }
    }

    /// A new artifact in `artifacts/` under a fresh name.
    fn artifact(&self, extension: &str) -> Artifact {
        let stem = format!("w_{:016x}", RandomState::new().hash_one(()));
        Artifact::create(&self.artifacts, &stem, extension, self.wrap.as_ref())
    }

    /// Reads what the response calls for: nothing of a redirect, the start
    /// of a body that is not 2xx, and a page's whole body up to one byte
    /// past the limit. A text page is read whole; any other page is read in
    /// pieces into where it goes, and the read fails at the first piece
    /// after `stopped` turns true.
    fn read_reply(
        &self,
        head: Head,
        body: &mut dyn Read,
        stopped: &dyn Fn() -> bool,
    ) -> io::Result<Reply> {
        let redirect = matches!(head.status, 301 | 302 | 303 | 307 | 308);
        match (&head.location, redirect, head.status) {
            (Some(location), true, _) => Ok(Reply::Redirect(location.clone())),
            (_, _, 200..300) => {
                let limit = MAX_BODY.saturating_add(1);
                let kind = head
                    .content_type
                    .as_deref()
                    .map_or(Kind::Unsupported, kind_of);
                let mut sink = Sink {
                    artifact: None,
                    html: None,
                    stopped,
                };
                match kind {
                    Kind::Text => {
                        let bytes = read_up_to(body, limit, head.content_length)?;
                        return Ok(Reply::Page(head, Body::Text(bytes)));
                    }
                    Kind::Markdown => {
                        sink.artifact = Some(self.artifact("html"));
                        sink.html = Some(Html::new(head.content_type.as_deref()));
                    }
                    Kind::Saved(extension) => sink.artifact = Some(self.artifact(extension)),
                    Kind::Unsupported => {}
                }
                let length = download::copy(body, limit, &mut sink)?;
                let page = match (sink.artifact, sink.html) {
                    (Some(artifact), Some(html)) => Body::Html {
                        artifact,
                        html: Box::new(html),
                        length,
                    },
                    (Some(artifact), None) => Body::Saved { artifact, length },
                    (None, _) => Body::Unsupported { length },
                };
                Ok(Reply::Page(head, page))
            }
            _ => {
                let bytes = read_up_to(body, ERROR_BODY, head.content_length)?;
                Ok(Reply::Refused(head, bytes))
            }
        }
    }
}

/// What a content type's media type says to do with the body.
enum Kind {
    Markdown,
    Text,
    Saved(&'static str),
    Unsupported,
}

fn kind_of(content_type: &str) -> Kind {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match essence.as_str() {
        "text/html" | "application/xhtml+xml" => Kind::Markdown,
        "application/pdf" => Kind::Saved("pdf"),
        "image/png" => Kind::Saved("png"),
        "image/jpeg" => Kind::Saved("jpg"),
        "image/gif" => Kind::Saved("gif"),
        "image/webp" => Kind::Saved("webp"),
        "application/json" | "application/xml" => Kind::Text,
        other => {
            let structured =
                |family: &str, suffix: &str| other.starts_with(family) && other.ends_with(suffix);
            if other.starts_with("text/")
                || structured("application/", "+json")
                || structured("application/", "+xml")
                || structured("image/", "+xml")
            {
                Kind::Text
            } else {
                Kind::Unsupported
            }
        }
    }
}

/// Reads at most `limit` bytes of `body`, reserving room for `hint` of
/// them first, never more than `limit`: the stated length is server input,
/// so it sizes the buffer and never decides how much is read.
fn read_up_to(body: &mut dyn Read, limit: u64, hint: Option<u64>) -> io::Result<Vec<u8>> {
    let reserve = hint.map_or(0, |hint| hint.min(limit));
    let mut bytes = Vec::with_capacity(usize::try_from(reserve).unwrap_or(0));
    body.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn refused(uri: &Uri, head: &Head, bytes: &[u8]) -> Output {
    let text = String::from_utf8_lossy(bytes);
    let cut = text.floor_char_boundary(usize::try_from(ERROR_BODY).unwrap_or(usize::MAX));
    failed(
        ErrorCode::HttpError,
        format!(
            "HTTP {} from {uri}. The body begins:\n{}",
            head.status,
            text.get(..cut).unwrap_or_default()
        ),
    )
}

fn blocked(host: &str) -> Output {
    failed(
        ErrorCode::BlockedHost,
        format!(
            "`{host}` is a link-local address or a cloud metadata host, which `web_fetch` \
             does not reach."
        ),
    )
}

fn stopped_output(uri: &Uri, stop: Stop) -> Output {
    match stop {
        Stop::Cancelled => text_output("Cancelled and stopped.\n".to_owned()),
        Stop::Timeout(Limit::Request) => failed(
            ErrorCode::Timeout,
            format!("{uri}: the request took over 60 seconds."),
        ),
        Stop::Timeout(Limit::Fetch) => failed(
            ErrorCode::Timeout,
            format!("{uri}: the fetch took over 5 minutes."),
        ),
    }
}

#[cfg(test)]
#[path = "web_fetch_tests.rs"]
mod tests;
