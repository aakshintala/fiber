//! One model call, ready to send, shared by the four protocols: each
//! protocol keeps its path, auth header, body and decoder, and builds this
//! call with them (`docs/model-routing.md`, "Protocols and providers").

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::diag::{DebugLog, HttpRequest, PrefixHashes, ProviderPurpose, Timing};
use contract::events::CacheLifetime;
use contract::provider::{CallError, CallUsage, Delta, InputSize, ModelCall, ModelRequest, Reply};
use contract::shapes::Tokens;

use crate::http::{self, Cancel};
use crate::redact::Secrets;
use crate::{Endpoint, Error};

/// A reply decoder run through a call: the stream, the call's cache
/// lifetime and the delta sink. A decoder that needs no lifetime drops it.
#[allow(
    clippy::result_large_err,
    reason = "the decode carries its partial alongside the error for the call's usage"
)]
pub(crate) type Decode = fn(
    &mut dyn BufRead,
    CacheLifetime,
    &mut dyn FnMut(Delta),
) -> Result<Reply, (Error, Option<CallUsage>)>;

/// What one observed model call records: the writer, the clock, the
/// attempt and the model. Immutable: a factory captures one route's
/// values and the loop passes the attempt explicitly, so no global or
/// mutable context names the call. The model is bound by the protocol's
/// `call_observed`, which knows its own endpoint; the direct test seam
/// binds it explicitly. The optional timing times one connector attempt
/// group as `net.connect`; without one the call still records its
/// `http_request` line over the plain connector.
#[derive(Clone)]
pub struct RequestObservation {
    log: Arc<dyn DebugLog>,
    clock: Arc<dyn Clock>,
    attempt: NonZeroU32,
    model: Option<String>,
    timing: Option<Timing>,
    cache_diagnostics: bool,
}

impl RequestObservation {
    /// Observes one call as `attempt` of its logical request, writing to
    /// `log` and stamping `clock`. The model binds later, per endpoint.
    pub fn new(log: Arc<dyn DebugLog>, clock: Arc<dyn Clock>, attempt: NonZeroU32) -> Self {
        Self {
            log,
            clock,
            attempt,
            model: None,
            timing: None,
            cache_diagnostics: false,
        }
    }

    /// Binds the model the call goes to: the endpoint's own id. Each
    /// native `call_observed` binds its own after the debugging check, so
    /// the ordinary path clones no model.
    pub fn with_model(mut self, model: String) -> Self {
        self.model = Some(model);
        self
    }

    /// Times the call's connector group through `timing`: one `net.connect`
    /// scope from the first socket attempt to the kept socket. The factory
    /// captures one route's callable and the loop passes the attempt
    /// explicitly, so no global or mutable context names the call.
    pub fn with_timing(mut self, timing: Timing) -> Self {
        self.timing = Some(timing);
        self
    }

    /// Asks the encoder for redacted prefix digests and the reported
    /// cache use: the observation then carries them into the line
    /// (`docs/prompt-cache.md`, "Cache diagnostics"). Off by default,
    /// so an unset `cache.diagnostics` changes no line.
    #[must_use]
    pub fn with_cache_diagnostics(mut self, on: bool) -> Self {
        self.cache_diagnostics = on;
        self
    }

    /// Whether the encoder hashes the request's cacheable segments: with
    /// the flag off it performs no digest work, branching before any
    /// hashing.
    pub(crate) fn cache_diagnostics(&self) -> bool {
        self.cache_diagnostics
    }

    /// Whether the writer records debug lines: an `info` writer runs the
    /// call unobserved.
    pub(crate) fn debugging(&self) -> bool {
        self.log.debugging()
    }
}

/// One model call's diagnostic record while it runs: the milestones every
/// `_ms` counts from the send start, and the bytes read so far. Built
/// once the send's outcome is known, finished exactly once when the call
/// returns or its open reader ends.
pub(crate) struct ObservationFinish {
    log: Arc<dyn DebugLog>,
    clock: Arc<dyn Clock>,
    provider: String,
    model: Option<String>,
    url: String,
    redact: Secrets,
    attempt: NonZeroU32,
    request_bytes: u64,
    start: Instant,
    headers_at: Option<Instant>,
    status: Option<u16>,
    first_token_at: Option<Instant>,
    bytes: u64,
    hashes: Option<PrefixHashes>,
    cache_read: Option<u64>,
    cache_write: Option<BTreeMap<String, u64>>,
    done: bool,
}

