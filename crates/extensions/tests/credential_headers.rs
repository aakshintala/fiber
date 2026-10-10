//! A `credential()` that returns `headers` (`docs/extensions.md`, "What
//! writing a provider looks like"; `docs/model-routing.md`, "Keys, tokens
//! and OAuth"): the headers ride every signed request after `authorization`,
//! are cached and refreshed with the token, and a header that names one
//! Fiber builds itself fails the call with `credential_failed`. Running-call
//! values stay reported until completion, completed history is bounded to
//! sixteen distinct values, and `sign()` errors redact their own values before
//! returning (`docs/errors.md`, "The shape").

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]
#![allow(
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod common;

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use common::{Setup, header, pair, script_provider, sign_with, write};
use contract::ErrorCode;
use contract::signing::{SignRequest, Signer};
use extensions::{LuaExtension, LuaProvider, REFRESH_BEFORE};
use fakes::Deadline;
use fakes::clock::FakeClock;
use fakes::{ProviderServer, Response};
use serde_json::json;

/// How long a test waits for one call, or for a background refresh: the
/// wall-clock limit on every wait, so a mutant cannot hang it
/// (`docs/testing.md`, "Waits and timeouts").
const WAIT: Duration = Duration::from_secs(5);
const CALL_DEADLINE: Duration = Duration::from_secs(5);

/// The fake clock's wall at construction: 2023-11-14T22:13:20Z.
const WALL: u64 = 1_700_000_000;

/// Runs `f` on its own thread under `WAIT`, so a call that never returns
/// fails the test instead of hanging it.
#[track_caller]
fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(f()));
    match Deadline::after(WAIT).recv(&rx) {
        Ok(answer) => answer,
        Err(_) => panic!("the call did not return within {WAIT:?}"),
    }
}

fn provider_on(setup: &Setup, credential_run: &str, sign_run: &str) -> Arc<LuaProvider> {
    script_provider(
        setup,
        FakeClock::new(),
        Some(credential_run),
        Some(sign_run),
    )
}

#[track_caller]
fn signer_of(provider: &Arc<LuaProvider>) -> Arc<dyn Signer> {
    within({
        let provider = Arc::clone(provider);
        move || {
            provider
                .signer(pair(provider.name(), "default"))
                .unwrap()
                .unwrap()
        }
    })
}

/// A row of the clash table: a header `credential()` returns, the headers
/// the request already carries, and whether the sign succeeds.
type ClashCase<'a> = (&'a str, &'a [(&'a str, &'a str)], bool);

/// A row of the shape table: what `credential()` returns, and the headers
/// the sign sends besides `authorization`, or `None` when it must fail.
type ShapeCase<'a> = (&'a str, Option<&'a [(&'a str, &'a str)]>);

#[test]
fn headers_ride_every_signed_request_after_authorization() {
    let setup = Setup::new();
    let provider = provider_on(
        &setup,
        "{ token = \"tok-1\", expires_at = 4102444800, \
         headers = { [\"chatgpt-account-id\"] = \"acct-1\" }, \
         email = \"someone@example.com\" }",
        "{ [\"x-saw-auth\"] = request.headers.authorization or \"missing\", \
         [\"x-saw-acct\"] = request.headers[\"chatgpt-account-id\"] or \"missing\" }",
    );
    let signer = signer_of(&provider);
    let headers = sign_with(
        &signer,
        &[("content-type".to_owned(), "application/json".to_owned())],
    )
    .unwrap();
    // The token first, then the credential headers, then what `sign()`
    // returned. The login's `email` rides nowhere: any other call ignores it.
    assert_eq!(
        headers[0],
        ("authorization".to_owned(), "Bearer tok-1".to_owned())
    );
    assert_eq!(
        headers[1],
        ("chatgpt-account-id".to_owned(), "acct-1".to_owned())
    );
    let mut rest = headers[2..].to_vec();
    rest.sort();
    assert_eq!(
        rest,
        [
            ("x-saw-acct".to_owned(), "acct-1".to_owned()),
            ("x-saw-auth".to_owned(), "Bearer tok-1".to_owned()),
        ]
    );
}

#[test]
fn sign_may_return_an_http_header_named_headers() {
    let setup = Setup::new();
    let provider = provider_on(
        &setup,
        "{ token = \"tok-1\", expires_at = 4102444800 }",
        "{ headers = \"sign-header\" }",
    );
    let signer = signer_of(&provider);
    assert_eq!(
        header(&sign_with(&signer, &[]).unwrap(), "headers"),
        Some("sign-header".to_owned())
    );
}

