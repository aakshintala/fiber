//! One remote MCP server over streamable HTTP (`docs/mcp.md`, "What Fiber
//! does with MCP"): every message is a `POST` to the server's `url`, a
//! JSON answer routed as a stdout line is, and an event-stream answer
//! routed event by event. Each request runs on its own exchange thread
//! over an agent built by [`net::agent`], which keeps the request's
//! socket so another thread can close it; the pump drains the writer
//! channel in order, starting each exchange and `POST`ing notifications
//! and answers itself. Deadlines and cancellation stay on the injected
//! clock: when a wait ends without an answer its socket is shut, and the
//! blocked exchange ends. Fiber speaks no legacy HTTP with server-sent
//! events (HTTP+SSE): an `initialize` answered `4xx` is probed once with
//! a `GET`, and a stream whose first event names `endpoint` fails the
//! start saying so.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufReader, Read};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Condvar, Mutex, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::diag::{DebugLog, HttpRequest};
use contract::tool::Cancel;
use serde_json::Value;
use support::counted::Counted;
use ureq::unversioned::resolver::DefaultResolver;

use crate::pipes::MAX_LINE;
use crate::registry::{STOPPING, lock};
use crate::rpc::Outcome;
use crate::server::{OpenServer, StartError, shutting_down};
use crate::start::redacted_url;
use crate::wait::Shared;

/// Starts the remote server at `url` over `proxy` under `limits`, and runs
/// the handshake through [`crate::server::Server::open`]: the only
/// production caller besides [`crate::server::Server::start`].
/// A direct remote start without timing: HTTP tests only.
#[cfg(test)]
#[allow(
    clippy::too_many_arguments,
    reason = "the test start's inputs plus the sink"
)]
pub(crate) fn start(
    url: &str,
    tokens: Arc<dyn contract::mcp_tokens::TokenFiles>,
    proxy: Option<ureq::Proxy>,
    limits: net::Limits,
    clock: &Arc<dyn contract::clock::Clock>,
    startup_timeout: Duration,
    client_version: &str,
    debug: Option<Arc<dyn DebugLog>>,
) -> Result<OpenServer, StartError> {
    start_observed(
        url,
        tokens,
        proxy,
        limits,
        clock,
        startup_timeout,
        client_version,
        None,
        debug,
        &mut None,
    )
}

/// As [`start`], threading the attempt's connection guard to the
/// tool-list boundary and keeping `timing` on the link for the actual
/// connector work: the handshake consumes the guard there, and every
/// later exchange builds its agent from the same context. `None` stays
/// untimed and builds the plain agent. `debug` observes every request
/// the link makes as `mcp`; `None` stays silent.
#[allow(
    clippy::too_many_arguments,
    reason = "the observed companion adds only the Timing, the sink and the connection guard"
)]
pub(crate) fn start_observed(
    url: &str,
    tokens: Arc<dyn contract::mcp_tokens::TokenFiles>,
    proxy: Option<ureq::Proxy>,
    limits: net::Limits,
    clock: &Arc<dyn contract::clock::Clock>,
    startup_timeout: Duration,
    client_version: &str,
    timing: Option<contract::diag::Timing>,
    debug: Option<Arc<dyn DebugLog>>,
    connection: &mut Option<Box<dyn Send>>,
) -> Result<OpenServer, StartError> {
    // Sticky: a start after the stop spawns nothing.
    if STOPPING.is_cancelled() {
        return Err(shutting_down());
    }
    check_url(url)?;
    let shared = Arc::new(Shared::default());
    let (writer, incoming) = mpsc::sync_channel(crate::server::WRITE_QUEUE);
    let writer = Arc::new(writer);
    let link = Arc::new(Link::new_observed(
        url.to_owned(),
        tokens,
        clock.clone(),
        proxy,
        limits,
        timing,
        debug,
    ));
    let pumping = Arc::clone(&link);
    let reading = Arc::clone(&shared);
    let answering = Arc::downgrade(&writer);
    thread::Builder::new()
        .name("mcp-http-pump".to_owned())
        .spawn(move || pump(incoming, pumping, reading, answering))
        .map_err(|error| {
            StartError::StartFailed(format!("The server could not be started: {error}."))
        })?;
    crate::server::Server::open_observed(
        shared,
        writer,
        crate::server::Peer::Remote(link),
        clock,
        startup_timeout,
        client_version,
        connection,
    )
}

