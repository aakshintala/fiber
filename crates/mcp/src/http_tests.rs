//! Remote servers over streamable HTTP, against a local fake HTTP MCP
//! server: every row names the request it asserts on from
//! [`ProviderServer::requests`], recorded before the response is sent, so
//! no row depends on arrival order.

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use contract::diag::{DebugLog, HttpRequest};
use fakes::clock::FakeClock;
use fakes::{Deadline, ProviderServer, Request, Response};
use serde_json::{Map, Value, json};

use crate::rpc::Named;
use crate::server::{CallError, OpenServer, StartError};

/// How long a test waits for a thread or a response, in real time.
const WITHIN: Duration = fakes::MUST_SUCCEED_WITHIN;

/// The session id the fake answers `initialize` with.
const SESSION: &str = "1868a90c";

struct Setup {
    fake: Arc<FakeClock>,
    lines: Arc<Lines>,
}

/// Every `http_request` line the observed servers write, in order.
#[derive(Default)]
struct Lines {
    lines: Mutex<Vec<HttpRequest>>,
    changed: Condvar,
}

impl DebugLog for Lines {
    fn debugging(&self) -> bool {
        true
    }

    fn http_request(&self, request: &HttpRequest) {
        self.lines.lock().unwrap().push(request.clone());
        self.changed.notify_all();
    }
}

impl Lines {
    fn recorded(&self) -> Vec<HttpRequest> {
        self.lines.lock().unwrap().clone()
    }

    /// Waits for the observed requests with the suite's real-time deadline.
    fn await_count(&self, count: usize) -> Vec<HttpRequest> {
        let deadline = Deadline::after(Duration::from_secs(10));
        let mut lines = self.lines.lock().unwrap();
        while lines.len() < count {
            let (updated, timeout) = self.changed.wait_timeout(lines, deadline.left()).unwrap();
            lines = updated;
            assert!(
                lines.len() >= count || !timeout.timed_out(),
                "waited {WITHIN:?} for {count} HTTP request lines, got {}",
                lines.len()
            );
        }
        lines.clone()
    }
}

impl Setup {
    fn new() -> Self {
        Self {
            fake: FakeClock::new(),
            lines: Arc::new(Lines::default()),
        }
    }

    fn clock(&self) -> Arc<dyn Clock> {
        self.fake.clone()
    }

    /// The sink the observed starts write their `mcp` lines to.
    fn sink(&self) -> Option<Arc<dyn DebugLog>> {
        Some(Arc::clone(&self.lines) as Arc<dyn DebugLog>)
    }

    /// Every `mcp` line recorded so far, in order.
    fn lines(&self) -> Vec<HttpRequest> {
        self.lines.recorded()
    }

    /// Starts the remote server at the fake's `/mcp`, expecting success,
    /// with a deadline naming the wait: the shared server the calls run
    /// through, and the lists it answered.
    #[track_caller]
    fn start(
        &self,
        server: &ProviderServer,
        timeout: Duration,
    ) -> (Arc<crate::server::Server>, crate::cache::Cached) {
        let url = format!("{}/mcp", server.url());
        let clock = self.clock();
        let opened = fakes::within("the remote start", Deadline::after(WITHIN), move || {
            super::start(
                &url,
                Arc::new(crate::test_support::FakeTokens::default()),
                None,
                net::LIMITS,
                &clock,
                timeout,
                "0.0.0",
                None,
            )
        })
        .expect("the remote server starts");
        let OpenServer { server, listed, .. } = opened;
        (Arc::new(server), listed)
    }

    /// As [`Setup::start`], writing one `mcp` line per request the server
    /// answers on this setup's sink: the observed start.
    #[track_caller]
    fn observed(
        &self,
        server: &ProviderServer,
        timeout: Duration,
    ) -> (Arc<crate::server::Server>, crate::cache::Cached) {
        let url = format!("{}/mcp", server.url());
        let clock = self.clock();
        let debug = self.sink();
        let opened = fakes::within(
            "the remote start",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                super::start(
                    &url,
                    Arc::new(crate::test_support::FakeTokens::default()),
                    None,
                    net::LIMITS,
                    &clock,
                    timeout,
                    "0.0.0",
                    debug,
                )
            },
        )
        .expect("the remote server starts");
        let OpenServer { server, listed, .. } = opened;
        (Arc::new(server), listed)
    }

    /// Starts the remote server at `url`, expecting failure, with a
    /// deadline naming the wait.
    #[track_caller]
    fn start_err(&self, url: &str, timeout: Duration) -> StartError {
        let url = url.to_owned();
        let clock = self.clock();
        let started = fakes::within("the remote start", Deadline::after(WITHIN), move || {
            super::start(
                &url,
                Arc::new(crate::test_support::FakeTokens::default()),
                None,
                net::LIMITS,
                &clock,
                timeout,
                "0.0.0",
                None,
            )
        });
        match started {
            Ok(_) => panic!("the remote start fails"),
            Err(error) => error,
        }
    }

    /// Starts the remote server at `url`, expecting success, with a
    /// deadline naming the wait: the shared server the calls run
    /// through, and the lists it answered.
    #[track_caller]
    fn start_at(&self, url: &str, timeout: Duration) -> Arc<crate::server::Server> {
        let url = url.to_owned();
        let clock = self.clock();
        let opened = fakes::within("the remote start", Deadline::after(WITHIN), move || {
            super::start(
                &url,
                Arc::new(crate::test_support::FakeTokens::default()),
                None,
                net::LIMITS,
                &clock,
                timeout,
                "0.0.0",
                None,
            )
        })
        .expect("the remote server starts");
        let OpenServer { server, .. } = opened;
        Arc::new(server)
    }

    /// Calls `method` with an empty `echo` call on `server`, with a
    /// deadline naming the wait.
    #[track_caller]
    fn call(
        &self,
        server: &Arc<crate::server::Server>,
        method: &'static str,
        timeout: Duration,
        cancel: &fakes::CancelToken,
    ) -> Result<Value, CallError> {
        let (done, result) = std::sync::mpsc::channel();
        let server = Arc::clone(server);
        let cancel = cancel.clone();
        thread::spawn(move || {
            let arguments = Map::new();
            let params = Named {
                arguments: &arguments,
                name: "echo",
            };
            let answer = server.call(method, &params, timeout, &cancel);
            done.send(answer).expect("collected");
        });
        Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| panic!("the call answers within {WITHIN:?}"))
    }
}

/// One JSON-RPC answer string.
fn result(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

/// One event-stream body carrying `data`.
fn event(data: &str) -> String {
    format!("event: message\ndata: {data}\n\n")
}

/// The request body's JSON-RPC method.
fn body_method(req: &Request) -> String {
    let body: Value = serde_json::from_slice(&req.body).expect("a JSON body");
    body.get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn token_start(
    setup: &Setup,
    url: String,
    files: Arc<crate::test_support::FakeTokens>,
) -> Result<OpenServer, StartError> {
    let clock = setup.clock();
    fakes::within("OAuth remote start", Deadline::after(WITHIN), move || {
        super::start(
            &url,
            files,
            None,
            net::LIMITS,
            &clock,
            Duration::from_secs(5),
            "0.0.0",
            None,
        )
    })
}
fn token_files(
    url: &str,
    endpoint: &str,
    expiry: Option<u64>,
) -> Arc<crate::test_support::FakeTokens> {
    let files = Arc::new(crate::test_support::FakeTokens::default());
    crate::store_token(
        files.as_ref(),
        url,
        &crate::Token {
            access_token: contract::Secret::new("at-1".into()),
            refresh_token: Some(contract::Secret::new("rt-1".into())),
            expires_at: expiry,
            token_endpoint: endpoint.into(),
            client_id: "fiber-abc".into(),
        },
    )
    .unwrap();
    files
}
#[test]
fn every_request_carries_only_its_urls_bearer() {
    for stored in [false, true] {
        let fake = ProviderServer::start_responding(standard()).unwrap();
        let setup = Setup::new();
        let url = format!("{}/mcp", fake.url());
        let files = if stored {
            token_files(&url, "https://auth/token", None)
        } else {
            Arc::new(crate::test_support::FakeTokens::default())
        };
        let opened = token_start(&setup, url, files).unwrap();
        let server = Arc::new(opened.server);
        assert!(
            setup
                .call(
                    &server,
                    "tools/call",
                    Duration::from_secs(5),
                    &fakes::CancelToken::new()
                )
                .is_ok()
        );
        fakes::within("remote stop", Deadline::after(WITHIN), move || {
            server.stop()
        });
        let requests = fake.requests();
        assert_eq!(
            requests
                .iter()
                .map(|request| request.method.as_str())
                .collect::<Vec<_>>(),
            ["POST", "POST", "POST", "POST", "DELETE"]
        );
        let expected = stored.then(|| fakes::fingerprint("Bearer at-1"));
        for request in requests {
            assert_eq!(request.header("authorization"), expected.as_deref());
        }
    }
}
#[test]
fn an_in_window_token_sends_before_its_background_refresh_finishes() {
    let auth = fakes::OauthServer::start(vec![fakes::OauthReply::token("at-2", "rt-2", 3600)]);
    auth.hold();
    let fake = ProviderServer::start_responding(standard()).unwrap();
    let setup = Setup::new();
    let url = format!("{}/mcp", fake.url());
    let now = contract::clock::wall_ms(setup.clock().wall()) / 1000;
    let files = token_files(&url, &auth.url(), Some(now + 300));
    let (written, received) = std::sync::mpsc::channel();
    files.state.lock().unwrap().written = Some(written);
    let opened = token_start(&setup, url, files.clone()).unwrap();
    assert!(auth.await_requests(1, Deadline::after(WITHIN)));
    assert_eq!(
        fake.requests()[0].header("authorization"),
        Some(fakes::fingerprint("Bearer at-1").as_str())
    );
    assert_eq!(files.state.lock().unwrap().writes, 1);
    auth.release();
    Deadline::after(WITHIN).recv_or_fail(&received, "the background token is saved");
    assert_eq!(files.state.lock().unwrap().writes, 2);
    fakes::within("remote stop", Deadline::after(WITHIN), move || {
        opened.server.stop()
    });
}

#[test]
fn a_refresh_request_carries_no_bearer() {
    let auth = ProviderServer::start(vec![Response::status(
        200,
        json!({"access_token":"at-2", "refresh_token":"rt-2", "expires_in":3600}).to_string(),
    )])
    .unwrap();
    let fake = ProviderServer::start_responding(standard()).unwrap();
    let setup = Setup::new();
    let url = format!("{}/mcp", fake.url());
    let opened = token_start(&setup, url.clone(), token_files(&url, &auth.url(), Some(0))).unwrap();
    fakes::within("remote stop", Deadline::after(WITHIN), move || {
        opened.server.stop()
    });
    assert_eq!(auth.requests().len(), 1);
    assert_eq!(auth.requests()[0].header("authorization"), None);
    for request in fake.requests() {
        assert_eq!(
            request.header("authorization"),
            Some(fakes::fingerprint("Bearer at-2").as_str())
        );
    }
}
#[test]
fn the_legacy_probe_carries_the_stored_bearer() {
    let fake =
        ProviderServer::start(vec![Response::status(405, ""), Response::status(404, "")]).unwrap();
    let setup = Setup::new();
    let url = format!("{}/mcp", fake.url());
    assert!(matches!(
        token_start(
            &setup,
            url.clone(),
            token_files(&url, "https://auth/token", None)
        ),
        Err(StartError::StartFailed(_))
    ));
    let requests = fake.requests();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.method.as_str())
            .collect::<Vec<_>>(),
        ["POST", "GET"]
    );
    for request in requests {
        assert_eq!(
            request.header("authorization"),
            Some(fakes::fingerprint("Bearer at-1").as_str())
        );
    }
}

