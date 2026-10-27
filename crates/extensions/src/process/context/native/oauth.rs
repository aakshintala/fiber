use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};

use contract::tool::Cancel;

use crate::host::admission::CallbackContext;

use super::super::WAIT_POLL;
use super::super::{NativeHostServices, ProcessContext};

impl NativeHostServices {
    /// Runs `host.oauth.callback`: serves one request on the port and
    /// answers its query pairs, unattended and unreadable exactly as in
    /// Lua. The wait ends when the callback ends, answered stale.
    pub(super) fn oauth_callback(
        &self,
        context: &ProcessContext,
        callback: &CallbackContext,
        port: u16,
        path: Option<&str>,
        cancel: &dyn Cancel,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let _ = context;
        if !self.browser.attended() {
            return Err((
                contract::ErrorCode::AuthenticationFailed,
                "host.oauth.callback needs a person to log in, and nobody is attached".to_owned(),
            ));
        }
        let (answered, answer) = std::sync::mpsc::channel();
        let deliver: crate::oauth::Deliver = Arc::new(move |reply| match answered.send(reply) {
            Ok(()) | Err(_) => {}
        });
        let _serve = crate::oauth::listen(
            port,
            path.map(str::to_owned),
            &deliver,
            self.browser.as_ref(),
        )
        .map_err(|reply| match reply {
            crate::host::Reply::Query(Err(failure)) => failure,
            crate::host::Reply::Query(Ok(_))
            | crate::host::Reply::Http(_)
            | crate::host::Reply::Ask(_)
            | crate::host::Reply::Drive(_)
            | crate::host::Reply::Exec(_)
            | crate::host::Reply::Tool(_)
            | crate::host::Reply::Model(_)
            | crate::host::Reply::Delegate(_)
            | crate::host::Reply::Lock(_)
            | crate::host::Reply::Slept => (
                contract::ErrorCode::IoFailed,
                format!("host.oauth.callback: port {port}: the listener failed"),
            ),
        })?;
        loop {
            if cancel.is_cancelled() || self.ended(callback) {
                return Err((
                    contract::ErrorCode::Closing,
                    "host.oauth.callback: the callback ended".to_owned(),
                ));
            }
            match answer.try_recv() {
                Ok(crate::host::Reply::Query(Ok(pairs))) => {
                    let mut query = Map::new();
                    for (name, value) in pairs {
                        query.insert(name, Value::String(value));
                    }
                    return Ok(Some(Value::Object(query)));
                }
                Ok(crate::host::Reply::Query(Err(failure))) => return Err(failure),
                Ok(_) => {
                    return Err((
                        contract::ErrorCode::IoFailed,
                        format!("host.oauth.callback: port {port}: the listener failed"),
                    ));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => self.rest(),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return Err((
                        contract::ErrorCode::IoFailed,
                        format!("host.oauth.callback: port {port}: the listener failed"),
                    ));
                }
            }
        }
    }

    /// Runs `host.oauth.open`: shows the URL in a browser.
    pub(super) fn oauth_open(
        &self,
        url: &str,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        if !self.browser.attended() {
            return Err((
                contract::ErrorCode::AuthenticationFailed,
                "host.oauth.open needs a person to log in, and nobody is attached".to_owned(),
            ));
        }
        self.browser.open(url);
        Ok(None)
    }

    /// Runs `host.oauth.show`: shows the device-code to enter at the URL.
    pub(super) fn oauth_show(
        &self,
        url: &str,
        code: &str,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        if !self.browser.attended() {
            return Err((
                contract::ErrorCode::AuthenticationFailed,
                "host.oauth.show needs a person to log in, and nobody is attached".to_owned(),
            ));
        }
        self.browser.show(url, code);
        Ok(None)
    }

    /// Answers `host.oauth.pkce` with a fresh challenge and verifier.
    pub(super) fn pkcs() -> Result<Option<Value>, (contract::ErrorCode, String)> {
        let (challenge, verifier) = crate::oauth::pkce_pair()?;
        let mut pair = Map::new();
        pair.insert("challenge".to_owned(), Value::String(challenge));
        pair.insert("verifier".to_owned(), Value::String(verifier));
        Ok(Some(Value::Object(pair)))
    }

