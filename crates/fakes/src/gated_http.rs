//! The gated streamable-HTTP MCP fake (`docs/testing.md`, "Fakes"): a raw
//! HTTP/1.1 server whose `tools/call` (or `resources/read`) stream is gated.
//! The handshake and `tools/list` are answered as JSON, and the gated
//! stream carries the elicitation first and the gated answer only after the
//! client's POST of the elicitation's answer arrives. The ask is therefore
//! always observed before the answer lands: no scheduler order can flip the
//! elicitation to the remover's decline. Every request body is recorded in
//! arrival order.
//!
//! Accepting, head reading and every reply run on the shared HTTP core;
//! only the MCP handshake and gating sequence below is this fake's own.
//! Dropping the fake stops its serving thread: no handshake-only test
//! leaves one behind.

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use support::lock;

use serde_json::{Value, json};

use crate::http_server::{Connection, HttpServer, Limits, ReplyEnd, request::RequestOutcome};

/// The clock the fake bounds its requests on.
fn clock() -> Arc<dyn contract::clock::Clock> {
    Arc::new(crate::clock::SystemClock)
}

/// The session id the fake answers `initialize` with.
const SESSION: &str = "1868a90c";

/// Which handshake the fake answers: the `tools/call` shape lists one echo
/// tool under a tools-only capability, and the `resources/read` shape lists
/// no tools under tools and resources. The wire behaviour either test
/// asserts is unchanged from the per-test fakes this replaces.
#[derive(Clone, Copy)]
enum Shape {
    /// Serves one gated `tools/call` elicitation.
    Call,
    /// Serves one gated `resources/read` elicitation.
    Read,
}

impl Shape {
    /// The `capabilities` of the `initialize` answer.
    fn capabilities(self) -> Value {
        match self {
            Self::Call => json!({"tools": {}}),
            Self::Read => json!({"tools": {}, "resources": {}}),
        }
    }

    /// The `result` of the `tools/list` answer.
    fn listed(self) -> Value {
        match self {
            Self::Call => json!({"tools": [{
                "name": "echo",
                "description": "Echoes.",
                "inputSchema": {"type": "object"},
            }]}),
            Self::Read => json!({"tools": []}),
        }
    }

    /// What the gated stream carries, for wait names.
    // Exempt: the name only words a wait's failure message. No consumer's
    // behaviour depends on it, and a test reaches that failure only by
    // waiting out the fake's bound on the wall clock.
    #[cfg_attr(false, mutants::skip)]
    fn gated(self) -> &'static str {
        match self {
            Self::Call => "the gated call's stream",
            Self::Read => "the gated read's stream",
        }
    }
}

/// A raw streamable-HTTP fake whose gated stream carries one `elicitation`,
/// answered with `result` once the elicitation is answered. Dropping it
/// stops its serving thread.
pub struct GatedHttp {
    /// The fake's `/mcp` URL.
    url: String,
    /// Every request body, in arrival order.
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
    /// The core listening underneath: shutdown ends the serving thread.
    core: HttpServer,
    /// The serving thread, joined by drop.
    serving: Option<JoinHandle<()>>,
}

impl GatedHttp {
    /// Starts the fake serving one `tools/call` `elicitation`, answered
    /// with `answer` once the elicitation is answered.
    pub fn start_call(elicitation: String, answer: Value) -> Self {
        Self::start(elicitation, answer, Shape::Call)
    }

    /// Starts the fake serving one `resources/read` `elicitation`,
    /// answered with `contents` once the elicitation is answered.
    pub fn start_read(elicitation: String, contents: Value) -> Self {
        Self::start(elicitation, contents, Shape::Read)
    }

    /// The fake's `/mcp` URL.
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// The elicitation answer bodies: every recorded body with
    /// `id == "el-1"`, in arrival order.
    pub fn elicitation_answers(&self) -> Vec<Value> {
        // Plain data: a panicked holder has already failed its test.
        lock(&self.bodies)
            .iter()
            .filter_map(|body| serde_json::from_slice::<Value>(body).ok())
            .filter(|body| body.get("id") == Some(&json!("el-1")))
            .collect()
    }

    /// Binds the fake and serves `shape` on its own thread: the port
    /// accepts connections when this returns.
    #[allow(
        clippy::expect_used,
        reason = "a fake that cannot bind a loopback port has nothing to run; a test using it fails without it"
    )]
    fn start(elicitation: String, result: Value, shape: Shape) -> Self {
        let core = HttpServer::bind(clock(), Limits::default()).expect("the fake binds");
        let url = format!("http://{}/mcp", core.addr());
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let serving_core = core.clone();
        let recorded = Arc::clone(&bodies);
        let serving = std::thread::Builder::new()
            .name("fakes-gated-http".to_owned())
            .spawn(move || {
                serve(&serving_core, recorded, elicitation, result, shape);
            })
            .expect("the fake serves");
        Self {
            url,
            bodies,
            core,
            serving: Some(serving),
        }
    }
}

