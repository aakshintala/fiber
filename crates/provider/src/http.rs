//! Sending a request over HTTP, on a socket Fiber owns so another thread can
//! close it (`docs/architecture.md`, "Cancellation"). Each call runs its own
//! agent over the shared connector in `net`, which keeps a handle to the
//! call's socket; shutting that handle down ends a read blocked inside
//! ureq, under TLS too. A provider
//! that signs its requests is asked for its headers on every send, a retry
//! included (`docs/model-routing.md`, "Signing a request").

use std::io::{self, Read};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use contract::clock::Clock;
use contract::diag::Timing;
use contract::signing::{SignRequest, Signer};
use serde_json::Value;
use ureq::unversioned::resolver::DefaultResolver;

use crate::Error;
use crate::redact::Secrets;

mod date;

fn validate_signed_header(name: &str, value: &str) -> Result<(), contract::signing::Error> {
    if ureq::http::HeaderName::from_bytes(name.as_bytes()).is_err() {
        return Err(contract::signing::Error::NotHeaders(format!(
            "a header name that is not valid HTTP: {name:?}"
        )));
    }
    if ureq::http::HeaderValue::from_str(value).is_err() {
        return Err(contract::signing::Error::NotHeaders(format!(
            "header `{name}` has an invalid value"
        )));
    }
    Ok(())
}

/// One call's cancellation: whether it was cancelled, and its open socket.
#[derive(Debug, Default)]
pub(crate) struct Cancel {
    state: Mutex<CancelState>,
}

#[derive(Debug, Default)]
struct CancelState {
    cancelled: bool,
    socket: Option<TcpStream>,
}

impl Cancel {
    /// Marks the call cancelled and closes its socket, if one is open. A
    /// socket opened later is closed as it opens.
    pub(crate) fn cancel(&self) {
        let mut state = self.lock();
        state.cancelled = true;
        if let Some(socket) = state.socket.take() {
            // A socket the peer already closed fails to shut down, and is
            // closed either way.
            let _closed = socket.shutdown(Shutdown::Both);
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.lock().cancelled
    }

    fn lock(&self) -> MutexGuard<'_, CancelState> {
        // Release builds abort on panic, so no holder can poison the lock.
        support::lock(&self.state)
    }
}

impl net::Keep for Cancel {
    /// Keeps a handle to `socket`, or refuses it once the call is cancelled.
    fn keep(&self, socket: &TcpStream) -> io::Result<()> {
        let mut state = self.lock();
        if state.cancelled {
            return Err(io::Error::other("the call was cancelled"));
        }
        state.socket = Some(socket.try_clone()?);
        Ok(())
    }

    fn is_stopped(&self) -> bool {
        self.is_cancelled()
    }
}

/// Where one call goes, and under which socket limits: the proxy choice
/// and the limits travel together, so no call gains an eighth argument.
#[derive(Debug)]
pub(crate) struct Route {
    /// How the call reaches its server.
    pub(crate) via: Via,
    /// The socket limits the call runs under.
    pub(crate) limits: net::Limits,
}

/// How one call reaches its server: the environment's proxy, no proxy,
/// or one named proxy value.
#[derive(Debug, Clone)]
pub(crate) enum Via {
    /// The proxy the environment names, as ureq reads it.
    Environment,
    /// No proxy, whatever the environment names.
    Direct,
    /// One named proxy value.
    #[allow(
        dead_code,
        reason = "only tests name an explicit proxy; production reads the environment"
    )]
    Through(ureq::Proxy),
}

impl Route {
    /// The route for `endpoint`: direct when it says so, else the
    /// environment's proxy, carrying its limits.
    pub(crate) fn of(endpoint: &crate::Endpoint) -> Self {
        Self {
            via: if endpoint.direct {
                Via::Direct
            } else {
                Via::Environment
            },
            limits: endpoint.limits,
        }
    }
}

/// POSTs `body` to `url` with the headers `signer` adds for this request,
/// and returns the response body to read with the response's
/// `x-should-retry` header, which also governs a failure the body reports
/// later, and its `x-amzn-requestid`; or the failure a non-2xx status
/// reports. `route` chooses the
/// proxy and the socket limits: `Via::Environment` reads
/// `ureq::Proxy::try_from_env()` at the call, so tests pass an explicit
/// [`Via::Through`] value instead.
pub(crate) fn post_signed(
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
    signer: Option<&dyn Signer>,
    route: &Route,
    cancel: &Arc<Cancel>,
    secrets: &mut Secrets,
) -> Result<(impl Read + use<>, Option<bool>, Option<String>), Error> {
    let signed = prepare(url, headers, body, signer, cancel, secrets)?;
    let (result, _) = send(url, headers, &signed, body, route, cancel, None);
    let response = result?;
    let (status, should_retry, request_id) = head(&response);
    if !(200..300).contains(&status) {
        return Err(status_error(response, url, should_retry, status));
    }
    Ok((response.into_body().into_reader(), should_retry, request_id))
}