#[test]
fn headers_are_cached_with_the_token() {
    let setup = Setup::new();
    // `calls` counts the `credential()` runs: `sign()` reports it, so two
    // signs with the same count prove one cached token and its headers.
    let provider = provider_on(
        &setup,
        "(function() calls = (calls or 0) + 1 return { token = \"tok-1\", \
         expires_at = 4102444800, \
         headers = { [\"chatgpt-account-id\"] = \"acct-\" .. calls } } end)()",
        "{ [\"x-calls\"] = tostring(calls) }",
    );
    let signer = signer_of(&provider);
    for _ in 0..2 {
        let headers = sign_with(&signer, &[]).unwrap();
        assert_eq!(
            header(&headers, "chatgpt-account-id"),
            Some("acct-1".to_owned())
        );
        assert_eq!(header(&headers, "x-calls"), Some("1".to_owned()));
    }
}

#[test]
fn a_refresh_replaces_the_token_and_its_headers_together() {
    assert_eq!(REFRESH_BEFORE, Duration::from_secs(300));
    let setup = Setup::new();
    let clock = FakeClock::new();
    let provider = script_provider(
        &setup,
        clock.clone(),
        Some(
            "(function() calls = (calls or 0) + 1 \
             if calls == 1 then return { token = \"tok-1\", expires_at = 1700003600, \
             headers = { [\"chatgpt-account-id\"] = \"acct-1\" } } end \
             return { token = \"tok-2\", expires_at = 4102444800, \
             headers = { [\"chatgpt-account-id\"] = \"acct-2\" } } end)()",
        ),
        Some("{}"),
    );
    let signer = signer_of(&provider);
    let first = sign_with(&signer, &[]).unwrap();
    assert_eq!(
        header(&first, "authorization"),
        Some("Bearer tok-1".to_owned())
    );
    assert_eq!(
        header(&first, "chatgpt-account-id"),
        Some("acct-1".to_owned())
    );
    // Into the refresh window, still short of expiry.
    clock.advance(
        Duration::from_secs(3600)
            .checked_sub(REFRESH_BEFORE)
            .unwrap(),
    );
    // Every sign until the refresh lands is one consistent pair or the
    // other, never a new token with old headers.
    let (done, refreshed) = mpsc::channel();
    let polling = Arc::clone(&signer);
    std::thread::spawn(move || {
        for _ in 0..100_000 {
            let headers = polling
                .sign(&SignRequest {
                    method: "POST",
                    url: "http://127.0.0.1:1/v1/responses",
                    headers: &[],
                    body: b"{}",
                })
                .unwrap();
            let token = header(&headers, "authorization");
            let account = header(&headers, "chatgpt-account-id");
            assert!(
                (token.clone(), account.clone())
                    == (Some("Bearer tok-1".to_owned()), Some("acct-1".to_owned()))
                    || (token, account)
                        == (Some("Bearer tok-2".to_owned()), Some("acct-2".to_owned())),
                "a request never pairs a token with another token's headers"
            );
            if header(&headers, "authorization") == Some("Bearer tok-2".to_owned()) {
                match done.send(()) {
                    Ok(()) | Err(mpsc::SendError(())) => {}
                }
                return;
            }
            std::thread::yield_now();
        }
    });
    Deadline::after(WAIT)
        .recv(&refreshed)
        .unwrap_or_else(|_| panic!("the refresh did not land within {WAIT:?}"));
}

#[test]
fn a_clashing_credential_header_is_credential_failed() {
    // Each row: a header `credential()` returns, the headers the request
    // already carries, and whether the sign succeeds. A clash compares
    // ASCII case-insensitively, against `authorization` and against every
    // header already on the request.
    let cases: &[ClashCase<'_>] = &[
        ("authorization", &[], false),
        ("Authorization", &[], false),
        ("AUTHORIZATION", &[], false),
        (
            "content-type",
            &[("content-type", "application/json")],
            false,
        ),
        (
            "Content-Type",
            &[("content-type", "application/json")],
            false,
        ),
        ("x-provider", &[("X-Provider", "p")], false),
        ("x-fresh", &[("content-type", "application/json")], true),
        ("x-fresh", &[], true),
    ];
    for (name, sent, ok) in cases {
        let setup = Setup::new();
        let provider = provider_on(
            &setup,
            &format!(
                "{{ token = \"tok-1\", expires_at = 4102444800, \
                 headers = {{ [\"{name}\"] = \"s3cr3t-v\" }} }}"
            ),
            "{}",
        );
        let signer = signer_of(&provider);
        let owned: Vec<(String, String)> = sent
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect();
        let signed = sign_with(&signer, &owned);
        if *ok {
            let headers = signed.unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(header(&headers, name), Some("s3cr3t-v".to_owned()));
        } else {
            let Err(contract::signing::Error::Credential { code, message }) = signed else {
                panic!("{name}: a clashing header must fail the sign");
            };
            assert_eq!(code, ErrorCode::CredentialFailed, "{name}");
            assert!(message.contains(name), "{name}: {message}");
            assert!(
                !message.contains("s3cr3t-v"),
                "{name}: the message names the header, never its value: {message}"
            );
        }
    }
}

