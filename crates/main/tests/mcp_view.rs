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
use std::path::PathBuf;
use std::process::Child;
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Request, Response, Watchdog};
use serde_json::{Value, json};
use support::{
    KillGroup, Socket, first_line, group_alive, spawn_watched, until, until_close, write_json,
};

/// A driven session: the child, its socket client and the test setup.
struct Driver {
    child: Child,
    guard: KillGroup,
    watchdog: Watchdog,
    stderr: mpsc::Receiver<String>,
    client: Socket,
    setup: support::Setup,
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
            let mut text = String::new();
            stderr_pipe
                .take(u64::MAX)
                .read_to_string(&mut text)
                .unwrap();
            tx.send(text).unwrap();
        });
        first_line(setup.deadline, &lines);
        let client = Socket::connect(setup.deadline, &setup.session_socket(id));
        client.send(r#"{"id":"sub","command":"subscribe","args":{"level":"full"}}"#);
        // The startup wait returns once both `clients` and
        // `extensions_loaded` have arrived, in any order.
        let mut seen_clients = false;
        let mut seen_extensions = false;
        until(&client, "startup", |line| {
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
            client,
            setup,
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
    fn finish(self) -> String {
        self.client.send(r#"{"id":"close","command":"close"}"#);
        until_close(&self.client);
        let group = self.child.id();
        let mut child = self.child;
        let (tx, done) = mpsc::channel();
        thread::spawn(move || {
            tx.send(child.wait()).unwrap();
        });
        let status = self.setup.deadline.recv(&done).unwrap().unwrap();
        let stderr = self.setup.deadline.recv(&self.stderr).unwrap();
        assert!(status.success(), "{stderr}");
        assert!(!group_alive(self.setup.deadline, group));
        std::mem::forget(self.guard);
        self.watchdog.stand_down(self.setup.deadline.cleanup());
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
fn list_shows_a_ready_a_failed_and_a_needs_login_server() {
    let setup = support::Setup::new();
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
    let setup = support::Setup::new();
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
    let driver = Driver::start(setup, &doors::mint("s_"), empty.as_os_str());

    let rows = driver.servers("c_before");
    let before = Driver::row(&rows, "remote");
    assert_eq!(before["state"], "needs-login", "{rows:?}");
    assert!(before.get("auth_url").is_none(), "{rows:?}");

    driver.client.send(
        &json!({"id": "c_login", "command": "mcp_login", "args": {"server": "remote"}}).to_string(),
    );
    // The login waits on the loopback callback; its row carries the URL
    // to open by hand until the callback arrives.
    let mut auth_url = String::new();
    for round in 0..60 {
        let rows = driver.servers(&format!("c_wait_{round}"));
        if let Some(url) = Driver::row(&rows, "remote")
            .get("auth_url")
            .and_then(Value::as_str)
        {
            auth_url = url.to_owned();
            break;
        }
    }
    assert!(!auth_url.is_empty(), "no auth_url while the login waits");
    // One login in flight per server: a second one is refused.
    let busy = driver.command("c_login_again", "mcp_login", json!({"server": "remote"}));
    assert_eq!(busy["kind"], "command_rejected", "{busy:?}");
    assert_eq!(busy["payload"]["code"], "busy", "{busy:?}");
    drive_callback(&auth_url);
    let logged_in = support::recv_answer(&driver.client, "c_login", "the login acknowledgement");
    assert_eq!(logged_in["kind"], "command_accepted", "{logged_in:?}");

    // The stored token starts the server: its row is ready.
    let mut ready = false;
    for round in 0..60 {
        let rows = driver.servers(&format!("c_ready_{round}"));
        let row = Driver::row(&rows, "remote");
        if row["state"] == "ready" {
            assert!(row.get("auth_url").is_none(), "{rows:?}");
            assert_eq!(row["tools"], 1, "{rows:?}");
            ready = true;
            break;
        }
    }
    assert!(ready, "the server never became ready after the login");
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
    let setup = support::Setup::new();
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