/// The standard fake: `initialize` answered as JSON with a session id,
/// notifications answered `202`, `tools/list` answered as an event
/// stream, `tools/call` answered as JSON, `GET` answered `404`, `DELETE`
/// answered `200`.
fn standard() -> impl Fn(&Request) -> Response + Send + Sync + 'static {
    move |req: &Request| {
        if req.method == "GET" {
            return Response::status(404, "no stream here");
        }
        if req.method == "DELETE" {
            return Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::status(
                200,
                result(
                    &id,
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "remote", "version": "1"},
                    }),
                ),
            )
            .header("mcp-session-id", SESSION),
            Some("notifications/initialized") | Some("notifications/cancelled") | None => {
                Response::status(202, "")
            }
            Some("tools/list") => Response::stream(event(&result(
                &id,
                json!({"tools": [{"name": "echo", "description": "Echoes.", "inputSchema": {"type": "object"}}]}),
            ))),
            Some("tools/call") => Response::status(
                200,
                result(&id, json!({"content": [{"type": "text", "text": "hi"}]})),
            ),
            Some(other) => panic!("unexpected method {other}"),
        }
    }
}

/// The tool names `listed` holds.
fn listed_names(listed: &crate::cache::Cached) -> Vec<String> {
    listed.tools.iter().map(|tool| tool.name.clone()).collect()
}

/// One connected TCP pair, with a deadline on both socket operations.
fn socket_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("the local listener binds");
    let address = listener.local_addr().expect("the listener address");
    let client = fakes::within(
        "the local socket connect",
        Deadline::after(WITHIN),
        move || TcpStream::connect(address),
    )
    .expect("the local socket connects");
    let (peer, _) = fakes::within(
        "the local socket accept",
        Deadline::after(WITHIN),
        move || listener.accept(),
    )
    .expect("the local socket is accepted");
    (client, peer)
}

#[test]
fn start_lists_tools_and_later_requests_carry_session_and_version() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(standard()).unwrap();
    let (remote, listed) = setup.start(&server, Duration::from_secs(5));
    assert_eq!(listed_names(&listed), ["echo"]);
    let _ = remote;
    let requests = server.requests();
    assert_eq!(
        requests.len(),
        3,
        "initialize, notifications/initialized, tools/list: {requests:?}"
    );
    let init = &requests[0];
    assert_eq!(init.method, "POST");
    assert_eq!(init.path, "/mcp");
    assert_eq!(body_method(init), "initialize");
    assert_eq!(init.header("content-type"), Some("application/json"));
    assert_eq!(
        init.header("accept"),
        Some("application/json, text/event-stream")
    );
    assert_eq!(
        init.header("mcp-session-id"),
        None,
        "the first request carries no session id"
    );
    assert_eq!(
        init.header("mcp-protocol-version"),
        None,
        "the first request carries no version"
    );
    let initialized = &requests[1];
    assert_eq!(body_method(initialized), "notifications/initialized");
    assert_eq!(initialized.header("mcp-session-id"), Some(SESSION));
    assert_eq!(
        initialized.header("mcp-protocol-version"),
        Some("2025-06-18")
    );
    let list = &requests[2];
    assert_eq!(body_method(list), "tools/list");
    assert_eq!(list.header("mcp-session-id"), Some(SESSION));
    assert_eq!(list.header("mcp-protocol-version"), Some("2025-06-18"));
}

#[test]
fn a_missing_or_bad_protocol_version_sends_the_fallback() {
    // Without the fallback a server that names no usable version would
    // send an unchecked value: both rows pin `PROTOCOL_VERSION` here.
    for (name, protocol) in [("missing", Value::Null), ("bad", json!("not\na\nheader"))] {
        let setup = Setup::new();
        let server = ProviderServer::start_responding(move |req: &Request| {
            if req.method == "GET" {
                return Response::status(404, "no stream here");
            }
            if req.method == "DELETE" {
                return Response::status(200, "");
            }
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let id = body.get("id").cloned().unwrap_or(Value::Null);
            match body.get("method").and_then(Value::as_str) {
                Some("initialize") => {
                    let mut answered = json!({
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "remote", "version": "1"},
                    });
                    if !protocol.is_null() {
                        answered["protocolVersion"] = protocol.clone();
                    }
                    Response::status(200, result(&id, answered)).header("mcp-session-id", SESSION)
                }
                Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
                _ => Response::status(202, ""),
            }
        })
        .unwrap();
        let (_remote, listed) = setup.start(&server, Duration::from_secs(5));
        assert!(listed_names(&listed).is_empty(), "row: {name}");
        let requests = server.requests();
        assert_eq!(requests.len(), 3, "row: {name}: {requests:?}");
        assert_eq!(
            requests[2].header("mcp-protocol-version"),
            Some("2025-06-18"),
            "row: {name}"
        );
    }
}

#[test]
fn a_start_without_a_session_id_sends_no_session_header() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(move |req: &Request| {
        if req.method == "DELETE" {
            return Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::status(
                200,
                result(
                    &id,
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "remote", "version": "1"},
                    }),
                ),
            ),
            Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
            _ => Response::status(202, ""),
        }
    })
    .unwrap();
    let (_remote, listed) = setup.start(&server, Duration::from_secs(5));
    assert!(listed_names(&listed).is_empty());
    let requests = server.requests();
    assert_eq!(requests.len(), 3, "{requests:?}");
    assert_eq!(requests[1].header("mcp-session-id"), None);
    assert_eq!(requests[2].header("mcp-session-id"), None);
}

#[test]
fn a_bad_session_id_fails_the_start() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(move |req: &Request| {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::status(
                200,
                result(
                    &id,
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "remote", "version": "1"},
                    }),
                ),
            )
            // Non-ASCII bytes ride a header line httparse accepts, but
            // are no valid header value to read back, so the exchange
            // fails the start instead of storing them.
            .header("mcp-session-id", "sessïon-✓"),
            _ => Response::status(202, ""),
        }
    })
    .unwrap();
    let url = format!("{}/mcp", server.url());
    let error = setup.start_err(&url, Duration::from_secs(5));
    assert_eq!(
        error,
        StartError::StartFailed(
            "The server's session id is not a valid HTTP header value.".to_owned()
        ),
        "its sentence, and the body is never routed"
    );
    assert!(
        server
            .requests()
            .iter()
            .all(|req| body_method(req) == "initialize"),
        "nothing past initialize is sent: {:?}",
        server.requests()
    );
}

#[test]
fn a_call_answered_by_a_stream_with_a_ping_returns_and_answers_it() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(move |req: &Request| {
        if req.method == "DELETE" {
            return Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::status(
                200,
                result(
                    &id,
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "remote", "version": "1"},
                    }),
                ),
            )
            .header("mcp-session-id", SESSION),
            Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
            Some("tools/call") => {
                let ping = json!({"jsonrpc": "2.0", "id": 99, "method": "ping"}).to_string();
                let answer = result(&id, json!({"content": [{"type": "text", "text": "hi"}]}));
                Response::stream(format!(
                    "event: message\ndata: {ping}\n\nevent: message\ndata: {answer}\n\n"
                ))
            }
            _ => Response::status(202, ""),
        }
    })
    .unwrap();
    let (remote, _listed) = setup.start(&server, Duration::from_secs(5));
    let answer = setup.call(
        &remote,
        "tools/call",
        Duration::from_secs(60),
        &fakes::CancelToken::new(),
    );
    let answered = answer.expect("the streamed call answers");
    assert_eq!(
        answered.get("content").and_then(|content| content.get(0)),
        Some(&json!({"type": "text", "text": "hi"}))
    );
    assert!(
        server.await_requests(5, Deadline::after(WITHIN)),
        "the ping answer is POSTed too"
    );
    let pinged = server
        .requests()
        .into_iter()
        .find(|req| {
            serde_json::from_slice::<Value>(&req.body)
                .ok()
                .and_then(|body| body.get("id").cloned())
                == Some(json!(99))
        })
        .expect("a POST answers the ping's id");
    let posted: Value = serde_json::from_slice(&pinged.body).expect("a JSON answer");
    assert_eq!(posted.get("result"), Some(&json!({})), "ping answers {{}}");
    assert!(
        !remote.is_gone(),
        "a stream carrying the answer is no death"
    );
}

#[test]
fn a_call_answered_as_a_charset_stream_returns() {
    // The media type compares before any `;`, so a charset still reads as
    // a stream: without the split this row fails its status.
    let setup = Setup::new();
    let server = ProviderServer::start_responding(move |req: &Request| {
        if req.method == "DELETE" {
            return Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::status(
                200,
                result(
                    &id,
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "remote", "version": "1"},
                    }),
                ),
            )
            .header("mcp-session-id", SESSION),
            Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
            Some("tools/call") => Response {
                status: 200,
                headers: vec![(
                    "content-type".to_owned(),
                    "text/event-stream; charset=utf-8".to_owned(),
                )],
                body: event(&result(
                    &id,
                    json!({"content": [{"type": "text", "text": "hi"}]}),
                ))
                .into_bytes(),
                drop_connection: false,
                stall: false,
            },
            _ => Response::status(202, ""),
        }
    })
    .unwrap();
    let (remote, _listed) = setup.start(&server, Duration::from_secs(5));
    let answered = setup
        .call(
            &remote,
            "tools/call",
            Duration::from_secs(60),
            &fakes::CancelToken::new(),
        )
        .expect("the charset stream answers");
    assert_eq!(
        answered.get("content").and_then(|content| content.get(0)),
        Some(&json!({"type": "text", "text": "hi"}))
    );
}

#[test]
fn an_initialize_answered_4xx_with_an_endpoint_stream_is_legacy() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        Response::status(405, "method not allowed"),
        Response::stream("event: endpoint\ndata: /messages?sessionId=abc\n\n"),
    ])
    .unwrap();
    let url = format!("{}/mcp", server.url());
    let error = setup.start_err(&url, Duration::from_secs(5));
    let StartError::StartFailed(message) = error else {
        panic!("a legacy server fails StartFailed, got {error:?}");
    };
    assert!(message.contains("HTTP+SSE"), "{message}");
    assert!(message.contains("streamable HTTP"), "{message}");
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[1].method, "GET");
    assert_eq!(
        requests[1].header("accept"),
        Some("text/event-stream"),
        "the probe asks for a stream"
    );
}

#[test]
fn an_initialize_answered_404_without_an_endpoint_stream_names_its_status() {
    for (name, probe) in [
        ("get_404", Response::status(404, "nothing here")),
        (
            "message_event",
            Response::stream("event: message\ndata: hello\n\n"),
        ),
    ] {
        let setup = Setup::new();
        let server = ProviderServer::start([Response::status(404, "no session"), probe]).unwrap();
        let url = format!("{}/mcp", server.url());
        let error = setup.start_err(&url, Duration::from_secs(5));
        assert_eq!(
            error,
            StartError::StartFailed("The server answered HTTP 404.".to_owned()),
            "row {name}: the POST's status, not HTTP+SSE"
        );
    }
}

/// Probes an endpoint-shaped body and requires the POST's original status.
fn probe_error(status: u16, media: &str) -> StartError {
    let setup = Setup::new();
    let probe = Response {
        status,
        headers: vec![("content-type".to_owned(), media.to_owned())],
        body: b"event: endpoint\ndata: /messages?sessionId=abc\n\n".to_vec(),
        drop_connection: false,
        stall: false,
    };
    let server =
        ProviderServer::start([Response::status(405, "method not allowed"), probe]).unwrap();
    let url = format!("{}/mcp", server.url());
    let error = setup.start_err(&url, Duration::from_secs(5));
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "the probe is sent after initialize");
    assert_eq!(requests[1].method, "GET", "the legacy probe is a GET");
    error
}