#[test]
fn credential_header_shapes() {
    // Each row: what `credential()` returns, and the headers the sign
    // sends besides `authorization`, or that the sign fails naming
    // `headers`. An empty table, and an empty table encoded as `[]`, mean
    // no headers, as for `sign()`.
    let cases: &[ShapeCase<'_>] = &[
        ("{ token = \"tok-1\", expires_at = 4102444800 }", Some(&[])),
        (
            "{ token = \"tok-1\", expires_at = 4102444800, headers = {} }",
            Some(&[]),
        ),
        (
            "{ token = \"tok-1\", expires_at = 4102444800, \
             headers = json.decode(\"[]\") }",
            Some(&[]),
        ),
        // A null carried over from a host call is no headers, as for `sign()`.
        (
            "{ token = \"tok-1\", expires_at = 4102444800, \
             headers = json.decode(\"null\") }",
            Some(&[]),
        ),
        (
            "{ token = \"tok-1\", expires_at = 4102444800, headers = { a = \"v\" } }",
            Some(&[("a", "v")]),
        ),
        (
            "{ token = \"tok-1\", expires_at = 4102444800, \
             headers = { a = \"v\" }, email = \"someone@example.com\" }",
            Some(&[("a", "v")]),
        ),
        (
            "{ token = \"tok-1\", expires_at = 4102444800, headers = \"x\" }",
            None,
        ),
        (
            "{ token = \"tok-1\", expires_at = 4102444800, headers = 5 }",
            None,
        ),
        (
            "{ token = \"tok-1\", expires_at = 4102444800, headers = true }",
            None,
        ),
        (
            "{ token = \"tok-1\", expires_at = 4102444800, headers = { \"v\" } }",
            None,
        ),
        (
            "{ token = \"tok-1\", expires_at = 4102444800, headers = { a = 1 } }",
            None,
        ),
        (
            "{ token = \"tok-1\", expires_at = 4102444800, headers = { [42] = \"v\" } }",
            None,
        ),
        (
            "{ token = \"tok-1\", expires_at = 4102444800, headers = { [1] = \"v\" } }",
            None,
        ),
    ];
    for (returned, expected) in cases {
        let setup = Setup::new();
        let provider = provider_on(&setup, returned, "{}");
        let signer = signer_of(&provider);
        match (sign_with(&signer, &[]), expected) {
            (Ok(headers), Some(want)) => {
                let mut rest: Vec<(String, String)> = headers
                    .into_iter()
                    .filter(|(name, _)| !name.eq_ignore_ascii_case("authorization"))
                    .collect();
                rest.sort();
                let mut want: Vec<(String, String)> = want
                    .iter()
                    .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                    .collect();
                want.sort();
                assert_eq!(rest, want, "{returned}");
            }
            (Err(e), None) => {
                let contract::signing::Error::Credential { code, message } = e else {
                    panic!("{returned}: a bad `headers` shape must fail the sign: {e:?}");
                };
                assert_eq!(code, ErrorCode::CredentialFailed, "{returned}");
                assert!(message.contains("headers"), "{returned}: {message}");
            }
            (Ok(headers), None) => panic!("{returned}: a bad shape must fail: {headers:?}"),
            (Err(e), Some(_)) => panic!("{returned}: a good shape must sign: {e:?}"),
        }
    }
}

#[test]
fn sign_still_overrides_authorization_with_headers_present() {
    let setup = Setup::new();
    let provider = provider_on(
        &setup,
        "{ token = \"tok-1\", expires_at = 4102444800, \
         headers = { [\"chatgpt-account-id\"] = \"acct-1\" } }",
        "{ Authorization = \"Bearer custom\" }",
    );
    let signer = signer_of(&provider);
    assert_eq!(
        sign_with(&signer, &[]).unwrap(),
        [
            ("chatgpt-account-id".to_owned(), "acct-1".to_owned()),
            ("Authorization".to_owned(), "Bearer custom".to_owned()),
        ]
    );
    // The replaced token is still reported, with the header values.
    let mut reported: Vec<String> = signer
        .credentials()
        .iter()
        .map(|secret| secret.expose().to_owned())
        .collect();
    reported.sort();
    assert_eq!(reported, ["acct-1", "tok-1"]);
}