/// Signs the request and checks every header that will be sent: the fixed
/// headers Fiber built alongside the signed ones. A cancelled call, a
/// signing failure and a refused header fail before anything is sent, so
/// the caller stamps no send. A refused fixed header fails as a refused
/// signed header does: both name values that could never be sent.
fn prepare(
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
    signer: Option<&dyn Signer>,
    cancel: &Arc<Cancel>,
    secrets: &mut Secrets,
) -> Result<Vec<(String, String)>, Error> {
    // A call cancelled before it starts never resolves or connects.
    if cancel.is_cancelled() {
        return Err(Error::Connection("the call was cancelled".into()));
    }
    for (name, value) in headers {
        if let Err(why) = validate_signed_header(name, value) {
            return Err(Error::Sign(why));
        }
    }
    let signed = match signer {
        Some(signer) => {
            let signed = signer.sign(&SignRequest {
                method: "POST",
                url,
                headers,
                body,
            });
            // Added whether `sign()` succeeds or fails: a `sign()` error
            // that echoes the credential it was handed is stored redacted.
            for credential in signer.credentials() {
                secrets.add(credential);
            }
            signed.map_err(Error::Sign)?
        }
        None => Vec::new(),
    };
    for (name, value) in &signed {
        if let Err(why) = validate_signed_header(name, value) {
            return Err(Error::Sign(why));
        }
    }
    for (name, value) in &signed {
        secrets.add_header(name, value);
    }
    Ok(signed)
}

/// Sends the signed request over `route`: the response with the send
/// stamp. The stamp is `clock.now()` just before the request is written
/// to the network, after signing, header checks and agent setup, and is
/// reported even when the send fails. `None` reads no clock and starts no
/// connector scope, so the ordinary path performs no diagnostic work.
/// When supplied, the observed path selects the timing agent, which times
/// one actual connector attempt group as `net.connect`.
fn send(
    url: &str,
    headers: &[(String, String)],
    signed: &[(String, String)],
    body: &[u8],
    route: &Route,
    cancel: &Arc<Cancel>,
    observed: Option<&SendObservation<'_>>,
) -> (
    Result<ureq::http::Response<ureq::Body>, Error>,
    Option<Instant>,
) {
    // One agent per call, so its connector keeps this call's socket.
    let proxy = match &route.via {
        Via::Environment => ureq::Proxy::try_from_env(),
        Via::Direct => None,
        Via::Through(proxy) => Some(proxy.clone()),
    };
    let config = net::config()
        .proxy(proxy)
        .http_status_as_error(false)
        .max_redirects(0)
        .build();
    let agent = match observed.and_then(|observe| observe.timing) {
        Some(timing) => net::agent_observed(
            config,
            Arc::<Cancel>::clone(cancel),
            DefaultResolver::default(),
            route.limits,
            timing.clone(),
        ),
        None => net::agent(
            config,
            Arc::<Cancel>::clone(cancel),
            DefaultResolver::default(),
            route.limits,
        ),
    };
    let mut request = agent.post(url);
    for (name, value) in headers.iter().chain(signed) {
        request = request.header(name, value);
    }
    let start = observed.map(|observe| observe.clock.now());
    let response = request
        .send(body)
        .map_err(|e| Error::Connection(e.to_string()));
    (response, start)
}

/// What one observed send stamps and times: the clock with the optional
/// connector timing. Grouped, so the shared send keeps one observation
/// input beside the call's own.
struct SendObservation<'a> {
    clock: &'a Arc<dyn Clock>,
    timing: Option<&'a Timing>,
}
/// A response's status with its `x-should-retry` header and AWS's
/// `x-amzn-requestid`, kept only when it is not empty, read once
/// the status line and headers have arrived. A protocol that takes the
/// request id as its generation's says so on its call
/// (`docs/model-routing.md`, "Bedrock Converse wire facts").
fn head(response: &ureq::http::Response<ureq::Body>) -> (u16, Option<bool>, Option<String>) {
    let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok());
    let should_retry = header("x-should-retry")
        .map(|v| v.trim().to_ascii_lowercase())
        .and_then(|v| v.parse::<bool>().ok());
    let request_id = header("x-amzn-requestid")
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    (response.status().as_u16(), should_retry, request_id)
}