#[test]
fn a_success_status_without_sse_is_not_a_legacy_probe() {
    assert_eq!(
        probe_error(200, "text/plain"),
        StartError::StartFailed("The server answered HTTP 405.".to_owned()),
    );
}

#[test]
fn an_sse_probe_without_a_success_status_is_not_legacy() {
    assert_eq!(
        probe_error(404, "text/event-stream"),
        StartError::StartFailed("The server answered HTTP 405.".to_owned()),
    );
}

#[test]
fn an_initialize_answered_as_a_stream_never_probes() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(move |req: &Request| {
        if req.method == "DELETE" {
            return Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::stream(event(&result(
                &id,
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "remote", "version": "1"},
                }),
            )))
            .header("mcp-session-id", SESSION),
            Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
            _ => Response::status(202, ""),
        }
    })
    .unwrap();
    let (_remote, listed) = setup.start(&server, Duration::from_secs(5));
    assert!(listed_names(&listed).is_empty());
    assert!(
        server.requests().iter().all(|req| req.method == "POST"),
        "no GET is ever sent: {:?}",
        server.requests()
    );
}

#[test]
fn http_failures_fail_only_their_request() {
    // No sentence quotes the body, which may be large or hold a secret.
    for (name, answer, message) in [
        (
            "status_500",
            Response::status(500, "secret-body-marker"),
            "The server answered HTTP 500.",
        ),
        (
            "status_202",
            Response::status(202, ""),
            "The server answered HTTP 202.",
        ),
        (
            "text_plain",
            Response {
                status: 200,
                headers: vec![("content-type".to_owned(), "text/plain".to_owned())],
                body: b"secret-body-marker".to_vec(),
                drop_connection: false,
                stall: false,
            },
            "The server answered with media type `text/plain`, not JSON or an event stream.",
        ),
    ] {
        let setup = Setup::new();
        let server = ProviderServer::start_responding({
            move |req: &Request| {
                if req.method == "DELETE" {
                    return Response::status(200, "");
                }
                let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
                let id = body.get("id").cloned().unwrap_or(Value::Null);
                match body.get("method").and_then(Value::as_str) {
                    Some("initialize") => Response::status(
                        200,
                        result(
                            &id,
                            json!({
                                "protocolVersion": "2025-06-18",
                                "capabilities": {"tools": {}},
                                "serverInfo": {"name": "remote", "version": "1"},
                            }),
                        ),
                    )
                    .header("mcp-session-id", SESSION),
                    Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
                    Some("tools/call") => answer.clone(),
                    _ => Response::status(202, ""),
                }
            }
        })
        .unwrap();
        let (remote, _listed) = setup.start(&server, Duration::from_secs(5));
        let error = setup
            .call(
                &remote,
                "tools/call",
                Duration::from_secs(60),
                &fakes::CancelToken::new(),
            )
            .expect_err("the request fails");
        assert_eq!(error, CallError::Http(message.to_owned()), "row: {name}");
        assert!(
            !format!("{error:?}").contains("secret-body-marker"),
            "row {name}: the body is never quoted"
        );
        assert!(!remote.is_gone(), "row {name}: the server lives on");
    }
}

#[test]
fn a_login_failure_names_the_login() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::status(401, "denied")]).unwrap();
    let url = format!("{}/mcp", server.url());
    let error = setup.start_err(&url, Duration::from_secs(5));
    assert_eq!(
        error,
        StartError::Login("The server answered HTTP 401 and needs a login.".to_owned())
    );
}

#[test]
fn login_failures_cover_the_probe_initialized_and_both_lists() {
    for failure in [
        "probe",
        "initialized-empty",
        "initialized-tools",
        "tools/list",
        "prompts/list",
    ] {
        let fake = ProviderServer::start_responding(move |request| {
            if failure == "probe" { return Response::status(if request.method == "GET" { 401 } else { 405 }, "secret"); }
            let method = body_method(request);
            if method == "initialize" {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                return Response::status(200, result(&body["id"], json!({"protocolVersion":"2025-06-18", "capabilities":if failure == "initialized-empty" { json!({}) } else { json!({"tools":{}, "prompts":{}}) }})));
            }
            if (failure.starts_with("initialized") && method == "notifications/initialized") || method == failure { return Response::status(401, "secret"); }
            standard()(request)
        }).unwrap();
        let setup = Setup::new();
        assert!(
            matches!(
                setup.start_err(&format!("{}/mcp", fake.url()), Duration::from_secs(5)),
                StartError::Login(_)
            ),
            "{failure}"
        );
        if failure.starts_with("initialized") {
            assert_eq!(fake.requests().len(), 2);
        }
        if failure == "probe" {
            assert_eq!(fake.requests().len(), 2);
        }
    }
}
#[test]
fn initialized_only_fails_the_start_on_401() {
    for dropped in [false, true] {
        let fake = ProviderServer::start_responding(move |request| {
            if request.method == "DELETE" {
                return Response::status(401, "");
            }
            if body_method(request) == "notifications/initialized" {
                if dropped {
                    return Response::drop_connection();
                }
                return Response::status(500, "");
            }
            standard()(request)
        })
        .unwrap();
        let setup = Setup::new();
        let (server, _) = setup.start(&fake, Duration::from_secs(5));
        fakes::within("remote stop", Deadline::after(WITHIN), move || {
            server.stop()
        });
        assert_eq!(fake.requests()[1].method, "POST");
    }
}
#[test]
fn userinfo_is_refused_before_sending_even_through_the_transport() {
    let fake = ProviderServer::start([]).unwrap();
    let setup = Setup::new();
    let url = fake.url().replacen("http://", "http://u:pw@", 1);
    assert_eq!(
        setup.start_err(&url, Duration::from_secs(5)),
        StartError::StartFailed(
            "Its `url` has a user name or password, which Fiber does not send.".into()
        )
    );
    assert!(fake.requests().is_empty());
}

#[test]
fn initialized_ack_wait_ends_at_the_startup_deadline() {
    let setup = Setup::new();
    let fake = ProviderServer::start_responding(standard()).unwrap();
    fake.hold_from(2, fakes::deadline::BUDGET);
    let url = format!("{}/mcp", fake.url());
    let clock = setup.clock();
    let startup = Duration::from_secs(5);
    let deadline = clock.now() + startup;
    let (done, received) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        done.send(super::start(
            &url,
            Arc::new(crate::test_support::FakeTokens::default()),
            None,
            net::LIMITS,
            &clock,
            startup,
            "0.0.0",
            None,
        ))
        .unwrap();
    });
    assert!(fake.await_requests(2, Deadline::after(WITHIN)));
    setup
        .fake
        .advance_when_parked(Some(deadline), None, startup, &Deadline::after(WITHIN));
    assert!(matches!(
        Deadline::after(WITHIN).recv_or_fail(&received, "initialized ack startup ends"),
        Err(StartError::Deadline)
    ));
    worker.join().unwrap();
}
#[test]
fn a_pump_login_failure_refuses_waiting_calls() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let fake = ProviderServer::start_responding(move |request| {
        if request.method == "DELETE" {
            return Response::status(401, "");
        }
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let method = body.get("method").and_then(Value::as_str);
        if method == Some("tools/call") && seen.fetch_add(1, Ordering::SeqCst) == 0 {
            return Response::stall(
                200,
                event(&json!({"jsonrpc":"2.0", "id":99, "method":"ping"}).to_string()),
                1_000_000,
            )
            .header("content-type", "text/event-stream");
        }
        if body.get("id") == Some(&json!(99)) {
            return Response::status(401, "");
        }
        standard()(request)
    })
    .unwrap();
    let setup = Setup::new();
    let (server, _) = setup.start(&fake, Duration::from_secs(5));
    assert!(matches!(
        setup.call(
            &server,
            "tools/call",
            Duration::from_secs(60),
            &fakes::CancelToken::new()
        ),
        Err(CallError::Login(_))
    ));
    assert!(!server.is_gone());
    assert!(
        setup
            .call(
                &server,
                "tools/call",
                Duration::from_secs(60),
                &fakes::CancelToken::new()
            )
            .is_ok()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    fakes::within("remote stop", Deadline::after(WITHIN), move || {
        server.stop()
    });
}

#[test]
fn a_pump_401_without_waiters_is_not_sticky() {
    let fake = ProviderServer::start_responding(|request| {
        if body_method(request) == "test/unauthenticated" {
            Response::status(401, "")
        } else {
            standard()(request)
        }
    })
    .unwrap();
    let setup = Setup::new();
    let link = Arc::new(super::Link::new(
        format!("{}/mcp", fake.url()),
        Arc::new(crate::test_support::FakeTokens::default()),
        setup.clock(),
        None,
        net::LIMITS,
        None,
    ));
    let shared = Arc::new(crate::wait::Shared::default());
    let (sender, incoming) = std::sync::mpsc::sync_channel(crate::server::WRITE_QUEUE);
    let sender = Arc::new(sender);
    let weak = Arc::downgrade(&sender);
    let pumping = shared.clone();
    let (done, finished) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        super::pump(incoming, link, pumping, weak);
        done.send(()).unwrap();
    });
    sender
        .send(crate::rpc::encode_notification("test/unauthenticated", &json!({})).into_bytes())
        .unwrap();
    let observed = shared.clone();
    crate::test_support::await_until("the pump handled the 401 with no waiter", move || {
        super::lock(&observed.inner).seq != 0
    });
    assert_eq!(fake.requests().len(), 1);
    shared.insert(1);
    sender
        .send(crate::rpc::encode_request(1, "tools/call", &json!({})).into_bytes())
        .unwrap();
    let observed = shared.clone();
    crate::test_support::await_until("the next request receives a result", move || {
        observed.view(1, &crate::wait::NoCancel).response.is_some()
    });
    assert!(matches!(
        shared.view(1, &crate::wait::NoCancel).response,
        Some(crate::rpc::Outcome::Result(_))
    ));
    drop(sender);
    Deadline::after(WITHIN).recv_or_fail(&finished, "the pump ends");
    worker.join().unwrap();
}

