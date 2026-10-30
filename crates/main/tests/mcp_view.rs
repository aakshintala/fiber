//! Binary-level tests of the per-server MCP driver commands
//! (`docs/testing.md`, "Levels"; `docs/mcp.md`, "Servers, their state and
//! the per-server commands"): a session lists a ready, a failed and a
//! needs-login server with their states; `mcp_reconnect` on the failed one
//! reconnects it; `mcp_login` on the needs-login one completes a login
//! against a fake authorization server and the server becomes ready. Every
//! run carries a wall-clock deadline, and every child runs in its own
//! process group under a watchdog.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Request, Response, Watchdog};
use serde_json::{Value, json};
use support::{KillGroup, Socket, spawn_watched, until, until_close, write_json};

/// A driven session: the child, its socket client and the test setup.
/// The child's stderr streams over a channel: the login writes its
/// authorization URL there before it waits on the loopback callback, so
/// waiting for that line waits for the login itself.
struct Driver {
    child: Child,
    guard: KillGroup,
    watchdog: Watchdog,
    stderr: mpsc::Receiver<String>,
    overheard: Vec<String>,
    client: Socket,
    setup: support::Setup,
    /// Every line the startup wait read, including the ones its check
    /// read past: a `full` subscribe replays the log, so a server's
    /// failure may already sit here before the test's first query.
    startup: Vec<Value>,
}

impl Driver {
    /// Starts `fiber session` with `path` as its `PATH` and subscribes
    /// `full`. The login test passes an empty directory, so no opener
    /// can start; the others pass the process's own `PATH`, so the
    /// fixture servers can run.
    fn start(setup: support::Setup, id: &str, path: &std::ffi::OsStr) -> Self {
        let workspace = setup.workspace();
        let mut command = setup.fiber(&[
            "session",
            "--id",
            id,
            "--workspace",
            workspace.to_str().unwrap(),
        ]);
        command.env("PATH", path);
        let (mut child, watchdog) = spawn_watched(&mut command);
        let guard = KillGroup(child.id());
        let stdout = child.stdout.take().unwrap();
        let stderr_pipe = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let (tx, stderr) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stderr_pipe).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let first = match setup.deadline.recv(&lines) {
            Ok(first) => first,
            Err(error) => {
                let child = match child.try_wait() {
                    Ok(None) => String::from("still running"),
                    Ok(Some(status)) => format!("exited with {status}"),
                    Err(_) => String::from("unknown"),
                };
                let stderr: Vec<String> = stderr.try_iter().collect();
                let snap = home_snapshot(&setup.home());
                match error {
                    std::sync::mpsc::RecvTimeoutError::Timeout => {
                        let servers = configured_servers(&setup.home());
                        let report = startup_timeout_report(&servers, &child, &stderr, &snap);
                        panic!("{report}");
                    }
                    std::sync::mpsc::RecvTimeoutError::Disconnected => {
                        let sections = startup_timeout_sections(&stderr, &snap);
                        panic!(
                            "waited until the deadline for fiber_started: Disconnected\n{sections}"
                        );
                    }
                }
            }
        };
        serde_json::from_str::<Value>(&first).unwrap();
        let client = Socket::connect(setup.deadline, &setup.session_socket(id));
        client.send(r#"{"id":"sub","command":"subscribe","args":{"level":"full"}}"#);
        // The startup wait returns once both `clients` and
        // `extensions_loaded` have arrived, in any order.
        let mut seen_clients = false;
        let mut seen_extensions = false;
        let startup = until(&client, "startup", |line| {
            if line["kind"] == "clients" {
                seen_clients = true;
            }
            if line["kind"] == "extensions_loaded" {
                seen_extensions = true;
            }
            seen_clients && seen_extensions
        });
        Self {
            child,
            guard,
            watchdog,
            stderr,
            overheard: Vec::new(),
            client,
            setup,
            startup,
        }
    }

    /// The next child-stderr line matching `done`, each taking what
    /// remains of the test's deadline: expiry panics naming `what`.
    /// Earlier lines stay in `overheard` for `finish`'s failure report.
    #[track_caller]
    fn stderr_until(&mut self, what: &str, mut done: impl FnMut(&str) -> bool) -> String {
        loop {
            let line = self.setup.deadline.recv_or_fail(&self.stderr, what);
            let hit = done(&line);
            self.overheard.push(line.clone());
            if hit {
                return line;
            }
        }
    }

    /// Sends a driver command with `args` and returns its acknowledgement.
    #[track_caller]
    fn command(&self, id: &str, command: &str, args: Value) -> Value {
        let line = json!({"id": id, "command": command, "args": args}).to_string();
        self.client.send(&line);
        support::recv_answer(&self.client, id, &format!("the {command} acknowledgement"))
    }

    /// The `mcp_servers` answer's rows.
    #[track_caller]
    fn servers(&self, id: &str) -> Vec<Value> {
        let answer = self.command(id, "mcp_servers", json!({}));
        assert_eq!(answer["kind"], "command_accepted", "{answer:?}");
        answer["payload"]["result"]["servers"]
            .as_array()
            .unwrap_or_else(|| panic!("no servers array in {answer:?}"))
            .clone()
    }