impl ObservationFinish {
    pub(crate) fn new(
        observation: &RequestObservation,
        provider: &str,
        url: &str,
        request_bytes: u64,
        start: Instant,
    ) -> Self {
        Self {
            log: Arc::clone(&observation.log),
            clock: Arc::clone(&observation.clock),
            provider: provider.to_owned(),
            model: observation.model.clone(),
            url: url.to_owned(),
            redact: Secrets::default(),
            attempt: observation.attempt,
            request_bytes,
            start,
            headers_at: None,
            status: None,
            first_token_at: None,
            bytes: 0,
            hashes: None,
            cache_read: None,
            cache_write: None,
            done: false,
        }
    }

    /// Records the response head: when it arrived and what status came.
    pub(crate) fn sent(&mut self, headers_at: Instant, status: u16) {
        self.headers_at = Some(headers_at);
        self.status = Some(status);
    }

    /// Records a sent failure's milestones: the head when it arrived and
    /// the bytes read so far. A send that failed before the head keeps
    /// both absent.
    pub(crate) fn dispatched(&mut self, dispatched: &crate::http::Dispatched) {
        self.headers_at = dispatched.headers_at;
        self.status = dispatched.status;
        self.bytes = dispatched.bytes_read;
    }

    /// Sets the secrets the recorded target is redacted with: the call's
    /// own, enriched by the send, with every fixed header value of this
    /// attempt. Only the recorded host and path go through it; the
    /// request itself is unchanged.
    pub(crate) fn set_redact(&mut self, redact: Secrets) {
        self.redact = redact;
    }

    /// Stamps the first streamed token, once: the first `Delta` of any
    /// kind latches it before the delta is forwarded, and a later one
    /// never overwrites it. Headers, framing and reads alone never set it.
    pub(crate) fn latch_first_token(&mut self) {
        if self.first_token_at.is_none() {
            self.first_token_at = Some(self.clock.now());
        }
    }

    /// Holds the response bytes read so far.
    pub(crate) fn set_bytes(&mut self, bytes: u64) {
        self.bytes = bytes;
    }

    /// Holds the encoder's redacted prefix digests, once, on the observed
    /// run path: absent on a send failure, so those lines stay out of a
    /// miss count.
    pub(crate) fn set_hashes(&mut self, hashes: PrefixHashes) {
        self.hashes = Some(hashes);
    }

    /// Holds what the decoded response reported: its cache read and its
    /// cache write keyed by lifetime. Called once, after a successful
    /// decode, so a line carrying usage always carries a 2xx status;
    /// absent on a send or decode failure.
    pub(crate) fn set_usage(&mut self, tokens: &Tokens) {
        self.cache_read = Some(tokens.cache_read);
        self.cache_write = Some(tokens.cache_write.clone());
    }

    /// Writes the one `http_request` line, once. A second call writes
    /// nothing: the open reader's error and its drop share this guard.
    pub(crate) fn finish(&mut self) {
        if self.done {
            return;
        }
        self.done = true;
        let total = self.clock.now();
        let (host, path) = http::sanitized_target(&self.url, &self.redact);
        let mut request =
            HttpRequest::for_provider(self.provider.clone(), ProviderPurpose::ModelCall);
        request.model = self.model.clone();
        request.host = host;
        request.path = path;
        request.status = self.status;
        request.attempt = self.attempt.get();
        request.request_bytes = self.request_bytes;
        request.response_bytes = self.bytes;
        request.headers_ms = self.headers_at.map(|at| ms_since(self.start, at));
        request.first_token_ms = self.first_token_at.map(|at| ms_since(self.start, at));
        request.total_ms = ms_since(self.start, total);
        request.prefix_hashes = self.hashes.clone();
        request.cache_read = self.cache_read;
        request.cache_write = self.cache_write.clone();
        self.log.http_request(&request);
    }
}