/// One non-2xx response's header fields: the parsed `retry-after` wait,
/// whether the header was present at all, the `date` value, and the
/// exception AWS names in `x-amzn-errortype`. Both send paths read them the
/// same way, so they share this parsing.
struct StatusMeta {
    retry_after: Option<f64>,
    has_retry_after: bool,
    date: Option<String>,
    error_type: Option<String>,
}

/// Reads one non-2xx response's header fields.
fn status_meta(headers: &ureq::http::HeaderMap) -> StatusMeta {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_ascii_lowercase())
    };
    StatusMeta {
        retry_after: header("retry-after")
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|wait| wait.is_finite() && *wait >= 0.0),
        // Presence alone vetoes the body's wait: a `retry-after` header
        // that is present but unparseable leaves `retry_after` unset.
        has_retry_after: headers.get("retry-after").is_some(),
        // The date keeps its case: `http_date` matches `GMT` exactly.
        date: headers
            .get("date")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_owned()),
        // AWS names the exception before a `:` and the namespace it comes
        // from, as in `ValidationException:http://internal.amazon.com/...`.
        error_type: headers
            .get("x-amzn-errortype")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(':').next())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned),
    }
}

/// The failure a non-2xx status reports for `body`: the status with the
/// `retry-after` wait, falling back to the usage-limit body's reset when
/// the reply carried no `retry-after` header. Both send paths share this
/// fallback, so an observed failure matches the ordinary one field for
/// field.
fn status_failure(
    status: u16,
    body: String,
    should_retry: Option<bool>,
    url: &str,
    meta: &StatusMeta,
) -> Error {
    let retry_after = meta.retry_after.or_else(|| {
        if meta.has_retry_after {
            None
        } else {
            usage_reset(&body, meta.date.as_deref())
        }
    });
    Error::Status {
        status,
        body,
        retry_after,
        should_retry,
        url: crate::error::target(url),
        error_type: meta.error_type.clone(),
    }
}

/// The failure a non-2xx status reports, reading the error body exactly as
/// the ordinary path does: the same bounded lossy read, the same stalled
/// timeout, the same empty fallback and the same `retry-after` wait.
fn status_error(
    response: ureq::http::Response<ureq::Body>,
    url: &str,
    should_retry: Option<bool>,
    status: u16,
) -> Error {
    let meta = status_meta(response.headers());
    // A stalled error body is a dropped connection, retried as one,
    // keeping the `x-should-retry` header's effect: a timeout becomes
    // `Stalled`, carrying the veto when the headers sent one. Any
    // other read failure keeps today's empty body and the status's
    // code.
    let body = match response.into_body().read_to_string() {
        Ok(body) => body,
        Err(error) => {
            if net::timed_out(&error) {
                return Error::Stalled {
                    message: error.to_string(),
                    should_retry,
                };
            }
            String::new()
        }
    };
    status_failure(status, body, should_retry, url, &meta)
}

/// The body limit the observed error read shares with the ordinary one:
/// ureq's own default for `read_to_string`, so an oversized error body
/// fails the same way on both paths.
const ERROR_BODY_LIMIT: u64 = 10 * 1024 * 1024;

/// Counts the body bytes read through it, saturating: the observed path's
/// `response_bytes`. A plain pass-through otherwise: reads, errors and
/// EOF behave exactly as the inner reader's do.
#[derive(Debug, Default)]
pub(crate) struct CountingReader<R> {
    inner: R,
    count: u64,
}

impl<R> CountingReader<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self { inner, count: 0 }
    }

    /// The bytes read so far.
    pub(crate) fn count(&self) -> u64 {
        self.count
    }

    /// Adds `n` more bytes read, saturating at `u64::MAX`.
    pub(crate) fn add(&mut self, n: u64) {
        self.count = self.count.saturating_add(n);
    }
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.add(n as u64);
        Ok(n)
    }
}

/// One sent request's milestones: what the observed path stamps around the
/// ordinary send. `start` is the send stamp, `headers_at` when the status
/// line and headers had been read.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Dispatched {
    pub(crate) start: Instant,
    pub(crate) headers_at: Option<Instant>,
    pub(crate) status: Option<u16>,
    pub(crate) bytes_read: u64,
}

/// What the observed send reports on failure: the error the ordinary path
/// reports, with the milestones when the request was sent. `sent` is `None`
/// when the request never reached the network: a pre-send cancel, a
/// signing failure or a refused header.
#[derive(Debug)]
pub(crate) struct ObservedFailure {
    pub(crate) error: Error,
    pub(crate) sent: Option<Dispatched>,
}