/// The `url` is one Fiber can `POST` to: an http or https URL. No request
/// is made: anything else fails the start. The grammar carries the host
/// check: an http URL without a host never parses (`docs/mcp.md`
/// has no URL scheme of its own, so only these two pass).
fn check_url(url: &str) -> Result<(), StartError> {
    if crate::url_has_userinfo(url) {
        return Err(StartError::StartFailed(
            "Its `url` has a user name or password, which Fiber does not send.".into(),
        ));
    }
    let bad = || StartError::StartFailed("Its `url` is not an http or https URL.".to_owned());
    let uri: ureq::http::Uri = url.parse().map_err(|_| bad())?;
    let scheme = uri.scheme_str().map(str::to_owned).ok_or_else(bad)?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(bad());
    }
    Ok(())
}

/// The version every request after `initialize` carries: the `initialize`
/// result's `protocolVersion` when it is a string that is a valid header
/// value, else [`crate::server::PROTOCOL_VERSION`]. A bad version never
/// reaches the client's builder unchecked.
pub(crate) fn negotiated_version(result: &Value) -> String {
    result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|version| ureq::http::HeaderValue::from_str(version).is_ok())
        .unwrap_or(crate::server::PROTOCOL_VERSION)
        .to_owned()
}

/// A remote server's session: its URL, and the state every exchange thread
/// shares. No lock is held across a connect, a read, a write or a join:
/// every method takes the lock, copies what it needs, and drops it.
pub(crate) struct Link {
    url: String,
    proxy: Option<ureq::Proxy>,
    limits: net::Limits,
    state: Mutex<LinkState>,
    bearer: Arc<crate::token::Bearer>,
    poll: Condvar,
    /// The timing context actual connector work builds its observed
    /// agent from: `None` builds the plain agent. Never logged.
    timing: Option<contract::diag::Timing>,
    /// Where each request's `http_request` line goes, or `None` when the
    /// server stays silent. The session installs its sink at debug.
    debug: Option<Arc<dyn DebugLog>>,
    /// The injected clock every line's stamps are read on.
    clock: Arc<dyn Clock>,
}

// The URL's userinfo and query can carry a token, and the session id
// travels as a header value, so both print redacted
// (`docs/code-quality.md`, "Errors").
impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("url", &redacted_url(&self.url))
            .field("proxy", &self.proxy)
            .field("limits", &self.limits)
            .field("state", &lock(&self.state))
            .finish()
    }
}

#[derive(Default)]
struct LinkState {
    session: Option<String>,
    version: Option<String>,
    proven: bool,
    sockets: BTreeMap<u64, TcpStream>,
    abandoned: BTreeSet<u64>,
    closing: bool,
}

// The session id travels as the `mcp-session-id` header value, so only
// its presence prints (`docs/code-quality.md`, "Errors").
impl std::fmt::Debug for LinkState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkState")
            .field("session", &self.session.as_ref().map(|_| "<redacted>"))
            .field("version", &self.version)
            .field("proven", &self.proven)
            .field("sockets", &self.sockets)
            .field("abandoned", &self.abandoned)
            .field("closing", &self.closing)
            .finish()
    }
}

impl Link {
    /// A link without timing: HTTP tests only.
    #[cfg(test)]
    fn new(
        url: String,
        tokens: Arc<dyn contract::mcp_tokens::TokenFiles>,
        clock: Arc<dyn contract::clock::Clock>,
        proxy: Option<ureq::Proxy>,
        limits: net::Limits,
        debug: Option<Arc<dyn DebugLog>>,
    ) -> Self {
        Self::new_observed(url, tokens, clock, proxy, limits, None, debug)
    }

    /// As [`Link::new`], keeping `timing` for the observed agent:
    /// `None` builds the plain agent, as before. `debug` observes every
    /// request the link makes as `mcp`; `None` stays silent.
    fn new_observed(
        url: String,
        tokens: Arc<dyn contract::mcp_tokens::TokenFiles>,
        clock: Arc<dyn contract::clock::Clock>,
        proxy: Option<ureq::Proxy>,
        limits: net::Limits,
        timing: Option<contract::diag::Timing>,
        debug: Option<Arc<dyn DebugLog>>,
    ) -> Self {
        let bearer = Arc::new(crate::token::Bearer::new(&url, tokens, clock.clone()));
        Self {
            url,
            proxy,
            limits,
            state: Mutex::new(LinkState::default()),
            bearer,
            poll: Condvar::new(),
            timing,
            debug,
            clock,
        }
    }