/// Whole milliseconds from `start` to `end`: truncated, saturating to
/// `u64::MAX`. A clock that runs backwards contributes zero: no panic
/// and no fabricated duration.
pub(crate) fn ms_since(start: Instant, end: Instant) -> u64 {
    duration_ms(end.checked_duration_since(start).unwrap_or(Duration::ZERO))
}

/// Whole milliseconds in `elapsed`: truncated, saturating to `u64::MAX`.
pub(crate) fn duration_ms(elapsed: Duration) -> u64 {
    elapsed.as_millis().try_into().unwrap_or(u64::MAX)
}

/// One model call, ready to send.
pub struct Call {
    url: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
    input_size: InputSize,
    provider: String,
    login_advice: crate::LoginAdvice,
    signer: Option<Arc<dyn contract::signing::Signer>>,
    route: http::Route,
    cancel: Arc<Cancel>,
    secrets: Secrets,
    lifetime: CacheLifetime,
    hashes: Option<PrefixHashes>,
    decode: Decode,
    on_error: fn(Error) -> Error,
    observation: Option<RequestObservation>,
    /// Whether the response's `x-amzn-requestid` names the generation when
    /// the stream names none, as on `bedrock-converse`
    /// (`docs/model-routing.md`, "Bedrock Converse wire facts").
    named_by_request_id: bool,
}

impl Call {
    /// The call sending `body` to `url` with `headers`, decoded by
    /// `decode`. The reply's `input_size` is the call's.
    pub(crate) fn new(
        endpoint: &Endpoint,
        request: &ModelRequest,
        url: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        decode: Decode,
    ) -> Call {
        Call {
            input_size: crate::images::input_size(&body, request, endpoint.text_only),
            provider: endpoint.provider.clone(),
            login_advice: endpoint.login_advice,
            signer: endpoint.signer.clone(),
            route: http::Route::of(endpoint),
            cancel: Arc::default(),
            secrets: endpoint.secrets(),
            lifetime: request.cache_lifetime,
            hashes: None,
            url,
            headers,
            body,
            decode,
            on_error: std::convert::identity,
            observation: None,
            named_by_request_id: false,
        }
    }

    /// Takes the response's `x-amzn-requestid` as the generation's id, for a
    /// protocol whose stream names none. Without the header, or with an
    /// empty one, the generation stays unnamed and Fiber mints its id.
    #[must_use]
    pub(crate) fn named_by_request_id(self) -> Call {
        Call {
            named_by_request_id: true,
            ..self
        }
    }