    /// The row for `name` in `rows`.
    #[track_caller]
    fn row(rows: &[Value], name: &str) -> Value {
        rows.iter()
            .find(|row| row["name"] == name)
            .unwrap_or_else(|| panic!("no row for {name} in {rows:?}"))
            .clone()
    }

    /// Closes the session and asserts it left nothing behind.
    #[track_caller]
    fn finish(mut self) -> String {
        self.client.send(r#"{"id":"close","command":"close"}"#);
        until_close(&self.client);
        let group = self.child.id();
        let mut child = self.child;
        let (tx, done) = mpsc::channel();
        thread::spawn(move || {
            tx.send(child.wait()).unwrap();
        });
        let status = self.setup.deadline.recv(&done).unwrap().unwrap();
        // The stderr sender hangs up after the child's last line: each
        // receive takes what remains of the test's deadline, and the
        // hangup ends the drain.
        loop {
            match self.setup.deadline.recv(&self.stderr) {
                Ok(line) => self.overheard.push(line),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(error) => panic!("the child's stderr never closed: {error:?}"),
            }
        }
        let stderr = self.overheard.join("\n");
        assert!(status.success(), "{stderr}");
        assert!(!fakes::group_alive(group));
        std::mem::forget(self.guard);
        self.watchdog
            .stand_down(self.setup.deadline.cleanup_phase());
        stderr
    }
}

/// Installs the fake provider with model `fake/m` and writes the global
/// configuration with that model and `servers` under `mcp.servers`.
/// Returns the model server, held for the session's lifetime.
fn configure(setup: &support::Setup, servers: &Value) -> ProviderServer {
    let model = ProviderServer::start_with_fallback([], support::hello()).unwrap();
    setup.provider(&model);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "mcp": {"servers": servers}}),
    );
    model
}

/// Writes the fixture server's directory for one echo tool.
fn fixture_dir(setup: &support::Setup, name: &str) -> PathBuf {
    let dir = setup.root.path().join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("tools.json"),
        json!([{
            "name": "echo",
            "description": "Echoes.",
            "inputSchema": {"type": "object"},
            "annotations": {"readOnlyHint": true},
        }])
        .to_string(),
    )
    .unwrap();
    dir
}

/// A fake OAuth authorization server and remote MCP server on one port:
/// the probe at `/mcp` answers 401 with the metadata header, the
/// documents, `/register` and `/token` serve the flow, and POSTs to
/// `/mcp` run the handshake and the tool list.
fn oauth_and_mcp() -> ProviderServer {
    ProviderServer::start_responding(|req: &Request| {
        let base = format!("http://{}", req.header("host").unwrap_or_default());
        if req.method == "GET" && req.path == "/mcp" {
            return Response::status(401, "{}").header(
                "www-authenticate",
                &format!("Bearer resource_metadata=\"{base}/prm\""),
            );
        }
        if req.method == "GET" && req.path == "/prm" {
            return Response::status(200, json!({"authorization_servers": [base]}).to_string());
        }
        if req.method == "GET" && req.path == "/.well-known/oauth-authorization-server" {
            return Response::status(
                200,
                json!({
                    "issuer": base,
                    "authorization_endpoint": format!("{base}/authorize"),
                    "token_endpoint": format!("{base}/token"),
                    "registration_endpoint": format!("{base}/register"),
                    "code_challenge_methods_supported": ["S256"],
                })
                .to_string(),
            );
        }
        if req.method == "POST" && req.path == "/register" {
            return Response::status(200, json!({"client_id": "c1"}).to_string());
        }
        if req.method == "POST" && req.path == "/token" {
            return Response::status(
                200,
                json!({
                    "access_token": "at-1",
                    "refresh_token": "rt-1",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                })
                .to_string(),
            );
        }
        if req.method == "GET" {
            return Response::status(404, "no stream here");
        }
        if req.method == "DELETE" {
            return Response::status(200, "");
        }
        // The JSON-RPC route needs the login's token: without it the
        // server answers 401 with the metadata header, exactly as the
        // probe does, so the session records needs-login and the row
        // carries the login sentence until the token lands. The fake
        // records credential values as fingerprints, so the check
        // compares against the token's fingerprint.
        if req.header("authorization") != Some(fakes::fingerprint("Bearer at-1")).as_deref() {
            return Response::status(401, "{}").header(
                "www-authenticate",
                &format!("Bearer resource_metadata=\"{base}/prm\""),
            );
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        let answer =
            |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::status(
                200,
                answer(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "remote", "version": "1"},
                })),
            )
            .header("mcp-session-id", "remote-1"),
            Some("notifications/initialized") | Some("notifications/cancelled") | None => {
                Response::status(202, "")
            }
            Some("tools/list") => Response::status(
                200,
                answer(json!({"tools": [{
                    "name": "echo",
                    "description": "Echoes.",
                    "inputSchema": {"type": "object"},
                    "annotations": {"readOnlyHint": true},
                }]})),
            ),
            Some(other) => panic!("unexpected MCP method {other}"),
        }
    })
    .unwrap()
}

/// The value of `name` in `url`'s query, percent-decoded.
fn query_param(url: &str, name: &str) -> String {
    url.split_once('?')
        .unwrap()
        .1
        .split('&')
        .find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key == name).then(|| percent_decode(value))
        })
        .unwrap_or_default()
}

