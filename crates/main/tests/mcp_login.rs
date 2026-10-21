//! Binary-level tests of `fiber mcp login|logout`
//! (`docs/testing.md`, "Levels"; `docs/invocation.md`, "Commands and
//! flags"): the built `fiber` runs the authorization flow against one
//! fake server, stores the token by canonical URL, sends it on `ask`,
//! and deletes it on `logout`. Every run carries a wall-clock deadline,
//! and every child runs in its own process group under `Watchdog`.

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
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Request, Response, Watchdog};
use serde_json::{Value, json};
use support::{
    Deadline, KillGroup, StdoutRun, function_call, hello, read_kinds, spawn_watched, stream,
};

struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
    #[allow(dead_code, reason = "held only for its drop")]
    guard: Watchdog,
}

impl Setup {
    fn new() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fl");
        let guard = Watchdog::matching(&root.path().display().to_string());
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self {
            deadline,
            root,
            guard,
        }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("w")
    }

    /// Installs providers `fake` with models `m` and `r`, makes `fake/m`
    /// the configured model and `fake/r` its reviewer, so reviewed calls
    /// (every remote call declares `network`) are allowed.
    fn provider_with_reviewer(&self, server: &ProviderServer) {
        let source = self.root.path().join("src");
        support::write_json(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
        );
        support::write_json(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [
                    {"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000},
                    {"id": "r", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000},
                ]
            }),
        );
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(source),
            "0.0.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
        support::write_json(
            &self.home().join("config.json"),
            &json!({"model": "fake/m", "reviewer": {"model": "fake/r"}}),
        );
    }

    /// Adds `servers` under `mcp.servers` to the global configuration,
    /// keeping the configured model and reviewer, creating the file when
    /// no provider was installed.
    fn configure(&self, servers: &Value) {
        let path = self.home().join("config.json");
        let mut config: Value = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(json!({}));
        config["mcp"] = json!({"servers": servers});
        support::write_json(&path, &config);
    }

    /// One `fiber` invocation: the environment every test runs under,
    /// with `PATH` holding no opener, so `open` cannot start.
    fn fiber(&self, args: &[&str], empty_path: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.workspace())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", empty_path)
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        command
    }

    /// Runs `fiber` with `args` to its exit under the deadline.
    #[track_caller]
    fn run(&self, args: &[&str], empty_path: &Path) -> StdoutRun {
        let mut command = self.fiber(args, empty_path);
        let (child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let guard = KillGroup(group);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match self.deadline.recv(&finished) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(
                self.deadline,
                group,
                &finished,
                &format!("`fiber {}` to exit", args.join(" ")),
            ),
        };
        assert!(
            !support::group_alive(self.deadline, group),
            "`fiber` left a process behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup_phase());
        StdoutRun::from(output)
    }
}

/// Like `call_kinds`, for reviewed calls: the reviewer's usage and its
/// `permission_resolved` fall between the turn's own lines and each
/// `tool_call_started`.
fn reviewed_kinds(servers: &[&[&'static str]]) -> Vec<&'static str> {
    let read = read_kinds();
    let start = read
        .iter()
        .position(|kind| *kind == "step_started")
        .unwrap();
    let started = read
        .iter()
        .position(|kind| *kind == "tool_call_started")
        .unwrap();
    let completed = read
        .iter()
        .position(|kind| *kind == "tool_call_completed")
        .unwrap();
    let mut kinds = read[..start].to_vec();
    for lines in servers {
        kinds.extend_from_slice(&read[start..started]);
        kinds.extend(["usage_recorded", "permission_resolved"]);
        kinds.extend_from_slice(&read[started..completed]);
        kinds.extend_from_slice(lines);
        kinds.push("tool_call_completed");
    }
    kinds.extend_from_slice(&read[completed + 1..]);
    kinds
}

/// A fake OAuth authorization server and remote MCP server on one port:
/// the probe at `/mcp` answers 401 with the metadata header, the
/// documents, `/register` and `/token` serve the flow, and POSTs to
/// `/mcp` run the handshake, the tool list and `call`.
fn oauth_and_mcp(
    call: impl Fn(&Request, &Value) -> Response + Send + Sync + 'static,
) -> ProviderServer {
    ProviderServer::start_responding(move |req: &Request| {
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
            Some("tools/call") => call(req, &id),
            Some(other) => panic!("unexpected MCP method {other}"),
        }
    })
    .unwrap()
}

fn echo_answer(id: &Value) -> Response {
    Response::status(
        200,
        json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "hi"}]}})
            .to_string(),
    )
}

