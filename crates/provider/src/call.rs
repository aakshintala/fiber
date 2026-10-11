//! One model call, ready to send, shared by the four protocols: each
//! protocol keeps its path, auth header, body and decoder, and builds this
//! call with them (`docs/model-routing.md`, "Protocols and providers").

use std::io::{BufRead, BufReader, Read};
use std::sync::Arc;

use contract::events::CacheLifetime;
use contract::provider::{CallError, CallUsage, Delta, InputSize, ModelCall, ModelRequest, Reply};

use crate::http::{self, Cancel};
use crate::redact::Secrets;
use crate::{Endpoint, Error};

/// A reply decoder run through a call: the stream, the call's cache
/// lifetime and the delta sink. A decoder that needs no lifetime drops it.
#[allow(
    clippy::result_large_err,
    reason = "the decode carries its partial alongside the error for the call's usage"
)]
pub(crate) type Decode =
    fn(&mut dyn BufRead, CacheLifetime, &mut dyn FnMut(Delta)) -> Result<Reply, (Error, Option<CallUsage>)>;

/// One model call, ready to send.
pub struct Call {
    url: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
    input_size: InputSize,
    provider: String,
    signer: Option<Arc<dyn contract::signing::Signer>>,
    route: http::Route,
    cancel: Arc<Cancel>,
    secrets: Secrets,
    lifetime: CacheLifetime,
    decode: Decode,
    on_error: fn(Error) -> Error,
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
            signer: endpoint.signer.clone(),
            route: http::Route::of(endpoint),
            cancel: Arc::default(),
            secrets: endpoint.secrets(),
            lifetime: request.cache_lifetime,
            url,
            headers,
            body,
            decode,
            on_error: std::convert::identity,
        }
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

    /// Sends the request and returns the reply's bytes, unread.
    pub fn open(&self) -> Result<impl Read + use<>, Error> {
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
        .map(|(body, _)| body)
    }
}

impl ModelCall for Call {
    fn run(&self, sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        let mut secrets = self.secrets.clone();
        let (decoded, should_retry) = match http::post_signed(
            &self.url,
            &self.headers,
            &self.body,
            self.signer.as_deref(),
            &self.route,
            &self.cancel,
            &mut secrets,
        ) {
            Ok((stream, should_retry)) => (
                (self.decode)(&mut BufReader::new(stream), self.lifetime, sink),
                should_retry,
            ),
            Err(e) => {
                let should_retry = e.should_retry();
                (Err(((self.on_error)(e), None)), should_retry)
            }
        };
        // Whatever a cancelled call returns, the cancel ended it.
        if self.cancel.is_cancelled() {
            return Err(CallError::Cancelled {
                usage: crate::unfinished::carried(&decoded, self.input_size),
            });
        }
        let usage = crate::unfinished::carried(&decoded, self.input_size);
        match decoded {
            Ok(mut reply) => {
                reply.input_size = self.input_size;
                Ok(reply)
            }
            Err((error, _)) => Err(CallError::Failed {
                failure: error.failure(&self.provider, &secrets),
                should_retry,
                usage,
            }),
        }
    }

    fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// The headers every request sends, in order: `content-type`, `accept`,
/// each of `fixed` (such as Anthropic's `anthropic-version`, which stays
/// between `accept` and `user-agent` so an endpoint's own repeat of the
/// name reads as today), `user-agent`, the auth header when there is one,
/// each of the endpoint's own headers, then the cache key under
/// `cache_key_header` when one is set.
pub(crate) fn base_headers(
    endpoint: &Endpoint,
    fixed: &[(&str, &str)],
    auth: Option<(&str, String)>,
    cache_key_header: Option<&str>,
    cache_key: &str,
) -> Vec<(String, String)> {
    let mut headers = vec![
        ("content-type".to_owned(), "application/json".to_owned()),
        ("accept".to_owned(), "text/event-stream".to_owned()),
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