fn percent_decode(text: &str) -> String {
    let mut bytes = text.bytes();
    let mut out = Vec::with_capacity(text.len());
    while let Some(byte) = bytes.next() {
        match byte {
            b'+' => out.push(b' '),
            b'%' => {
                let mut ahead = bytes.clone();
                let hex = |next: Option<u8>| next.and_then(|b| char::from(b).to_digit(16));
                match (hex(ahead.next()), hex(ahead.next())) {
                    (Some(high), Some(low)) => {
                        if let Ok(value) = u8::try_from(high * 16 + low) {
                            out.push(value);
                            bytes = ahead;
                        } else {
                            out.push(b'%');
                        }
                    }
                    _ => out.push(b'%'),
                }
            }
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether `line` is the remote server's first start failing as
/// not-logged-in: the signal the login test's first `mcp_servers` query
/// waits for, so it never reads the `connecting` row of a start still in
/// flight (`docs/mcp.md`, "Servers, their state and the per-server
/// commands"). It names the server and the reason, so no other server's
/// failure releases the wait early.
fn remote_not_logged_in(line: &Value) -> bool {
    line["kind"] == "mcp_server_failed"
        && line["payload"]["server"] == "remote"
        && line["payload"]["reason"] == "not_logged_in"
}

/// Sends one OAuth callback to the loopback redirect in `auth_url`.
fn drive_callback(auth_url: &str) {
    let redirect = query_param(auth_url, "redirect_uri");
    let state = query_param(auth_url, "state");
    assert!(!redirect.is_empty(), "no redirect_uri in {auth_url}");
    assert!(!state.is_empty(), "no state in {auth_url}");
    let (_, rest) = redirect.split_once("://").unwrap();
    let (host, path) = rest.split_once('/').unwrap();
    let mut callback = TcpStream::connect(host).unwrap();
    callback
        .write_all(
            format!("GET /{path}?code=abc&state={state} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
    let mut reply = String::new();
    callback.read_to_string(&mut reply).unwrap();
}

#[test]
fn new_with_mcp_fixture_warms_before_the_deadline_starts() {
    // The first exec of a freshly linked binary can stall for seconds on
    // macOS, so the fixture-starting setup warms the fixture through the
    // shared once-per-process warm before `Setup::new` starts the test's
    // deadline. This pins only the changed helper: it fails when the
    // `warm_up` call is removed or when the warm moves after the setup.
    // Ordering contract: docs/testing/fakes.md:163-165.
    let source = include_str!("support/mod.rs");
    let warmer = source.find("fn warm_once").expect("warm_once");
    let warmed = source.find("impl Setup").expect("impl Setup");
    let warm_up = source.find("fakes::warm_up").expect("warm_up");
    assert!(warmer < warm_up, "warm_once runs fakes::warm_up");
    assert!(warm_up < warmed, "warm_once runs fakes::warm_up");
    let helper = source
        .find("fn new_with_mcp_fixture")
        .expect("new_with_mcp_fixture");
    let warm = source
        .match_indices("warm_once(&fakes")
        .map(|(at, _)| at)
        .find(|&at| at > helper)
        .expect("new_with_mcp_fixture warms");
    let started = source
        .match_indices("Self::new()")
        .map(|(at, _)| at)
        .find(|&at| at > helper)
        .expect("new_with_mcp_fixture starts the deadline");
    assert!(warm < started, "new_with_mcp_fixture warms first");
}

#[test]
fn list_shows_a_ready_a_failed_and_a_needs_login_server() {
    let setup = support::Setup::new_with_mcp_fixture();
    let dir = fixture_dir(&setup, "fx");
    let remote = oauth_and_mcp();
    let _model = configure(
        &setup,
        &json!({
            "down": {"command": "/no/such/command"},
            "remote": {"url": format!("{}/mcp", remote.url())},
            "up": {
                "command": "/bin/bash",
                "args": [fakes::mcp_fixture().display().to_string(), dir.display().to_string()],
            },
        }),
    );
    let driver = Driver::start(
        setup,
        &doors::mint("s_"),
        &std::env::var_os("PATH").unwrap(),
    );

    // The session settles every server before it records their startup
    // failures together (`loop::mcp_servers_started`), so once both
    // `mcp_server_failed` lines are observed the `up` row is ready too,
    // never the `connecting` of a start still in flight: an initial
    // ready emits no event of its own (`crates/mcp/src/slot.rs` records
    // `Ready` only on restart), and the failures are written only after
    // every start reported. A `full` subscribe replays the log, so either
    // failure may already sit in the startup lines the startup wait read
    // past; one wait covers both, in either order.
    let mut seen_down = driver
        .startup
        .iter()
        .any(|line| line["kind"] == "mcp_server_failed" && line["payload"]["server"] == "down");
    let mut seen_remote = driver.startup.iter().any(remote_not_logged_in);
    if !(seen_down && seen_remote) {
        until(
            &driver.client,
            "both failing servers to record their first-start failures",
            |line| {
                seen_down = seen_down
                    || (line["kind"] == "mcp_server_failed" && line["payload"]["server"] == "down");
                seen_remote = seen_remote || remote_not_logged_in(line);
                seen_down && seen_remote
            },
        );
    }
    let rows = driver.servers("c_list");

    let names: Vec<&str> = rows
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["down", "remote", "up"], "{rows:?}");
    let down = Driver::row(&rows, "down");
    assert_eq!(down["layer"], "global", "{down:?}");
    assert_eq!(down["state"], "failed", "{down:?}");
    assert!(!down["reason"].as_str().unwrap().is_empty(), "{down:?}");
    assert_eq!(down["tools"], 0, "{down:?}");
    assert_eq!(down["approved"], true, "{down:?}");
    assert!(down.get("auth_url").is_none(), "{down:?}");
    let remote = Driver::row(&rows, "remote");
    assert_eq!(remote["layer"], "global", "{remote:?}");
    assert_eq!(remote["state"], "needs-login", "{remote:?}");
    assert_eq!(
        remote["reason"],
        json!(
            "The MCP server `remote` failed to start: The server answered HTTP 401 and needs a login. Run `fiber mcp login remote` to log in."
        ),
        "{remote:?}"
    );
    assert_eq!(remote["approved"], true, "{remote:?}");
    assert_eq!(remote["tools"], 0, "{remote:?}");
    assert!(remote.get("auth_url").is_none(), "{remote:?}");
    let up = Driver::row(&rows, "up");
    assert_eq!(up["layer"], "global", "{up:?}");
    assert_eq!(up["state"], "ready", "{up:?}");
    assert_eq!(up["reason"], "", "{up:?}");
    assert_eq!(up["tools"], 1, "{up:?}");
    assert_eq!(up["approved"], true, "{up:?}");
    assert!(up.get("auth_url").is_none(), "{up:?}");
    driver.finish();
}

#[test]
fn reconnect_restarts_the_failed_server() {
    let setup = support::Setup::new_with_mcp_fixture();
    let dir = fixture_dir(&setup, "fx");
    let marker = setup.root.path().join("marker");
    let wrapper = setup.root.path().join("flaky.sh");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/bash\nif [ ! -f \"{}\" ]; then exit 1; fi\nexec \"{}\" \"{}\"\n",
            marker.display(),
            fakes::mcp_fixture().display(),
            dir.display()
        ),
    )
    .unwrap();
    let _model = configure(
        &setup,
        &json!({"flaky": {"command": "/bin/bash", "args": [wrapper.display().to_string()]}}),
    );
    let driver = Driver::start(
        setup,
        &doors::mint("s_"),
        &std::env::var_os("PATH").unwrap(),
    );

    let rows = driver.servers("c_before");
    assert_eq!(Driver::row(&rows, "flaky")["state"], "failed", "{rows:?}");

    // The declaration is unchanged; the next start succeeds.
    fs::write(&marker, "now").unwrap();
    let reconnected = driver.command("c_reconnect", "mcp_reconnect", json!({"server": "flaky"}));
    assert_eq!(reconnected["kind"], "command_accepted", "{reconnected:?}");

    let rows = driver.servers("c_after");
    let flaky = Driver::row(&rows, "flaky");
    assert_eq!(flaky["state"], "ready", "{rows:?}");
    assert_eq!(flaky["tools"], 1, "{rows:?}");
    driver.finish();
}

#[test]
fn login_against_a_fake_authorization_server_makes_the_server_ready() {
    let setup = support::Setup::new();
    let empty = setup.root.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let remote = oauth_and_mcp();
    let _model = configure(
        &setup,
        &json!({"remote": {"url": format!("{}/mcp", remote.url())}}),
    );
    let mut driver = Driver::start(setup, &doors::mint("s_"), empty.as_os_str());

    // The session starts its servers before the test's `full` subscribe
    // replays the log, so the failure may already sit in the startup
    // lines the startup wait read past: only when it does not, wait for
    // its live line. Either way the failure is recorded before the first
    // query, so the row reads `needs-login`, never the `connecting` of a
    // start still in flight.
    if !driver.startup.iter().any(remote_not_logged_in) {
        until(
            &driver.client,
            "the remote server to fail its first start as not logged in",
            remote_not_logged_in,
        );
    }
    let rows = driver.servers("c_before");
    let before = Driver::row(&rows, "remote");
    assert_eq!(before["state"], "needs-login", "{rows:?}");
    assert!(before.get("auth_url").is_none(), "{rows:?}");

    driver.client.send(
        &json!({"id": "c_login", "command": "mcp_login", "args": {"server": "remote"}}).to_string(),
    );
    // The login writes its authorization URL on the child's stderr
    // before it waits on the loopback callback: the line proves the
    // wait and carries the callback's address, so one `mcp_servers`
    // query after it sees the row's `auth_url`.
    let auth_url = driver.stderr_until("the login to present its authorization URL", |line| {
        line.contains("/authorize?")
    });
    let rows = driver.servers("c_wait");
    assert_eq!(
        Driver::row(&rows, "remote")
            .get("auth_url")
            .and_then(Value::as_str),
        Some(auth_url.as_str()),
        "{rows:?}"
    );
    // One login in flight per server: a second one is refused.
    let busy = driver.command("c_login_again", "mcp_login", json!({"server": "remote"}));
    assert_eq!(busy["kind"], "command_rejected", "{busy:?}");
    assert_eq!(busy["payload"]["code"], "busy", "{busy:?}");
    drive_callback(&auth_url);
    // The login thread delivers the reconnect and answers only after
    // its acknowledgement: its answer proves the server restarted, so
    // one `mcp_servers` query after it sees the ready row.
    let logged_in = support::recv_answer(&driver.client, "c_login", "the login acknowledgement");
    assert_eq!(logged_in["kind"], "command_accepted", "{logged_in:?}");
    let rows = driver.servers("c_ready");
    let row = Driver::row(&rows, "remote");
    assert_eq!(row["state"], "ready", "{rows:?}");
    assert!(row.get("auth_url").is_none(), "{rows:?}");
    assert_eq!(row["tools"], 1, "{rows:?}");
    driver.finish();
}

#[test]
fn a_login_during_a_permission_hold_is_accepted_despite_the_busy_reconnect() {
    // The turn holds on the shell call's permission ask, where the
    // loop reads its inbox: it is busy while the login thread
    // delivers its reconnect, which the turn rejects as busy. The
    // login still succeeded, so its acknowledgement is an acceptance;
    // a mutant answering the rejection would refuse it. Headless
    // sessions allow shell without asking, so the standing ask rule
    // holds the turn (`docs/permissions.md`, "Headless").
    let setup = support::Setup::new();
    let empty = setup.root.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let remote = oauth_and_mcp();
    let model = ProviderServer::start_with_fallback(
        [support::stream(&[support::function_call(
            "call_1",
            "shell",
            &json!({"command": "echo hi"}),
        )])],
        support::hello(),
    )
    .unwrap();
    setup.provider(&model);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "mcp": {"servers": {"remote": {"url": format!("{}/mcp", remote.url())}}}}),
    );
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "echo"})
        ),
    )
    .unwrap();
    let mut driver = Driver::start(setup, &doors::mint("s_"), empty.as_os_str());
    driver.client.send(
        &json!({"id": "c_prompt", "command": "prompt", "args": {"content": [{"type": "text", "text": "hi"}]}}).to_string(),
    );
    let mut request = String::new();
    for line in support::until(&driver.client, "the shell permission request", |line| {
        line["kind"] == "permission_requested"
    }) {
        if line["kind"] == "permission_requested" {
            request = line["payload"]["request_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
        }
    }
    assert!(
        !request.is_empty(),
        "the permission ask carries its request id"
    );
    driver.client.send(
        &json!({"id": "c_login", "command": "mcp_login", "args": {"server": "remote"}}).to_string(),
    );
    let auth_url = driver.stderr_until("the login to present its authorization URL", |line| {
        line.contains("/authorize?")
    });
    drive_callback(&auth_url);
    let logged_in = support::recv_answer(&driver.client, "c_login", "the login acknowledgement");
    assert_eq!(logged_in["kind"], "command_accepted", "{logged_in:?}");
    driver.client.send(
        &json!({"id": "c_allow", "command": "reply", "args": {"request_id": request, "decision": "allow"}}).to_string(),
    );
    support::until(&driver.client, "the turn to complete", |line| {
        line["kind"] == "turn_completed"
    });
    driver.finish();
}