#[test]
fn startup_deadlines_end_both_token_lock_waits_and_stalled_token_posts() {
    use contract::mcp_tokens::TokenFiles;
    for held_lock in [true, false] {
        let auth = fakes::OauthServer::start(vec![fakes::OauthReply::token("at-2", "rt-2", 3600)]);
        auth.hold();
        let fake = ProviderServer::start_responding(standard()).unwrap();
        let setup = Setup::new();
        let url = format!("{}/mcp", fake.url());
        let files = token_files(&url, &auth.url(), Some(0));
        let key = crate::token::key(&url);
        let before = files.read(&key).unwrap();
        let held = held_lock.then(|| files.try_lock(&key).unwrap().unwrap());
        let (busy, received_busy) = std::sync::mpsc::channel();
        files.state.lock().unwrap().busy = Some(busy);
        let ended = super::wait_exchange_done(1);
        let clock = setup.clock();
        let deadline = clock.now() + Duration::from_secs(5);
        let tokens = files.clone();
        let (done, received) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            done.send(super::start(
                &url,
                tokens,
                None,
                net::LIMITS,
                &clock,
                Duration::from_secs(5),
                "0.0.0",
                None,
            ))
            .unwrap();
        });
        if held_lock {
            Deadline::after(WITHIN)
                .recv_or_fail(&received_busy, "startup refresh polls the held lock");
        } else {
            assert!(auth.await_requests(1, Deadline::after(WITHIN)));
        }
        setup.fake.advance_when_parked(
            Some(deadline),
            None,
            Duration::from_secs(5),
            &Deadline::after(WITHIN),
        );
        assert!(matches!(
            Deadline::after(WITHIN).recv_or_fail(&received, "startup deadline interrupts OAuth"),
            Err(StartError::Deadline)
        ));
        assert_eq!(
            Deadline::after(WITHIN).recv_or_fail(&ended, "the interrupted token exchange ends"),
            1
        );
        worker.join().unwrap();
        auth.release();
        assert_eq!(files.read(&key).unwrap(), before);
        assert_eq!(auth.request_count(), usize::from(!held_lock));
        assert_eq!(fake.requests().len(), 0);
        drop(held);
    }
}
#[test]
fn cancellation_ends_a_token_lock_wait_on_a_running_server() {
    use contract::mcp_tokens::TokenFiles;
    let auth = fakes::OauthServer::start(vec![]);
    let fake = ProviderServer::start_responding(standard()).unwrap();
    let setup = Setup::new();
    let url = format!("{}/mcp", fake.url());
    let files = token_files(&url, &auth.url(), None);
    let server = Arc::new(
        token_start(&setup, url.clone(), files.clone())
            .unwrap()
            .server,
    );
    let key = crate::token::key(&url);
    files.state.lock().unwrap().records.get_mut(&key).unwrap()["expires_at"] = json!(0);
    let held = files.try_lock(&key).unwrap().unwrap();
    let (busy, received_busy) = std::sync::mpsc::channel();
    files.state.lock().unwrap().busy = Some(busy);
    let ended = super::wait_exchange_done(3);
    let cancel = fakes::CancelToken::new();
    let firing = cancel.clone();
    let calling = server.clone();
    let deadline = setup.clock().now() + Duration::from_secs(60);
    let (done, received) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        done.send(calling.call("tools/call", &json!({}), Duration::from_secs(60), &firing))
            .unwrap();
    });
    Deadline::after(WITHIN).recv_or_fail(&received_busy, "the call polls the held token lock");
    assert!(
        setup
            .fake
            .await_parked(deadline, fakes::Deadline::after(WITHIN))
    );
    cancel.cancel();
    assert_eq!(
        Deadline::after(WITHIN).recv_or_fail(&received, "the call is cancelled"),
        Err(CallError::Cancelled)
    );
    assert_eq!(
        Deadline::after(WITHIN).recv_or_fail(&ended, "the cancelled exchange ends"),
        3
    );
    worker.join().unwrap();
    assert_eq!(auth.request_count(), 0);
    drop(held);
    fakes::within("remote stop", Deadline::after(WITHIN), move || {
        server.stop()
    });
}
#[test]
fn a_rejected_refresh_fails_the_start_without_changing_the_record() {
    use contract::mcp_tokens::TokenFiles;
    let auth = fakes::OauthServer::start(vec![fakes::OauthReply::raw(400, "secret")]);
    let fake = ProviderServer::start_responding(standard()).unwrap();
    let setup = Setup::new();
    let url = format!("{}/mcp", fake.url());
    let files = token_files(&url, &auth.url(), Some(0));
    let key = crate::token::key(&url);
    let before = files.read(&key).unwrap();
    assert!(matches!(
        token_start(&setup, url, files.clone()),
        Err(StartError::Login(_))
    ));
    assert_eq!(files.read(&key).unwrap(), before);
    assert_eq!(auth.request_count(), 1);
    assert!(fake.requests().is_empty());
}

/// A refresh reply of exactly `len` bytes. `closing_last` pads inside a
/// string so the JSON's closing brace is the final byte, and truncating
/// one byte breaks the JSON; otherwise the pad is trailing whitespace,
/// which still parses when truncated.
fn refresh_reply(len: usize, closing_last: bool) -> String {
    const JSON: &str =
        r#"{"access_token":"at-2","refresh_token":"rt-2","expires_in":3600,"pad":""}"#;
    if closing_last {
        let split = JSON.len() - 2;
        format!(
            "{}{}{}",
            &JSON[..split],
            " ".repeat(len - JSON.len()),
            &JSON[split..]
        )
    } else {
        format!("{JSON}{}", " ".repeat(len - JSON.len()))
    }
}

#[test]
fn a_refresh_reply_up_to_the_body_cap_renews_the_login() {
    for (name, len) in [
        ("cap_minus_one", crate::pipes::MAX_LINE - 1),
        ("at_cap", crate::pipes::MAX_LINE),
    ] {
        let auth =
            ProviderServer::start(vec![Response::status(200, refresh_reply(len, true))]).unwrap();
        let fake = ProviderServer::start_responding(standard()).unwrap();
        let setup = Setup::new();
        let url = format!("{}/mcp", fake.url());
        let opened =
            token_start(&setup, url.clone(), token_files(&url, &auth.url(), Some(0))).unwrap();
        fakes::within("remote stop", Deadline::after(WITHIN), move || {
            opened.server.stop()
        });
        assert_eq!(auth.requests().len(), 1, "row {name}: one refresh");
        let requests = fake.requests();
        assert!(
            !requests.is_empty(),
            "row {name}: the renewed login is sent"
        );
        for request in requests {
            assert_eq!(
                request.header("authorization"),
                Some(fakes::fingerprint("Bearer at-2").as_str()),
                "row {name}"
            );
        }
    }
}

#[test]
fn a_refresh_reply_one_past_the_body_cap_is_refused() {
    use contract::mcp_tokens::TokenFiles;
    let auth = ProviderServer::start(vec![Response::status(
        200,
        refresh_reply(crate::pipes::MAX_LINE + 1, false),
    )])
    .unwrap();
    let fake = ProviderServer::start_responding(standard()).unwrap();
    let setup = Setup::new();
    let url = format!("{}/mcp", fake.url());
    let files = token_files(&url, &auth.url(), Some(0));
    let key = crate::token::key(&url);
    let before = files.read(&key).unwrap();
    assert!(matches!(
        token_start(&setup, url, files.clone()),
        Err(StartError::Login(_))
    ));
    assert_eq!(files.read(&key).unwrap(), before);
    assert_eq!(auth.requests().len(), 1);
    assert!(fake.requests().is_empty());
}

#[test]
fn a_bad_url_never_sends() {
    for url in ["ws://127.0.0.1:9/mcp", "not a url", "http://"] {
        let setup = Setup::new();
        let server = ProviderServer::start([]).unwrap();
        let error = setup.start_err(url, Duration::from_secs(5));
        assert_eq!(
            error,
            StartError::StartFailed("Its `url` is not an http or https URL.".to_owned()),
            "row: {url}"
        );
        assert!(server.requests().is_empty(), "row {url}: nothing is sent");
    }
}

#[test]
fn an_uppercase_https_scheme_passes_validation() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    let url = format!("HTTPS{}", &server.url()[4..]);
    let error = setup.start_err(&format!("{url}/mcp"), Duration::from_secs(5));
    let StartError::StartFailed(message) = error else {
        panic!("a refused TLS handshake fails StartFailed, got {error:?}");
    };
    assert!(
        !message.contains("not an http or https URL"),
        "the scheme passed validation: {message}"
    );
}

#[test]
fn a_timed_out_call_shuts_its_socket_and_leaves_the_server_live() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(move |req: &Request| {
        if req.method == "DELETE" {
            return Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::status(
                200,
                result(
                    &id,
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "remote", "version": "1"},
                    }),
                ),
            )
            .header("mcp-session-id", SESSION),
            Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
            Some("tools/call") => Response::stall(200, ": still thinking\n", 1_000_000)
                .header("content-type", "text/event-stream"),
            _ => Response::status(202, ""),
        }
    })
    .unwrap();
    let (remote, _listed) = setup.start(&server, Duration::from_secs(5));
    let timeout = Duration::from_secs(60);
    let deadline = setup.fake.now().checked_add(timeout).expect("deadline");
    let waiting = Arc::clone(&remote);
    let (done, result) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let arguments = Map::new();
        let params = Named {
            arguments: &arguments,
            name: "echo",
        };
        let answer = waiting.call("tools/call", &params, timeout, &fakes::CancelToken::new());
        done.send(answer).expect("collected");
    });
    assert!(
        server.await_partial(1, Deadline::after(WITHIN)),
        "the stalled answer starts within {WITHIN:?}"
    );
    setup
        .fake
        .advance_when_parked(Some(deadline), None, timeout, &Deadline::after(WITHIN));
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| panic!("the timed-out call answers within {WITHIN:?}")),
        Err(CallError::Timeout),
    );
    assert!(
        server.await_closed(1, Deadline::after(WITHIN)),
        "the abandon shut the socket within {WITHIN:?}"
    );
    assert!(!remote.is_gone(), "a timeout is no death");
}

#[test]
fn a_cancelled_call_sends_cancelled_and_shuts_its_socket() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(move |req: &Request| {
        if req.method == "DELETE" {
            return Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::status(
                200,
                result(
                    &id,
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "remote", "version": "1"},
                    }),
                ),
            )
            .header("mcp-session-id", SESSION),
            Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
            Some("tools/call") => Response::stall(200, ": still thinking\n", 1_000_000)
                .header("content-type", "text/event-stream"),
            _ => Response::status(202, ""),
        }
    })
    .unwrap();
    let (remote, _listed) = setup.start(&server, Duration::from_secs(5));
    let timeout = Duration::from_secs(600);
    let cancel = fakes::CancelToken::new();
    let waiting = Arc::clone(&remote);
    let firing = cancel.clone();
    let (done, result) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let arguments = Map::new();
        let params = Named {
            arguments: &arguments,
            name: "echo",
        };
        let answer = waiting.call("tools/call", &params, timeout, &firing);
        done.send(answer).expect("collected");
    });
    assert!(
        server.await_partial(1, Deadline::after(WITHIN)),
        "the stalled answer starts within {WITHIN:?}"
    );
    cancel.cancel();
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| panic!("the cancelled call answers within {WITHIN:?}")),
        Err(CallError::Cancelled),
    );
    assert!(
        server.await_requests(5, Deadline::after(WITHIN)),
        "the cancel notice is POSTed within {WITHIN:?}"
    );
    let requests = server.requests();
    let cancelled = requests
        .iter()
        .find(|req| body_method(req) == "notifications/cancelled")
        .expect("a cancel notice is recorded");
    let call = requests
        .iter()
        .find(|req| body_method(req) == "tools/call")
        .expect("the call was sent");
    let call_id: Value = serde_json::from_slice(&call.body).expect("a JSON call");
    let notice: Value = serde_json::from_slice(&cancelled.body).expect("a JSON notice");
    assert_eq!(
        notice
            .get("params")
            .and_then(|params| params.get("requestId")),
        call_id.get("id"),
        "the notice names the call"
    );
    assert!(
        server.await_closed(1, Deadline::after(WITHIN)),
        "the abandon shut the socket within {WITHIN:?}"
    );
    assert!(!remote.is_gone(), "a cancel is no death");
}

#[test]
fn a_stalled_handshake_misses_its_startup_deadline_and_shuts_its_socket() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::stall(200, ": still thinking\n", 1_000_000)
        .header("content-type", "text/event-stream")])
    .unwrap();
    let url = format!("{}/mcp", server.url());
    let startup = Duration::from_secs(5);
    let deadline = setup.fake.now().checked_add(startup).expect("deadline");
    let clock = setup.clock();
    let (done, result) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let answer = super::start(
            &url,
            Arc::new(crate::test_support::FakeTokens::default()),
            None,
            net::LIMITS,
            &clock,
            startup,
            "0.0.0",
            None,
        );
        done.send(answer).expect("collected");
    });
    assert!(
        server.await_partial(1, Deadline::after(WITHIN)),
        "the stalled answer starts within {WITHIN:?}"
    );
    setup
        .fake
        .advance_when_parked(Some(deadline), None, startup, &Deadline::after(WITHIN));
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| panic!("the stalled start ends within {WITHIN:?}"))
            .err(),
        Some(StartError::Deadline),
    );
    assert!(
        server.await_closed(1, Deadline::after(WITHIN)),
        "the abandon shut the socket within {WITHIN:?}"
    );
}

