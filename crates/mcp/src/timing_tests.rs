//! The `mcp.connect` connection timer (`docs/state.md`, "Diagnostic
//! logs"): one guard per actual connection attempt, from starting or
//! dialing the server until its tool list is in hand. Time comes from the
//! injected clock through a recording callable, never a second production
//! timer; every wait below names the test's one deadline, and the fake
//! clock advances only after the handshake worker is seen waiting on it.

use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::diag::{Op, Timing};
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use crate::test_support::{FakeImages, Setup, WITHIN};

/// One completed guard: the invocation's operation and tool with its
/// measured start and end on the fake clock.
struct Completion {
    op: Op,
    tool: Option<String>,
    start: Instant,
    end: Instant,
}

/// A recording `Timing`: each invocation stamps the fake clock's now,
/// each owned guard's drop stamps it again, so a test pins exactly one
/// scope per attempt and its exact length. The guard is owned, never
/// stored: after the attempt exactly one completion exists.
struct Capture {
    clock: Arc<FakeClock>,
    calls: Mutex<Vec<Op>>,
    completions: Mutex<Vec<Completion>>,
    /// Sends the first attempt's thread once, so a test learns the
    /// handshake worker without polling.
    notify: Mutex<Option<mpsc::Sender<thread::ThreadId>>>,
}

impl Capture {
    fn new(clock: Arc<FakeClock>) -> Arc<Self> {
        Arc::new(Self {
            clock,
            calls: Mutex::new(Vec::new()),
            completions: Mutex::new(Vec::new()),
            notify: Mutex::new(None),
        })
    }

    /// Sends the first attempt's thread on `tx` when it enters.
    fn notify_first(&self, tx: mpsc::Sender<thread::ThreadId>) {
        *self.notify.lock().unwrap() = Some(tx);
    }

    fn timing(self: &Arc<Self>) -> Timing {
        let me = Arc::clone(self);
        Arc::new(move |op, tool, started| {
            // The attempt never stamps its start: the callable's own read
            // is the start, so a mutant that passes a stamp fails here.
            assert!(started.is_none(), "the connection guard starts unstamped");
            let start = me.clock.now();
            let thread = thread::current().id();
            me.calls.lock().unwrap().push(op);
            if let Some(tx) = me.notify.lock().unwrap().take() {
                tx.send(thread).unwrap_or(());
            }
            let guard = Guard {
                capture: Arc::clone(&me),
                op,
                tool: tool.map(str::to_owned),
                start,
            };
            Some(Box::new(guard) as Box<dyn Send>)
        })
    }

    fn calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    /// How many `mcp.connect` scopes started: the shared callable also
    /// serves the link's `net.connect` scopes, which are not counted.
    fn mcp_calls(&self) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|op| **op == Op::McpConnect)
            .count()
    }

    fn completions(&self) -> Vec<(Op, Option<String>, Duration)> {
        self.completions
            .lock()
            .unwrap()
            .iter()
            .map(|done| {
                (
                    done.op,
                    done.tool.clone(),
                    done.end
                        .checked_duration_since(done.start)
                        .unwrap_or(Duration::ZERO),
                )
            })
            .collect()
    }
}

/// The capture's owned guard: dropping it stamps the fake clock's end.
struct Guard {
    capture: Arc<Capture>,
    op: Op,
    tool: Option<String>,
    start: Instant,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let end = self.capture.clock.now();
        self.capture.completions.lock().unwrap().push(Completion {
            op: self.op,
            tool: self.tool.clone(),
            start: self.start,
            end,
        });
    }
}

/// Starts `specs` observed on `setup`'s directories with `capture`
/// installed, on a worker, receiving the result within the deadline.
fn start_observed(
    setup: &Setup,
    specs: Vec<crate::start::ServerSpec>,
    capture: &Arc<Capture>,
) -> crate::start::Started {
    let workspace = setup.workspace();
    let artifacts = setup.artifacts();
    let cache = setup.cache();
    let clock = setup.clock();
    let images: Arc<dyn contract::images::Images> = FakeImages::ok();
    let timing = capture.timing();
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        done.send(crate::start::start_observed(
            specs,
            &workspace,
            &artifacts,
            images,
            &cache,
            &clock,
            "0.0.0",
            Some(timing),
            None,
        ))
        .unwrap_or(());
    });
    Deadline::after(WITHIN)
        .recv(&result)
        .unwrap_or_else(|_| panic!("the observed start ends within {WITHIN:?}"))
}

/// Launches one spec observed on a worker, receiving the outcome within
/// the deadline.
fn launch_observed(
    setup: &Setup,
    spec: crate::start::ServerSpec,
    capture: &Arc<Capture>,
) -> Result<crate::server::OpenServer, crate::server::StartError> {
    let workspace = setup.workspace();
    let clock = setup.clock();
    let timing = capture.timing();
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        done.send(crate::start::launch_observed(
            &spec,
            &workspace,
            &clock,
            "0.0.0",
            Some(timing),
            None,
        ))
        .unwrap_or(());
    });
    Deadline::after(WITHIN)
        .recv(&result)
        .unwrap_or_else(|_| panic!("the observed launch ends within {WITHIN:?}"))
}

/// The single completion an attempt must write: one `mcp.connect`
/// call with no tool, completed once, measured `want` on the fake clock.
/// The link's `net.connect` scopes through the same callable are not
/// counted.
fn assert_one_completion(capture: &Capture, want: Duration) {
    assert_eq!(capture.mcp_calls(), 1, "one attempt starts one guard");
    let done: Vec<_> = capture
        .completions()
        .into_iter()
        .filter(|(op, _, _)| *op == Op::McpConnect)
        .collect();
    assert_eq!(done.len(), 1, "one attempt completes once");
    assert_eq!(done[0].1, None);
    assert_eq!(done[0].2, want, "the guard spans the held list");
}

#[test]
fn an_observed_eager_stdio_start_times_one_connection() {
    let setup = Setup::with_tools(&json!([{
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {"type": "object"},
    }]));
    let capture = Capture::new(setup.fake.clone());
    let started = start_observed(&setup, vec![setup.spec("fx")], &capture);
    assert!(started.failed.is_empty());
    // The fake clock never moved, so the guard spans nothing.
    assert_one_completion(&capture, Duration::ZERO);
    setup.stop(started.servers);
}

#[test]
fn a_plain_start_records_nothing() {
    let setup = Setup::with_tools(&json!([{
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {"type": "object"},
    }]));
    let capture = Capture::new(setup.fake.clone());
    let started = setup.start(vec![setup.spec("fx")]);
    assert!(started.failed.is_empty());
    assert_eq!(capture.calls(), 0, "no callable runs without a context");
    assert!(capture.completions().is_empty());
    setup.stop(started.servers);
}