#[test]
fn a_login_whose_server_is_removed_before_its_callback_is_rejected() {
    // Dropping the server and reloading before driving the callback
    // makes the delivered reconnect unknown: the acknowledgement is
    // a rejection, not an acceptance. A mutant accepting every
    // rejection would accept it.
    let setup = support::Setup::new();
    let empty = setup.root.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let remote = oauth_and_mcp();
    let _model = configure(
        &setup,
        &json!({"remote": {"url": format!("{}/mcp", remote.url())}}),
    );
    let mut driver = Driver::start(setup, &doors::mint("s_"), empty.as_os_str());
    driver.client.send(
        &json!({"id": "c_login", "command": "mcp_login", "args": {"server": "remote"}}).to_string(),
    );
    let auth_url = driver.stderr_until("the login to present its authorization URL", |line| {
        line.contains("/authorize?")
    });
    write_json(
        &driver.setup.home().join("config.json"),
        &json!({"model": "fake/m", "mcp": {"servers": {}}}),
    );
    let reloaded = driver.command("c_reload", "reload", json!({}));
    assert_eq!(reloaded["kind"], "command_accepted", "{reloaded:?}");
    drive_callback(&auth_url);
    let logged_in = support::recv_answer(&driver.client, "c_login", "the login acknowledgement");
    assert_eq!(logged_in["kind"], "command_rejected", "{logged_in:?}");
    assert_eq!(
        logged_in["payload"]["code"], "invalid_arguments",
        "{logged_in:?}"
    );
    driver.finish();
}