#[test]
fn a_completed_call_with_one_token_and_sixteen_headers_keeps_only_sixteen_history_values() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let blocking = ProviderServer::start([Response::status(200, "{}")]).unwrap();
    blocking.hold();
    let block_url = blocking.url();
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "calls_c = 0\n\
             fiber.provider(\"p\", {{\n\
             credential = {{ timeout = 60000, run = function()\n\
             calls_c = calls_c + 1\n\
             if calls_c == 1 then\n\
             return {{ token = \"tok-unused\", expires_at = 1700000301 }}\n\
             end\n\
             if calls_c == 2 then\n\
             local headers = {{}}\n\
             for i = 1, 16 do headers[\"x-\" .. i] = \"hv-\" .. i end\n\
             return {{ token = \"tok-old\", expires_at = 1700000602, headers = headers }}\n\
             end\n\
             return {{ token = \"tok-current\", expires_at = 4102444800 }}\n\
             end }},\n\
             sign = {{ timeout = 36000000, run = function(request)\n\
             if request.headers.authorization == \"Bearer tok-old\" then\n\
             host.http({{ url = \"{block_url}/s\", method = \"POST\" }})\n\
             end\n\
             return {{}}\n\
             end }},\n\
             }})\n"
        ),
    );
    let extension = Arc::new(LuaExtension::new("ext", dir, setup.home(), clock.clone()));
    let provider = LuaProvider::new(extension, "p");
    let signer = signer_of(&provider);
    // A token fetched and replaced before any sign leaves no trace.
    let unused = within({
        let provider = Arc::clone(&provider);
        move || provider.token(&pair(provider.name(), "default"))
    })
    .unwrap();
    assert_eq!(unused.expose(), "tok-unused");
    clock.advance(Duration::from_secs(301));
    let (done, first) = mpsc::channel();
    let held = Arc::clone(&signer);
    std::thread::spawn(move || {
        let url = "http://127.0.0.1:1/v1/responses";
        done.send(held.sign(&SignRequest {
            method: "POST",
            url,
            headers: &[],
            body: b"{}",
        }))
        .unwrap_or(());
    });
    assert!(
        blocking.await_requests(1, CALL_DEADLINE),
        "the call with seventeen values reaches its held sign request"
    );

    clock.advance(Duration::from_secs(301));
    let second = sign_with(&signer, &[]).unwrap();
    assert_eq!(
        header(&second, "authorization"),
        Some("Bearer tok-current".to_owned())
    );
    blocking.release();
    Deadline::after(CALL_DEADLINE)
        .recv(&first)
        .unwrap_or_else(|_| panic!("the held call returns within {CALL_DEADLINE:?}"))
        .unwrap();

    let mut reported: Vec<String> = signer
        .credentials()
        .iter()
        .map(|secret| secret.expose().to_owned())
        .collect();
    reported.sort();
    let mut expected: Vec<String> = (1..=16).map(|n| format!("hv-{n}")).collect();
    expected.push("tok-current".to_owned());
    expected.sort();
    assert_eq!(
        reported, expected,
        "sixteen completed values plus the current cache"
    );
    assert!(!reported.contains(&"tok-old".to_owned()));
    assert!(!reported.contains(&"tok-unused".to_owned()));
}