/// A reviewer verdict allowing the call.
fn allow_text() -> Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": "allow"}]}})])
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

/// Runs `fiber mcp login <name>` with no opener on `PATH`, sends one
/// callback to the shown redirect, and returns the exit and every stderr
/// line. The authorization URL is read from stderr through one `Deadline`
/// on a scoped thread; the child runs in its own process group.
fn login_with_callback(setup: &Setup, name: &str, empty_path: &Path) -> (Option<i32>, Vec<String>) {
    let mut command = setup.fiber(&["mcp", "login", name], empty_path);
    let (mut child, watchdog) = spawn_watched(&mut command);
    let group = child.id();
    let guard = KillGroup(group);
    let stderr = child.stderr.take().unwrap();
    let (url_sent, url_received) = mpsc::channel();
    thread::scope(|scope| {
        let reader = scope.spawn(move || {
            let mut lines = Vec::new();
            let mut sent = false;
            for line in BufReader::new(stderr).lines() {
                let line = line.unwrap();
                if !sent && line.starts_with("http") {
                    url_sent.send(line.clone()).unwrap();
                    sent = true;
                }
                lines.push(line);
            }
            lines
        });
        let url = setup
            .deadline
            .recv_or_fail(&url_received, "the authorization URL");
        let redirect = query_param(&url, "redirect_uri");
        let state = query_param(&url, "state");
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
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait()).unwrap());
        let status = match setup.deadline.recv(&finished) {
            Ok(status) => status.unwrap(),
            Err(_) => support::expired(
                setup.deadline,
                group,
                &finished,
                "`fiber mcp login` to exit",
            ),
        };
        let lines = reader.join().unwrap();
        assert!(
            !support::group_alive(setup.deadline, group),
            "`fiber` left a process behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(setup.deadline.cleanup_phase());
        (status.code(), lines)
    })
}

#[test]
fn login_stores_the_token_ask_sends_it_and_logout_deletes_it() {
    let setup = Setup::new();
    let empty = setup.root.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let model = ProviderServer::start([
        stream(&[function_call("echo", "mcp__x__echo", &json!({}))]),
        allow_text(),
        hello(),
        stream(&[function_call("echo", "mcp__x__echo", &json!({}))]),
        allow_text(),
        hello(),
    ])
    .unwrap();
    setup.provider_with_reviewer(&model);
    let mcp = oauth_and_mcp(|_request, id| echo_answer(id));
    let url = format!("{}/mcp", mcp.url());
    setup.configure(&json!({"x": {"url": url}}));
    let (code, lines) = login_with_callback(&setup, "x", &empty);
    assert_eq!(code, Some(0), "{lines:?}");
    // The token file: mode 0600, recording the URL, under the SHA-256 of
    // the canonical URL.
    let key = mcp::token_key(&url).unwrap();
    let file = setup.home().join("credentials/mcp").join(&key);
    assert_eq!(
        lines
            .iter()
            .find(|line| line.starts_with("fiber: stored credentials/mcp/")),
        Some(&format!("fiber: stored credentials/mcp/{key}")),
        "{lines:?}"
    );
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let stored: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(stored["url"], json!(url));
    assert_eq!(stored["access_token"], json!("at-1"));
    // The token endpoint saw the PKCE verifier.
    let exchange = mcp
        .requests()
        .into_iter()
        .find(|request| request.path == "/token")
        .unwrap();
    let body = String::from_utf8_lossy(&exchange.body).into_owned();
    let verifier = body
        .split('&')
        .find_map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (name == "code_verifier").then(|| value.to_owned())
        })
        .unwrap();
    assert_eq!(verifier.len(), 43);
    // Before the logout, an ask sends the token on its requests.
    let run = setup.run(&["ask", "echo"], &empty);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.kinds(), reviewed_kinds(&[&[]]));
    assert_eq!(
        run.first("tool_call_completed")["payload"]["status"],
        "completed"
    );
    assert!(
        mcp.requests()
            .iter()
            .any(|request| request.header("authorization")
                == Some(fakes::fingerprint("Bearer at-1").as_str()))
    );
    // The logout removes the file; a second ask sends no token.
    let logout = setup.run(&["mcp", "logout", "x"], &empty);
    assert_eq!(logout.code, Some(0), "{logout:?}");
    assert!(!file.exists());
    let seen = mcp.requests().len();
    let run = setup.run(&["ask", "echo"], &empty);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(
        mcp.requests()[seen..]
            .iter()
            .filter(|request| !request_method(request).is_empty())
            .all(|request| request.header("authorization").is_none()),
        "a request carried a token after the logout"
    );
}