#[test]
fn reconnect_of_an_unknown_server_is_invalid_arguments() {
    let setup = support::Setup::new();
    let _model = configure(&setup, &json!({}));
    let driver = Driver::start(
        setup,
        &doors::mint("s_"),
        &std::env::var_os("PATH").unwrap(),
    );

    let rejected = driver.command("c_nope", "mcp_reconnect", json!({"server": "nope"}));
    assert_eq!(rejected["kind"], "command_rejected", "{rejected:?}");
    assert_eq!(
        rejected["payload"]["code"], "invalid_arguments",
        "{rejected:?}"
    );
    let refused = driver.command("c_login_nope", "mcp_login", json!({"server": "nope"}));
    assert_eq!(refused["kind"], "command_rejected", "{refused:?}");
    assert_eq!(
        refused["payload"]["code"], "invalid_arguments",
        "{refused:?}"
    );
    driver.finish();
}

#[test]
fn reconnect_of_an_unapproved_repository_server_gives_the_approve_advice() {
    let setup = support::Setup::new();
    let workspace = setup.workspace();
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(&workspace)
        .status()
        .unwrap();
    assert!(status.success());
    let dir = fixture_dir(&setup, "fx");
    let _model = configure(&setup, &json!({}));
    fs::create_dir_all(workspace.join(".fiber")).unwrap();
    fs::write(
        workspace.join(".fiber/config.json"),
        json!({"mcp": {"servers": {"db": {
            "command": "/bin/bash",
            "args": ["srv/run.sh", dir.display().to_string()],
        }}}})
        .to_string(),
    )
    .unwrap();
    let driver = Driver::start(
        setup,
        &doors::mint("s_"),
        &std::env::var_os("PATH").unwrap(),
    );

    let rows = driver.servers("c_list");
    let db = Driver::row(&rows, "db");
    assert_eq!(db["layer"], "repository", "{db:?}");
    assert_eq!(db["approved"], false, "{db:?}");

    let rejected = driver.command("c_reconnect", "mcp_reconnect", json!({"server": "db"}));
    assert_eq!(rejected["kind"], "command_rejected", "{rejected:?}");
    assert_eq!(
        rejected["payload"]["code"], "mcp_server_unapproved",
        "{rejected:?}"
    );
    assert!(
        rejected["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("fiber approve"),
        "{rejected:?}"
    );
    driver.finish();
}

#[test]
fn two_quick_reconnects_spawn_the_server_once() {
    let setup = support::Setup::new_with_mcp_fixture();
    let dir = fixture_dir(&setup, "fx");
    let marker = setup.root.path().join("marker");
    let spawns = setup.root.path().join("spawns");
    let wrapper = setup.root.path().join("flaky.sh");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/bash\nprintf 'spawn\\n' >> \"{}\"\nif [ ! -f \"{}\" ]; then exit 1; fi\nexec \"{}\" \"{}\"\n",
            spawns.display(),
            marker.display(),
            fakes::mcp_fixture().display(),
            dir.display(),
        ),
    )
    .unwrap();
    let _model = configure(
        &setup,
        &json!({
            "flaky": {
                "command": "/bin/bash",
                "args": [wrapper.display().to_string()],
                "env": {"PATH": std::env::var_os("PATH").unwrap().to_str().unwrap()},
            },
            "up": {"command": fakes::mcp_fixture().display().to_string(), "args": [dir.display().to_string()]},
        }),
    );
    let driver = Driver::start(
        setup,
        &doors::mint("s_"),
        &std::env::var_os("PATH").unwrap(),
    );

    let rows = driver.servers("c_before");
    assert_eq!(Driver::row(&rows, "flaky")["state"], "failed", "{rows:?}");

    // The declaration is unchanged; the next start succeeds.
    fs::write(&marker, "now").unwrap();
    let first = driver.command("c_one", "mcp_reconnect", json!({"server": "flaky"}));
    assert_eq!(first["kind"], "command_accepted", "{first:?}");
    let second = driver.command("c_two", "mcp_reconnect", json!({"server": "flaky"}));
    assert_eq!(second["kind"], "command_accepted", "{second:?}");

    let rows = driver.servers("c_after");
    let flaky = Driver::row(&rows, "flaky");
    assert_eq!(flaky["state"], "ready", "{rows:?}");
    // The restart touched only the server it named: the other server
    // never left ready.
    assert_eq!(Driver::row(&rows, "up")["state"], "ready", "{rows:?}");
    assert_eq!(
        fs::read_to_string(&spawns).unwrap().lines().count(),
        2,
        "the failed start plus one restart spawn"
    );
    driver.finish();
}