    /// Runs `host.oauth.poll`: polls the URL until the flow finishes,
    /// waiting like `authorization_pending` on the pending statuses and
    /// slowing down on `slow_down`, exactly as in Lua. The wait ends
    /// when the callback ends, answered stale.
    #[allow(clippy::too_many_arguments, reason = "one decoded call's fields")]
    pub(super) fn oauth_poll(
        &self,
        callback: &CallbackContext,
        url: &str,
        method: &str,
        headers: &[(String, String)],
        body: Option<Vec<u8>>,
        pending: Option<&[u16]>,
        interval: u64,
        cancel: &dyn Cancel,
    ) -> Result<Option<Value>, (contract::ErrorCode, String)> {
        const GRACE: Duration = Duration::from_secs(1);
        let timeout = callback
            .deadline
            .map(|end| end.saturating_duration_since(self.clock.now()) + GRACE);
        let mut wait = interval;
        loop {
            if cancel.is_cancelled() || self.ended(callback) {
                return Err((
                    contract::ErrorCode::Closing,
                    "host.oauth.poll: the callback ended".to_owned(),
                ));
            }
            if !self.browser.attended() {
                return Err((
                    contract::ErrorCode::AuthenticationFailed,
                    "host.oauth.poll needs a person to log in, and nobody is attached".to_owned(),
                ));
            }
            let (status, bytes) = crate::host::perform(&crate::host::HttpRequest {
                method: method.to_owned(),
                url: url.to_owned(),
                headers: headers.to_owned(),
                body: body.clone(),
                timeout,
            })?;
            let waiting = pending.is_some_and(|pending| pending.contains(&status));
            if !waiting {
                // Only a `{` after any space opens the object the ruling
                // requires: a JSON array is not one.
                let text = String::from_utf8_lossy(&bytes);
                let body = text
                    .trim_start()
                    .strip_prefix('{')
                    .and_then(|_| serde_json::from_str::<Value>(&text).ok())
                    .filter(Value::is_object);
                let Some(body) = body else {
                    return Err((
                        contract::ErrorCode::UnreadableReply,
                        format!(
                            "host.oauth.poll: status {status} with a body that is not a JSON object"
                        ),
                    ));
                };
                let code = body.get("error").and_then(Value::as_str);
                match code {
                    None => {
                        if !(200..=299).contains(&status) {
                            return Err((
                                contract::ErrorCode::HttpError,
                                format!("host.oauth.poll: status {status}"),
                            ));
                        }
                        return Ok(Some(body));
                    }
                    Some("slow_down") => {
                        wait += 5;
                        if wait > 3600 {
                            return Err((
                                contract::ErrorCode::RateLimited,
                                "host.oauth.poll: the server asked for more than 3600 seconds between polls"
                                    .to_owned(),
                            ));
                        }
                    }
                    Some("authorization_pending") => {}
                    Some(code) => {
                        let detail = body
                            .get("error_description")
                            .and_then(Value::as_str)
                            .map(|detail| format!(": {detail}"))
                            .unwrap_or_default();
                        return Err((
                            contract::ErrorCode::AuthenticationFailed,
                            format!("host.oauth.poll: {code}{detail}"),
                        ));
                    }
                }
            }
            self.rest_for(Duration::from_secs(wait), callback, cancel);
        }
    }

    /// Whether the callback ended: its deadline passed. Cancellation is
    /// the caller's own watch; a passed deadline ends every wait here.
    pub(super) fn ended(&self, callback: &CallbackContext) -> bool {
        callback.deadline.is_some_and(|end| self.clock.now() >= end)
    }

    /// Rests one slice on the injected clock, re-reading the wait after.
    pub(super) fn rest(&self) {
        let flag = std::sync::Mutex::new(());
        let changed = std::sync::Condvar::new();
        let guard = support::lock(&flag);
        let _ = support::clock::park(
            self.clock.as_ref(),
            None,
            Some(WAIT_POLL),
            &changed,
            guard,
            |()| false,
        );
    }

    /// Rests `wait` on the injected clock in slices, re-reading after
    /// each, so a fake clock the test moves ends the rest early.
    pub(super) fn rest_for(&self, wait: Duration, callback: &CallbackContext, cancel: &dyn Cancel) {
        let start = self.clock.now();
        loop {
            if cancel.is_cancelled() || self.ended(callback) {
                return;
            }
            if self.clock.now().duration_since(start) >= wait {
                return;
            }
            self.rest();
        }
    }
}