#[test]
fn a_dropped_connection_a_cut_stream_and_an_overlong_body_are_deaths() {
    for (name, answer) in [
        ("dropped", Response::drop_connection()),
        (
            "cut_stream",
            Response::stream(event(
                &json!({"jsonrpc": "2.0", "id": 999, "result": {}}).to_string(),
            )),
        ),
        (
            "overlong",
            Response::status(200, "x".repeat(crate::pipes::MAX_LINE + 1)),
        ),
        (
            "exactly_max_line_without_an_answer",
            Response::status(200, "x".repeat(crate::pipes::MAX_LINE)),
        ),
    ] {
        let setup = Setup::new();
        let server = ProviderServer::start_responding({
            move |req: &Request| {
                if req.method == "DELETE" {
                    return Response::status(200, "");
                }
                let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
                let id = body.get("id").cloned().unwrap_or(Value::Null);
                match body.get("method").and_then(Value::as_str) {
                    Some("initialize") => Response::status(
                        200,
                        result(
                            &id,
                            json!({
                                "protocolVersion": "2025-06-18",
                                "capabilities": {"tools": {}},
                                "serverInfo": {"name": "remote", "version": "1"},
                            }),
                        ),
                    )
                    .header("mcp-session-id", SESSION),
                    Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
                    Some("tools/call") => answer.clone(),
                    _ => Response::status(202, ""),
                }
            }
        })
        .unwrap();
        let (remote, _listed) = setup.start(&server, Duration::from_secs(5));
        let error = setup
            .call(
                &remote,
                "tools/call",
                Duration::from_secs(60),
                &fakes::CancelToken::new(),
            )
            .expect_err("the call fails");
        assert_eq!(error, CallError::Gone, "row: {name}");
        assert!(remote.is_gone(), "row {name}: the server is gone");
    }
}

#[test]
fn a_body_one_past_max_line_is_rejected() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(move |req: &Request| {
        if req.method == "DELETE" {
            return Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::status(
                200,
                result(
                    &id,
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "remote", "version": "1"},
                    }),
                ),
            )
            .header("mcp-session-id", SESSION),
            Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
            Some("tools/call") => {
                let mut answer = result(&id, json!({"content": []}));
                answer.push_str(&" ".repeat(crate::pipes::MAX_LINE - answer.len()));
                assert_eq!(answer.len(), crate::pipes::MAX_LINE);
                answer.push(' ');
                Response::status(200, answer)
            }
            _ => Response::status(202, ""),
        }
    })
    .unwrap();
    let (remote, _listed) = setup.start(&server, Duration::from_secs(5));
    let error = setup
        .call(
            &remote,
            "tools/call",
            Duration::from_secs(60),
            &fakes::CancelToken::new(),
        )
        .expect_err("one byte past the body cap ends the exchange");
    assert_eq!(error, CallError::Gone);
    assert!(remote.is_gone(), "the overlong answer ends the server");
}

#[test]
fn a_body_at_exactly_max_line_carrying_the_answer_returns_it() {
    // `>` into `>=` fails the start here: the cap still lets a full
    // answer through.
    let setup = Setup::new();
    let server = ProviderServer::start_responding(move |req: &Request| {
        if req.method == "DELETE" {
            return Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::status(
                200,
                result(
                    &id,
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "remote", "version": "1"},
                    }),
                ),
            )
            .header("mcp-session-id", SESSION),
            Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
            Some("tools/call") => {
                let mut answer = json!({"content": [{"text": "", "type": "text"}]});
                let empty = result(&id, answer.clone());
                answer["content"][0]["text"] =
                    json!("x".repeat(crate::pipes::MAX_LINE - empty.len()));
                let padded = result(&id, answer);
                assert_eq!(padded.len(), crate::pipes::MAX_LINE);
                Response::status(200, padded)
            }
            _ => Response::status(202, ""),
        }
    })
    .unwrap();
    let (remote, _listed) = setup.start(&server, Duration::from_secs(5));
    let answered = setup
        .call(
            &remote,
            "tools/call",
            Duration::from_secs(60),
            &fakes::CancelToken::new(),
        )
        .expect("a full-size answer returns");
    let text = answered
        .get("content")
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .and_then(|part| part.get("text"))
        .and_then(Value::as_str)
        .expect("the full-size answer carries text");
    assert!(!text.is_empty());
    assert!(!remote.is_gone());
}

#[test]
fn an_answer_followed_by_a_close_is_no_death() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(move |req: &Request| {
        if req.method == "DELETE" {
            return Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::status(
                200,
                result(
                    &id,
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "remote", "version": "1"},
                    }),
                ),
            )
            .header("mcp-session-id", SESSION)
            .header("connection", "close"),
            Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
            Some("tools/call") => Response::status(
                200,
                result(&id, json!({"content": [{"type": "text", "text": "hi"}]})),
            )
            .header("connection", "close"),
            _ => Response::status(202, ""),
        }
    })
    .unwrap();
    let (remote, _listed) = setup.start(&server, Duration::from_secs(5));
    setup
        .call(
            &remote,
            "tools/call",
            Duration::from_secs(60),
            &fakes::CancelToken::new(),
        )
        .expect("the call answers");
    assert!(
        !remote.is_gone(),
        "a response that carried the answer is no death"
    );
}

#[test]
fn a_404_on_a_proven_session_expires_without_dying() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(move |req: &Request| {
        if req.method == "DELETE" {
            return Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::status(
                200,
                result(
                    &id,
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "remote", "version": "1"},
                    }),
                ),
            )
            .header("mcp-session-id", SESSION),
            // The list proves the session; the call finds it ended.
            Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
            Some("tools/call") => Response::status(404, "ended"),
            _ => Response::status(202, ""),
        }
    })
    .unwrap();
    let (remote, _listed) = setup.start(&server, Duration::from_secs(5));
    let error = setup
        .call(
            &remote,
            "tools/call",
            Duration::from_secs(60),
            &fakes::CancelToken::new(),
        )
        .expect_err("the expired call fails");
    assert_eq!(error, CallError::Expired);
    assert!(remote.is_expired(), "the session expired");
    assert!(remote.is_gone(), "an expiry reads as gone to waiters");
}

#[test]
fn a_404_before_the_session_is_proven_is_a_death() {
    // No capability to list, so no request proves the session: the first
    // request after `initialize` answers `404`.
    for (name, initialized) in [
        ("initialized_202", Response::status(202, "")),
        ("initialized_404", Response::status(404, "late")),
    ] {
        let setup = Setup::new();
        let server = ProviderServer::start_responding(move |req: &Request| {
            if req.method == "DELETE" {
                return Response::status(200, "");
            }
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let id = body.get("id").cloned().unwrap_or(Value::Null);
            match body.get("method").and_then(Value::as_str) {
                Some("initialize") => Response::status(
                    200,
                    result(
                        &id,
                        json!({
                            "protocolVersion": "2025-06-18",
                            "capabilities": {},
                            "serverInfo": {"name": "remote", "version": "1"},
                        }),
                    ),
                )
                .header("mcp-session-id", SESSION),
                Some("notifications/initialized") => initialized.clone(),
                Some("tools/call") => Response::status(404, "ended"),
                _ => Response::status(202, ""),
            }
        })
        .unwrap();
        let (remote, listed) = setup.start(&server, Duration::from_secs(5));
        assert!(listed.tools.is_empty(), "row: {name}");
        let error = setup
            .call(
                &remote,
                "tools/call",
                Duration::from_secs(60),
                &fakes::CancelToken::new(),
            )
            .expect_err("the call fails");
        assert_eq!(error, CallError::Gone, "row: {name}");
        assert!(remote.is_gone(), "row {name}: the server is gone");
        assert!(
            !remote.is_expired(),
            "row {name}: never proven, never expired"
        );
    }
}

#[test]
fn a_404_with_no_session_id_fails_only_its_request() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(move |req: &Request| {
        if req.method == "DELETE" {
            return Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::status(
                200,
                result(
                    &id,
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "remote", "version": "1"},
                    }),
                ),
            ),
            Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
            Some("tools/call") => Response::status(404, "missing"),
            _ => Response::status(202, ""),
        }
    })
    .unwrap();
    let (remote, _listed) = setup.start(&server, Duration::from_secs(5));
    let error = setup
        .call(
            &remote,
            "tools/call",
            Duration::from_secs(60),
            &fakes::CancelToken::new(),
        )
        .expect_err("the call fails");
    assert_eq!(
        error,
        CallError::Http("The server answered HTTP 404.".to_owned())
    );
    assert!(!remote.is_gone(), "without a session a 404 is no death");
}

#[test]
fn a_404_read_after_its_call_ended_marks_nothing() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(move |req: &Request| {
        if req.method == "DELETE" {
            return Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::status(
                200,
                result(
                    &id,
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "remote", "version": "1"},
                    }),
                ),
            )
            .header("mcp-session-id", SESSION),
            Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
            Some("tools/call") => Response::status(404, "ended"),
            _ => Response::status(202, ""),
        }
    })
    .unwrap();
    let (remote, _listed) = setup.start(&server, Duration::from_secs(5));
    let timeout = Duration::from_secs(60);
    let deadline = setup.fake.now().checked_add(timeout).expect("deadline");
    // Between the status read and the guarded mark, the clock passes the
    // call's deadline: the waiter times out, removes its slot and
    // abandons first, so the `404` marks nothing.
    let fake = Arc::clone(&setup.fake);
    let (caller_id, received_id) = std::sync::mpsc::channel();
    super::pause_after_status(move || {
        let caller = Deadline::after(WITHIN)
            .recv(&received_id)
            .expect("the caller reports its thread before it parks");
        fake.advance_when_parked(
            Some(deadline),
            Some(caller),
            timeout,
            &Deadline::after(WITHIN),
        );
        assert!(
            server.await_closed(1, Deadline::after(WITHIN)),
            "the abandon shut the socket within {WITHIN:?}"
        );
    });
    let waiting = Arc::clone(&remote);
    let (done, result) = std::sync::mpsc::channel();
    thread::spawn(move || {
        caller_id
            .send(std::thread::current().id())
            .expect("the hook waits for the caller thread");
        let arguments = Map::new();
        let params = Named {
            arguments: &arguments,
            name: "echo",
        };
        let answer = waiting.call("tools/call", &params, timeout, &fakes::CancelToken::new());
        done.send(answer).expect("collected");
    });
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| panic!("the timed-out call answers within {WITHIN:?}")),
        Err(CallError::Timeout),
    );
    assert!(!remote.is_gone(), "the late 404 is no death");
    assert!(!remote.is_expired(), "the late 404 is no expiry");
}