/// The session runtime directories under the test home that a stalled
/// startup may have reached: `run` holds session sockets, `cache/mcp`
/// holds cached server lists, `logs` holds diagnostic logs. Names only:
/// nothing here creates or writes.
struct HomeSnapshot {
    run: Vec<String>,
    cache_mcp: Vec<String>,
    logs: Vec<String>,
}

/// The state the startup wait had observed when it expired: every
/// stderr line so far, showing the first 30 with the total when there
/// are more, then the runtime snapshot. An empty stderr names itself,
/// so silence reads as silence instead of as a missing section.
fn startup_timeout_sections(stderr: &[String], snap: &HomeSnapshot) -> String {
    const SHOWN: usize = 30;
    let mut sections = if stderr.is_empty() {
        String::from("stderr (0 lines):\n(no stderr received)")
    } else {
        let mut section = format!("stderr ({} lines):", stderr.len());
        for line in stderr.iter().take(SHOWN) {
            section.push('\n');
            section.push_str(line);
        }
        if stderr.len() > SHOWN {
            section.push_str(&format!(
                "\n... (truncated: showing first {SHOWN} of {} lines)",
                stderr.len()
            ));
        }
        section
    };
    for (name, entries) in [
        ("run", &snap.run),
        ("cache/mcp", &snap.cache_mcp),
        ("logs", &snap.logs),
    ] {
        sections.push_str(&format!("\n{name}: [{}]", entries.join(", ")));
    }
    sections
}

