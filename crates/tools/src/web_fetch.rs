//! `web_fetch` (`docs/tools.md`, "web_fetch"): one GET of a URL the model
//! names, redirects followed hop by hop, each hop judged and bounded, and the
//! page returned as text or saved to the session's `artifacts/`.

mod http;
mod markdown;
mod target;

use std::collections::hash_map::RandomState;
use std::fs::{self, OpenOptions};
use std::hash::BuildHasher;
use std::io::{self, Read, Write};
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
        }
    }

    /// Looks names up with `resolve`.
    #[cfg(test)]
    pub(crate) fn with_resolver(mut self, resolve: Resolve) -> Self {
        self.resolve = resolve;
        self
    }

    /// Fixes the proxy, `None` meaning a direct connection.
    #[cfg(test)]
    pub(crate) fn with_proxy(mut self, proxy: Option<ureq::Proxy>) -> Self {
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
                 URL after redirects, the HTTP status and the content type. HTML is \
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

/// What one hop's response came to.
enum Reply {
    /// A redirect to follow, with its `location`.
    Redirect(String),
    /// A 2xx response: the head and the body read, one byte past the limit
    /// at most.
    Page(Head, Vec<u8>),
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
                Ok(Reply::Page(head, bytes)) => return self.page(&uri, &head, &bytes),
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
        hop.get(&request, read_reply).map_err(|message| {
            Box::new(failed(
                ErrorCode::ConnectionFailed,
                format!("could not fetch {uri}: {message}."),
            ))
        })
    }

    /// A 2xx response, as the result.
    fn page(&self, uri: &Uri, head: &Head, bytes: &[u8]) -> Output {
        if u64::try_from(bytes.len()).is_ok_and(|length| length > MAX_BODY) {
            return failed(
                ErrorCode::TooLarge,
                format!("{uri} is larger than 10 MiB ({MAX_BODY} bytes); nothing was kept."),
            );
        }
        let kind = head
            .content_type
            .as_deref()
            .map_or(Kind::Unsupported, kind_of);
        let first = format!(
            "{uri} {} {}",
            head.status,
            head.content_type.as_deref().unwrap_or_default()
        );
        match kind {
            Kind::Markdown => text_output(format!(
                "{first}\n\n{}",
                markdown::to_markdown(&String::from_utf8_lossy(bytes))
            )),
            Kind::Text => text_output(format!("{first}\n\n{}", String::from_utf8_lossy(bytes))),
            Kind::Saved(extension) => match self.save(bytes, extension) {
                Ok(path) => text_output(format!(
                    "{first}\n\nSaved to {path} ({} bytes). Read it with `read`.\n",
                    bytes.len()
                )),
                Err(message) => failed(ErrorCode::ToolError, message),
            },
            Kind::Unsupported => failed(
                ErrorCode::UnsupportedFile,
                format!(
                    "{uri} is {}, which `web_fetch` cannot read ({} bytes).",
                    head.content_type
                        .as_deref()
                        .map_or("of no content type".to_owned(), |ty| format!("`{ty}`")),
                    bytes.len()
                ),
            ),
        }
    }

    /// Saves the download under a fresh name in `artifacts/`, as it came.
    fn save(&self, bytes: &[u8], extension: &str) -> Result<String, String> {
        let stem = format!("w_{:016x}", RandomState::new().hash_one(()));
        let path = self.artifacts.join(format!("{stem}.{extension}"));
        let saved = fs::create_dir_all(&self.artifacts).and_then(|()| {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?
                .write_all(bytes)
        });
        match saved {
            Ok(()) => Ok(path.display().to_string()),
            Err(error) => Err(format!(
                "could not save the download to {}: {error}.",
                path.display()
            )),
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

/// Reads what the response calls for: nothing of a redirect, the whole body
/// of a page up to the limit, and the start of any other body.
fn read_reply(head: Head, body: &mut dyn Read) -> io::Result<Reply> {
    let redirect = matches!(head.status, 301 | 302 | 303 | 307 | 308);
    match (&head.location, redirect, head.status) {
        (Some(location), true, _) => Ok(Reply::Redirect(location.clone())),
        (_, _, 200..300) => {
            let bytes = read_up_to(body, MAX_BODY.saturating_add(1))?;
            Ok(Reply::Page(head, bytes))
        }
        _ => {
            let bytes = read_up_to(body, ERROR_BODY)?;
            Ok(Reply::Refused(head, bytes))
        }
    }
}

fn read_up_to(body: &mut dyn Read, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
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