/// What the observed send reports on success: the counted body with the
/// response's `x-should-retry` and `x-amzn-requestid` headers and the send
/// milestones.
pub(crate) struct ObservedSend {
    pub(crate) stream: CountingReader<ureq::BodyReader<'static>>,
    pub(crate) should_retry: Option<bool>,
    pub(crate) request_id: Option<String>,
    pub(crate) status: u16,
    pub(crate) start: Instant,
    pub(crate) headers_at: Instant,
}

/// As [`post_signed`], stamping the send and counting the body for one
/// observed call. Success carries the counted stream; a failure after the
/// send carries what the headers said with the bytes read so far; a
/// failure before the send carries no milestones and writes no line.
/// Every `Error` matches what [`post_signed`] reports for the same
/// outcome, including the error body.
#[allow(
    clippy::too_many_arguments,
    reason = "one send with its observation inputs: the call's own plus the writer, clock and attempt"
)]
#[allow(
    clippy::result_large_err,
    reason = "the failure carries the call's error beside the send's milestones, once per call"
)]
pub(crate) fn post_signed_observed(
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
    signer: Option<&dyn Signer>,
    route: &Route,
    cancel: &Arc<Cancel>,
    secrets: &mut Secrets,
    clock: &Arc<dyn Clock>,
    timing: Option<&Timing>,
) -> Result<ObservedSend, ObservedFailure> {
    let failed = |error: Error| ObservedFailure { error, sent: None };
    let signed = prepare(url, headers, body, signer, cancel, secrets).map_err(failed)?;
    let observe = SendObservation { clock, timing };
    let (result, start) = send(url, headers, &signed, body, route, cancel, Some(&observe));
    // `send` stamps only when handed a clock, so this is always set here.
    let start = start.unwrap_or_else(|| clock.now());
    // A failed send after the stamp still reports it, with the response
    // fields omitted: the request went out but no response came.
    let response = result.map_err(|error| ObservedFailure {
        error,
        sent: Some(Dispatched {
            start,
            headers_at: None,
            status: None,
            bytes_read: 0,
        }),
    })?;
    let headers_at = clock.now();
    let (status, should_retry, request_id) = head(&response);
    if !(200..300).contains(&status) {
        return Err(observed_status_error(
            response,
            url,
            should_retry,
            status,
            start,
            headers_at,
        ));
    }
    Ok(ObservedSend {
        stream: CountingReader::new(response.into_body().into_reader()),
        should_retry,
        request_id,
        status,
        start,
        headers_at,
    })
}

/// As [`status_error`], counting the error body through the same bounded
/// read: the counter sits under ureq's lossy buffering, so the body
/// matches the ordinary path's byte for byte while the counter holds the
/// transfer-decoded bytes actually read, including a partial body when
/// the read fails.
fn observed_status_error(
    response: ureq::http::Response<ureq::Body>,
    url: &str,
    should_retry: Option<bool>,
    status: u16,
    start: Instant,
    headers_at: Instant,
) -> ObservedFailure {
    let sent = |bytes_read: u64| {
        Some(Dispatched {
            start,
            headers_at: Some(headers_at),
            status: Some(status),
            bytes_read,
        })
    };
    let meta = status_meta(response.headers());
    // Whether the body decodes lossy: ureq applies its lossy reader only
    // to `text/*` responses, so the recorded body matches the ordinary
    // path's byte for byte only under the same condition.
    let is_text = is_text_body(response.headers());
    // The same bounded read the ordinary path makes, counted beneath
    // ureq's lossy buffering: the counter wraps the transfer-decoded
    // reader without the lossy layer, whose 8-byte prefetch would swallow
    // a short body into its buffer before a stall, and the bytes decode
    // afterwards, so the body still matches byte for byte while the count
    // holds what was actually read.
    let mut counted = CountingReader::new(
        response
            .into_body()
            .into_with_config()
            .limit(ERROR_BODY_LIMIT)
            .reader(),
    );
    let mut raw = Vec::new();
    let body = match counted.read_to_end(&mut raw) {
        Ok(_) => decode_error_body(&raw, is_text),
        Err(error) => {
            let bytes_read = counted.count();
            if read_timed_out(&error) {
                return ObservedFailure {
                    error: Error::Stalled {
                        message: error.to_string(),
                        should_retry,
                    },
                    sent: sent(bytes_read),
                };
            }
            String::new()
        }
    };
    let bytes_read = counted.count();
    ObservedFailure {
        error: status_failure(status, body, should_retry, url, &meta),
        sent: sent(bytes_read),
    }
}