/// The `fiber_started` timeout's diagnostic: the head plus the state
/// sections. The Timeout branch panics with this; the Disconnected
/// branch keeps today's head text and appends the same sections.
fn startup_timeout_report(
    servers: &[String],
    child: &str,
    stderr: &[String],
    snap: &HomeSnapshot,
) -> String {
    let servers = if servers.is_empty() {
        String::from("no servers configured")
    } else {
        format!("servers [{}]", servers.join(", "))
    };
    format!(
        "waited until the deadline for fiber_started with {servers}; child {child}\n{}",
        startup_timeout_sections(stderr, snap)
    )
}

/// The configured server names in `home/config.json`: the keys of
/// `mcp.servers`. Infallible: a missing file, a directory, unreadable
/// or non-UTF-8 bytes, invalid JSON, or a missing or non-object `mcp`
/// or `mcp.servers` all read as no servers. Sorted, so the report is
/// stable whatever order the file lists them in.
fn configured_servers(home: &Path) -> Vec<String> {
    let bytes = match fs::read(home.join("config.json")) {
        Ok(bytes) => bytes,
        Err(_) => return Vec::new(),
    };
    let config: Value = match serde_json::from_slice(&bytes) {
        Ok(config) => config,
        Err(_) => return Vec::new(),
    };
    let servers = match config
        .get("mcp")
        .and_then(|mcp| mcp.get("servers"))
        .and_then(Value::as_object)
    {
        Some(servers) => servers,
        None => return Vec::new(),
    };
    let mut names: Vec<String> = servers.keys().cloned().collect();
    names.sort();
    names
}

/// The sorted entry names under `path`: an unreadable directory lists
/// as empty, so the report never fails on a missing runtime directory.
fn dir_names(path: &Path) -> Vec<String> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The runtime snapshot for `home`.
fn home_snapshot(home: &Path) -> HomeSnapshot {
    HomeSnapshot {
        run: dir_names(&home.join("run")),
        cache_mcp: dir_names(&home.join("cache").join("mcp")),
        logs: dir_names(&home.join("logs")),
    }
}

/// The `fiber_started` startup-timeout diagnostic for #3515: when the
/// startup wait expires, the panic names the startup state it found —
/// the configured servers, the child status, stderr so far, and the
/// runtime directories under the test home — instead of only the
/// deadline. The Timeout branch itself is not forceable live by
/// design, so these unit tests pin the builder, the reader, and the
/// snapshot.
#[test]
fn startup_timeout_report_names_servers_child_stderr_and_snapshot() {
    let snap = HomeSnapshot {
        run: vec!["s_00000000000000d1".to_owned()],
        cache_mcp: Vec::new(),
        logs: vec!["fiber.log".to_owned()],
    };
    let report = startup_timeout_report(
        &["flaky".to_owned(), "up".to_owned()],
        "still running",
        &["a first stderr line".to_owned()],
        &snap,
    );
    assert!(
        report.contains("waited until the deadline for fiber_started with servers [flaky, up]"),
        "{report}"
    );
    assert!(report.contains("child still running"), "{report}");
    assert!(report.contains("stderr (1 lines):"), "{report}");
    assert!(report.contains("a first stderr line"), "{report}");
    assert!(report.contains("run: [s_00000000000000d1]"), "{report}");
    assert!(report.contains("cache/mcp: []"), "{report}");
    assert!(report.contains("logs: [fiber.log]"), "{report}");
}

#[test]
fn startup_timeout_report_with_no_servers_or_stderr_names_both() {
    let snap = HomeSnapshot {
        run: Vec::new(),
        cache_mcp: Vec::new(),
        logs: Vec::new(),
    };
    let report = startup_timeout_report(&[], "still running", &[], &snap);
    assert_eq!(
        report,
        "waited until the deadline for fiber_started with no servers configured; child still running\n\
         stderr (0 lines):\n\
         (no stderr received)\n\
         run: []\n\
         cache/mcp: []\n\
         logs: []",
    );
}

#[test]
fn startup_timeout_report_names_each_child_status() {
    let snap = HomeSnapshot {
        run: Vec::new(),
        cache_mcp: Vec::new(),
        logs: Vec::new(),
    };
    for (status, fragment) in [
        ("still running", "child still running"),
        (
            "exited with exit status: 1",
            "child exited with exit status: 1",
        ),
        ("unknown", "child unknown"),
    ] {
        let report = startup_timeout_report(&["up".to_owned()], status, &[], &snap);
        assert!(report.contains(fragment), "{report}");
    }
}

#[test]
fn startup_timeout_report_shows_thirty_stderr_lines_whole() {
    let lines: Vec<String> = (0..30).map(|n| format!("err-{n:02}")).collect();
    let snap = HomeSnapshot {
        run: Vec::new(),
        cache_mcp: Vec::new(),
        logs: Vec::new(),
    };
    let report = startup_timeout_report(&["up".to_owned()], "still running", &lines, &snap);
    assert!(report.contains("stderr (30 lines):"), "{report}");
    assert!(report.contains("err-29"), "{report}");
    assert!(!report.contains("truncated"), "{report}");
}

