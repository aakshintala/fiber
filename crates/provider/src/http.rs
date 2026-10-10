//! Sending a request over HTTP, on a socket Fiber owns so another thread can
//! close it (`docs/architecture.md`, "Cancellation"). Each call runs its own
//! agent over the shared connector in `net`, which keeps a handle to the
//! call's socket; shutting that handle down ends a read blocked inside
//! ureq, under TLS too. A provider
//! that signs its requests is asked for its headers on every send, a retry
//! included (`docs/model-routing.md`, "Signing a request").

use std::io::{self, Read};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

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
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
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

/// POSTs `body` to `url` with the headers `signer` adds for this request,
/// and returns the response body to read with the response's
/// `x-should-retry` header, which also governs a failure the body reports
/// later; or the failure a non-2xx status reports.
pub(crate) fn post_signed(
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
    signer: Option<&dyn Signer>,
    direct: bool,
    cancel: &Arc<Cancel>,
    secrets: &mut Secrets,
) -> Result<(impl Read + use<>, Option<bool>), Error> {
    post_with(
        url,
        headers,
        body,
        signer,
        cancel,
        secrets,
        if direct {
            None
        } else {
            ureq::Proxy::try_from_env()
        },
    )
}

/// [`post_signed`], with the proxy chosen by the caller: `None` connects
/// directly, `Some` tunnels through it. `post_signed` passes what the
/// environment names, so tests pass an explicit value instead.
fn post_with(
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
    signer: Option<&dyn Signer>,
    cancel: &Arc<Cancel>,
    secrets: &mut Secrets,
    proxy: Option<ureq::Proxy>,
) -> Result<(impl Read + use<>, Option<bool>), Error> {
    // A call cancelled before it starts never resolves or connects.
    if cancel.is_cancelled() {
        return Err(Error::Connection("the call was cancelled".into()));
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
    // One agent per call, so its connector keeps this call's socket.
    let agent = net::agent(
        net::config()
            .proxy(proxy)
            .http_status_as_error(false)
            .max_redirects(0)
            .build(),
        Arc::<Cancel>::clone(cancel),
        DefaultResolver::default(),
        net::LIMITS,
    );
    let mut request = agent.post(url);
    for (name, value) in headers.iter().chain(&signed) {
        request = request.header(name, value);
    }
    let response = request
        .send(body)
        .map_err(|e| Error::Connection(e.to_string()))?;
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_ascii_lowercase())
    };
    let should_retry = header("x-should-retry").and_then(|v| v.parse::<bool>().ok());
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let retry_after = header("retry-after")
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|wait| wait.is_finite() && *wait >= 0.0);
        // Presence alone vetoes the body's wait: a `retry-after` header
        // that is present but unparseable leaves `retry_after` unset.
        let has_retry_after = response.headers().get("retry-after").is_some();
        let date = response
            .headers()
            .get("date")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_owned());
        let body = response.into_body().read_to_string().unwrap_or_default();
        let retry_after = retry_after.or_else(|| {
            if has_retry_after {
                None
            } else {
                usage_reset(&body, date.as_deref())
            }
        });
        return Err(Error::Status {
            status,
            body,
            retry_after,
            should_retry,
            url: crate::error::target(url),
        });
    }
    Ok((response.into_body().into_reader(), should_retry))
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