#[test]
fn parallel_servers_time_independently() {
    let setup = Setup::with_tools(&json!([{
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {"type": "object"},
    }]));
    let capture = Capture::new(setup.fake.clone());
    let started = start_observed(&setup, vec![setup.spec("a"), setup.spec("b")], &capture);
    assert!(started.failed.is_empty());
    assert_eq!(started.tools.len(), 2);
    assert_eq!(capture.mcp_calls(), 2, "each server starts its own guard");
    let done = capture.completions();
    assert_eq!(done.len(), 2, "each attempt completes once");
    for (_, tool, _) in &done {
        assert_eq!(*tool, None, "no connection names a tool");
    }
    setup.stop(started.servers);
}

#[test]
fn a_cached_server_declares_without_a_timer_until_its_first_call() {
    let setup = Setup::with_tools(&json!([{
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {"type": "object"},
    }]));
    let capture = Capture::new(setup.fake.clone());
    // The first start populates the cache.
    let first = start_observed(&setup, vec![setup.spec("fx")], &capture);
    assert!(first.failed.is_empty());
    assert_eq!(capture.mcp_calls(), 1);
    setup.stop(first.servers);
    // Declared from the cache, the second start spawns nothing.
    let second = start_observed(&setup, vec![setup.spec("fx")], &capture);
    assert!(second.failed.is_empty());
    assert_eq!(
        capture.mcp_calls(),
        1,
        "a cache-only declaration starts no guard"
    );
    assert_eq!(capture.completions().len(), 1);
    setup.stop(second.servers);
}

#[test]
fn a_required_server_with_a_cache_still_starts_timed() {
    let setup = Setup::with_tools(&json!([{
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {"type": "object"},
    }]));
    let capture = Capture::new(setup.fake.clone());
    let first = start_observed(&setup, vec![setup.spec("fx")], &capture);
    assert!(first.failed.is_empty());
    setup.stop(first.servers);
    let mut spec = setup.spec("fx");
    spec.required = true;
    let second = start_observed(&setup, vec![spec], &capture);
    assert!(second.failed.is_empty());
    assert_eq!(
        capture.mcp_calls(),
        2,
        "a required server starts even with a cache"
    );
    assert_eq!(capture.completions().len(), 2);
    setup.stop(second.servers);
}

/// How many `method` lines the fixture's request log holds.
fn logged(dir: &std::path::Path, method: &str) -> usize {
    std::fs::read_to_string(dir.join("requests.log"))
        .unwrap_or_default()
        .matches(&format!("\"method\":\"{method}\""))
        .count()
}

#[test]
fn the_connection_ends_at_the_tool_list_before_prompts() {
    let setup = Setup::with_tools(&json!([{
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {"type": "object"},
    }]));
    setup.prompts(&json!([{"name": "hello", "description": "Says hi."}]));
    let dir = setup.dir.path().to_path_buf();
    let spec = setup.spec("fx");
    let workspace = setup.workspace();
    let artifacts = setup.artifacts();
    let cache = setup.cache();
    let clock = setup.clock();
    let calls = Arc::new(Mutex::new(0_usize));
    // At the guard's drop the tool list is complete and no prompt was
    // asked for yet: the drop runs on the handshake worker between the
    // two, so the counts cannot race it.
    let at_drop = Arc::new(Mutex::new((0_usize, 0_usize)));
    let timing: Timing = {
        let calls = Arc::clone(&calls);
        let at_drop = Arc::clone(&at_drop);
        Arc::new(move |op, tool: Option<&str>, _started| {
            assert_eq!(op, Op::McpConnect);
            assert_eq!(tool, None);
            *calls.lock().unwrap() += 1;
            Some(Box::new(BoundaryProbe {
                dir: dir.clone(),
                at_drop: Arc::clone(&at_drop),
            }) as Box<dyn Send>)
        })
    };
    let images: Arc<dyn contract::images::Images> = FakeImages::ok();
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        done.send(crate::start::start_observed(
            vec![spec],
            &workspace,
            &artifacts,
            images,
            &cache,
            &clock,
            "0.0.0",
            Some(timing),
            None,
        ))
        .unwrap_or(());
    });
    let started: crate::start::Started = Deadline::after(WITHIN)
        .recv(&result)
        .unwrap_or_else(|_| panic!("the observed start ends within {WITHIN:?}"));
    assert!(started.failed.is_empty());
    assert!(
        !started.prompts.commands().is_empty(),
        "the prompts still list"
    );
    assert_eq!(*calls.lock().unwrap(), 1);
    let (tools, prompts) = *at_drop.lock().unwrap();
    assert_eq!(tools, 1, "the full tool list precedes the boundary");
    assert_eq!(prompts, 0, "no prompt is asked for before the boundary");
    setup.stop(started.servers);
}

/// Records the fixture's request counts at the connection guard's drop.
struct BoundaryProbe {
    dir: std::path::PathBuf,
    at_drop: Arc<Mutex<(usize, usize)>>,
}

impl Drop for BoundaryProbe {
    fn drop(&mut self) {
        *self.at_drop.lock().unwrap() = (
            logged(&self.dir, "tools/list"),
            logged(&self.dir, "prompts/list"),
        );
    }
}

#[test]
fn a_prompt_error_after_the_tool_list_writes_no_second_line() {
    let setup = Setup::with_tools(&json!([{
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {"type": "object"},
    }]));
    setup.prompts_raw("error");
    let capture = Capture::new(setup.fake.clone());
    let started = start_observed(&setup, vec![setup.spec("fx")], &capture);
    assert!(started.failed.is_empty());
    assert_one_completion(&capture, Duration::ZERO);
    setup.stop(started.servers);
}

#[test]
fn a_prompt_only_server_ends_at_the_known_empty_list() {
    let setup = Setup::new();
    // `error` makes the fixture prompt-only: no tools capability, and
    // `tools/list` answers "method not found".
    setup.write("tools.json", "error");
    setup.prompts(&json!([{"name": "hello", "description": "Says hi."}]));
    let capture = Capture::new(setup.fake.clone());
    let started = start_observed(&setup, vec![setup.spec("fx")], &capture);
    assert!(started.failed.is_empty());
    assert!(started.tools.is_empty(), "no tools are declared");
    assert!(
        !started.prompts.commands().is_empty(),
        "the prompts still list"
    );
    assert_eq!(
        logged(setup.dir.path(), "tools/list"),
        0,
        "no tool page is ever asked for"
    );
    assert_one_completion(&capture, Duration::ZERO);
    setup.stop(started.servers);
}