#[test]
fn a_running_call_values_are_reported_until_it_finishes() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let blocking = ProviderServer::start([Response::status(200, "{}")]).unwrap();
    blocking.hold();
    let block_url = blocking.url();
    // The first `sign()` blocks on the held server; later ones return at
    // once. Every fetch returns a new token and header value with an expiry
    // 301 seconds past its own wall: past the refresh window, so no
    // background refresh steals a fetch, and each advance of 301 seconds
    // expires it, so the next sign on the test thread fetches synchronously.
    // The advances total far less than the first `sign()`'s own timeout, so
    // it stays parked while they happen (`docs/testing.md`, "Waits and
    // timeouts").
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "calls_c = 0\n\
             calls_s = 0\n\
             fiber.provider(\"p\", {{\n\
             credential = {{ timeout = 60000, run = function()\n\
             calls_c = calls_c + 1\n\
             return {{ token = \"tok-\" .. calls_c, \
             expires_at = 1700000000 + (calls_c - 1) * 301 + 301, \
             headers = {{ [\"x-h\"] = \"hv-\" .. calls_c }} }}\n\
             end }},\n\
             sign = {{ timeout = 36000000, run = function(request)\n\
             calls_s = calls_s + 1\n\
             if calls_s == 1 then host.http({{ url = \"{block_url}/s\", method = \"POST\" }}) end\n\
             return {{}}\n\
             end }},\n\
             }})\n"
        ),
    );
    let extension = Arc::new(LuaExtension::new("ext", dir, setup.home(), clock.clone()));
    let provider = LuaProvider::new(extension, "p");
    let signer = signer_of(&provider);
    let (done, first) = mpsc::channel();
    let held = Arc::clone(&signer);
    std::thread::spawn(move || {
        let url = "http://127.0.0.1:1/v1/responses".to_owned();
        let result = held.sign(&SignRequest {
            method: "POST",
            url: &url,
            headers: &[],
            body: b"{}",
        });
        done.send(result).unwrap_or(());
    });
    assert!(
        blocking.await_requests(1, WAIT),
        "waited for the first sign to block on the held server"
    );
    // Seventeen synchronous refreshes while the first call runs: its two
    // values are never dropped, whatever newer values arrive.
    for round in 1..=17_u32 {
        clock.advance(Duration::from_secs(301));
        let headers = sign_with(&signer, &[]).unwrap();
        assert_eq!(
            header(&headers, "authorization"),
            Some(format!("Bearer tok-{}", round + 1)),
            "round {round}"
        );
    }
    let mut values: Vec<String> = signer
        .credentials()
        .iter()
        .map(|secret| secret.expose().to_owned())
        .collect();
    values.sort();
    assert!(
        values.contains(&"tok-1".to_owned()) && values.contains(&"hv-1".to_owned()),
        "a running call's values survive the bound: {values:?}"
    );
    assert!(
        !values.contains(&"tok-2".to_owned()),
        "the oldest idle values are still dropped past sixteen: {values:?}"
    );
    blocking.release();
    let headers = Deadline::after(WAIT)
        .recv(&first)
        .unwrap_or_else(|_| panic!("the held sign did not return within {WAIT:?}"))
        .unwrap();
    assert_eq!(
        header(&headers, "authorization"),
        Some("Bearer tok-1".to_owned())
    );
    assert_eq!(header(&headers, "x-h"), Some("hv-1".to_owned()));
    // Call 1 completes last, so its values are among the most recently
    // completed values. They are retained by recency, not pinned past the
    // call (`docs/errors.md`, "The shape").
    let mut values: Vec<String> = signer
        .credentials()
        .iter()
        .map(|secret| secret.expose().to_owned())
        .collect();
    values.sort();
    assert_eq!(values.len(), 16, "bounded once quiescent: {values:?}");
    assert!(
        values.contains(&"tok-1".to_owned()) && values.contains(&"hv-1".to_owned()),
        "the returned call stays listed: {values:?}"
    );
    assert!(
        !values.contains(&"tok-2".to_owned()) && !values.contains(&"hv-2".to_owned()),
        "the oldest idle values are dropped: {values:?}"
    );
}