    /// The send stamp, `None` when silent so no clock is read at info.
    fn stamp(&self) -> Option<Instant> {
        self.debug.as_ref().map(|_| self.clock.now())
    }

    /// Writes one `mcp` line for a request that reached the client: no
    /// identity, host, path, URL, server name or credential, attempt 1,
    /// the posted byte length, the consumed response bytes, the status
    /// when a response came, and the send/headers/end stamps. A sink
    /// that stopped debugging writes nothing.
    fn observe(
        &self,
        start: Option<Instant>,
        headers: Option<Instant>,
        status: Option<u16>,
        request_bytes: u64,
        response_bytes: u64,
    ) {
        let (Some(debug), Some(start)) = (self.debug.as_ref(), start) else {
            return;
        };
        if !debug.debugging() {
            return;
        }
        let end = self.clock.now();
        let millis = |instant: Instant| {
            u64::try_from(instant.saturating_duration_since(start).as_millis()).unwrap_or(u64::MAX)
        };
        let mut line = HttpRequest::for_mcp();
        line.status = status;
        line.headers_ms = headers.map(millis);
        line.total_ms = millis(end);
        line.request_bytes = request_bytes;
        line.response_bytes = response_bytes;
        debug.http_request(&line);
    }

    /// The protocol version the `initialize` result negotiated: every later
    /// request carries it.
    pub(crate) fn negotiated(&self, version: &str) {
        lock(&self.state).version = Some(version.to_owned());
    }

    /// Shuts request `id`'s socket and refuses any it opens later. A stop
    /// that lands between the last check and the open still closes the
    /// socket at once, through [`net::Keep`].
    pub(crate) fn abandon(&self, id: u64) {
        let mut state = lock(&self.state);
        state.abandoned.insert(id);
        if let Some(socket) = state.sockets.remove(&id) {
            // A socket the peer already closed fails to shut down, and is
            // closed either way.
            let _closed = socket.shutdown(Shutdown::Both);
        }
        drop(state);
        self.poll.notify_all();
    }

    /// Shuts every socket and refuses new ones. Idempotent.
    pub(crate) fn close(&self) {
        let mut state = lock(&self.state);
        state.closing = true;
        let sockets = std::mem::take(&mut state.sockets);
        drop(state);
        self.poll.notify_all();
        for (_, socket) in sockets {
            let _closed = socket.shutdown(Shutdown::Both);
        }
    }

    /// The session id the server gave, when it gave one.
    fn session(&self) -> Option<String> {
        lock(&self.state).session.clone()
    }

    /// Whether a response to a request that carried the session id proved
    /// it: an expired session is normal behaviour, not a death
    /// (`docs/mcp.md`, "When a server dies").
    fn is_proven(&self) -> bool {
        lock(&self.state).proven
    }

    /// Stores the `initialize` response's session id and unproves the new
    /// session: only a later request's answer proves it.
    fn set_session(&self, session: String) {
        let mut state = lock(&self.state);
        state.session = Some(session);
        state.proven = false;
    }

    /// Proves the session: a request that carried it was answered with any
    /// status other than `404`.
    fn set_proven(&self) {
        lock(&self.state).proven = true;
    }

    /// Keeps a handle to `socket` for `id`'s exchange, or refuses it once
    /// the request was abandoned or the link is closing.
    fn keep(&self, id: u64, socket: &TcpStream) -> io::Result<()> {
        let mut state = lock(&self.state);
        if state.closing || state.abandoned.contains(&id) {
            return Err(io::Error::other("the request was abandoned"));
        }
        state.sockets.insert(id, socket.try_clone()?);
        Ok(())
    }

    /// Whether `id`'s exchange may still open sockets.
    fn stopped(&self, id: u64) -> bool {
        let state = lock(&self.state);
        state.closing || state.abandoned.contains(&id)
    }
}

/// One exchange's hold on its socket: [`net::Keep`] keyed by request id (0
/// for the pump's own posts).
#[derive(Debug)]
struct Exchange {
    id: u64,
    link: Arc<Link>,
}