/// One fake remote MCP server answering from each request's method: the
/// handshake with one `echo` tool, `tools/call` by `calls`, `prompts/list`
/// by `prompts`, and `tools/list` paged while `pages` holds cursors to
/// hand out.
fn remote_server(
    pages: Vec<Vec<Value>>,
    prompts: Option<Vec<Value>>,
    calls: impl Fn(&fakes::Request) -> fakes::Response + Send + Sync + 'static,
) -> fakes::ProviderServer {
    fakes::ProviderServer::start_responding(move |req: &fakes::Request| {
        if req.method == "DELETE" {
            return fakes::Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        let result =
            |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => fakes::Response::status(
                200,
                result(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}, "prompts": {}},
                    "serverInfo": {"name": "remote", "version": "1"},
                })),
            )
            .header("mcp-session-id", "1868a90c"),
            Some("notifications/initialized") | Some("notifications/cancelled") | None => {
                fakes::Response::status(202, "")
            }
            Some("tools/list") => {
                let cursor = body
                    .get("params")
                    .and_then(|params| params.get("cursor"))
                    .and_then(Value::as_str);
                let (tools, next) = match cursor {
                    None if pages.len() > 1 => {
                        (pages.first().cloned().unwrap_or_default(), Some("again"))
                    }
                    _ => (pages.last().cloned().unwrap_or_default(), None),
                };
                let mut answer = json!({"tools": tools});
                if let Some(cursor) = next {
                    answer["nextCursor"] = cursor.into();
                }
                fakes::Response::status(200, result(answer))
            }
            Some("prompts/list") => match &prompts {
                Some(prompts) => fakes::Response::status(200, result(json!({"prompts": prompts}))),
                // Prompt discovery fails; the connection timer already
                // ended at the tool list.
                None => fakes::Response::status(500, "no prompts"),
            },
            Some("tools/call") => calls(req),
            Some(other) => panic!("unexpected method {other}"),
        }
    })
    .unwrap()
}

fn echo_tool() -> Value {
    json!({"name": "echo", "description": "Echoes.", "inputSchema": {"type": "object"}})
}

fn echo_call(req: &fakes::Request) -> fakes::Response {
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    fakes::Response::status(
        200,
        json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "hi"}]}})
            .to_string(),
    )
}

#[test]
fn paged_tool_lists_complete_once() {
    let setup = Setup::new();
    let fake = remote_server(
        vec![
            vec![echo_tool()],
            vec![
                json!({"name": "second", "description": "Second.", "inputSchema": {"type": "object"}}),
            ],
        ],
        Some(Vec::new()),
        echo_call,
    );
    let url = format!("{}/mcp", fake.url());
    let capture = Capture::new(setup.fake.clone());
    let open = launch_observed(&setup, setup.remote_spec("fx", &url), &capture)
        .expect("the paged server opens");
    assert_eq!(open.listed.tools.len(), 2, "both pages land");
    assert_one_completion(&capture, Duration::ZERO);
}

#[test]
fn an_empty_tool_list_completes_once() {
    let setup = Setup::new();
    let fake = remote_server(vec![Vec::new()], Some(Vec::new()), echo_call);
    let url = format!("{}/mcp", fake.url());
    let capture = Capture::new(setup.fake.clone());
    let open = launch_observed(&setup, setup.remote_spec("fx", &url), &capture)
        .expect("the empty server opens");
    assert!(open.listed.tools.is_empty());
    assert_one_completion(&capture, Duration::ZERO);
}

/// Runs one observed launch on a worker with `capture`, holding every fake
/// answer until released, and returns the worker's result receiver with the
/// attempt's thread.
fn held_launch(
    deadline: &Deadline,
    setup: &Setup,
    spec: crate::start::ServerSpec,
    capture: &Arc<Capture>,
) -> (
    mpsc::Receiver<Result<crate::server::OpenServer, crate::server::StartError>>,
    thread::ThreadId,
) {
    let (tx, thread_rx) = mpsc::channel();
    capture.notify_first(tx);
    let workspace = setup.workspace();
    let clock = setup.clock();
    let timing = capture.timing();
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        done.send(crate::start::launch_observed(
            &spec,
            &workspace,
            &clock,
            "0.0.0",
            Some(timing),
            None,
        ))
        .unwrap_or(());
    });
    (
        result,
        deadline.recv_or_fail(&thread_rx, "the attempt enters Timing"),
    )
}

#[test]
fn a_held_tool_list_measures_the_advance_and_prompts_add_nothing() {
    let test_deadline = Deadline::start();
    let setup = Setup::new();
    let fake = remote_server(
        vec![vec![echo_tool()]],
        Some(vec![json!({"name": "hello", "description": "Says hi."})]),
        echo_call,
    );
    // Every answer waits for its release, so the handshake parks once per
    // request on the fake clock.
    fake.hold();
    let url = format!("{}/mcp", fake.url());
    let spec = setup.remote_spec("fx", &url);
    let capture = Capture::new(setup.fake.clone());
    let origin = setup.fake.now();
    let until = origin
        .checked_add(spec.startup_timeout)
        .expect("startup deadline");
    let (result, worker) = held_launch(&test_deadline, &setup, spec, &capture);
    // The held `initialize`: the handshake worker waits on the fake
    // clock before it moves.
    assert!(
        fake.await_requests(1, test_deadline),
        "the initialize arrives"
    );
    let mark = setup.fake.advance_when_parked(
        Some(until),
        Some(worker),
        Duration::from_millis(5),
        &Deadline::after(test_deadline.left()),
    );
    fake.release_one();
    // The held `notifications/initialized`, then the held `tools/list`.
    assert!(
        fake.await_requests(2, test_deadline),
        "the initialized notice arrives"
    );
    assert!(
        setup
            .fake
            .await_parked_since(&mark, Some(until), test_deadline),
        "the handshake re-parks past the advance"
    );
    fake.release_one();
    assert!(
        fake.await_requests(3, test_deadline),
        "the tools/list arrives"
    );
    assert!(
        setup
            .fake
            .await_parked_since(&mark, Some(until), test_deadline),
        "the handshake waits on the held list"
    );
    fake.release_one();
    // The held `prompts/list`: the tool boundary already completed at
    // exactly the advance.
    assert!(
        fake.await_requests(4, test_deadline),
        "the prompts/list arrives"
    );
    assert!(
        setup
            .fake
            .await_parked_since(&mark, Some(until), test_deadline),
        "the handshake waits on the held prompts"
    );
    assert_one_completion(&capture, Duration::from_millis(5));
    // More time passes only after the prompts wait signal; releasing then
    // finishes the handshake without a second line.
    setup.fake.advance_when_parked(
        Some(until),
        Some(worker),
        Duration::from_millis(7),
        &Deadline::after(test_deadline.left()),
    );
    fake.release();
    let open = Deadline::after(WITHIN)
        .recv(&result)
        .unwrap_or_else(|_| panic!("the held launch ends within {WITHIN:?}"))
        .expect("the held server opens");
    assert_eq!(open.listed.tools.len(), 1);
    assert_eq!(open.listed.prompts.len(), 1);
    assert_one_completion(&capture, Duration::from_millis(5));
}