#[test]
fn a_404_to_a_pump_post_changes_nothing() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let setup = Setup::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = ProviderServer::start_responding({
        let calls = Arc::clone(&calls);
        move |req: &Request| {
            if req.method == "DELETE" {
                return Response::status(200, "");
            }
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let id = body.get("id").cloned().unwrap_or(Value::Null);
            match body.get("method").and_then(Value::as_str) {
                Some("initialize") => Response::status(
                    200,
                    result(
                        &id,
                        json!({
                            "protocolVersion": "2025-06-18",
                            "capabilities": {"tools": {}},
                            "serverInfo": {"name": "remote", "version": "1"},
                        }),
                    ),
                )
                .header("mcp-session-id", SESSION),
                Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
                Some("tools/call") => {
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        Response::stall(200, ": still thinking\n", 1_000_000)
                            .header("content-type", "text/event-stream")
                    } else {
                        Response::status(
                            200,
                            result(&id, json!({"content": [{"type": "text", "text": "hi"}]})),
                        )
                    }
                }
                // The pump's own cancel notice is answered `404` on the
                // proven session: only request exchanges act on one.
                Some("notifications/cancelled") => Response::status(404, "late"),
                _ => Response::status(202, ""),
            }
        }
    })
    .unwrap();
    let (remote, _listed) = setup.start(&server, Duration::from_secs(5));
    // Hold from the cancel notice on: the handshake and the stalled call
    // flow, the notice waits.
    server.hold_from(5, WITHIN);
    let timeout = Duration::from_secs(600);
    let cancel = fakes::CancelToken::new();
    let waiting = Arc::clone(&remote);
    let firing = cancel.clone();
    let (done, result) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let arguments = Map::new();
        let params = Named {
            arguments: &arguments,
            name: "echo",
        };
        let answer = waiting.call("tools/call", &params, timeout, &firing);
        done.send(answer).expect("collected");
    });
    assert!(
        server.await_partial(1, Deadline::after(WITHIN)),
        "the stalled answer starts within {WITHIN:?}"
    );
    cancel.cancel();
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| panic!("the cancelled call answers within {WITHIN:?}")),
        Err(CallError::Cancelled),
    );
    assert!(
        server.await_requests(5, Deadline::after(WITHIN)),
        "the cancel notice is recorded within {WITHIN:?}"
    );
    server.release();
    // The next call proves the pump read the `404` and moved on: its
    // request is recorded after the notice, and the session still holds.
    let answered = setup.call(
        &remote,
        "tools/call",
        Duration::from_secs(60),
        &fakes::CancelToken::new(),
    );
    assert!(
        answered.is_ok(),
        "the session survives the pump's 404: {answered:?}"
    );
    assert!(!remote.is_gone(), "the pump's 404 is no death");
    assert!(!remote.is_expired(), "the pump's 404 is no expiry");
}

#[test]
fn a_call_queued_behind_a_stalled_notice_is_never_sent_once_timed_out() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let setup = Setup::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = ProviderServer::start_responding({
        let calls = Arc::clone(&calls);
        move |req: &Request| {
            if req.method == "DELETE" {
                return Response::status(200, "");
            }
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let id = body.get("id").cloned().unwrap_or(Value::Null);
            match body.get("method").and_then(Value::as_str) {
                Some("initialize") => Response::status(
                    200,
                    result(
                        &id,
                        json!({
                            "protocolVersion": "2025-06-18",
                            "capabilities": {"tools": {}},
                            "serverInfo": {"name": "remote", "version": "1"},
                        }),
                    ),
                )
                .header("mcp-session-id", SESSION),
                Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
                Some("tools/call") => {
                    // Only the first call stalls; the later one answers.
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        Response::stall(200, ": still thinking\n", 1_000_000)
                            .header("content-type", "text/event-stream")
                    } else {
                        Response::status(
                            200,
                            result(&id, json!({"content": [{"type": "text", "text": "hi"}]})),
                        )
                    }
                }
                _ => Response::status(202, ""),
            }
        }
    })
    .unwrap();
    let (remote, _listed) = setup.start(&server, Duration::from_secs(5));
    // Request ids: initialize 1, tools/list 2, the first call 3. The hold
    // from the cancel notice (5) on keeps the pump on it while the second
    // call (4) waits behind.
    server.hold_from(5, WITHIN);
    let cancel = fakes::CancelToken::new();
    let first = Arc::clone(&remote);
    let firing = cancel.clone();
    let (stalled, first_result) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let arguments = Map::new();
        let params = Named {
            arguments: &arguments,
            name: "echo",
        };
        let answer = first.call("tools/call", &params, Duration::from_secs(600), &firing);
        stalled.send(answer).expect("collected");
    });
    assert!(
        server.await_partial(1, Deadline::after(WITHIN)),
        "the stalled answer starts within {WITHIN:?}"
    );
    cancel.cancel();
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&first_result)
            .unwrap_or_else(|_| panic!("the cancelled call answers within {WITHIN:?}")),
        Err(CallError::Cancelled),
    );
    assert!(
        server.await_requests(5, Deadline::after(WITHIN)),
        "the cancel notice is recorded within {WITHIN:?}"
    );
    // The second call queues behind the held notice: parked, never sent.
    let timeout = Duration::from_secs(60);
    let deadline = setup.fake.now().checked_add(timeout).expect("deadline");
    let second = Arc::clone(&remote);
    let (queued, second_result) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let arguments = Map::new();
        let params = Named {
            arguments: &arguments,
            name: "echo",
        };
        let answer = second.call("tools/call", &params, timeout, &fakes::CancelToken::new());
        queued.send(answer).expect("collected");
    });
    assert!(
        setup
            .fake
            .await_parked(deadline, fakes::Deadline::after(WITHIN)),
        "the queued call waits on its deadline within {WITHIN:?}"
    );
    assert!(
        server
            .requests()
            .iter()
            .filter(|req| body_method(req) == "tools/call")
            .count()
            == 1,
        "the queued call is never sent while held: {:?}",
        server.requests()
    );
    setup
        .fake
        .advance_when_parked(Some(deadline), None, timeout, &Deadline::after(WITHIN));
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&second_result)
            .unwrap_or_else(|_| panic!("the queued call times out within {WITHIN:?}")),
        Err(CallError::Timeout),
    );
    // The removed exchange runs once the held notice goes: installed
    // before the release, so its end is never missed.
    let finished = super::wait_exchange_done(4);
    server.release();
    // A third call proves the pump moved on past the notice and the
    // removed request: only its own request is ever sent.
    let answered = setup.call(
        &remote,
        "tools/call",
        Duration::from_secs(60),
        &fakes::CancelToken::new(),
    );
    assert!(answered.is_ok(), "the third call answers: {answered:?}");
    // The removed exchange was spawned before the third call's, so a
    // build that sends it has its request recorded before it ends: the
    // end below is the signal the absence is read after, never a span
    // of time.
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&finished)
            .unwrap_or_else(|_| panic!("the removed exchange ends within {WITHIN:?}")),
        4,
        "the removed exchange ends",
    );
    let mut ids: Vec<Value> = server
        .requests()
        .iter()
        .filter(|req| body_method(req) == "tools/call")
        .map(|req| {
            serde_json::from_slice::<Value>(&req.body)
                .expect("a JSON call")
                .get("id")
                .cloned()
                .expect("an id")
        })
        .collect();
    ids.sort_by_key(|left| left.as_u64());
    assert_eq!(
        ids,
        [json!(3), json!(5)],
        "the timed-out call was never sent"
    );
}

#[test]
fn stop_says_goodbye_only_with_a_live_session() {
    // With a session the last request is `DELETE` with its id, and the
    // stop returns without the clock moving.
    let setup = Setup::new();
    let server = ProviderServer::start_responding(standard()).unwrap();
    let (remote, _listed) = setup.start(&server, Duration::from_secs(5));
    let before = setup.fake.now();
    remote.stop();
    assert_eq!(setup.fake.now(), before, "the clock never moved");
    let requests = server.requests();
    let goodbye = requests.last().expect("a goodbye is sent");
    assert_eq!(goodbye.method, "DELETE");
    assert_eq!(goodbye.header("mcp-session-id"), Some(SESSION));
}

#[test]
fn stop_without_a_session_sends_no_goodbye() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(move |req: &Request| {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::status(
                200,
                result(
                    &id,
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "remote", "version": "1"},
                    }),
                ),
            ),
            Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
            _ => Response::status(202, ""),
        }
    })
    .unwrap();
    let (remote, _listed) = setup.start(&server, Duration::from_secs(5));
    remote.stop();
    assert!(
        server.requests().iter().all(|req| req.method != "DELETE"),
        "no goodbye without a session: {:?}",
        server.requests()
    );
}

#[test]
fn stop_after_expiry_sends_no_goodbye() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(move |req: &Request| {
        if req.method == "DELETE" {
            return Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::status(
                200,
                result(
                    &id,
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "remote", "version": "1"},
                    }),
                ),
            )
            .header("mcp-session-id", SESSION),
            Some("tools/list") => Response::status(200, result(&id, json!({"tools": []}))),
            Some("tools/call") => Response::status(404, "ended"),
            _ => Response::status(202, ""),
        }
    })
    .unwrap();
    let (remote, _listed) = setup.start(&server, Duration::from_secs(5));
    let error = setup.call(
        &remote,
        "tools/call",
        Duration::from_secs(60),
        &fakes::CancelToken::new(),
    );
    assert_eq!(error, Err(CallError::Expired));
    remote.stop();
    assert!(
        server.requests().iter().all(|req| req.method != "DELETE"),
        "no goodbye after expiry: {:?}",
        server.requests()
    );
}

#[test]
fn the_pump_does_not_delete_an_expired_session() {
    let server = ProviderServer::start_responding(|_| Response::status(200, "")).unwrap();
    let link = Arc::new(super::Link::new(
        format!("{}/mcp", server.url()),
        Arc::new(crate::test_support::FakeTokens::default()),
        Setup::new().clock(),
        None,
        net::LIMITS,
        None,
    ));
    link.set_session(SESSION.to_owned());
    let shared = Arc::new(crate::wait::Shared::default());
    shared.insert(1);
    assert!(shared.expire_if_awaiting(1), "the session is expired");
    let (incoming_sender, incoming) = std::sync::mpsc::channel();
    drop(incoming_sender);
    let (writer, _outgoing) = std::sync::mpsc::sync_channel(1);
    let writer = Arc::new(writer);
    let weak_writer = Arc::downgrade(&writer);
    let (done, finished) = std::sync::mpsc::channel();
    thread::spawn(move || {
        super::pump(incoming, link, shared, weak_writer);
        done.send(()).expect("the pump finished");
    });
    Deadline::after(WITHIN)
        .recv(&finished)
        .unwrap_or_else(|_| panic!("the pump finishes within {WITHIN:?}"));
    assert!(
        server
            .requests()
            .iter()
            .all(|request| request.method != "DELETE"),
        "expiry is not a goodbye: {:?}",
        server.requests()
    );
}

#[test]
fn link_keep_refuses_a_socket_for_a_closed_link() {
    let link = super::Link::new(
        "http://127.0.0.1/mcp".to_owned(),
        Arc::new(crate::test_support::FakeTokens::default()),
        Setup::new().clock(),
        None,
        net::LIMITS,
        None,
    );
    link.close();
    let (socket, _peer) = socket_pair();
    assert!(
        link.keep(7, &socket).is_err(),
        "closed links refuse sockets"
    );
    assert!(link.stopped(7), "closed links are stopped");
    let exchange = super::Exchange {
        id: 7,
        link: Arc::new(link),
    };
    assert!(net::Keep::is_stopped(&exchange), "the exchange is stopped");
}

#[test]
fn link_keep_refuses_a_socket_for_an_abandoned_exchange() {
    let link = super::Link::new(
        "http://127.0.0.1/mcp".to_owned(),
        Arc::new(crate::test_support::FakeTokens::default()),
        Setup::new().clock(),
        None,
        net::LIMITS,
        None,
    );
    link.abandon(7);
    let (socket, _peer) = socket_pair();
    assert!(
        link.keep(7, &socket).is_err(),
        "an abandoned exchange refuses sockets",
    );
    assert!(link.stopped(7), "an abandoned exchange is stopped");
    let exchange = super::Exchange {
        id: 7,
        link: Arc::new(link),
    };
    assert!(net::Keep::is_stopped(&exchange), "the exchange is stopped");
}