impl net::Keep for Exchange {
    /// Keeps a handle to `socket`, or refuses it once the request was
    /// abandoned or the link is closing.
    fn keep(&self, socket: &TcpStream) -> io::Result<()> {
        self.link.keep(self.id, socket)
    }

    fn is_stopped(&self) -> bool {
        self.link.stopped(self.id)
    }
}

const REFRESH_KEY: u64 = u64::MAX;
impl crate::token::Refresh for Exchange {
    fn stopped(&self) -> bool {
        self.link.stopped(self.id)
    }
    // exempted: a 20 ms wait that paces the refresh loop's retry; removing it
    // changes CPU use, not an outcome, and no test can observe the difference
    // without a timing assert (docs/testing.md "Waits and timeouts").
    #[cfg_attr(false, mutants::skip)]
    fn pause(&self) {
        let _paused = self
            .link
            .poll
            .wait_timeout(lock(&self.link.state), Duration::from_millis(20));
    }
    fn post(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, Vec<u8>), ()> {
        // The encoded form's length is the posted count: the bytes sent
        // are identical to `send_form`'s, with the same content type.
        let body = crate::login::form_body(form);
        let start = self.link.stamp();
        let response = match agent_for(&self.link, self.id)
            .post(url)
            .header("content-type", "application/x-www-form-urlencoded")
            .send(body.as_str())
        {
            Ok(response) => response,
            Err(_) => {
                self.link.observe(
                    start,
                    None,
                    None,
                    u64::try_from(body.len()).unwrap_or(u64::MAX),
                    0,
                );
                return Err(());
            }
        };
        let headers = self.link.stamp();
        let mut response = response;
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        let read = response
            .body_mut()
            .as_reader()
            .take(MAX_LINE as u64 + 1)
            .read_to_end(&mut bytes);
        // A body-read failure retains the status and headers and the
        // bytes consumed: the token endpoint answered, and the reply is
        // what failed.
        self.link.observe(
            start,
            headers,
            Some(status),
            u64::try_from(body.len()).unwrap_or(u64::MAX),
            u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        );
        read.map_err(|_| ())?;
        Ok((status, bytes))
    }
}

/// One agent per exchange, so its connector keeps this exchange's socket.
/// With a timing context the exchange builds the observed agent, timing
/// its actual connector work as `net.connect`; without one it builds the
/// plain agent. Building either agent starts no scope.
fn agent_for(link: &Arc<Link>, id: u64) -> ureq::Agent {
    let config = net::config()
        .proxy(link.proxy.clone())
        .http_status_as_error(false)
        .max_redirects(0)
        .build();
    let keep = Arc::new(Exchange {
        id,
        link: Arc::clone(link),
    });
    match &link.timing {
        Some(timing) => net::agent_observed(
            config,
            keep,
            DefaultResolver::default(),
            link.limits,
            timing.clone(),
        ),
        None => net::agent(config, keep, DefaultResolver::default(), link.limits),
    }
}

/// The headers of every request: JSON in, JSON or an event stream back,
/// with the session id and the negotiated version once the handshake set
/// them, so the first request carries neither. The one header function a
/// later authentication builds on.
fn headers<B>(
    request: ureq::RequestBuilder<B>,
    session: Option<&str>,
    version: Option<&str>,
    authorization: Option<&str>,
) -> ureq::RequestBuilder<B> {
    let mut request = request
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    if let Some(authorization) = authorization {
        request = request.header("authorization", authorization);
    }
    if let Some(session) = session {
        request = request.header("mcp-session-id", session);
    }
    if let Some(version) = version {
        request = request.header("mcp-protocol-version", version);
    }
    request
}

/// The sentences for an HTTP failure. No sentence quotes the response
/// body, which may be large or hold a secret (`docs/code-quality.md`,
/// "Errors").
fn http_status(status: u16) -> String {
    format!("The server answered HTTP {status}.")
}
const LOGIN: &str = "The server answered HTTP 401 and needs a login.";

fn media_sentence(media: &str) -> String {
    format!("The server answered with media type `{media}`, not JSON or an event stream.")
}