#[test]
fn a_failed_prompt_list_after_a_held_tool_list_writes_no_second_line() {
    let test_deadline = Deadline::start();
    let setup = Setup::new();
    let fake = remote_server(vec![vec![echo_tool()]], None, echo_call);
    fake.hold();
    let url = format!("{}/mcp", fake.url());
    let spec = setup.remote_spec("fx", &url);
    let capture = Capture::new(setup.fake.clone());
    let origin = setup.fake.now();
    let until = origin
        .checked_add(spec.startup_timeout)
        .expect("startup deadline");
    let (result, worker) = held_launch(&test_deadline, &setup, spec, &capture);
    assert!(
        fake.await_requests(1, test_deadline),
        "the initialize arrives"
    );
    let mark = setup.fake.advance_when_parked(
        Some(until),
        Some(worker),
        Duration::from_millis(5),
        &Deadline::after(test_deadline.left()),
    );
    fake.release_one();
    assert!(
        fake.await_requests(2, test_deadline),
        "the initialized notice arrives"
    );
    assert!(
        setup
            .fake
            .await_parked_since(&mark, Some(until), test_deadline),
        "the handshake re-parks past the advance"
    );
    fake.release_one();
    assert!(
        fake.await_requests(3, test_deadline),
        "the tools/list arrives"
    );
    assert!(
        setup
            .fake
            .await_parked_since(&mark, Some(until), test_deadline),
        "the handshake waits on the held list"
    );
    fake.release();
    let open = Deadline::after(WITHIN)
        .recv(&result)
        .unwrap_or_else(|_| panic!("the held launch ends within {WITHIN:?}"))
        .expect("the server opens without prompts");
    assert_eq!(open.listed.tools.len(), 1);
    assert!(open.listed.prompts.is_empty());
    assert_one_completion(&capture, Duration::from_millis(5));
}

#[test]
fn a_refused_guard_still_completes_once() {
    let setup = Setup::new();
    let mut spec = setup.spec("fx");
    spec.stdio_mut().run = Some(crate::start::Program {
        command: "never-runs".to_owned(),
        args: Vec::new(),
        guard: Arc::new(|| Err("the content is unapproved".to_owned())),
    });
    let capture = Capture::new(setup.fake.clone());
    let Err(error) = launch_observed(&setup, spec, &capture) else {
        panic!("the guard refuses the launch");
    };
    assert!(matches!(error, crate::server::StartError::StartFailed(_)));
    assert_one_completion(&capture, Duration::ZERO);
}

#[test]
fn a_missing_secret_still_completes_once() {
    let setup = Setup::new();
    let mut spec = setup.spec("fx");
    spec.stdio_mut()
        .secrets
        .insert("TOKEN".to_owned(), "mcp.fx.TOKEN".to_owned());
    let capture = Capture::new(setup.fake.clone());
    let Err(error) = launch_observed(&setup, spec, &capture) else {
        panic!("the missing secret fails the launch");
    };
    assert!(matches!(
        error,
        crate::server::StartError::SecretMissing { .. }
    ));
    assert_one_completion(&capture, Duration::ZERO);
}

#[test]
fn a_failed_spawn_still_completes_once() {
    let setup = Setup::new();
    let mut spec = setup.spec("fx");
    spec.stdio_mut().command = "/nonexistent-fiber-server".to_owned();
    let capture = Capture::new(setup.fake.clone());
    let Err(error) = launch_observed(&setup, spec, &capture) else {
        panic!("the failed spawn fails the launch");
    };
    assert!(matches!(error, crate::server::StartError::StartFailed(_)));
    assert_one_completion(&capture, Duration::ZERO);
}

#[test]
fn a_failed_initialize_still_completes_once() {
    let setup = Setup::new();
    let fake = fakes::ProviderServer::start_responding(|req: &fakes::Request| {
        if req.method == "DELETE" {
            return fakes::Response::status(200, "");
        }
        fakes::Response::status(500, "no handshake")
    })
    .unwrap();
    let url = format!("{}/mcp", fake.url());
    let capture = Capture::new(setup.fake.clone());
    let Err(error) = launch_observed(&setup, setup.remote_spec("fx", &url), &capture) else {
        panic!("the failed initialize fails the launch");
    };
    assert!(matches!(error, crate::server::StartError::StartFailed(_)));
    assert_one_completion(&capture, Duration::ZERO);
}

#[test]
fn a_failed_tool_list_still_completes_once() {
    let setup = Setup::new();
    let fake = fakes::ProviderServer::start_responding(|req: &fakes::Request| {
        if req.method == "DELETE" {
            return fakes::Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        let result =
            |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => fakes::Response::status(
                200,
                result(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "remote", "version": "1"},
                })),
            )
            .header("mcp-session-id", "1868a90c"),
            Some("notifications/initialized") | Some("notifications/cancelled") | None => {
                fakes::Response::status(202, "")
            }
            // The tool list fails; the guard drops on the failure path.
            Some("tools/list") => fakes::Response::status(500, "no list"),
            Some(other) => panic!("unexpected method {other}"),
        }
    })
    .unwrap();
    let url = format!("{}/mcp", fake.url());
    let capture = Capture::new(setup.fake.clone());
    let Err(error) = launch_observed(&setup, setup.remote_spec("fx", &url), &capture) else {
        panic!("the failed tool list fails the launch");
    };
    assert!(matches!(error, crate::server::StartError::StartFailed(_)));
    assert_one_completion(&capture, Duration::ZERO);
}

#[test]
fn a_missed_startup_deadline_still_completes_once() {
    let setup = Setup::new();
    // Silent: `sleep` answers nothing, so only the deadline ends the
    // start. Armed before the launch: the marker survives `exec` as
    // argv[0], so a failure below still kills the detached group.
    let marker = setup.dir.path().display().to_string();
    let watchdog = fakes::Watchdog::matching(&marker);
    let quoted = marker.replace('\'', "'\\''");
    let mut spec = setup.spec("slow");
    spec.stdio_mut().command = "/bin/bash".to_owned();
    spec.stdio_mut().args = vec!["-c".to_owned(), format!("exec -a '{quoted}' /bin/sleep 30")];
    let until = setup
        .fake
        .now()
        .checked_add(spec.startup_timeout)
        .expect("deadline");
    let capture = Capture::new(setup.fake.clone());
    let workspace = setup.workspace();
    let clock = setup.clock();
    let timing = capture.timing();
    let (done, result) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            done.send(crate::start::launch_observed(
                &spec,
                &workspace,
                &clock,
                "0.0.0",
                Some(timing),
                None,
            ))
            .unwrap_or(());
        });
        setup.fake.advance_when_parked(
            Some(until),
            None,
            spec.startup_timeout,
            &Deadline::after(WITHIN),
        );
        let words = Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| panic!("the timed-out launch ends within {WITHIN:?}"));
        let Err(error) = words else {
            panic!("the deadline fails the launch");
        };
        assert!(matches!(error, crate::server::StartError::Deadline));
        watchdog.stand_down(fakes::Deadline::after(WITHIN));
    });
    assert_one_completion(&capture, spec.startup_timeout);
}