#[test]
fn link_close_shuts_every_live_socket() {
    let link = super::Link::new(
        "http://127.0.0.1/mcp".to_owned(),
        Arc::new(crate::test_support::FakeTokens::default()),
        Setup::new().clock(),
        None,
        net::LIMITS,
        None,
    );
    let (first, first_peer) = socket_pair();
    let (second, second_peer) = socket_pair();
    link.keep(1, &first).expect("the first socket is kept");
    link.keep(2, &second).expect("the second socket is kept");
    link.close();
    for (label, mut peer) in [("first", first_peer), ("second", second_peer)] {
        peer.set_read_timeout(Some(Duration::from_millis(100)))
            .expect("the read has a kernel bound");
        let (done, result) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let mut byte = [0];
            done.send(peer.read(&mut byte))
                .expect("the read is collected");
        });
        let read = Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| panic!("the {label} socket closes within {WITHIN:?}"));
        assert_eq!(
            read.expect("the peer read succeeds"),
            0,
            "{label} socket EOF"
        );
    }
}

#[test]
fn a_stop_during_a_stalled_handshake_shuts_it_down() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::stall(200, ": still thinking\n", 1_000_000)
        .header("content-type", "text/event-stream")])
    .unwrap();
    let url = format!("{}/mcp", server.url());
    let clock = setup.clock();
    let (done, result) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let answer = super::start(
            &url,
            Arc::new(crate::test_support::FakeTokens::default()),
            None,
            net::LIMITS,
            &clock,
            Duration::from_secs(5),
            "0.0.0",
            None,
        );
        done.send(answer).expect("collected");
    });
    assert!(
        server.await_partial(1, Deadline::after(WITHIN)),
        "the stalled answer starts within {WITHIN:?}"
    );
    crate::registry::stop_every_start();
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| panic!("the stopped start ends within {WITHIN:?}"))
            .err(),
        Some(StartError::StartFailed(
            "Fiber is shutting down.".to_owned()
        )),
    );
    assert!(
        server.await_closed(1, Deadline::after(WITHIN)),
        "the abandon shut the socket within {WITHIN:?}"
    );
}

#[test]
fn a_start_through_a_proxy_tunnels_to_the_server() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(standard()).unwrap();
    let proxy = fakes::ConnectProxy::start().unwrap();
    let through = ureq::Proxy::builder(ureq::ProxyProtocol::Http)
        .host("127.0.0.1")
        .port(proxy.port())
        .build()
        .unwrap();
    let url = format!("{}/mcp", server.url());
    let clock = setup.clock();
    let opened = fakes::within("the remote start", Deadline::after(WITHIN), move || {
        super::start(
            &url,
            Arc::new(crate::test_support::FakeTokens::default()),
            Some(through),
            net::LIMITS,
            &clock,
            Duration::from_secs(5),
            "0.0.0",
            None,
        )
    })
    .expect("the start tunnels");
    assert_eq!(listed_names(&opened.listed), ["echo"]);
    let port = server.url().rsplit(':').next().unwrap().to_owned();
    let target = format!("127.0.0.1:{port}");
    assert!(
        proxy.await_connects(1, Deadline::after(WITHIN)),
        "the proxy recorded CONNECT {target}"
    );
    // One tunnel per request: each exchange runs its own agent, so the
    // handshake opens three, every one to the server.
    assert_eq!(proxy.connects(), [target.clone(), target.clone(), target]);
    assert!(!server.requests().is_empty());
}

#[test]
fn the_pump_posts_in_order_before_starting_exchanges() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(standard()).unwrap();
    server.hold();
    let url = format!("{}/mcp", server.url());
    let clock = setup.clock();
    let (done, result) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let answer = super::start(
            &url,
            Arc::new(crate::test_support::FakeTokens::default()),
            None,
            net::LIMITS,
            &clock,
            Duration::from_secs(5),
            "0.0.0",
            None,
        );
        done.send(answer).expect("collected");
    });
    // One release per request: initialize first, so its answer lets the
    // pump post initialized, which waits for its own release before the
    // pump takes tools/list.
    server.release_one();
    assert!(
        server.await_requests(2, Deadline::after(WITHIN)),
        "initialize and initialized arrive within {WITHIN:?}"
    );
    assert_eq!(
        server.requests().len(),
        2,
        "tools/list waits for initialized's 202: {:?}",
        server.requests()
    );
    server.release_one();
    assert!(
        server.await_requests(3, Deadline::after(WITHIN)),
        "tools/list follows within {WITHIN:?}"
    );
    server.release_one();
    let opened = Deadline::after(WITHIN)
        .recv(&result)
        .unwrap_or_else(|_| panic!("the held start ends within {WITHIN:?}"))
        .expect("the held start opens");
    assert_eq!(listed_names(&opened.listed), ["echo"]);
}

#[test]
fn link_debug_redacts_url_secrets_and_the_session_id() {
    let link = super::Link::new(
        "https://deploy:s3cr3t-t0k3n@example.com:8443/mcp?access_token=qu3ry-t0k3n".to_owned(),
        Arc::new(crate::test_support::FakeTokens::default()),
        Setup::new().clock(),
        None,
        net::LIMITS,
        None,
    );
    link.set_session("sess-abc-123".to_owned());
    let printed = format!("{:?}", link);
    for secret in ["deploy", "s3cr3t-t0k3n", "qu3ry-t0k3n", "sess-abc-123"] {
        assert!(!printed.contains(secret), "no secret prints: {printed}");
    }
    assert!(
        printed.contains("<redacted>"),
        "a secret redacts: {printed}"
    );
    let state = super::LinkState {
        session: Some("sess-abc-123".to_owned()),
        ..Default::default()
    };
    let printed = format!("{:?}", state);
    assert!(
        !printed.contains("sess-abc-123"),
        "no session id prints: {printed}"
    );
    assert!(
        printed.contains("<redacted>"),
        "a secret redacts: {printed}"
    );
}

/// A fake answering `initialize` with `capabilities`, `tools/list` with
/// one `echo` tool, and `tools/call` with `hi`.
fn with_capabilities(capabilities: Value) -> impl Fn(&Request) -> Response + Send + Sync + 'static {
    move |req: &Request| {
        if req.method == "GET" {
            return Response::status(404, "no stream here");
        }
        if req.method == "DELETE" {
            return Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::status(
                200,
                result(
                    &id,
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": capabilities,
                        "serverInfo": {"name": "remote", "version": "1"},
                    }),
                ),
            )
            .header("mcp-session-id", SESSION),
            Some("notifications/initialized") | Some("notifications/cancelled") | None => {
                Response::status(202, "")
            }
            Some("tools/list") => Response::stream(event(&result(
                &id,
                json!({"tools": [{"name": "echo", "description": "Echoes.", "inputSchema": {"type": "object"}}]}),
            ))),
            Some("tools/call") => Response::status(
                200,
                result(&id, json!({"content": [{"type": "text", "text": "hi"}]})),
            ),
            Some(other) => panic!("unexpected method {other}"),
        }
    }
}

#[test]
fn a_server_advertising_resources_as_an_object_offers_them() {
    let setup = Setup::new();
    let server =
        ProviderServer::start_responding(with_capabilities(json!({"tools": {}, "resources": {}})))
            .unwrap();
    let (_remote, listed) = setup.start(&server, Duration::from_secs(5));
    assert_eq!(listed_names(&listed), ["echo"]);
    assert!(
        listed.resources,
        "an object resources capability offers resources",
    );
}

#[test]
fn a_non_object_resources_capability_offers_none() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(with_capabilities(
        json!({"tools": {}, "resources": true}),
    ))
    .unwrap();
    let (_remote, listed) = setup.start(&server, Duration::from_secs(5));
    assert_eq!(listed_names(&listed), ["echo"]);
    assert!(
        !listed.resources,
        "a non-object resources capability offers none",
    );
}

#[test]
fn a_server_without_the_resources_capability_offers_none() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(standard()).unwrap();
    let (_remote, listed) = setup.start(&server, Duration::from_secs(5));
    assert_eq!(listed_names(&listed), ["echo"]);
    assert!(!listed.resources, "no resources capability offers none",);
}

#[test]
fn a_streamed_elicitation_gets_exactly_one_answer() {
    // Over streamable HTTP the elicitation arrives as a stream event,
    // routed as a stdout line is: the call is held until its ask is
    // observed, so without asker forwarding no ask ever arrives and
    // this wait fails instead of the remover's decline passing. The
    // fake holds the call's answer until the elicitation's answer POST
    // arrives, so the ask is always observed first: the answer below is
    // the asker's, never the remover's.
    let setup = Setup::new();
    let elicitation = json!({
        "jsonrpc": "2.0",
        "id": "el-1",
        "method": "elicitation/create",
        "params": {
            "message": "m",
            "requestedSchema": {
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"],
            },
        },
    })
    .to_string();
    let gated = fakes::GatedHttp::start_call(
        elicitation,
        json!({"content": [{"type": "text", "text": "hi"}]}),
    );
    let remote = setup.start_at(&gated.url(), Duration::from_secs(5));
    let (ask, seen, send) = crate::elicit::tests::FakeAsk::held("a_5");
    let (done, result) = std::sync::mpsc::channel();
    {
        let remote = Arc::clone(&remote);
        thread::spawn(move || {
            let arguments = Map::new();
            let params = Named {
                arguments: &arguments,
                name: "echo",
            };
            let answer = remote.call_asking(
                "tools/call",
                &params,
                Duration::from_secs(60),
                &fakes::CancelToken::new(),
                Some(crate::elicit::Asker {
                    ask: &ask,
                    server: "remote",
                }),
            );
            done.send(answer).expect("collected");
        });
    }
    Deadline::after(WITHIN).recv_or_fail(&seen, "the elicitation's ask");
    send.send(contract::tool::Answered::Reply(
        contract::events::Answer::Text {
            text: "prod".to_owned(),
        },
    ))
    .expect("the answer sends");
    let answered = Deadline::after(WITHIN)
        .recv(&result)
        .unwrap_or_else(|_| panic!("the streamed call answers within {WITHIN:?}"))
        .expect("the streamed call succeeds");
    assert_eq!(
        answered.get("content").and_then(|content| content.get(0)),
        Some(&json!({"type": "text", "text": "hi"})),
    );
    // The pump POSTs the elicitation's answer before the fake releases
    // the call's answer, so by the time the call completes the POST is
    // recorded: no wait is needed.
    let mut answers = gated.elicitation_answers().into_iter();
    let answer = answers.next().expect("one answer to the elicitation");
    assert!(answers.next().is_none(), "exactly one answer");
    // The ask is recorded before its answer is POSTed, and the call was
    // held until the ask arrived, so the answer is the asker's `accept`
    // with the content: no wait is needed and no decline passes.
    assert_eq!(
        answer.get("result"),
        Some(&json!({"action": "accept", "content": {"name": "prod"}})),
        "asked means accept with the content",
    );
}

/// The `mcp` shape every transport line carries: the fixed purpose with
/// no identity, host or path, the first attempt, and the stamps ordered.
fn assert_mcp_shape(line: &HttpRequest) {
    assert_eq!(line.purpose(), contract::diag::Purpose::Mcp);
    assert_eq!(line.identity(), None, "no identity: {line:?}");
    assert_eq!(line.model, None, "no model: {line:?}");
    assert_eq!(line.host, None, "no host: {line:?}");
    assert_eq!(line.path, None, "no path: {line:?}");
    assert_eq!(line.attempt, 1, "every exchange sends once: {line:?}");
    if let Some(headers) = line.headers_ms {
        assert!(
            headers <= line.total_ms,
            "headers land before the end: {line:?}"
        );
    }
}