    /// `decoded`, its generation named by `request_id` on a call that takes
    /// the request id; unchanged on any other call.
    #[allow(
        clippy::result_large_err,
        reason = "the decode carries its partial alongside the error for the call's usage"
    )]
    fn named(
        &self,
        mut decoded: Result<Reply, (Error, Option<CallUsage>)>,
        request_id: Option<String>,
    ) -> Result<Reply, (Error, Option<CallUsage>)> {
        if !self.named_by_request_id {
            return decoded;
        }
        let generation = match &mut decoded {
            Ok(reply) => &mut reply.generation_id,
            Err((_, Some(partial))) => &mut partial.generation_id,
            Err((_, None)) => return decoded,
        };
        *generation = request_id.map(contract::GenerationId);
        decoded
    }

    /// Carries the encoder's redacted prefix digests: the observed run
    /// path writes them into the line. Only a `call_observed` with the
    /// setting on builds one, so an unobserved or opted-out call carries
    /// none.
    #[must_use]
    pub(crate) fn with_hashes(mut self, hashes: PrefixHashes) -> Self {
        self.hashes = Some(hashes);
        self
    }

    /// Runs `hook` over a failed send's error, after `should_retry` is
    /// read from it. A decode failure never runs it.
    #[must_use]
    pub(crate) fn on_error(self, hook: fn(Error) -> Error) -> Call {
        Call {
            on_error: hook,
            ..self
        }
    }

    /// Observes this call, writing one `http_request` line when the
    /// call was sent. At `info` the call runs exactly as an unobserved one
    /// does: no clock is read and no line is written.
    #[must_use]
    pub fn observed(self, observation: RequestObservation) -> Call {
        Call {
            observation: Some(observation),
            ..self
        }
    }

    /// The observation when it writes: `Some` only while the writer is at
    /// `debug`. An `info` writer passed here runs the call unchanged, with
    /// no clock read, no state and no line.
    fn active(&self) -> Option<&RequestObservation> {
        self.observation
            .as_ref()
            .filter(|observation| observation.debugging())
    }

    /// The secrets the recorded target is redacted with: the send's own,
    /// enriched with every fixed header value of this attempt.
    fn redact_with(&self, secrets: &Secrets) -> Secrets {
        let mut redact = secrets.clone();
        for (name, value) in &self.headers {
            redact.add_header(name, value);
        }
        redact
    }

    /// Sends the request and returns the reply's bytes, unread.
    pub fn open(&self) -> Result<impl Read + use<>, Error> {
        match self.active() {
            None => {
                let mut secrets = self.secrets.clone();
                http::post_signed(
                    &self.url,
                    &self.headers,
                    &self.body,
                    self.signer.as_deref(),
                    &self.route,
                    &self.cancel,
                    &mut secrets,
                )
                .map(|(body, _, _)| OpenBody::plain(Box::new(body)))
            }
            Some(observation) => self.open_observed(observation),
        }
    }

    /// Sends the request observed: the finish guard moves into the returned
    /// reader, so a read error, the end of the stream or the drop writes
    /// the one line. A send that fails after the request went out writes
    /// it at once; one that never sent writes none.
    fn open_observed(&self, observation: &RequestObservation) -> Result<OpenBody, Error> {
        let mut secrets = self.secrets.clone();
        let request_bytes = self.body.len() as u64;
        match http::post_signed_observed(
            &self.url,
            &self.headers,
            &self.body,
            self.signer.as_deref(),
            &self.route,
            &self.cancel,
            &mut secrets,
            &observation.clock,
            observation.timing.as_ref(),
        ) {
            Ok(sent) => {
                let mut finish = ObservationFinish::new(
                    observation,
                    &self.provider,
                    &self.url,
                    request_bytes,
                    sent.start,
                );
                finish.sent(sent.headers_at, sent.status);
                finish.set_redact(self.redact_with(&secrets));
                if let Some(hashes) = &self.hashes {
                    finish.set_hashes(hashes.clone());
                }
                Ok(OpenBody::observed(Box::new(sent.stream), finish))
            }
            Err(failed) => {
                if let Some(dispatched) = failed.sent {
                    let mut finish = ObservationFinish::new(
                        observation,
                        &self.provider,
                        &self.url,
                        request_bytes,
                        dispatched.start,
                    );
                    finish.dispatched(&dispatched);
                    finish.set_redact(self.redact_with(&secrets));
                    // The bytes went out, so their digests are known even
                    // though the reply failed; the usage stays absent.
                    if let Some(hashes) = &self.hashes {
                        finish.set_hashes(hashes.clone());
                    }
                    finish.finish();
                }
                Err(failed.error)
            }
        }
    }
}

impl ModelCall for Call {
    fn run(&self, sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        match self.active() {
            None => {
                let mut secrets = self.secrets.clone();
                let (decoded, should_retry) = self.send_plain(sink, &mut secrets);
                Self::finish_send(
                    &self.cancel,
                    self.input_size,
                    &self.provider,
                    self.login_advice,
                    &secrets,
                    decoded,
                    should_retry,
                )
            }
            Some(observation) => self.run_observed(observation, sink),
        }
    }

    fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// A send's outcome: the decoded reply, carrying its partial usage on
/// failure, with the response's `x-should-retry` header.
type Decoded = (Result<Reply, (Error, Option<CallUsage>)>, Option<bool>);

impl Call {
    /// Sends the request unobserved and decodes the reply: today's send.
    #[allow(
        clippy::result_large_err,
        reason = "the decode carries its partial alongside the error for the call's usage"
    )]
    fn send_plain(&self, sink: &mut dyn FnMut(Delta), secrets: &mut Secrets) -> Decoded {
        match http::post_signed(
            &self.url,
            &self.headers,
            &self.body,
            self.signer.as_deref(),
            &self.route,
            &self.cancel,
            secrets,
        ) {
            Ok((stream, should_retry, request_id)) => (
                self.named(
                    (self.decode)(&mut BufReader::new(stream), self.lifetime, sink),
                    request_id,
                ),
                should_retry,
            ),
            Err(e) => {
                let should_retry = e.should_retry();
                (Err(((self.on_error)(e), None)), should_retry)
            }
        }
    }

    /// Builds the call's result from its decoded reply: the cancel that
    /// landed ends it, whatever it returned.
    #[allow(
        clippy::result_large_err,
        reason = "the error is the model call's, returned unchanged"
    )]
    fn finish_send(
        cancel: &Cancel,
        input_size: InputSize,
        provider: &str,
        login_advice: crate::LoginAdvice,
        secrets: &Secrets,
        decoded: Result<Reply, (Error, Option<CallUsage>)>,
        should_retry: Option<bool>,
    ) -> Result<Reply, CallError> {
        // Whatever a cancelled call returns, the cancel ended it.
        if cancel.is_cancelled() {
            return Err(CallError::Cancelled {
                usage: crate::unfinished::carried(&decoded, input_size),
            });
        }
        let usage = crate::unfinished::carried(&decoded, input_size);
        match decoded {
            Ok(mut reply) => {
                reply.input_size = input_size;
                Ok(reply)
            }
            Err((error, _)) => Err(CallError::Failed {
                failure: error.failure(provider, login_advice, secrets),
                should_retry,
                usage,
            }),
        }
    }

    /// Runs the call observed: one `http_request` line when the request
    /// was sent, whatever the call returns. The total runs through decoding
    /// and the result above, not merely the last byte read. A request never
    /// sent writes no line.
    #[allow(
        clippy::result_large_err,
        reason = "the error is the model call's, returned unchanged"
    )]
    fn run_observed(
        &self,
        observation: &RequestObservation,
        sink: &mut dyn FnMut(Delta),
    ) -> Result<Reply, CallError> {
        let mut secrets = self.secrets.clone();
        // The request body's length as posted: the same figure as the
        // call's `InputSize::bytes`.
        let request_bytes = self.body.len() as u64;
        let outcome = http::post_signed_observed(
            &self.url,
            &self.headers,
            &self.body,
            self.signer.as_deref(),
            &self.route,
            &self.cancel,
            &mut secrets,
            &observation.clock,
            observation.timing.as_ref(),
        );
        // The record, built once the send's outcome is known: a request
        // never sent leaves it absent and writes no line.
        let mut finish: Option<ObservationFinish> = None;
        let (decoded, should_retry) = match outcome {
            Ok(sent) => {
                let mut record = ObservationFinish::new(
                    observation,
                    &self.provider,
                    &self.url,
                    request_bytes,
                    sent.start,
                );
                record.sent(sent.headers_at, sent.status);
                record.set_redact(self.redact_with(&secrets));
                if let Some(hashes) = &self.hashes {
                    record.set_hashes(hashes.clone());
                }
                let mut counted = sent.stream;
                // The first `Delta` of any kind latches the token stamp
                // before it is forwarded; every delta reaches the caller
                // unchanged.
                let latched = &mut record;
                let decoded = (self.decode)(
                    &mut BufReader::new(&mut counted),
                    self.lifetime,
                    &mut |delta| {
                        latched.latch_first_token();
                        sink(delta);
                    },
                );
                // The buffered reader may have read ahead: the count, not
                // the reader's end, says what came. A decoded reply reports
                // its cache use only with the setting on; a failure or an
                // unflagged observation leaves it absent, so those lines
                // stay out of a miss count.
                record.set_bytes(counted.count());
                if observation.cache_diagnostics()
                    && let Ok(reply) = &decoded
                {
                    record.set_usage(&reply.tokens);
                }
                finish = Some(record);
                (self.named(decoded, sent.request_id), sent.should_retry)
            }
            Err(failed) => {
                if let Some(dispatched) = failed.sent {
                    let mut record = ObservationFinish::new(
                        observation,
                        &self.provider,
                        &self.url,
                        request_bytes,
                        dispatched.start,
                    );
                    record.dispatched(&dispatched);
                    record.set_redact(self.redact_with(&secrets));
                    // The bytes went out, so their digests are known even
                    // though the reply failed; the usage stays absent.
                    if let Some(hashes) = &self.hashes {
                        record.set_hashes(hashes.clone());
                    }
                    finish = Some(record);
                }
                let should_retry = failed.error.should_retry();
                (Err(((self.on_error)(failed.error), None)), should_retry)
            }
        };
        let result = Self::finish_send(
            &self.cancel,
            self.input_size,
            &self.provider,
            self.login_advice,
            &secrets,
            decoded,
            should_retry,
        );
        if let Some(record) = finish.as_mut() {
            record.finish();
        }
        result
    }
}