#[test]
fn a_shutdown_during_launch_still_completes_once() {
    let setup = Setup::new();
    // Silent, so the handshake is still waiting when the stop lands.
    // Armed before the launch, as above.
    let marker = setup.dir.path().display().to_string();
    let watchdog = fakes::Watchdog::matching(&marker);
    let quoted = marker.replace('\'', "'\\''");
    let mut spec = setup.spec("slow");
    spec.stdio_mut().command = "/bin/bash".to_owned();
    spec.stdio_mut().args = vec!["-c".to_owned(), format!("exec -a '{quoted}' /bin/sleep 30")];
    let until = setup
        .fake
        .now()
        .checked_add(spec.startup_timeout)
        .expect("deadline");
    let capture = Capture::new(setup.fake.clone());
    let workspace = setup.workspace();
    let clock = setup.clock();
    let timing = capture.timing();
    let (done, result) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            done.send(crate::start::launch_observed(
                &spec,
                &workspace,
                &clock,
                "0.0.0",
                Some(timing),
                None,
            ))
            .unwrap_or(());
        });
        assert!(
            setup
                .fake
                .await_parked(until, fakes::Deadline::after(WITHIN)),
            "the launch waits on the startup deadline"
        );
        crate::registry::stop_every_start();
        let words = Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| panic!("the stopped launch ends within {WITHIN:?}"));
        let Err(error) = words else {
            panic!("the stop fails the launch");
        };
        assert!(matches!(error, crate::server::StartError::StartFailed(_)));
        watchdog.stand_down(fakes::Deadline::after(WITHIN));
    });
    assert_one_completion(&capture, Duration::ZERO);
}

use crate::slot::{Served, Slot};

/// The only slot of an observed start.
fn only_slot(started: &crate::start::Started) -> Arc<Slot> {
    let slots = started.servers.slots.lock().unwrap().clone();
    assert_eq!(slots.len(), 1);
    Arc::clone(&slots[0])
}

/// Kills the fixture child behind `slot`'s running server and waits until
/// its reader has seen the exit, bounded by the test helper's deadline.
fn kill_running(setup: &Setup, slot: &Arc<Slot>) {
    let live = match setup.serve(slot) {
        Served::Up(server, _) => server,
        Served::Failed(_) => panic!("the server runs before the kill"),
    };
    fakes::kill_pid(setup.pid(), "KILL").expect("the server dies");
    crate::test_support::await_until("the killed server to be gone", move || live.is_gone());
}

/// How many `initialize` requests the fixture log holds: one per spawn.
fn initializes(dir: &std::path::Path) -> usize {
    logged(dir, "initialize")
}

#[test]
fn first_use_times_one_and_live_reuse_times_none() {
    let setup = Setup::with_tools(&json!([{
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {"type": "object"},
    }]));
    let capture = Capture::new(setup.fake.clone());
    // The plain start populates the cache; the observed start then
    // declares from it without spawning.
    let primed = setup.start(vec![setup.spec("fx")]);
    setup.stop(primed.servers);
    let dir = setup.dir.path().to_path_buf();
    let primed_spawns = initializes(&dir);
    assert_eq!(primed_spawns, 1, "priming spawns once");
    let started = start_observed(&setup, vec![setup.spec("fx")], &capture);
    assert!(started.failed.is_empty());
    assert_eq!(
        capture.mcp_calls(),
        0,
        "declaring from the cache spawns nothing"
    );
    assert_eq!(
        initializes(&dir),
        primed_spawns,
        "no spawn for a declaration"
    );
    let slot = only_slot(&started);
    // First use starts the server under the slot's own context.
    match setup.serve(&slot) {
        Served::Up(_, records) => assert!(records.is_empty(), "a first start records nothing"),
        Served::Failed(_) => panic!("the first use starts"),
    }
    assert_eq!(capture.mcp_calls(), 1);
    assert_eq!(
        initializes(&dir),
        primed_spawns + 1,
        "one spawn for the first use"
    );
    // A live slot is reused with no new attempt.
    match setup.serve(&slot) {
        Served::Up(_, records) => assert!(records.is_empty()),
        Served::Failed(_) => panic!("the live server serves"),
    }
    assert_eq!(capture.mcp_calls(), 1, "a live slot starts no guard");
    setup.stop(started.servers);
}

#[test]
fn a_killed_server_restarts_once_then_stays_dead() {
    let setup = Setup::with_tools(&json!([{
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {"type": "object"},
    }]));
    let capture = Capture::new(setup.fake.clone());
    let started = start_observed(&setup, vec![setup.spec("fx")], &capture);
    assert!(started.failed.is_empty());
    let slot = only_slot(&started);
    let dir = setup.dir.path().to_path_buf();
    // One death: the next serve restarts, timed, with the death and the
    // readiness recorded once.
    kill_running(&setup, &slot);
    match setup.serve(&slot) {
        Served::Up(_, records) => {
            assert_eq!(records.len(), 2, "the death and the readiness");
        }
        Served::Failed(_) => panic!("the first death restarts"),
    }
    assert_eq!(capture.mcp_calls(), 2);
    assert_eq!(initializes(&dir), 2, "one start, one restart");
    // A second death is final: no further attempt, no further timer.
    kill_running(&setup, &slot);
    match setup.serve(&slot) {
        Served::Up(_, _) => panic!("the second death stays dead"),
        Served::Failed(failed) => assert!(!failed.records.is_empty()),
    }
    assert_eq!(
        capture.mcp_calls(),
        2,
        "an exhausted budget starts no guard"
    );
    assert_eq!(initializes(&dir), 2);
    // A dead slot stays dead: still no attempt.
    match setup.serve(&slot) {
        Served::Up(_, _) => panic!("the dead slot stays dead"),
        Served::Failed(_) => {}
    }
    assert_eq!(capture.mcp_calls(), 2);
    // A stopped slot stays stopped: still no attempt.
    slot.stop();
    match setup.serve(&slot) {
        Served::Up(_, _) => panic!("the stopped slot stays stopped"),
        Served::Failed(_) => {}
    }
    assert_eq!(capture.mcp_calls(), 2, "a stopped slot starts no guard");
    setup.stop(started.servers);
}