impl Drop for GatedHttp {
    // Exempt: removing the drop still closes the port through the core
    // field's own drop, which runs the same shutdown; the explicit drop
    // only reaps the already-stopping serving thread within the shutdown
    // bound, which no consumer observes.
    #[cfg_attr(false, mutants::skip)]
    fn drop(&mut self) {
        // Shutdown ends the serving thread's current wait; joining it
        // reaps it within the core's bound, so no handshake-only test
        // leaves a thread behind.
        self.core.shutdown();
        if let Some(serving) = self.serving.take() {
            crate::http_server::join_within(serving, crate::http_server::SHUTDOWN_WITHIN);
        }
    }
}

/// One request: its stream, method and body. `None` when shutdown ended
/// the wait: the serving thread exits with it.
#[allow(
    clippy::panic,
    reason = "a fake's serving thread; a failure is the test's, named by the request it waited for"
)]
fn accept(
    core: &HttpServer,
    bodies: &Arc<Mutex<Vec<Vec<u8>>>>,
    what: &str,
) -> Option<(Connection, String, Vec<u8>)> {
    let mut conn = core.accept_expected(what)?;
    let request = match crate::http_server::request::read_request(&mut conn) {
        RequestOutcome::Complete(request) => request,
        RequestOutcome::Eof => {
            panic!("the gated HTTP fake's client closed during {what}")
        }
        RequestOutcome::Invalid(why) => {
            panic!("the gated HTTP fake could not read {what}: {why}")
        }
        RequestOutcome::Stopped => return None,
    };
    lock(bodies).push(request.body.clone());
    Some((conn, request.method, request.body))
}

/// Answers one request: the status, headers and whole body, then the
/// connection closes. False when shutdown ended the write: the serving
/// thread exits with it.
fn respond(conn: &mut Connection, status: u16, headers: &[(&str, &str)], body: &[u8]) -> bool {
    let mut head = format!("HTTP/1.1 {status} Fake\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    ));
    conn.reply_full(head.as_bytes(), body, None) == ReplyEnd::Done
}

/// Serves one gated elicitation: the handshake, the gated stream carrying
/// the elicitation, the elicitation's answer, then anything else until the
/// goodbye `DELETE`. Shutdown ends every wait stopped, and the serving
/// thread exits with it.
fn serve(
    core: &HttpServer,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
    elicitation: String,
    result: Value,
    shape: Shape,
) {
    // The handshake: initialize as JSON with a session id,
    // notifications `202`, `tools/list` as JSON.
    let Some((mut stream, _, body)) = accept(core, &bodies, "the initialize request") else {
        return;
    };
    let id = body_id(&body);
    let initialized = answer(
        &id,
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": shape.capabilities(),
            "serverInfo": {"name": "remote", "version": "1"},
        }),
    );
    if !respond(
        &mut stream,
        200,
        &[
            ("content-type", "application/json"),
            ("mcp-session-id", SESSION),
        ],
        initialized.as_bytes(),
    ) {
        return;
    }
    let Some((mut stream, _, _)) = accept(core, &bodies, "the notifications request") else {
        return;
    };
    if !respond(&mut stream, 202, &[], b"") {
        return;
    }
    let Some((mut stream, _, body)) = accept(core, &bodies, "the tools/list request") else {
        return;
    };
    let id = body_id(&body);
    let listed = answer(&id, shape.listed());
    if !respond(
        &mut stream,
        200,
        &[("content-type", "application/json")],
        listed.as_bytes(),
    ) {
        return;
    }
    // The gated stream: the elicitation first, held open. The gated
    // answer goes out only after the client's POST of the elicitation's
    // answer arrives below.
    let Some((mut stream, _, gated_body)) = accept(core, &bodies, shape.gated()) else {
        return;
    };
    let gated_id = body_id(&gated_body);
    if !stream.write_all(
        b"HTTP/1.1 200 Fake\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
        shape.gated(),
    ) {
        return;
    }
    if !stream.write_all(
        format!("event: message\ndata: {elicitation}\n\n").as_bytes(),
        shape.gated(),
    ) {
        return;
    }
    // The next POST is the elicitation's answer: `202` it, then send
    // the gated answer when it accepts, or close unanswered when it
    // declines (without asker forwarding the remover declines, and
    // the ask wait below fails the test first).
    let Some((mut answered_conn, _, body)) = accept(core, &bodies, "the elicitation's answer")
    else {
        return;
    };
    let answered: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !respond(&mut answered_conn, 202, &[], b"") {
        return;
    }
    if answered.get("result") == Some(&json!({"action": "accept", "content": {"name": "prod"}})) {
        let done = answer(&gated_id, result);
        if !stream.write_all(
            format!("event: message\ndata: {done}\n\n").as_bytes(),
            shape.gated(),
        ) {
            return;
        }
    }
    drop(stream);
    // Drain until goodbye: anything else gets `202`, `DELETE` ends
    // the fake with `200`.
    loop {
        let Some((mut stream, method, _)) = accept(core, &bodies, "the goodbye DELETE") else {
            return;
        };
        if method == "DELETE" {
            respond(&mut stream, 200, &[], b"");
            return;
        }
        respond(&mut stream, 202, &[], b"");
    }
}

/// The `id` of a JSON-RPC body, `Null` when it has none.
fn body_id(body: &[u8]) -> Value {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("id").cloned())
        .unwrap_or(Value::Null)
}

/// One JSON-RPC answer string.
fn answer(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

#[cfg(test)]
#[path = "gated_http_tests.rs"]
mod tests;
