//! A session on a Lua provider warms its cache like any other
//! (`docs/prompt-cache.md`, "Warming while idle"): the provider's Lua never
//! builds the request body, so a refresh differs from the step's request
//! only in the output cap, and `sign()` runs on it as on any request.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::commands::{Reply as Answer, ReplyAnswer};
use contract::events::Decision;
use contract::inbox::{Ack, Delivery, Message};
use contract::shapes::{ContentPart, Origin, Sender};
use contract::{CommandId, RequestId, SessionId};
use fakes::clock::FakeClock;
use fakes::{ProviderServer, Request, Response};
use log::Log;
use r#loop::Loop;
use serde_json::{Value, json};

const DEADLINE: Duration = Duration::from_secs(10);

/// A reply the fixture's `models()` and `credential()` both read, so
/// discovery and the token request succeed in either arrival order.
fn listing_and_token() -> Response {
    Response::status(
        200,
        json!({
            "data": [{"id": "m1", "context_length": 1000}],
            "access_token": "tok-1",
            "expires_at": 2_000_000_000_u64,
        })
        .to_string(),
    )
}

/// An OpenAI Responses stream answering `Hello.`.
fn hello() -> Response {
    let events = [
        json!({"type": "response.output_text.delta", "delta": "Hello."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
        json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed",
            "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
        }}),
    ];
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    Response::stream(body)
}

/// A driver's prompt on the loop's inbox.
fn prompt(text: &str) -> Delivery {
    Delivery::Prompt(
        Message {
            content: vec![ContentPart::Text { text: text.into() }],
            sender: Sender {
                origin: Origin::Driver,
                command_id: Some(CommandId("c_1".into())),
            },
        },
        Ack(Box::new(|_| {})),
    )
}

/// Moves the clock to `to` and wakes the loop with a reply naming nothing,
/// as a real clock's timeout would; returns once the loop has taken it.
fn advance_to(clock: &FakeClock, inbox: &mpsc::Sender<Delivery>, to: Instant) {
    clock.advance(to.saturating_duration_since(clock.now()));
    let (done, taken) = mpsc::channel();
    inbox
        .send(Delivery::Reply(
            Answer {
                request_id: RequestId("r_absent".into()),
                answer: ReplyAnswer::Approval {
                    decision: Decision::Deny,
                    feedback: None,
                    remember: None,
                },
            },
            Ack(Box::new(move |_| {
                done.send(()).unwrap();
            })),
        ))
        .unwrap();
    taken
        .recv_timeout(DEADLINE)
        .expect("the loop took the wake");
}

/// The fixture's signature over `request`, checked to be there.
fn signed(request: &Request) -> &str {
    assert_eq!(
        request.header("x-fixture-saw"),
        Some("body_sha256,headers,method,url")
    );
    let signature = request.header("x-fixture-signature").unwrap_or("");
    assert_eq!(signature.len(), 64, "{signature:?}");
    request.header("x-fixture-content-sha256").unwrap()
}

#[test]
fn a_lua_providers_session_refreshes_its_cache_signed_with_only_the_cap_changed() {
    let root = fakes::TempDir::new("fiber-lua-warm");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    // Discovery, the token, the turn's step, then its refresh.
    let server = ProviderServer::start_with_fallback(
        [listing_and_token(), listing_and_token(), hello()],
        hello(),
    )
    .unwrap();
    extensions::plan(
        &home,
        &extensions::Request::Path(fakes::lua_fixture()),
        "0.1.0",
        &extensions::Origin::github(),
        &*FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    config::store_secret(&home, "fixture.url", &config::Secret::new(server.url())).unwrap();
    config::store_secret(&home, "fixture.api_key", &config::Secret::new("k1".into())).unwrap();
    std::fs::write(
        home.join("config.json"),
        json!({"model": "fixture/m1", "cache": {"warm_idle": true, "warm_cap": 1}}).to_string(),
    )
    .unwrap();
    let clock = FakeClock::new();
    let shared: Arc<dyn Clock> = clock.clone();

    // Discovery and the credential request block, so setup runs on its own
    // thread under a deadline instead of hanging the test.
    let (setup_done, setup) = mpsc::channel();
    let setup_home = home.clone();
    let setup_workspace = workspace.clone();
    let setup_clock = Arc::clone(&shared);
    thread::spawn(move || {
        drop(setup_done.send(super::parts_in(
            setup_home,
            setup_workspace,
            None,
            None,
            None,
            None,
            setup_clock,
            None,
        )));
    });
    let parts = setup
        .recv_timeout(DEADLINE)
        .expect("setup ended in time")
        .unwrap();

    assert_eq!(parts.warm, Some(1), "the session warms for one lifetime");
    let log = Arc::new(
        Log::create(
            &parts.sessions,
            SessionId("s_warm".into()),
            Arc::clone(&shared),
        )
        .unwrap(),
    );
    let permissions = super::ask_permissions(
        &parts.home,
        &parts.project,
        workspace.display().to_string(),
        parts.credential_files,
        &shared,
    );
    let (inbox, deliveries) = mpsc::channel();
    let looped = Loop::start(
        log,
        parts.provider,
        parts.model,
        parts.prompt,
        deliveries,
        Vec::new(),
        permissions,
        None,
    )
    .unwrap()
    .idle_exit(Some(Duration::from_secs(60)))
    .warm(parts.warm);
    let start = clock.now();
    inbox.send(prompt("hi")).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(looped.run()).unwrap());
    // On the default 1-hour lifetime the refresh is due 30 s before it ends.
    let due = start + Duration::from_secs(3570);
    assert!(clock.await_parked(due, DEADLINE), "the refresh is due");
    advance_to(&clock, &inbox, due);
    // Warming stopped at the cap, 3600 s; the idle clock counts from there.
    let exit = start + Duration::from_secs(3660);
    assert!(
        clock.await_parked(exit, DEADLINE),
        "idle counts from the cap"
    );
    advance_to(&clock, &inbox, exit);
    let ran = finished.recv_timeout(DEADLINE).expect("run ended in time");
    assert!(ran.is_ok(), "{ran:?}");

    let sent: Vec<Request> = server
        .requests()
        .into_iter()
        .filter(|request| request.path == "/v1/responses")
        .collect();
    assert_eq!(sent.len(), 2, "the step and its refresh: {sent:?}");
    // `sign()` ran on the refresh, over the refresh's own body.
    assert_ne!(signed(&sent[0]), signed(&sent[1]));
    let step: Value = serde_json::from_slice(&sent[0].body).unwrap();
    let mut refresh: Value = serde_json::from_slice(&sent[1].body).unwrap();
    assert!(step.get("max_output_tokens").is_none(), "{step}");
    // The Responses API's floor of 16 is what a one-token cap sends.
    assert_eq!(
        refresh.as_object_mut().unwrap().remove("max_output_tokens"),
        Some(json!(16))
    );
    assert_eq!(refresh, step);
}