#[test]
fn startup_timeout_report_truncates_the_thirty_first_stderr_line() {
    let lines: Vec<String> = (0..31).map(|n| format!("err-{n:02}")).collect();
    let snap = HomeSnapshot {
        run: Vec::new(),
        cache_mcp: Vec::new(),
        logs: Vec::new(),
    };
    let report = startup_timeout_report(&["up".to_owned()], "still running", &lines, &snap);
    assert!(report.contains("stderr (31 lines):"), "{report}");
    assert!(report.contains("err-29"), "{report}");
    assert!(!report.contains("err-30"), "{report}");
    assert!(report.contains("truncated"), "{report}");
    assert!(report.contains("showing first 30 of 31 lines"), "{report}");
}

#[test]
fn startup_timeout_sections_match_the_report_tail() {
    let snap = HomeSnapshot {
        run: vec!["s_00000000000000d1".to_owned()],
        cache_mcp: Vec::new(),
        logs: Vec::new(),
    };
    let stderr = vec!["boom".to_owned()];
    let report = startup_timeout_report(&["up".to_owned()], "still running", &stderr, &snap);
    let sections = startup_timeout_sections(&stderr, &snap);
    assert!(report.ends_with(&sections), "{report}");
}

#[test]
fn home_snapshot_lists_sorted_runtime_names() {
    let root = fakes::TempDir::new("snap-full");
    let home = root.path();
    fs::create_dir_all(home.join("run")).unwrap();
    fs::write(home.join("run").join("b"), "").unwrap();
    fs::write(home.join("run").join("a"), "").unwrap();
    fs::create_dir_all(home.join("cache").join("mcp")).unwrap();
    fs::write(home.join("cache").join("mcp").join("up.json"), "{}").unwrap();
    fs::create_dir_all(home.join("logs")).unwrap();
    fs::write(home.join("logs").join("fiber.log"), "").unwrap();
    let snap = home_snapshot(home);
    assert_eq!(snap.run, vec!["a".to_owned(), "b".to_owned()]);
    assert_eq!(snap.cache_mcp, vec!["up.json".to_owned()]);
    assert_eq!(snap.logs, vec!["fiber.log".to_owned()]);
}

#[test]
fn home_snapshot_reads_missing_runtime_dirs_as_empty() {
    let root = fakes::TempDir::new("snap-missing");
    let snap = home_snapshot(root.path());
    assert_eq!(snap.run, Vec::<String>::new());
    assert_eq!(snap.cache_mcp, Vec::<String>::new());
    assert_eq!(snap.logs, Vec::<String>::new());
}

#[test]
fn dir_names_reads_a_missing_directory_as_empty() {
    let root = fakes::TempDir::new("snap-nodir");
    assert_eq!(dir_names(&root.path().join("absent")), Vec::<String>::new());
}

#[test]
fn configured_servers_reads_command_and_url_shapes_sorted() {
    let root = fakes::TempDir::new("servers-valid");
    write_json(
        &root.path().join("config.json"),
        &json!({"model": "fake/m", "mcp": {"servers": {
            "zeta": {"command": "/bin/true"},
            "alpha": {"url": "http://127.0.0.1:9/mcp"},
        }}}),
    );
    assert_eq!(
        configured_servers(root.path()),
        vec!["alpha".to_owned(), "zeta".to_owned()],
    );
}

#[test]
fn configured_servers_reads_a_missing_file_as_no_servers() {
    let root = fakes::TempDir::new("servers-missing");
    assert_eq!(configured_servers(root.path()), Vec::<String>::new());
}

#[test]
fn configured_servers_reads_unreadable_configs_as_no_servers() {
    let root = fakes::TempDir::new("servers-unreadable");
    fs::write(root.path().join("config.json"), b"not json{").unwrap();
    assert_eq!(configured_servers(root.path()), Vec::<String>::new());
    let bytes = fakes::TempDir::new("servers-bytes");
    fs::write(bytes.path().join("config.json"), [0xff, 0xfe, 0x00]).unwrap();
    assert_eq!(configured_servers(bytes.path()), Vec::<String>::new());
    let dir = fakes::TempDir::new("servers-dir");
    fs::create_dir_all(dir.path().join("config.json")).unwrap();
    assert_eq!(configured_servers(dir.path()), Vec::<String>::new());
}

#[test]
fn configured_servers_reads_shapes_without_object_servers_as_no_servers() {
    for config in [
        json!({}),
        json!({"model": "fake/m"}),
        json!({"mcp": "servers"}),
        json!({"mcp": {}}),
        json!({"mcp": {"servers": ["up"]}}),
        json!({"mcp": {"servers": "up"}}),
        json!({"mcp": {"servers": 1}}),
        json!({"mcp": {"servers": null}}),
    ] {
        let root = fakes::TempDir::new("servers-shape");
        write_json(&root.path().join("config.json"), &config);
        assert_eq!(
            configured_servers(root.path()),
            Vec::<String>::new(),
            "{config}"
        );
    }
}