#[test]
fn a_sign_error_is_redacted_before_the_call_returns_even_after_history_eviction() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let first_call = ProviderServer::start([Response::status(200, "{}")]).unwrap();
    first_call.hold();
    let later_calls = ProviderServer::start([Response::status(200, "{}")]).unwrap();
    later_calls.hold();
    let (first_url, later_url) = (first_call.url(), later_calls.url());
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "calls_c = 0\n\
             fiber.provider(\"p\", {{\n\
             credential = {{ timeout = 60000, run = function()\n\
             calls_c = calls_c + 1\n\
             return {{ token = \"tok-\" .. calls_c, expires_at = 1700000000 + (calls_c - 1) * 301 + 301 }}\n\
             end }},\n\
             sign = {{ timeout = 36000000, run = function(request)\n\
             local authorization = request.headers.authorization or \"missing\"\n\
             if authorization == \"Bearer tok-1\" then\n\
             host.http({{ url = \"{first_url}/s\", method = \"POST\" }})\n\
             error(\"saw \" .. authorization)\n\
             end\n\
             host.http({{ url = \"{later_url}/s\", method = \"POST\" }})\n\
             return {{}}\n\
             end }},\n\
             }})\n"
        ),
    );
    let extension = Arc::new(LuaExtension::new("ext", dir, setup.home(), clock.clone()));
    let provider = LuaProvider::new(extension, "p");
    let signer = signer_of(&provider);
    let (done, results) = mpsc::channel();

    let launch = |call: usize, signer: Arc<dyn Signer>, done: mpsc::Sender<_>| {
        std::thread::spawn(move || {
            let url = "http://127.0.0.1:1/v1/responses";
            let result = signer.sign(&SignRequest {
                method: "POST",
                url,
                headers: &[],
                body: b"{}",
            });
            done.send((call, result)).unwrap_or(());
        });
    };

    // Each held request is a barrier: no call returns until the test releases
    // its server, and each bounded wait proves the call reached that point.
    launch(1, Arc::clone(&signer), done.clone());
    assert!(
        first_call.await_requests(1, CALL_DEADLINE),
        "call 1 reached its held sign request"
    );
    for call in 2..=17 {
        clock.advance(Duration::from_secs(301));
        launch(call, Arc::clone(&signer), done.clone());
        assert!(
            later_calls.await_requests(call - 1, CALL_DEADLINE),
            "call {call} reached its held sign request"
        );
    }

    let mut in_flight: Vec<String> = signer
        .credentials()
        .iter()
        .map(|secret| secret.expose().to_owned())
        .filter(|value| value.starts_with("tok-"))
        .collect();
    in_flight.sort();
    let mut expected: Vec<String> = (1..=17).map(|call| format!("tok-{call}")).collect();
    expected.sort();
    assert_eq!(in_flight, expected, "all running calls keep their token");

    first_call.release();
    let (call, result) = Deadline::after(CALL_DEADLINE)
        .recv(&results)
        .unwrap_or_else(|_| panic!("call 1 returns within {CALL_DEADLINE:?}"));
    assert_eq!(call, 1);
    let Err(contract::signing::Error::Failed(message)) = result else {
        panic!("call 1 fails after naming the token it used");
    };

    // Hold call 1's already-returned error while the other calls complete.
    later_calls.release();
    // One deadline for the whole wait: the loop drains one stream.
    let wait = Deadline::after(CALL_DEADLINE);
    for _ in 2..=17 {
        let (_, result) = wait
            .recv(&results)
            .unwrap_or_else(|_| panic!("the remaining calls return within {CALL_DEADLINE:?}"));
        result.unwrap();
    }
    assert!(
        message.contains("[redacted]") && !message.contains("tok-1"),
        "the returned error is already redacted: {message}"
    );
    let mut completed: Vec<String> = signer
        .credentials()
        .iter()
        .map(|secret| secret.expose().to_owned())
        .filter(|value| value.starts_with("tok-"))
        .collect();
    completed.sort();
    assert_eq!(
        completed.len(),
        16,
        "completed history is bounded: {completed:?}"
    );
    assert!(
        !completed.contains(&"tok-1".to_owned()),
        "call 1 was evicted: {completed:?}"
    );
}

#[test]
fn a_sign_error_is_redacted_after_a_credential_refresh() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let tokens = ProviderServer::start([
        Response::status(
            200,
            json!({ "token": "tok-A-value", "expires_at": WALL + 200 }).to_string(),
        ),
        Response::status(
            200,
            json!({ "token": "tok-B-value", "expires_at": 4102444800_u64 }).to_string(),
        ),
    ])
    .unwrap();
    let blocking = ProviderServer::start([Response::status(200, "{}")]).unwrap();
    blocking.hold();
    let (token_url, block_url) = (tokens.url(), blocking.url());
    // `credential()` reads the token over HTTP, so the token fake's request
    // count tells when the background refresh has returned the new token.
    // The first `sign()` blocks on the held server, then fails naming the
    // token it was handed; later ones return at once.
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "calls_s = 0\n\
             fiber.provider(\"p\", {{\n\
             credential = {{ timeout = 60000, run = function()\n\
             local reply = host.http({{ url = \"{token_url}/token\", method = \"POST\" }})\n\
             local got = json.decode(reply.body)\n\
             return {{ token = got.token, expires_at = got.expires_at, \
             headers = {{ [\"chatgpt-account-id\"] = \"acct_secret\" }} }}\n\
             end }},\n\
             sign = {{ timeout = 60000, run = function(request)\n\
             calls_s = calls_s + 1\n\
             if calls_s == 1 then\n\
             host.http({{ url = \"{block_url}/s\", method = \"POST\" }})\n\
             error(\"saw \" .. (request.headers.authorization or \"missing\"))\n\
             end\n\
             return {{}}\n\
             end }},\n\
             }})\n"
        ),
    );
    let extension = Arc::new(LuaExtension::new("ext", dir, setup.home(), clock));
    let provider = LuaProvider::new(extension, "p");
    let signer = signer_of(&provider);
    // Call 1 fetches the token within the refresh window and blocks in
    // `sign()`.
    let (done, first) = mpsc::channel();
    let held = Arc::clone(&signer);
    std::thread::spawn(move || {
        let url = "http://127.0.0.1:1/v1/responses".to_owned();
        done.send(held.sign(&SignRequest {
            method: "POST",
            url: &url,
            headers: &[],
            body: b"{}",
        }))
        .unwrap_or(());
    });
    assert!(
        blocking.await_requests(1, WAIT),
        "waited for the first sign to block on the held server"
    );
    // Call 2 signs with the same token and starts the background refresh.
    let second = sign_with(&signer, &[]).unwrap();
    assert_eq!(
        header(&second, "authorization"),
        Some("Bearer tok-A-value".to_owned())
    );
    assert!(
        tokens.await_requests(2, WAIT),
        "waited for the background refresh to return the new token"
    );
    // The refresh replaced the cache: a new sign carries the new token.
    let (refreshed, landed) = mpsc::channel();
    let polling = Arc::clone(&provider);
    std::thread::spawn(move || {
        for _ in 0..100_000 {
            match polling.token(&pair(polling.name(), "default")) {
                Ok(secret) if secret.expose() == "tok-B-value" => {
                    match refreshed.send(()) {
                        Ok(()) | Err(mpsc::SendError(())) => {}
                    }
                    return;
                }
                _ => std::thread::yield_now(),
            }
        }
    });
    Deadline::after(WAIT)
        .recv(&landed)
        .unwrap_or_else(|_| panic!("the refreshed token did not land within {WAIT:?}"));
    blocking.release();
    let Err(contract::signing::Error::Failed(message)) = Deadline::after(WAIT)
        .recv(&first)
        .unwrap_or_else(|_| panic!("the held sign did not return within {WAIT:?}"))
    else {
        panic!("the first sign fails naming the token it used");
    };
    assert!(
        message.contains("[redacted]")
            && !message.contains("tok-A-value")
            && !message.contains("tok-B-value"),
        "the error is redacted before it leaves sign: {message}"
    );
}