#[test]
fn a_late_death_report_during_replacement_records_nothing_extra() {
    let test_deadline = Deadline::start();
    let setup = Setup::with_tools(&json!([{
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {"type": "object"},
    }]));
    let capture = Capture::new(setup.fake.clone());
    let started = start_observed(&setup, vec![setup.spec("fx")], &capture);
    assert!(started.failed.is_empty());
    let slot = only_slot(&started);
    let old = match setup.serve(&slot) {
        Served::Up(server, _) => server,
        Served::Failed(_) => panic!("the server runs"),
    };
    kill_running(&setup, &slot);
    // The replacement holds after reserving the attempt: the pause
    // acknowledgement proves the slot is Down, so the old death report
    // below lands inside the reservation interval it claims to cover.
    // Without the hold the report could land before the replacement
    // starts or after it publishes, never exercising that interval.
    let (paused_tx, paused_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_deadline = test_deadline;
    slot.on_after_reserve(Box::new(move || {
        paused_tx.send(()).unwrap_or(());
        release_deadline.recv_or_fail(&release_rx, "the replacement release");
    }));
    let replacing = Arc::clone(&slot);
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        done.send(replacing.serve()).unwrap_or(());
    });
    test_deadline.recv_or_fail(&paused_rx, "the replacement reserves");
    let reported = slot.died(&old);
    assert!(
        reported.is_none(),
        "the late report during reservation records nothing"
    );
    release_tx.send(()).unwrap_or(());
    let served: Served = test_deadline.recv_or_fail(&result, "the replacement serve");
    let mut deaths = usize::from(reported.is_some());
    let mut readies = 0_usize;
    match &served {
        Served::Up(_, records) => {
            for record in records {
                match record {
                    contract::tool::ServerRecord::Failed(_) => deaths += 1,
                    contract::tool::ServerRecord::Ready(_) => readies += 1,
                }
            }
        }
        Served::Failed(_) => panic!("the death restarts"),
    }
    assert_eq!(deaths, 1, "one death is recorded once");
    assert_eq!(readies, 1, "the restart is recorded once");
    assert_eq!(capture.mcp_calls(), 2, "one start, one replacement");
    // The replacement serves with no further records.
    match setup.serve(&slot) {
        Served::Up(_, records) => assert!(records.is_empty()),
        Served::Failed(_) => panic!("the replacement serves"),
    }
    setup.stop(started.servers);
}

/// One lazy HTTP slot with `capture` installed and every answer held, for
/// the forced-collision tests.
fn held_lazy_slot(
    setup: &Setup,
    fake: &fakes::ProviderServer,
    capture: &Arc<Capture>,
) -> Arc<Slot> {
    let url = format!("{}/mcp", fake.url());
    let spec = setup.remote_spec("fx", &url);
    crate::slot::Slot::lazy_observed(
        spec,
        &setup.workspace(),
        &setup.cache(),
        &setup.clock(),
        "0.0.0",
        crate::cache::Cached::default(),
        Some(capture.timing()),
        None,
    )
}

#[test]
fn concurrent_first_starts_share_one_spawn_and_one_timer() {
    let test_deadline = Deadline::start();
    let setup = Setup::new();
    let fake = remote_server(vec![vec![echo_tool()]], Some(Vec::new()), echo_call);
    fake.hold();
    let capture = Capture::new(setup.fake.clone());
    let slot = held_lazy_slot(&setup, &fake, &capture);
    // The first serve launches and parks on the held list; the contender
    // then waits on the lease, signaled before anything is released.
    let first = Arc::clone(&slot);
    let (first_done, first_result) = mpsc::channel();
    thread::spawn(move || {
        first_done.send(first.serve()).unwrap_or(());
    });
    // Stepwise: each held answer goes only after it arrives, so the
    // handshake reaches its held list instead of parking on the
    // initialized acknowledgement behind the held notice.
    assert!(
        fake.await_requests(1, test_deadline),
        "the first attempt dials"
    );
    fake.release_one();
    assert!(
        fake.await_requests(2, test_deadline),
        "the initialized notice arrives"
    );
    fake.release_one();
    assert!(
        fake.await_requests(3, test_deadline),
        "the tools/list arrives"
    );
    let (wait_tx, waited) = mpsc::channel();
    slot.on_lease_wait(wait_tx);
    let second = Arc::clone(&slot);
    let (second_done, second_result) = mpsc::channel();
    thread::spawn(move || {
        second_done.send(second.serve()).unwrap_or(());
    });
    test_deadline.recv_or_fail(&waited, "the contender waits on the lease");
    fake.release();
    let first: Served = test_deadline.recv_or_fail(&first_result, "the first serve");
    let second: Served = test_deadline.recv_or_fail(&second_result, "the second serve");
    for served in [&first, &second] {
        match served {
            Served::Up(_, records) => assert!(records.is_empty()),
            Served::Failed(_) => panic!("both callers share the server"),
        }
    }
    assert_eq!(
        fake.requests()
            .iter()
            .filter(|req| http_method(req) == "initialize")
            .count(),
        1,
        "one spawn for two callers"
    );
    assert_eq!(capture.mcp_calls(), 1, "one attempt starts one guard");
    slot.stop();
}

/// The fake request's MCP method, from its JSON-RPC body.
fn http_method(req: &fakes::Request) -> &'static str {
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    match body.get("method").and_then(Value::as_str) {
        Some("initialize") => "initialize",
        Some("notifications/initialized") => "notifications/initialized",
        Some("tools/list") => "tools/list",
        Some("tools/call") => "tools/call",
        _ => "other",
    }
}

#[test]
fn stop_during_a_held_start_reaps_without_respawn() {
    let test_deadline = Deadline::start();
    let setup = Setup::new();
    let fake = remote_server(vec![vec![echo_tool()]], Some(Vec::new()), echo_call);
    fake.hold();
    let capture = Capture::new(setup.fake.clone());
    let slot = held_lazy_slot(&setup, &fake, &capture);
    let starting = Arc::clone(&slot);
    let (start_done, start_result) = mpsc::channel();
    thread::spawn(move || {
        start_done.send(starting.serve()).unwrap_or(());
    });
    assert!(fake.await_requests(1, test_deadline), "the attempt dials");
    fake.release_one();
    assert!(
        fake.await_requests(2, test_deadline),
        "the initialized notice arrives"
    );
    fake.release_one();
    assert!(
        fake.await_requests(3, test_deadline),
        "the tools/list arrives held"
    );
    // The stop waits on the lease, signaled before the release: it stops
    // the attempt's result instead of racing it.
    let (wait_tx, waited) = mpsc::channel();
    slot.on_lease_wait(wait_tx);
    let stopping = Arc::clone(&slot);
    let (stop_done, stopped) = mpsc::channel();
    thread::spawn(move || {
        stopping.stop();
        stop_done.send(()).unwrap_or(());
    });
    test_deadline.recv_or_fail(&waited, "the stop waits on the lease");
    fake.release();
    let served: Served = test_deadline.recv_or_fail(&start_result, "the held serve");
    test_deadline.recv_or_fail(&stopped, "the stop");
    match served {
        Served::Up(_, _) => {}
        Served::Failed(_) => panic!("the held attempt still opens"),
    }
    // The stop reaped the result: no later serve spawns again.
    match setup.serve(&slot) {
        Served::Up(_, _) => panic!("the stopped slot stays stopped"),
        Served::Failed(_) => {}
    }
    assert_eq!(
        fake.requests()
            .iter()
            .filter(|req| http_method(req) == "initialize")
            .count(),
        1,
        "the stop spawns nothing after the start"
    );
    assert_eq!(capture.mcp_calls(), 1);
}