/// The JSON-RPC method of a recorded fake request, `""` for `GET`/`DELETE`.
fn request_method(req: &Request) -> String {
    serde_json::from_slice::<Value>(&req.body)
        .ok()
        .and_then(|body| {
            body.get("method")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

#[test]
fn without_s256_the_login_is_refused_with_no_code() {
    let setup = Setup::new();
    let empty = setup.root.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let mcp = ProviderServer::start_responding(|req: &Request| {
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
                })
                .to_string(),
            );
        }
        Response::status(404, "{}")
    })
    .unwrap();
    setup.configure(&json!({"x": {"url": format!("{}/mcp", mcp.url())}}));
    let run = setup.run(&["mcp", "login", "x"], &empty);
    assert_eq!(run.code, Some(1), "{run:?}");
    assert!(run.stderr.contains('x'), "{run:?}");
    assert!(
        run.stderr
            .contains("its authorization server does not offer PKCE with S256"),
        "{run:?}"
    );
    assert!(!run.stderr.contains("config_invalid"), "{run:?}");
    assert!(
        mcp.requests()
            .iter()
            .all(|request| request.path != "/register"),
        "a registration request was sent"
    );
    assert!(
        !setup.home().join("credentials/mcp").exists(),
        "a token was stored"
    );
}

#[test]
fn a_repository_server_nobody_approved_is_refused_without_any_request() {
    let setup = Setup::new();
    let empty = setup.root.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let mcp = oauth_and_mcp(|_request, id| echo_answer(id));
    let url = format!("{}/mcp", mcp.url());
    fs::create_dir_all(setup.workspace().join(".fiber")).unwrap();
    fs::write(
        setup.workspace().join(".fiber/config.json"),
        json!({"mcp": {"servers": {"x": {"url": url}}}}).to_string(),
    )
    .unwrap();
    let run = setup.run(&["mcp", "login", "x"], &empty);
    assert_eq!(run.code, Some(1), "{run:?}");
    assert!(run.stderr.contains("fiber approve"), "{run:?}");
    assert!(
        mcp.requests().is_empty(),
        "the repository's server was reached"
    );
    assert!(
        run.stderr.lines().all(|line| !line.starts_with("http")),
        "the authorization URL was printed: {}",
        run.stderr
    );
    assert!(
        !setup.home().join("credentials/mcp").exists(),
        "a token was stored"
    );
    // The same after the declaration is approved and then edited: the
    // changed declaration is refused too, reaching nothing.
    fs::remove_file(setup.workspace().join(".fiber/config.json")).unwrap();
    let add = setup.run(&["mcp", "add", "--repo", "x", &url], &empty);
    assert_eq!(add.code, Some(0), "{add:?}");
    let second = oauth_and_mcp(|_request, id| echo_answer(id));
    let edited = format!("{}/mcp", second.url());
    fs::write(
        setup.workspace().join(".fiber/config.json"),
        json!({"mcp": {"servers": {"x": {"url": edited}}}}).to_string(),
    )
    .unwrap();
    let run = setup.run(&["mcp", "login", "x"], &empty);
    assert_eq!(run.code, Some(1), "{run:?}");
    assert!(run.stderr.contains("fiber approve"), "{run:?}");
    assert!(
        second.requests().is_empty(),
        "the edited server was reached"
    );
    assert!(
        !setup.home().join("credentials/mcp").exists(),
        "a token was stored"
    );
}

#[test]
fn login_and_logout_of_an_unknown_name_name_it() {
    let setup = Setup::new();
    let empty = setup.root.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let login = setup.run(&["mcp", "login", "zzz"], &empty);
    assert_eq!(login.code, Some(2), "{login:?}");
    assert!(login.stderr.contains("zzz"), "{login:?}");
    let logout = setup.run(&["mcp", "logout", "zzz"], &empty);
    assert_eq!(logout.code, Some(2), "{logout:?}");
    assert!(logout.stderr.contains("zzz"), "{logout:?}");
}