#[test]
fn an_observed_start_writes_one_mcp_line_per_post() {
    // docs/state.md, "Diagnostic logs": one line per MCP POST the transport sends.
    let setup = Setup::new();
    let server = ProviderServer::start_responding(standard()).unwrap();
    let (remote, _) = setup.observed(&server, Duration::from_secs(5));
    let lines = setup.lines();
    assert_eq!(
        lines.len(),
        3,
        "initialize, notifications/initialized, tools/list: {lines:?}"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    for (line, request) in lines.iter().zip(requests.iter()) {
        assert_mcp_shape(line);
        assert_eq!(
            line.request_bytes,
            request.body.len() as u64,
            "the posted length: {line:?}"
        );
    }
    // The stamps read the frozen fake clock: everything lands at zero.
    for line in &lines {
        assert_eq!(line.headers_ms, Some(0), "{line:?}");
        assert_eq!(line.total_ms, 0, "{line:?}");
    }
    assert_eq!(lines[0].status, Some(200));
    assert!(lines[0].response_bytes > 0, "{:?}", lines[0]);
    assert_eq!(lines[1].status, Some(202));
    assert_eq!(lines[1].response_bytes, 0, "a 202 carries no body");
    // The list streams: the counted bytes are the whole event. The id is
    // the client's, read back out of the recorded request.
    let list_id: Value = serde_json::from_slice(&requests[2].body).unwrap();
    let list_id = list_id.get("id").unwrap().clone();
    let listed = event(&result(
        &list_id,
        json!({"tools": [{"name": "echo", "description": "Echoes.", "inputSchema": {"type": "object"}}]}),
    ));
    assert_eq!(lines[2].status, Some(200));
    assert_eq!(
        lines[2].response_bytes,
        listed.len() as u64,
        "the stream's counted bytes: {:?}",
        lines[2]
    );
    let before = lines.len();
    setup
        .call(
            &remote,
            "tools/call",
            Duration::from_secs(5),
            &fakes::CancelToken::new(),
        )
        .expect("the call answers");
    let lines = setup.lines();
    // docs/state.md, "Diagnostic logs": one line per POST the transport sends, and one per GET or DELETE it sends.
    assert_eq!(lines.len(), before + 1, "one call writes one line");
    let line = lines.last().unwrap();
    assert_mcp_shape(line);
    assert_eq!(line.status, Some(200));
    let calls: Vec<_> = server
        .requests()
        .into_iter()
        .filter(|request| body_method(request) == "tools/call")
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        line.request_bytes,
        calls[0].body.len() as u64,
        "the call's posted length: {line:?}"
    );
    assert!(line.response_bytes > 0, "{line:?}");
    fakes::within(
        "remote stop",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || remote.stop(),
    );
}

#[test]
fn a_refused_exchange_writes_a_statusless_line() {
    let setup = Setup::new();
    // A port nothing listens on: the initialize POST is refused.
    let listener = TcpListener::bind("127.0.0.1:0").expect("the listener binds");
    let port = listener.local_addr().expect("the address").port();
    drop(listener);
    let url = format!("http://127.0.0.1:{port}/mcp");
    let clock = setup.clock();
    let debug = setup.sink();
    let start = fakes::within(
        "the refused start",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            super::start(
                &url,
                Arc::new(crate::test_support::FakeTokens::default()),
                None,
                net::LIMITS,
                &clock,
                Duration::from_secs(5),
                "0.0.0",
                debug,
            )
        },
    );
    assert!(start.is_err(), "a refused server never starts");
    let lines = setup.lines();
    // docs/state.md, "Diagnostic logs": a failure after the send writes one line without status or headers_ms.
    assert_eq!(lines.len(), 1, "one refused post writes one line");
    let line = &lines[0];
    assert_mcp_shape(line);
    assert_eq!(line.status, None, "no head ever came: {line:?}");
    assert_eq!(line.headers_ms, None, "{line:?}");
    assert_eq!(line.response_bytes, 0, "{line:?}");
    assert!(line.request_bytes > 0, "{line:?}");
}

#[test]
fn an_initialize_4xx_writes_the_post_and_probe_lines() {
    let setup = Setup::new();
    let server = ProviderServer::start(vec![
        Response::status(405, ""),
        Response::status(404, "gone"),
    ])
    .unwrap();
    let url = format!("{}/mcp", server.url());
    let clock = setup.clock();
    let debug = setup.sink();
    let start = fakes::within(
        "the probed start",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            super::start(
                &url,
                Arc::new(crate::test_support::FakeTokens::default()),
                None,
                net::LIMITS,
                &clock,
                Duration::from_secs(5),
                "0.0.0",
                debug,
            )
        },
    );
    assert!(start.is_err(), "a legacy-only server never starts");
    let lines = setup.lines();
    // docs/state.md, "Diagnostic logs": one line per POST the transport sends, and one per GET or DELETE it sends.
    assert_eq!(lines.len(), 2, "the post and its probe: {lines:?}");
    let post = &lines[0];
    assert_mcp_shape(post);
    assert_eq!(post.status, Some(405), "{post:?}");
    assert_eq!(post.response_bytes, 0, "the 405 body is never read");
    let posts = server.requests();
    assert_eq!(posts.len(), 2);
    assert_eq!(post.request_bytes, posts[0].body.len() as u64, "{post:?}");
    assert_eq!(posts[1].method, "GET", "the probe follows: {posts:?}");
    let probe = &lines[1];
    assert_mcp_shape(probe);
    assert_eq!(probe.status, Some(404), "{probe:?}");
    assert_eq!(probe.request_bytes, 0, "a GET posts nothing");
    assert_eq!(probe.response_bytes, 0, "only a first event counts");
}

#[test]
fn the_pump_stop_with_a_session_writes_the_delete_line() {
    let setup = Setup::new();
    let server = ProviderServer::start_responding(standard()).unwrap();
    let (remote, _) = setup.observed(&server, Duration::from_secs(5));
    let is_goodbye = |request: &HttpRequest| {
        request.status == Some(200) && request.request_bytes == 0 && request.response_bytes == 0
    };
    let before = setup
        .lines()
        .iter()
        .filter(|request| is_goodbye(request))
        .count();
    fakes::within(
        "remote stop",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || remote.stop(),
    );
    // The stop joins the pump after its goodbye, so the line is written.
    let lines = setup.lines();
    // docs/state.md, "Diagnostic logs": one line per POST the transport sends, and one per GET or DELETE it sends. In this fixture only the DELETE has an empty body and a 200 response.
    let goodbyes: Vec<_> = lines.iter().filter(|request| is_goodbye(request)).collect();
    assert_eq!(
        goodbyes.len(),
        before + 1,
        "the goodbye writes one line: {lines:?}"
    );
    let line = goodbyes.last().unwrap();
    assert_mcp_shape(line);
    assert_eq!(line.status, Some(200), "{line:?}");
    assert_eq!(line.request_bytes, 0, "a DELETE posts nothing");
    assert_eq!(line.response_bytes, 0, "the goodbye body is never read");
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|request| request.method == "DELETE")
            .count(),
        1,
        "the line is the one goodbye request"
    );
}

/// An observed token start: as [`token_start`], writing one `mcp` line
/// per request on the setup's sink.
fn token_start_observed(
    setup: &Setup,
    url: String,
    files: Arc<crate::test_support::FakeTokens>,
) -> Result<OpenServer, StartError> {
    let clock = setup.clock();
    let debug = setup.sink();
    fakes::within(
        "OAuth remote start",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            super::start(
                &url,
                files,
                None,
                net::LIMITS,
                &clock,
                Duration::from_secs(5),
                "0.0.0",
                debug,
            )
        },
    )
}

#[test]
fn a_token_refresh_post_writes_an_mcp_line() {
    let reply =
        json!({"access_token":"at-2", "refresh_token":"rt-2", "expires_in":3600}).to_string();
    let auth = ProviderServer::start(vec![Response::status(200, reply.clone())]).unwrap();
    let fake = ProviderServer::start_responding(standard()).unwrap();
    let setup = Setup::new();
    let url = format!("{}/mcp", fake.url());
    let opened = token_start_observed(&setup, url.clone(), token_files(&url, &auth.url(), Some(0)))
        .expect("the expired login renews");
    assert_eq!(auth.requests().len(), 1, "one refresh post");
    let lines = setup.lines.await_count(4);
    // The refresh runs inside the initialize exchange, before its post:
    // refresh, initialize, notifications/initialized, tools/list.
    // docs/state.md, "Diagnostic logs": one line per POST the transport sends, and one per GET or DELETE it sends.
    assert_eq!(lines.len(), 4, "{lines:?}");
    let refreshed = lines
        .iter()
        .find(|line| line.request_bytes == auth.requests()[0].body.len() as u64)
        .expect("the refresh post wrote its line");
    assert_mcp_shape(refreshed);
    assert_eq!(refreshed.status, Some(200), "{refreshed:?}");
    assert_eq!(
        refreshed.response_bytes,
        reply.len() as u64,
        "the token reply is read whole: {refreshed:?}"
    );
    for line in &lines {
        assert_mcp_shape(line);
    }
    fakes::within(
        "remote stop",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || opened.server.stop(),
    );
    // The stop's goodbye is a fifth line, with no body either way.
    let lines = setup.lines.await_count(5);
    // docs/state.md, "Diagnostic logs": one line per POST the transport sends, and one per GET or DELETE it sends.
    assert_eq!(lines.len(), 5, "{lines:?}");
    let goodbye = lines.last().unwrap();
    assert_mcp_shape(goodbye);
    assert_eq!(goodbye.status, Some(200), "{goodbye:?}");
    assert_eq!(goodbye.request_bytes, 0, "{goodbye:?}");
    assert_eq!(goodbye.response_bytes, 0, "{goodbye:?}");
}

/// A sink that never debugs: its server stays silent.
struct Quiet {
    lines: Mutex<Vec<HttpRequest>>,
}

impl DebugLog for Quiet {
    fn debugging(&self) -> bool {
        false
    }

    fn http_request(&self, request: &HttpRequest) {
        self.lines.lock().unwrap().push(request.clone());
    }
}

#[test]
fn an_info_sink_records_nothing() {
    let setup = Setup::new();
    let quiet = Arc::new(Quiet {
        lines: Mutex::new(Vec::new()),
    });
    let server = ProviderServer::start_responding(standard()).unwrap();
    let url = format!("{}/mcp", server.url());
    let clock = setup.clock();
    let debug = Some(Arc::clone(&quiet) as Arc<dyn DebugLog>);
    let opened = fakes::within(
        "the remote start",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            super::start(
                &url,
                Arc::new(crate::test_support::FakeTokens::default()),
                None,
                net::LIMITS,
                &clock,
                Duration::from_secs(5),
                "0.0.0",
                debug,
            )
        },
    )
    .expect("the remote server starts");
    let remote = Arc::new(opened.server);
    setup
        .call(
            &remote,
            "tools/call",
            Duration::from_secs(5),
            &fakes::CancelToken::new(),
        )
        .expect("the call answers");
    fakes::within(
        "remote stop",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || remote.stop(),
    );
    assert!(
        quiet.lines.lock().unwrap().is_empty(),
        "an info sink records nothing"
    );
}

#[test]
fn form_body_encodes_like_the_client_sends() {
    // The query percent-encode set with space as `+`: what `send_form`
    // would send, byte for byte.
    assert_eq!(crate::login::form_body(&[]), "");
    assert_eq!(crate::login::form_body(&[("a", "b")]), "a=b");
    assert_eq!(
        crate::login::form_body(&[("grant_type", "refresh_token"), ("space key", "a b")]),
        "grant_type=refresh_token&space+key=a+b"
    );
    assert_eq!(
        crate::login::form_body(&[("url", "http://127.0.0.1:7/mcp?x=1&y=2")]),
        "url=http%3A%2F%2F127.0.0.1%3A7%2Fmcp%3Fx%3D1%26y%3D2"
    );
    assert_eq!(
        crate::login::form_body(&[("mark", "!()*'~-._"), ("plus", "a+b"), ("snow", "☃")]),
        "mark=!()*%27~-._&plus=a%2Bb&snow=%E2%98%83"
    );
}