/// A `Timing` whose guard records whether both slot locks were free at
/// each drop: a mutant that moves the drop under either lock fails the
/// test. The slot arrives through `cell` after construction.
fn probing_timing(
    clock: Arc<FakeClock>,
    cell: Arc<Mutex<Option<Arc<Slot>>>>,
    free: Arc<Mutex<Vec<bool>>>,
) -> Timing {
    Arc::new(move |op, tool: Option<&str>, _started| {
        assert_eq!(op, Op::McpConnect);
        assert_eq!(tool, None);
        struct Probe {
            clock: Arc<FakeClock>,
            cell: Arc<Mutex<Option<Arc<Slot>>>>,
            free: Arc<Mutex<Vec<bool>>>,
        }
        impl Drop for Probe {
            fn drop(&mut self) {
                let _end = self.clock.now();
                let free = self
                    .cell
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|slot| slot.locks_free())
                    .unwrap_or(true);
                self.free.lock().unwrap().push(free);
            }
        }
        Some(Box::new(Probe {
            clock: Arc::clone(&clock),
            cell: Arc::clone(&cell),
            free: Arc::clone(&free),
        }) as Box<dyn Send>)
    })
}

#[test]
fn no_guard_drops_with_a_slot_lock_held() {
    let setup = Setup::with_tools(&json!([{
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {"type": "object"},
    }]));
    let cell: Arc<Mutex<Option<Arc<Slot>>>> = Arc::new(Mutex::new(None));
    let free: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
    let timing = probing_timing(setup.fake.clone(), Arc::clone(&cell), Arc::clone(&free));
    // A lazy slot served observed: the success drop holds no lock.
    let lazy = crate::slot::Slot::lazy_observed(
        setup.spec("fx"),
        &setup.workspace(),
        &setup.cache(),
        &setup.clock(),
        "0.0.0",
        crate::cache::Cached::default(),
        Some(timing),
        None,
    );
    *cell.lock().unwrap() = Some(Arc::clone(&lazy));
    match setup.serve(&lazy) {
        Served::Up(_, _) => {}
        Served::Failed(_) => panic!("the lazy server starts"),
    }
    // A failing serve: the failure-path drop holds no lock either.
    let mut bad = setup.spec("doomed");
    bad.stdio_mut().command = "/nonexistent-fiber-server".to_owned();
    let failing = crate::slot::Slot::lazy_observed(
        bad,
        &setup.workspace(),
        &setup.cache(),
        &setup.clock(),
        "0.0.0",
        crate::cache::Cached::default(),
        Some(probing_timing(
            setup.fake.clone(),
            Arc::clone(&cell),
            Arc::clone(&free),
        )),
        None,
    );
    *cell.lock().unwrap() = Some(Arc::clone(&failing));
    match setup.serve(&failing) {
        Served::Up(_, _) => panic!("the bad server fails"),
        Served::Failed(_) => {}
    }
    let free = free.lock().unwrap().clone();
    assert_eq!(free.len(), 2, "both guards dropped");
    assert!(free.iter().all(|one| *one), "no drop held a lock");
    lazy.stop();
    failing.stop();
}

#[test]
fn a_remote_expiry_renews_without_spending_the_restart() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let setup = Setup::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let fake = fakes::ProviderServer::start_responding(move |req: &fakes::Request| {
        if req.method == "DELETE" {
            return fakes::Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        let result =
            |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => fakes::Response::status(
                200,
                result(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "remote", "version": "1"},
                })),
            )
            .header("mcp-session-id", "1868a90c"),
            Some("notifications/initialized") | Some("notifications/cancelled") | None => {
                fakes::Response::status(202, "")
            }
            Some("tools/list") => {
                fakes::Response::status(200, result(json!({"tools": [echo_tool()]})))
            }
            // The first call ends the session; the next echoes, the
            // one after drops the connection (a death), the next echoes
            // again (the restart lands), and the rest drop (final death).
            Some("tools/call") => {
                let call = seen.fetch_add(1, Ordering::SeqCst);
                if call == 0 {
                    fakes::Response::status(404, "ended")
                } else if call == 1 || call == 3 {
                    echo_call(req)
                } else {
                    fakes::Response::drop_connection()
                }
            }
            Some(other) => panic!("unexpected method {other}"),
        }
    })
    .unwrap();
    let url = format!("{}/mcp", fake.url());
    let capture = Capture::new(setup.fake.clone());
    let started = start_observed(&setup, vec![setup.remote_spec("fx", &url)], &capture);
    assert!(started.failed.is_empty());
    let slot = only_slot(&started);
    let tool = setup.tool(&started, "mcp__fx__echo");
    // The expiry ends the call; the renewal the next serve starts carries
    // no record and keeps the restart it replaces.
    let expired = setup.run(&tool);
    assert!(expired.error.is_some(), "the expired call fails");
    assert_eq!(capture.mcp_calls(), 1, "an expiry starts no guard");
    match setup.serve(&slot) {
        Served::Up(_, records) => assert!(records.is_empty(), "a renewal records nothing"),
        Served::Failed(_) => panic!("the expiry renews"),
    }
    assert_eq!(capture.mcp_calls(), 2, "the renewal is one attempt");
    let renewed = setup.run(&tool);
    assert!(renewed.error.is_none(), "{:?}", renewed.error);
    assert!(renewed.servers.is_empty(), "no record after the renewal");
    // The restart budget survived the renewal: a death still restarts.
    let died = setup.run(&tool);
    assert!(died.error.is_some(), "the death fails the call");
    let restarted = setup.run(&tool);
    assert!(restarted.error.is_none(), "{:?}", restarted.error);
    assert!(
        matches!(
            restarted.servers.as_slice(),
            [contract::tool::ServerRecord::Ready(_)]
        ),
        "only the readiness is recorded here: {:?}",
        restarted.servers
    );
    assert_eq!(capture.mcp_calls(), 3, "the death restarts once more");
    // A second death is final: no further attempt.
    let final_death = setup.run(&tool);
    assert!(
        final_death.error.is_some(),
        "the final death fails the call"
    );
    let dead = setup.run(&tool);
    assert!(dead.error.is_some(), "the dead slot fails");
    assert!(dead.servers.is_empty(), "nothing is recorded after death");
    assert_eq!(
        capture.mcp_calls(),
        3,
        "an exhausted budget starts no guard"
    );
    setup.stop(started.servers);
}

