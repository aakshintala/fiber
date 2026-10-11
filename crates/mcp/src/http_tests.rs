//! Remote servers over streamable HTTP, against a local fake HTTP MCP
//! server: every row names the request it asserts on from
//! [`ProviderServer::requests`], recorded before the response is sent, so
//! no row depends on arrival order.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
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
}

impl Setup {
    fn new() -> Self {
        Self {
            fake: FakeClock::new(),
        }
    }

    fn clock(&self) -> Arc<dyn Clock> {
        self.fake.clone()
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
        let opened = fakes::within("the remote start", WITHIN, move || {
            super::start(&url, None, net::LIMITS, &clock, timeout, "0.0.0")
        })
        .expect("the remote server starts");
        let OpenServer { server, listed } = opened;
        (Arc::new(server), listed)
    }

    /// Starts the remote server at `url`, expecting failure, with a
    /// deadline naming the wait.
    #[track_caller]
    fn start_err(&self, url: &str, timeout: Duration) -> StartError {
        let url = url.to_owned();
        let clock = self.clock();
        let started = fakes::within("the remote start", WITHIN, move || {
            super::start(&url, None, net::LIMITS, &clock, timeout, "0.0.0")
        });
        match started {
            Ok(_) => panic!("the remote start fails"),
            Err(error) => error,
        }
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
        server.await_requests(5, WITHIN),
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
        StartError::StartFailed("The server answered HTTP 401: it needs a login.".to_owned())
    );
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
        server.await_partial(1, WITHIN),
        "the stalled answer starts within {WITHIN:?}"
    );
    assert!(
        setup.fake.await_parked(deadline, WITHIN),
        "the caller waits on the call deadline within {WITHIN:?}"
    );
    setup.fake.advance(timeout);
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| panic!("the timed-out call answers within {WITHIN:?}")),
        Err(CallError::Timeout),
    );
    assert!(
        server.await_closed(1, WITHIN),
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
        server.await_partial(1, WITHIN),
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
        server.await_requests(5, WITHIN),
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
        server.await_closed(1, WITHIN),
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
        let answer = super::start(&url, None, net::LIMITS, &clock, startup, "0.0.0");
        done.send(answer).expect("collected");
    });
    assert!(
        server.await_partial(1, WITHIN),
        "the stalled answer starts within {WITHIN:?}"
    );
    assert!(
        setup.fake.await_parked(deadline, WITHIN),
        "the handshake waits on the startup deadline within {WITHIN:?}"
    );
    setup.fake.advance(startup);
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| panic!("the stalled start ends within {WITHIN:?}"))
            .err(),
        Some(StartError::Deadline),
    );
    assert!(
        server.await_closed(1, WITHIN),
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
                let mut padded = result(&id, json!({"content": []}));
                padded.push_str(&" ".repeat(crate::pipes::MAX_LINE - padded.len()));
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
    assert_eq!(answered.get("content"), Some(&json!([])));
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
    super::pause_after_status(move || {
        assert!(
            fake.await_parked(deadline, WITHIN),
            "the caller waits on the call deadline within {WITHIN:?}"
        );
        fake.advance(timeout);
        assert!(
            server.await_closed(1, WITHIN),
            "the abandon shut the socket within {WITHIN:?}"
        );
    });
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
        server.await_partial(1, WITHIN),
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
        server.await_requests(5, WITHIN),
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
        server.await_partial(1, WITHIN),
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
        server.await_requests(5, WITHIN),
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
        setup.fake.await_parked(deadline, WITHIN),
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
    setup.fake.advance(timeout);
    assert_eq!(
        Deadline::after(WITHIN)
            .recv(&second_result)
            .unwrap_or_else(|_| panic!("the queued call times out within {WITHIN:?}")),
        Err(CallError::Timeout),
    );
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
    // build that sends it has its request recorded by now. Poll briefly,
    // so the absence below never races a thread still starting.
    let settled = std::time::Instant::now();
    while server.requests().len() < 7 && settled.elapsed() < Duration::from_millis(500) {
        std::thread::sleep(Duration::from_millis(20));
    }
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
            None,
            net::LIMITS,
            &clock,
            Duration::from_secs(5),
            "0.0.0",
        );
        done.send(answer).expect("collected");
    });
    assert!(
        server.await_partial(1, WITHIN),
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
        server.await_closed(1, WITHIN),
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
    let opened = fakes::within("the remote start", WITHIN, move || {
        super::start(
            &url,
            Some(through),
            net::LIMITS,
            &clock,
            Duration::from_secs(5),
            "0.0.0",
        )
    })
    .expect("the start tunnels");
    assert_eq!(listed_names(&opened.listed), ["echo"]);
    let port = server.url().rsplit(':').next().unwrap().to_owned();
    let target = format!("127.0.0.1:{port}");
    assert!(
        proxy.await_connects(1, WITHIN),
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
            None,
            net::LIMITS,
            &clock,
            Duration::from_secs(5),
            "0.0.0",
        );
        done.send(answer).expect("collected");
    });
    // One release per request: initialize first, so its answer lets the
    // pump post initialized, which waits for its own release before the
    // pump takes tools/list.
    server.release_one();
    assert!(
        server.await_requests(2, WITHIN),
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
        server.await_requests(3, WITHIN),
        "tools/list follows within {WITHIN:?}"
    );
    server.release_one();
    let opened = Deadline::after(WITHIN)
        .recv(&result)
        .unwrap_or_else(|_| panic!("the held start ends within {WITHIN:?}"))
        .expect("the held start opens");
    assert_eq!(listed_names(&opened.listed), ["echo"]);
}