/// Whether an error body decodes lossy: ureq's lossy reader applies only
/// to `text/*` responses, so anything else decodes strict, exactly as the
/// ordinary path does.
fn is_text_body(headers: &ureq::http::HeaderMap) -> bool {
    headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|mime| mime.starts_with("text/"))
}

/// Decodes error `raw` bytes exactly as the ordinary bounded lossy read
/// does: `text/*` replaces invalid UTF-8, anything else must be valid
/// UTF-8, else the body stays empty as today.
fn decode_error_body(raw: &[u8], is_text: bool) -> String {
    if is_text {
        lossy_question(raw)
    } else {
        std::str::from_utf8(raw).unwrap_or("").to_owned()
    }
}

/// Replaces invalid UTF-8 with `?`, one per byte, as ureq's lossy reader
/// does: valid prefixes pass through, and bytes cut short of a character
/// at the end of the body are replaced too, since the whole body is in
/// hand and there is nothing more to wait for.
fn lossy_question(raw: &[u8]) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                out.push_str(valid);
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                let (prefix, tail) = rest.split_at(valid);
                out.push_str(std::str::from_utf8(prefix).unwrap_or(""));
                match error.error_len() {
                    Some(len) => {
                        for _ in 0..len {
                            out.push('?');
                        }
                        rest = tail.get(len..).unwrap_or(b"");
                    }
                    // Cut short at the end of the body: every remaining
                    // byte is replaced.
                    None => {
                        for _ in 0..tail.len() {
                            out.push('?');
                        }
                        break;
                    }
                }
            }
        }
    }
    out
}

/// Whether a body read through the counted reader failed on a timeout: the
/// same failures [`net::timed_out`] names, recovered from the I/O error.
/// A ureq timeout arrives wrapped as the error's payload; a socket read
/// that waited out its bound arrives with the timed-out kind.
fn read_timed_out(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::TimedOut {
        return true;
    }
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<ureq::Error>())
        .is_some_and(net::timed_out)
}

/// The recorded `host` and `path` for `url`: the authority without
/// userinfo, keeping an explicit port, and the path with no query string
/// or fragment, percent-decoded and redacted with `secrets`. A path whose
/// decoded bytes are not valid UTF-8 is omitted entirely, never logged
/// encoded; an empty host is omitted too. The request itself is unchanged.
pub(crate) fn sanitized_target(url: &str, secrets: &Secrets) -> (Option<String>, Option<String>) {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let (authority, path) = rest.find('/').map_or((rest, "/"), |at| rest.split_at(at));
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = (!host.is_empty()).then(|| secrets.redact(host));
    let path = String::from_utf8(percent_decode(path.as_bytes()))
        .ok()
        .map(|decoded| secrets.redact(&decoded));
    (host, path)
}

/// Percent-decodes `raw`: each `%` followed by two hex digits, in either
/// case, becomes its byte; anything else stays literal. The bytes are
/// consumed through the iterator, with no index arithmetic: a `%` peeks at
/// the next two bytes without taking them, so a cut-short or non-hex
/// follower is re-scanned as a fresh byte, ready to start another escape.
fn percent_decode(raw: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(raw.len());
    let mut bytes = raw.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        if byte != b'%' {
            decoded.push(byte);
            continue;
        }
        let mut look = bytes.clone();
        match (look.next(), look.next()) {
            (Some(high), Some(low)) => match (hex_val(high), hex_val(low)) {
                (Some(high), Some(low)) => {
                    decoded.push((high << 4) + low);
                    bytes.next();
                    bytes.next();
                }
                // Not hex: the `%` stays literal.
                (None, _) | (_, None) => {
                    decoded.push(byte);
                }
            },
            // Cut short: the `%` stays literal.
            _ => {
                decoded.push(byte);
            }
        }
    }
    decoded
}

/// A hex digit's value, in either case.
fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// The seconds from the failed reply's `Date` header to a usage-limit
/// body's `resets_at`: only when the reply carried no `retry-after`, the
/// body is a usage-limit body with an integer `resets_at`, and `Date`
/// parses as an IMF-fixdate (`docs/model-routing.md`, "Protocols and
/// providers"). No `Date`, an unparsable one, or a reset at or before the
/// `Date` leaves the wait absent; no clock is read.
fn usage_reset(body: &str, date: Option<&str>) -> Option<f64> {
    let value: Value = serde_json::from_str(body).ok()?;
    let error = value.get("error")?;
    if !crate::error::usage_limit(error) {
        return None;
    }
    let resets = error.get("resets_at")?.as_u64()?;
    let sent = date.and_then(date::http_date)?;
    let wait = resets.checked_sub(sent)?;
    if wait == 0 {
        return None;
    }
    Some(wait as f64)
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