impl Capture {
    /// How many `op` scopes completed: the shared callable serves both
    /// the connection and the link's connector scopes.
    fn completed(&self, op: Op) -> usize {
        self.completions
            .lock()
            .unwrap()
            .iter()
            .filter(|done| done.op == op)
            .count()
    }
}

#[test]
fn observed_http_records_connect_and_net_scopes() {
    let setup = Setup::new();
    let fake = remote_server(vec![vec![echo_tool()]], Some(Vec::new()), echo_call);
    let url = format!("{}/mcp", fake.url());
    let capture = Capture::new(setup.fake.clone());
    let open = launch_observed(&setup, setup.remote_spec("fx", &url), &capture)
        .expect("the remote server opens");
    assert_eq!(open.listed.tools.len(), 1);
    assert_one_completion(&capture, Duration::ZERO);
    assert!(
        capture.completed(Op::NetConnect) >= 1,
        "the actual connector work is timed"
    );
    // Neither scope names a tool or anything else.
    for done in capture.completions.lock().unwrap().iter() {
        assert_eq!(done.tool, None, "no scope names a tool");
        assert!(
            done.op == Op::McpConnect || done.op == Op::NetConnect,
            "no scope names another operation"
        );
    }
}

#[test]
fn a_none_returning_context_stays_safe() {
    let setup = Setup::new();
    let fake = remote_server(vec![vec![echo_tool()]], Some(Vec::new()), echo_call);
    let url = format!("{}/mcp", fake.url());
    let timing: Timing = Arc::new(|_, _, _| None);
    let workspace = setup.workspace();
    let clock = setup.clock();
    let open = crate::start::launch_observed(
        &setup.remote_spec("fx", &url),
        &workspace,
        &clock,
        "0.0.0",
        Some(timing),
        None,
    )
    .expect("a context answering None still opens");
    assert_eq!(open.listed.tools.len(), 1);
}

#[test]
fn a_bad_url_fails_before_any_net_scope() {
    let setup = Setup::new();
    let capture = Capture::new(setup.fake.clone());
    let mut spec = setup.remote_spec("fx", "not-an-http-url");
    spec.startup_timeout = Duration::from_secs(5);
    let Err(error) = launch_observed(&setup, spec, &capture) else {
        panic!("the bad URL fails the launch");
    };
    assert!(matches!(error, crate::server::StartError::StartFailed(_)));
    // The attempt completes once, but no agent was ever built: building
    // one starts no scope.
    assert_one_completion(&capture, Duration::ZERO);
    assert_eq!(capture.completed(Op::NetConnect), 0);
}

#[test]
fn live_tool_calls_add_no_connect_timer() {
    let setup = Setup::new();
    let fake = remote_server(vec![vec![echo_tool()]], Some(Vec::new()), echo_call);
    let url = format!("{}/mcp", fake.url());
    let capture = Capture::new(setup.fake.clone());
    let started = start_observed(&setup, vec![setup.remote_spec("fx", &url)], &capture);
    assert!(started.failed.is_empty());
    assert_eq!(capture.mcp_calls(), 1);
    let tool = setup.tool(&started, "mcp__fx__echo");
    for _ in 0..2 {
        let out = setup.run(&tool);
        assert!(out.error.is_none(), "{:?}", out.error);
    }
    assert_eq!(
        capture.mcp_calls(),
        1,
        "calls on a live connection start no guard"
    );
    setup.stop(started.servers);
}

#[test]
fn a_remote_death_restarts_timed() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let setup = Setup::new();
    let dying = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&dying);
    let fake = fakes::ProviderServer::start_responding(move |req: &fakes::Request| {
        if req.method == "DELETE" {
            return fakes::Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        let result =
            |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => fakes::Response::status(
                200,
                result(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "remote", "version": "1"},
                })),
            )
            .header("mcp-session-id", "1868a90c"),
            Some("notifications/initialized") | Some("notifications/cancelled") | None => {
                fakes::Response::status(202, "")
            }
            Some("tools/list") => {
                fakes::Response::status(200, result(json!({"tools": [echo_tool()]})))
            }
            Some("tools/call") => {
                if flag.load(Ordering::SeqCst) {
                    fakes::Response::drop_connection()
                } else {
                    echo_call(req)
                }
            }
            Some(other) => panic!("unexpected method {other}"),
        }
    })
    .unwrap();
    let url = format!("{}/mcp", fake.url());
    let capture = Capture::new(setup.fake.clone());
    let started = start_observed(&setup, vec![setup.remote_spec("fx", &url)], &capture);
    assert!(started.failed.is_empty());
    let tool = setup.tool(&started, "mcp__fx__echo");
    let first = setup.run(&tool);
    assert!(first.error.is_none(), "{:?}", first.error);
    assert_eq!(capture.mcp_calls(), 1);
    // The death fails the call; the restart the next call starts is a
    // second timed attempt, and the server answers again.
    dying.store(true, Ordering::SeqCst);
    let failed = setup.run(&tool);
    assert!(failed.error.is_some(), "the death fails the call");
    dying.store(false, Ordering::SeqCst);
    let back = setup.run(&tool);
    assert!(back.error.is_none(), "{:?}", back.error);
    assert_eq!(capture.mcp_calls(), 2, "the restart is a timed attempt");
    assert!(
        capture.completed(Op::NetConnect) >= 1,
        "reconnects time their connector work"
    );
    setup.stop(started.servers);
}

#[test]
fn servers_keep_the_start_timing_context_for_later_loads() {
    let setup = Setup::with_tools(&json!([{
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {"type": "object"},
    }]));
    let capture = Capture::new(setup.fake.clone());
    let observed = start_observed(&setup, vec![setup.spec("fx")], &capture);
    assert!(observed.failed.is_empty());
    assert!(
        observed.servers.timing().is_some(),
        "the observed start keeps its context for Later::load"
    );
    setup.stop(observed.servers);
    let plain = setup.start(vec![setup.spec("plain")]);
    assert!(plain.failed.is_empty());
    assert!(
        plain.servers.timing().is_none(),
        "the plain start keeps no context"
    );
    setup.stop(plain.servers);
}