#[test]
fn a_sign_error_naming_a_credential_header_value_is_redacted_before_return() {
    let setup = Setup::new();
    let provider = provider_on(
        &setup,
        "{ token = \"tok-1\", expires_at = 4102444800, \
         headers = { [\"chatgpt-account-id\"] = \"acct_secret\" } }",
        "error(\"saw \" .. request.headers.authorization .. \" and \" .. request.headers[\"chatgpt-account-id\"])",
    );
    let signer = signer_of(&provider);
    let Err(contract::signing::Error::Failed(message)) = sign_with(&signer, &[]) else {
        panic!("the sign fails naming the header it saw");
    };
    assert!(
        message.contains("[redacted]")
            && !message.contains("acct_secret")
            && !message.contains("tok-1"),
        "all credential values are redacted before return: {message}"
    );
}

#[test]
fn concurrent_calls_keep_in_flight_values_then_bound_completed_history() {
    // Each row gives the number of distinct running values and the number
    // retained once the calls complete.
    for (calls, want) in [(15_usize, 15_usize), (16, 16), (17, 16), (20, 16)] {
        let setup = Setup::new();
        let clock = FakeClock::new();
        let blocking = ProviderServer::start([Response::status(200, "{}")]).unwrap();
        blocking.hold();
        let last = ProviderServer::start([Response::status(200, "{}")]).unwrap();
        last.hold();
        let (block_url, last_url) = (blocking.url(), last.url());
        // An expiry 301 seconds past each fetch's own wall stays past the
        // refresh window, so no background refresh steals a fetch, and each
        // advance of 301 seconds expires it, so the next sign fetches
        // synchronously. The advances total far less than each sign's own
        // timeout, so every call stays parked while they happen
        // (`docs/testing.md`, "Waits and timeouts").
        let dir = setup.home().join("ext");
        write(
            &dir.join("init.lua"),
            &format!(
                "calls_c = 0\n\
                 fiber.provider(\"p\", {{\n\
                 credential = {{ timeout = 60000, run = function()\n\
                 calls_c = calls_c + 1\n\
                 return {{ token = \"tok-\" .. calls_c, \
                 expires_at = 1700000000 + (calls_c - 1) * 301 + 301 }}\n\
                 end }},\n\
                 sign = {{ timeout = 36000000, run = function(request)\n\
                 host.http({{ url = (request.headers.authorization == \"Bearer tok-{calls}\" and \"{last_url}\" or \"{block_url}\") .. \"/s\", method = \"POST\" }})\n\
                 return {{}}\n\
                 end }},\n\
                 }})\n"
            ),
        );
        let extension = Arc::new(LuaExtension::new("ext", dir, setup.home(), clock.clone()));
        let provider = LuaProvider::new(extension, "p");
        let signer = signer_of(&provider);
        let (done, finished) = mpsc::channel();
        let mut threads = Vec::with_capacity(calls);
        for round in 1..=calls {
            // The advance comes before the spawn: a clock move between a
            // thread's spawn and its call's start would end that call at
            // its deadline before it runs (`docs/testing.md`, "Waits and
            // timeouts").
            if round > 1 {
                clock.advance(Duration::from_secs(301));
            }
            let signing = Arc::clone(&signer);
            let completed = done.clone();
            threads.push(std::thread::spawn(move || {
                let url = "http://127.0.0.1:1/v1/responses".to_owned();
                completed
                    .send((
                        round,
                        signing.sign(&SignRequest {
                            method: "POST",
                            url: &url,
                            headers: &[],
                            body: b"{}",
                        }),
                    ))
                    .unwrap_or(());
            }));
            // The new call fetches its own token and blocks before the next
            // round, so every call is in flight at once.
            if round == calls {
                assert!(
                    blocking.await_requests(calls - 1, CALL_DEADLINE),
                    "calls {calls}: earlier signs reach their held requests"
                );
                assert!(
                    last.await_requests(1, CALL_DEADLINE),
                    "calls {calls}: latest cached token reaches its held request"
                );
            } else {
                assert!(
                    blocking.await_requests(round, CALL_DEADLINE),
                    "calls {calls}: sign {round} reaches its held request"
                );
            }
        }
        let mut in_flight: Vec<String> = signer
            .credentials()
            .iter()
            .map(|secret| secret.expose().to_owned())
            .collect();
        in_flight.sort();
        let mut expected_active: Vec<String> = (1..=calls).map(|n| format!("tok-{n}")).collect();
        expected_active.sort();
        assert_eq!(in_flight, expected_active, "calls {calls}: running values");

        // Finish calls using older cached tokens first. The latest cached
        // token completes last and stays among the sixteen retained calls.
        blocking.release();
        // One deadline for the whole wait: the loop drains one stream.
        let wait = Deadline::after(CALL_DEADLINE);
        for _ in 1..calls {
            let (round, result) = wait.recv(&finished).unwrap_or_else(|_| {
                panic!("calls {calls}: earlier signs return within {CALL_DEADLINE:?}")
            });
            assert_ne!(round, calls, "the latest cached token stays in flight");
            result.unwrap();
        }
        last.release();
        let (round, result) = Deadline::after(CALL_DEADLINE)
            .recv(&finished)
            .unwrap_or_else(|_| {
                panic!("calls {calls}: the latest sign returns within {CALL_DEADLINE:?}")
            });
        assert_eq!(round, calls, "the latest cached token finishes last");
        result.unwrap();
        for (index, thread) in threads.into_iter().enumerate() {
            thread
                .join()
                .unwrap_or_else(|_| panic!("calls {calls}: sign thread {} exits", index + 1));
        }
        let mut values: Vec<String> = signer
            .credentials()
            .iter()
            .map(|secret| secret.expose().to_owned())
            .collect();
        values.sort();
        assert_eq!(
            values.len(),
            want,
            "calls {calls}: bounded once quiescent: {values:?}"
        );
        assert!(!values.contains(&"never-used-token".to_owned()));
        if calls == 16 {
            let mut expected: Vec<String> = (1..=16_u32).map(|n| format!("tok-{n}")).collect();
            expected.sort();
            assert_eq!(values, expected, "exactly sixteen keeps every value");
        }
    }
}