/// The media type of `response`: the part before any `;`, trimmed,
/// case-insensitively, so `text/event-stream; charset=utf-8` still reads
/// as a stream.
fn media_type(response: &ureq::http::Response<ureq::Body>) -> String {
    response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        })
        .unwrap_or_default()
}

/// The pump: drains the writer channel in order. A message with both an
/// id and a method is a request, and the pump starts its exchange thread
/// and moves on; any other message (a notification, or an answer to a
/// server request) is `POST`ed by the pump itself, whose status is read
/// before the next message is taken, so `notifications/initialized` has
/// its `202` before `tools/list` is dequeued. When the channel closes
/// (stop, drop, failed start), the pump says goodbye with `DELETE` when
/// the server gave a session and still holds it, closes every socket, and
/// marks the server gone, which ends `stop`'s grace wait.
fn pump(
    incoming: mpsc::Receiver<Vec<u8>>,
    link: Arc<Link>,
    shared: Arc<Shared>,
    writer: Weak<mpsc::SyncSender<Vec<u8>>>,
) {
    for line in incoming {
        match split(&line) {
            Some((id, method)) => {
                let exchange = {
                    let link = Arc::clone(&link);
                    let shared = Arc::clone(&shared);
                    let writer = writer.clone();
                    move || {
                        exchange(id, method, line, link, shared, writer);
                        #[cfg(test)]
                        exchange_finished(id);
                    }
                };
                match thread::Builder::new()
                    .name("mcp-http-exchange".to_owned())
                    .spawn(exchange)
                {
                    Ok(_) => {}
                    // A thread that cannot spawn is that request's
                    // connection failing.
                    Err(_) => {
                        shared.gone_if_awaiting(id);
                    }
                }
            }
            None => match post_own(&link, &line) {
                Err(sentence) => shared.refuse_login(&sentence),
                Ok(Some(401)) => shared.refuse_login(LOGIN),
                Ok(_) => {
                    let initialized = serde_json::from_slice::<Value>(&line)
                        .ok()
                        .and_then(|value| {
                            value
                                .get("method")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .is_some_and(|method| method == "notifications/initialized");
                    if initialized {
                        shared
                            .deliver(crate::server::INITIALIZED_ACK, Outcome::Result(Value::Null));
                    }
                }
            },
        }
    }
    if link.session().is_some() && !lock(&shared.inner).expired {
        delete(&link);
    }
    link.close();
    shared.gone();
}

/// A message's request id and method, when it holds both: a request. Any
/// other message is `POST`ed by the pump itself.
fn split(line: &[u8]) -> Option<(u64, String)> {
    let value: Value = serde_json::from_slice(line).ok()?;
    let id = value.get("id")?.as_u64()?;
    let method = value.get("method")?.as_str()?.to_owned();
    Some((id, method))
}

/// `POST`s what the pump posts itself (a notification, or an answer to a
/// server request) under exchange key 0 and reads its status. The status
/// is ignored, `404` included: only request exchanges expire or kill, so a
/// late or unrelated `404` here changes nothing. When observed, the post
/// writes one `mcp` line: without status, headers time or body bytes when
/// no head came back, with the status, the headers time and no body bytes
/// once a head arrived, since the body is never read.
fn post_own(link: &Arc<Link>, line: &[u8]) -> Result<Option<u16>, String> {
    let agent = agent_for(link, 0);
    let (session, version) = {
        let state = lock(&link.state);
        (state.session.clone(), state.version.clone())
    };
    let url = link.url.clone();
    let authorization = link.bearer.stored()?;
    let start = link.stamp();
    let answered = match headers(
        agent.post(url.as_str()),
        session.as_deref(),
        version.as_deref(),
        authorization.as_deref(),
    )
    .send(line)
    {
        Ok(response) => response,
        Err(_) => {
            link.observe(
                start,
                None,
                None,
                u64::try_from(line.len()).unwrap_or(u64::MAX),
                0,
            );
            return Ok(None);
        }
    };
    let headers = link.stamp();
    let status = answered.status().as_u16();
    link.observe(
        start,
        headers,
        Some(status),
        u64::try_from(line.len()).unwrap_or(u64::MAX),
        0,
    );
    Ok(Some(status))
}

/// Says goodbye: `DELETE` with the session id, ignoring its status (the
/// spec allows `405`). When observed, the delete writes one `mcp` line:
/// without status, headers time or body bytes when no head came back,
/// with the status, the headers time and no body bytes once a head
/// arrived, since the body is never read.
fn delete(link: &Arc<Link>) {
    let agent = agent_for(link, 0);
    if let Some(session) = link.session() {
        let url = link.url.clone();
        let authorization = link.bearer.stored().unwrap_or(None);
        let mut request = agent
            .delete(url.as_str())
            .header("mcp-session-id", session.as_str());
        if let Some(authorization) = authorization {
            request = request.header("authorization", authorization);
        }
        let start = link.stamp();
        let answered = request.call();
        let headers = link.stamp();
        match answered {
            Ok(response) => link.observe(start, headers, Some(response.status().as_u16()), 0, 0),
            Err(_) => link.observe(start, None, None, 0, 0),
        }
    }
}

/// One request's exchange: `POST`s it and handles the answer. A transport
/// error while the answer is still awaited is the server dying; once the
/// wait ended (timeout, cancel, stop) the slot is gone first, so the
/// guarded mark finds no waiter and marks nothing.
fn exchange(
    id: u64,
    method: String,
    line: Vec<u8>,
    link: Arc<Link>,
    shared: Arc<Shared>,
    writer: Weak<mpsc::SyncSender<Vec<u8>>>,
) {
    // A call queued behind a stalled notice still ends on its own
    // deadline: its slot is gone by the time the pump starts this
    // exchange, and the request is never sent.
    if !shared.awaiting(id) {
        return;
    }
    let is_init = method == "initialize";
    let (session, version) = {
        let state = lock(&link.state);
        (state.session.clone(), state.version.clone())
    };
    let carried = session.is_some();
    let mut refreshing = false;
    let authorization = match link.bearer.authorization(&Exchange {
        id,
        link: link.clone(),
    }) {
        Ok(crate::token::Authorization::None) => None,
        Ok(crate::token::Authorization::Send(header)) => Some(header),
        Ok(crate::token::Authorization::SendAndRefresh(header)) => {
            refreshing = true;
            Some(header)
        }
        Err(outcome) => {
            shared.deliver(id, outcome);
            return;
        }
    };
    let agent = agent_for(&link, id);
    let url = link.url.clone();
    let posted = u64::try_from(line.len()).unwrap_or(u64::MAX);
    let start = link.stamp();
    let response = headers(
        agent.post(url.as_str()),
        session.as_deref(),
        version.as_deref(),
        authorization.as_deref(),
    )
    .send(&line);
    if refreshing {
        link.bearer.refresh_in_background(Box::new(Exchange {
            id: REFRESH_KEY,
            link: link.clone(),
        }));
    }
    let response = match response {
        Ok(response) => response,
        Err(_) => {
            // A transport error while the answer is still awaited is
            // the server dying; the post reached no head, so the line
            // carries no status, headers time or body bytes.
            link.observe(start, None, None, posted, 0);
            shared.gone_if_awaiting(id);
            return;
        }
    };
    let headers = link.stamp();
    let status = response.status().as_u16();
    if status == 401 {
        link.observe(start, headers, Some(status), posted, 0);
        shared.deliver(id, Outcome::Login(LOGIN.into()));
        return;
    }
    if is_init && (400..500).contains(&status) {
        // The probe writes its own `GET` line; this post still writes
        // its line, with no body read.
        link.observe(start, headers, Some(status), posted, 0);
        probe_legacy(id, status, &link, &shared);
        return;
    }
    if status == 404 && carried {
        after_status_hook();
        link.observe(start, headers, Some(status), posted, 0);
        if link.is_proven() {
            // The session expired: not a death. Each waiting call ends
            // `Expired`, and the slot starts a new session on the next
            // call without using the restart.
            shared.expire_if_awaiting(id);
        } else {
            // The first request of a new session: a death, so a server
            // that answers `404` to everything goes dead instead of
            // looping.
            shared.gone_if_awaiting(id);
        }
        return;
    }
    // A non-initialize 404 carrying a session id returned above. Marking a
    // sessionless link proven is harmless: only a 404 carrying its session
    // consults this flag.
    if !is_init {
        link.set_proven();
    }
    if !(200..300).contains(&status) || status == 202 {
        // A request needs an answer: `202`, and any other failure, fail
        // only this request, never the server. The body is never read.
        link.observe(start, headers, Some(status), posted, 0);
        shared.deliver(id, Outcome::Http(http_status(status)));
        return;
    }
    if is_init {
        // The session id is stored before the body routes, so
        // `notifications/initialized` and everything after carry it. A
        // session id that is not a valid header value fails the start,
        // and the body is never routed.
        match response
            .headers()
            .get("mcp-session-id")
            .map(|value| value.to_str())
        {
            Some(Ok(session)) => link.set_session(session.to_owned()),
            Some(Err(_)) => {
                link.observe(start, headers, Some(status), posted, 0);
                shared.deliver(
                    id,
                    Outcome::Http(
                        "The server's session id is not a valid HTTP header value.".to_owned(),
                    ),
                );
                return;
            }
            None => {}
        }
    }
    let media = media_type(&response);
    if media == "application/json" {
        let consumed = json_answer(id, response, &shared, &writer);
        link.observe(start, headers, Some(status), posted, consumed);
    } else if media == "text/event-stream" {
        let consumed = stream_answer(id, response, &shared, &writer);
        link.observe(start, headers, Some(status), posted, consumed);
    } else {
        link.observe(start, headers, Some(status), posted, 0);
        shared.deliver(id, Outcome::Http(media_sentence(&media)));
    }
}

/// Routes one JSON answer as a stdout line is routed. The body ends the
/// exchange either way: when it carried no answer while still awaited,
/// the server died. Returns the body bytes read, however the read ended.
fn json_answer(
    id: u64,
    response: ureq::http::Response<ureq::Body>,
    shared: &Arc<Shared>,
    writer: &Weak<mpsc::SyncSender<Vec<u8>>>,
) -> u64 {
    let mut body = Vec::new();
    match response
        .into_body()
        .into_reader()
        .take(MAX_LINE as u64 + 1)
        .read_to_end(&mut body)
    {
        Err(_) => {
            let consumed = u64::try_from(body.len()).unwrap_or(u64::MAX);
            shared.gone_if_awaiting(id);
            return consumed;
        }
        Ok(_) if body.len() > MAX_LINE => {
            let consumed = u64::try_from(body.len()).unwrap_or(u64::MAX);
            shared.gone_if_awaiting(id);
            return consumed;
        }
        Ok(_) => {}
    }
    crate::pipes::route(&String::from_utf8_lossy(&body), shared, writer);
    shared.gone_if_awaiting(id);
    u64::try_from(body.len()).unwrap_or(u64::MAX)
}

/// Routes one event-stream answer event by event. Reading stops once the
/// request is no longer awaited (its answer delivered, or its wait
/// ended): a stream that ends first, or runs past the cap, is the server
/// dying. Returns the body bytes consumed, however the read ended: the
/// bytes pulled through the reader, not headers, not re-serialized
/// events.
fn stream_answer(
    id: u64,
    response: ureq::http::Response<ureq::Body>,
    shared: &Arc<Shared>,
    writer: &Weak<mpsc::SyncSender<Vec<u8>>>,
) -> u64 {
    let mut counted = Counted::new(response.into_body().into_reader());
    {
        let mut reader = BufReader::new(&mut counted);
        loop {
            if !shared.awaiting(id) {
                break;
            }
            match crate::sse::next(&mut reader) {
                crate::sse::Next::Event(event) => crate::pipes::route(&event.data, shared, writer),
                crate::sse::Next::End | crate::sse::Next::TooLong => {
                    shared.gone_if_awaiting(id);
                    break;
                }
            }
        }
    }
    counted.consumed()
}

/// The legacy probe: an `initialize` answered `4xx` carries no session
/// yet, so a `GET` asking for an event stream tells HTTP+SSE apart from
/// any other failure. A stream whose first event is named `endpoint` is
/// the legacy transport, which Fiber does not speak; anything else fails
/// with the original `POST`'s status. The probe runs under `initialize`'s
/// exchange id, so the startup deadline and a stop close its socket too.
/// An `initialize` answered 2xx is streamable HTTP and never probed.
/// When observed, the probe writes one `mcp` line: without status,
/// headers time or body bytes when no head came back, with the status,
/// the headers time and the first event's body bytes once a head
/// arrived, and nothing when the stored token is missing.
fn probe_legacy(id: u64, post_status: u16, link: &Arc<Link>, shared: &Arc<Shared>) {
    let agent = agent_for(link, id);
    let url = link.url.clone();
    let authorization = match link.bearer.stored() {
        Ok(header) => header,
        Err(sentence) => {
            shared.deliver(id, Outcome::Login(sentence));
            return;
        }
    };
    let mut request = agent
        .get(url.as_str())
        .header("accept", "text/event-stream");
    if let Some(authorization) = authorization {
        request = request.header("authorization", authorization);
    }
    let start = link.stamp();
    let answered = request.call();
    let headers = link.stamp();
    let legacy = match answered {
        Ok(response) if response.status().as_u16() == 401 => {
            link.observe(start, headers, Some(401), 0, 0);
            shared.deliver(id, Outcome::Login(LOGIN.into()));
            return;
        }
        Ok(response)
            if (200..300).contains(&response.status().as_u16())
                && media_type(&response) == "text/event-stream" =>
        {
            let status = response.status().as_u16();
            let mut counted = Counted::new(response.into_body().into_reader());
            let legacy = {
                let mut reader = BufReader::new(&mut counted);
                match crate::sse::next(&mut reader) {
                    // Without the name check any streamable answer to the
                    // probe would read as legacy: the `message` mutant fails
                    // the start here.
                    crate::sse::Next::Event(event) if event.name == "endpoint" => true,
                    crate::sse::Next::Event(_)
                    | crate::sse::Next::End
                    | crate::sse::Next::TooLong => false,
                }
            };
            link.observe(start, headers, Some(status), 0, counted.consumed());
            legacy
        }
        Ok(response) => {
            let status = response.status().as_u16();
            link.observe(start, headers, Some(status), 0, 0);
            false
        }
        Err(_) => {
            link.observe(start, None, None, 0, 0);
            false
        }
    };
    shared.deliver(
        id,
        Outcome::Http(if legacy {
            "It offers only HTTP with server-sent events (HTTP+SSE), which Fiber does not speak; Fiber needs streamable HTTP.".to_owned()
        } else {
            http_status(post_status)
        }),
    );
}

/// Installs the hook [`exchange_finished`] sends to once, when the
/// exchange with `id` ends: the test receives that end with a deadline
/// and reads the recorded requests after it, so the absence of a
/// removed exchange's request never races a thread still starting.
#[cfg(test)]
pub(crate) fn wait_exchange_done(id: u64) -> mpsc::Receiver<u64> {
    let (done, finished) = mpsc::channel();
    *lock(&EXCHANGE_DONE) = Some((id, done));
    finished
}

/// The hook [`wait_exchange_done`] installs, if any.
#[cfg(test)]
static EXCHANGE_DONE: Mutex<Option<(u64, mpsc::Sender<u64>)>> = Mutex::new(None);

/// After an exchange ends, on every path, sends its id when it is the
/// one [`wait_exchange_done`] installed for, keeping the hook for a
/// later exchange otherwise: a removed exchange's end is the signal its
/// request was never sent, since any request it would send is recorded
/// before its answer ends it. As `slot.rs`'s `run_after_serve` hook, one
/// shot per install.
#[cfg(test)]
fn exchange_finished(id: u64) {
    let mut waiting = lock(&EXCHANGE_DONE);
    let finished = waiting.as_ref().is_some_and(|(wanted, _)| *wanted == id);
    if finished && let Some((_, done)) = waiting.take() {
        let _sent = done.send(id);
    }
}

/// Runs the test hook installed by [`pause_after_status`], if any.
fn after_status_hook() {
    #[cfg(test)]
    if let Some(hook) = lock(&AFTER_STATUS).take() {
        hook();
    }
}

/// Between reading a `404`'s status and the guarded mark, runs `hook`:
/// tests time the call out on the fake clock first, so the `404` marks
/// nothing. As `slot.rs`'s `run_after_serve` hook, one shot per install.
#[cfg(test)]
static AFTER_STATUS: Mutex<Option<Box<dyn FnOnce() + Send>>> = Mutex::new(None);

/// Installs the hook [`after_status_hook`] runs once, on the exchange
/// thread, before the guarded expiry or death mark.
#[cfg(test)]
pub(crate) fn pause_after_status(hook: impl FnOnce() + Send + 'static) {
    *lock(&AFTER_STATUS) = Some(Box::new(hook));
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