/// One `open` reply body: the stream with the bytes read so far and the
/// finish guard the observed path moves into it. A plain `open` carries no
/// guard and reads exactly as today. A read error, the end of the stream
/// or the drop ends the record once; a zero-length read without end-of-
/// stream and an interrupted read never end it.
struct OpenBody {
    inner: Box<dyn Read>,
    bytes: u64,
    finish: Option<ObservationFinish>,
}

impl OpenBody {
    fn plain(inner: Box<dyn Read>) -> Self {
        Self {
            inner,
            bytes: 0,
            finish: None,
        }
    }

    fn observed(inner: Box<dyn Read>, finish: ObservationFinish) -> Self {
        Self {
            inner,
            bytes: 0,
            finish: Some(finish),
        }
    }
}

impl Read for OpenBody {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.inner.read(buf) {
            // The end of the stream ends the record while the reader is
            // still alive. A zero-length read without end-of-stream never
            // ends it: an empty destination buffer cannot be the end.
            Ok(0) if !buf.is_empty() => {
                if let Some(record) = self.finish.as_mut() {
                    record.set_bytes(self.bytes);
                    record.finish();
                }
                Ok(0)
            }
            Ok(n) => {
                self.bytes = self.bytes.saturating_add(n as u64);
                Ok(n)
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => Err(error),
            Err(error) => {
                if let Some(record) = self.finish.as_mut() {
                    record.set_bytes(self.bytes);
                    record.finish();
                }
                Err(error)
            }
        }
    }
}

impl Drop for OpenBody {
    fn drop(&mut self) {
        if let Some(record) = self.finish.as_mut() {
            record.set_bytes(self.bytes);
            record.finish();
        }
    }
}

/// The headers every request sends, in order: `content-type`, `accept`
/// naming the reply's framing (`text/event-stream`, or AWS's
/// `application/vnd.amazon.eventstream`), each of `fixed` (such as
/// Anthropic's `anthropic-version`, which stays between `accept` and
/// `user-agent` so an endpoint's own repeat of the name reads as today),
/// `user-agent`, the auth header when there is one, each of the endpoint's
/// own headers, then the cache key under `cache_key_header` when one is set.
pub(crate) fn base_headers(
    endpoint: &Endpoint,
    accept: &str,
    fixed: &[(&str, &str)],
    auth: Option<(&str, String)>,
    cache_key_header: Option<&str>,
    cache_key: &str,
) -> Vec<(String, String)> {
    let mut headers = vec![
        ("content-type".to_owned(), "application/json".to_owned()),
        ("accept".to_owned(), accept.to_owned()),
    ];
    headers.extend(
        fixed
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
    );
    headers.push((
        "user-agent".to_owned(),
        concat!("fiber/", env!("CARGO_PKG_VERSION")).to_owned(),
    ));
    if let Some((name, value)) = auth {
        headers.push((name.to_owned(), value));
    }
    headers.extend(endpoint.headers.iter().cloned());
    if let Some(name) = cache_key_header {
        headers.push((name.to_owned(), cache_key.to_owned()));
    }
    headers
}

#[cfg(test)]
#[path = "call_tests.rs"]
mod tests;