/// While one `token()` waits on a held fetch, the cached token and the
/// signer's credentials stay readable: no `credential()` call runs while
/// the token lock is held.
#[test]
fn a_held_fetch_never_holds_the_token_lock() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let tokens = ProviderServer::start([Response::status(
        200,
        json!({"token": "tok-value", "expires_at": WALL + 200}).to_string(),
    )])
    .unwrap();
    tokens.hold();
    let token_url = tokens.url();
    let provider = script_provider(
        &setup,
        Arc::clone(&clock),
        Some(&format!(
            "(function()\n\
             local reply = host.http({{ url = \"{token_url}/token\", method = \"POST\" }})\n\
             local got = json.decode(reply.body)\n\
             return {{ token = got.token, expires_at = got.expires_at }}\n\
             end)()"
        )),
        None,
    );
    let pair = pair(provider.name(), "default");
    let fetching = Arc::clone(&provider);
    let worker_pair = pair.clone();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _sent = done_tx.send(fetching.token(&worker_pair));
    });
    assert!(
        tokens.await_requests(1, WAIT),
        "the first fetch reaches the server"
    );
    // The fetch is held, yet the cache reads return at once.
    within({
        let provider = Arc::clone(&provider);
        move || provider.cached_token(&pair)
    });
    let signer = signer_of(&provider);
    within({
        let signer = Arc::clone(&signer);
        move || signer.credentials()
    });
    tokens.release();
    let token = Deadline::after(WAIT)
        .recv(&done_rx)
        .unwrap_or_else(|_| panic!("the held fetch did not return within {WAIT:?}"))
        .unwrap();
    assert_eq!(token.expose(), "tok-value");
}
