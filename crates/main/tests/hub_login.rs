//! Session credential logins through the built binary and its hub.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test assertions and fixtures"
)]

#[path = "support/hub_start.rs"]
#[allow(dead_code, reason = "this test binary uses only `free_port`")]
mod hub_start;
mod support;

use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use fakes::{OauthReply, OauthServer, ProviderServer, Response};
use serde_json::json;
use support::*;

fn connect_hub(setup: &Setup, hub: &Arc<Mutex<Option<HubProc>>>) -> Socket {
    let slot = hub.clone();
    let deadline = setup.deadline;
    let mut serve = setup.fiber(&["hub", "serve"]);
    let mut paths = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/browser")];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    serve.env("PATH", std::env::join_paths(paths).unwrap());
    let home = setup.home();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut start = move || {
            *slot.lock().unwrap() = Some(HubProc::spawn_command(deadline, &mut serve));
            Ok(())
        };
        tx.send(doors::hub::connect(&home, &mut start, &SystemClock))
            .unwrap_or(());
    });
    let (stream, hello) = deadline.recv(&rx).expect("hub connected").unwrap();
    assert_eq!(serde_json::to_value(hello).unwrap()["kind"], "hub_hello");
    Socket::from(deadline, stream)
}

fn provider() -> ProviderServer {
    ProviderServer::start_responding(|request| match request.path.as_str() {
        "/v1/responses" => hello(),
        "/lua-refused" => Response::status(200, "{}"),
        _ => Response::status(500, "unexpected request"),
    })
    .unwrap()
}
fn install(setup: &Setup, provider: &ProviderServer, oauth: &OauthServer, port: Option<u16>) {
    let source = setup.root.path().join("source");
    let base = provider.url();
    let login = oauth.url();
    write_json(
        &source.join("extension.json"),
        &json!({"name":"acme", "version":"0.1.0", "fiber":"0.1.0", "api":1, "providers":{"acme":[format!("{base}/v1")]}}),
    );
    let interaction = match port {
        Some(port) => format!(
            "host.oauth.open('{login}/verify'); host.oauth.callback({{port={port},path=\"/cb\"}}); return 'callback-token'"
        ),
        None => format!(
            "host.oauth.open('{login}/verify'); local reply=host.oauth.poll({{url='{login}/token',body='device_code=d'}}); return reply.access_token"
        ),
    };
    fs::create_dir_all(source.join("providers")).unwrap();
    fs::write(
        source.join("providers/acme.lua"),
        format!(
            r#"
local calls = 0
fiber.provider("acme", {{
  models = {{timeout=60000,run=function()
    return {{{{id="m",protocol="openai-responses",base_url="{base}/v1",context_window=100000}}}}
  end}},
  credential = {{timeout=60000,run=function()
    calls = calls + 1
    host.fs.write("stamp", "wall time for this credential")
    local expiry = math.floor(host.fs.stat("stamp").modified_ms / 1000) + 120
    if calls == 1 then return {{token="tok-1",expires_at=expiry}} end
    pcall(host.oauth.refresh, function() host.status("refresh ran"); error("refresh failed") end)
    local ok, result = pcall(function() {interaction} end)
    if not ok then
      host.status("refused: " .. result.code)
      host.http({{method="POST",url="{base}/lua-refused",body=result.code}})
      error(result)
    end
    return {{token=result,expires_at=expiry}}
  end}}
}})
"#
        ),
    )
    .unwrap();
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    write_json(
        &setup.home().join("config.json"),
        &json!({"model":"acme/m"}),
    );
}
fn start(client: &Socket, workspace: &str, content: bool) -> String {
    let mut args = json!({"workspace":workspace});
    if content {
        args["content"] = json!([{"type":"text","text":"first"}]);
    }
    client.send(&json!({"id":"c_start","command":"start","args":args}).to_string());
    let ack = recv_answer(client, "c_start", "start acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    ack["payload"]["result"]["session_id"]
        .as_str()
        .unwrap()
        .into()
}
fn full(client: &Socket, session: &str, id: &str) {
    client.send(
        &json!({"id":id,"session_id":session,"command":"subscribe","args":{"level":"full"}})
            .to_string(),
    );
    let ack = recv_answer(client, id, "full subscribe acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
}
fn prompt(client: &Socket, session: &str, id: &str) {
    client.send(&json!({"id":id,"session_id":session,"command":"prompt","args":{"content":[{"type":"text","text":"hello"}]}}).to_string());
}
fn feed(client: &Socket) {
    client.send(r#"{"id":"c_feed","command":"feed"}"#);
    assert_eq!(
        recv_answer(client, "c_feed", "feed acknowledgement")["kind"],
        "command_accepted"
    );
}
fn close(client: &Socket, session: &str, id: &str) {
    client.send(&json!({"id":id,"session_id":session,"command":"close"}).to_string());
    until(client, "fiber_exited after close", |line| {
        line["kind"] == "fiber_exited" && line["session_id"] == session
    });
}
/// Returns whether the model turn already completed while waiting for its login notice.
fn login_url(client: &Socket, oauth: &OauthServer) -> bool {
    let lines = until(client, "login_url notice", |line| {
        line["kind"] == "notice" && line["payload"]["code"] == "login_url"
    });
    let notice = lines.last().unwrap();
    assert_eq!(notice["payload"]["extension"], "acme");
    assert_eq!(
        notice["payload"]["message"],
        format!("Open {}/verify to log in.", oauth.url())
    );
    lines.iter().any(|line| line["kind"] == "turn_completed")
}
fn turn(client: &Socket, seen: bool) {
    if !seen {
        until(client, "turn_completed while login waits", |line| {
            line["kind"] == "turn_completed"
        });
    }
}
fn refused(setup: &Setup, server: &ProviderServer, oauth: &OauthServer) {
    let mut seen = server.requests().len();
    loop {
        if let Some(request) = server
            .requests()
            .iter()
            .find(|request| request.path == "/lua-refused")
        {
            assert_eq!(request.body, b"authentication_failed");
            break;
        }
        assert!(
            server.await_requests(seen + 1, setup.deadline),
            "refused credential signal"
        );
        seen = server.requests().len();
    }
    assert_eq!(oauth.request_count(), 0, "refusal precedes the poll");
}
fn refusal_snapshot(client: &Socket) {
    let mut status = false;
    let mut completed = false;
    while !(status && completed) {
        let line = recv(
            client,
            "refused status and completed turn after subscribing",
        );
        status |= line["kind"] == "extension_ui"
            && line.to_string().contains("refused: authentication_failed");
        completed |= line["kind"] == "turn_completed";
    }
}
fn stop(hub: &Arc<Mutex<Option<HubProc>>>, guard: SessionGuard) {
    hub.lock().unwrap().take().unwrap().kill_and_wait();
    guard.wait_gone();
}

#[test]
fn an_attached_full_client_receives_the_mid_session_login_url() {
    let setup = Setup::new();
    let server = provider();
    let oauth = OauthServer::start(vec![OauthReply::token("logged-in", "refresh", 3600)]);
    oauth.hold();
    install(&setup, &server, &oauth, None);
    let hub = Arc::new(Mutex::new(None));
    let client = connect_hub(&setup, &hub);
    feed(&client);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start(&client, &workspace, false);
    full(&client, &session, "c_full");
    prompt(&client, &session, "c_prompt");
    let seen = login_url(&client, &oauth);
    assert!(
        oauth.await_requests(1, setup.deadline),
        "poll held after URL arrived"
    );
    turn(&client, seen);
    oauth.release();
    close(&client, &session, "c_close");
    drop(client);
    stop(&hub, guard);
}

#[test]
fn a_session_with_only_summary_clients_refuses_login_before_any_poll() {
    let setup = Setup::new();
    let server = provider();
    let oauth = OauthServer::start(vec![OauthReply::pending()]);
    install(&setup, &server, &oauth, None);
    let hub = Arc::new(Mutex::new(None));
    let client = connect_hub(&setup, &hub);
    feed(&client);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start(&client, &workspace, true);
    refused(&setup, &server, &oauth);
    full(&client, &session, "c_full");
    refusal_snapshot(&client);
    close(&client, &session, "c_close");
    drop(client);
    stop(&hub, guard);
}

#[test]
fn a_disconnected_callback_client_does_not_stop_the_next_turn() {
    retry_callback(&mut hub_start::free_port, ATTEMPTS, Deadline::start());
}

/// Callback attempts `retry_callback` makes, each on a port from `ports`.
const ATTEMPTS: usize = 5;
/// How long one listening probe connects, and then reads, before the
/// refusal post is looked at again.
const PROBE_SLICE: Duration = Duration::from_millis(100);

/// Whether the Lua already reported a taken callback port: on a port the
/// hub cannot bind the callback raises `io_failed`, which the fixture's
/// Lua posts to `/lua-refused` before raising.
fn callback_taken(server: &ProviderServer) -> bool {
    server
        .requests()
        .iter()
        .any(|request| request.path == "/lua-refused" && request.body == *b"io_failed")
}

/// Whether the hub's callback listener answers on `addr`: one GET to a
/// path other than the callback's own gets a 404 while the callback keeps
/// waiting, which proves the hub listens without consuming the callback.
/// Anything else (refused, timeout, no 404) is "not yet".
fn probe(addr: SocketAddr) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, PROBE_SLICE) else {
        return false;
    };
    if stream.set_read_timeout(Some(PROBE_SLICE)).is_err() {
        return false;
    }
    let port = addr.port();
    if write!(
        stream,
        "GET /probe HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .is_err()
    {
        return false;
    }
    let mut status = String::new();
    if BufReader::new(stream).read_line(&mut status).is_err() {
        return false;
    }
    status == "HTTP/1.1 404 Not Found\r\n"
}

/// Runs the disconnected-callback scenario on `port`, returning whether the
/// port was taken, so the whole scenario must run again on a fresh port.
/// Every wait draws on the one shared `deadline`, never renewed, so all
/// attempts together stay under it.
fn callback_attempt(port: u16, deadline: Deadline) -> bool {
    let mut setup = Setup::new();
    setup.deadline = deadline;
    let server = provider();
    let oauth = OauthServer::start(vec![]);
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    install(&setup, &server, &oauth, Some(port));
    let hub = Arc::new(Mutex::new(None));
    let client = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start(&client, &workspace, false);
    full(&client, &session, "c_full");
    prompt(&client, &session, "c_prompt");
    let seen = login_url(&client, &oauth);
    turn(&client, seen);
    loop {
        if callback_taken(&server) {
            close(&client, &session, "c_close_taken");
            drop(client);
            stop(&hub, guard);
            return true;
        }
        if probe(addr) {
            break;
        }
        assert!(!setup.deadline.left().is_zero(), "callback listener bound");
        std::thread::yield_now();
    }
    drop(client);
    let other = connect_hub(&setup, &hub);
    feed(&other);
    full(&other, &session, "c_other");
    until(&other, "only the replacement client counts", |line| {
        line["kind"] == "clients" && line["payload"]["count"] == 1
    });
    prompt(&other, &session, "c_second");
    turn(&other, false);
    close(&other, &session, "c_close");
    drop(other);
    stop(&hub, guard);
    callback_taken(&server)
}

/// Runs the scenario until one attempt's turn runs, drawing each
/// attempt's port from `ports`. Returns the port that ran; panics when
/// every attempt saw its port taken.
fn retry_callback(ports: &mut dyn FnMut() -> u16, attempts: usize, deadline: Deadline) -> u16 {
    for _ in 0..attempts {
        let port = ports();
        if !callback_attempt(port, deadline) {
            return port;
        }
    }
    panic!("callback port io_failed on all {attempts} attempts");
}

#[test]
fn a_taken_callback_port_is_retried_with_a_fresh_one() {
    let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let taken = held.local_addr().unwrap().port();
    let mut first = Some(taken);
    let ran = retry_callback(
        &mut || first.take().unwrap_or_else(hub_start::free_port),
        ATTEMPTS,
        Deadline::start(),
    );
    assert_ne!(ran, taken, "the scenario ran on a fresh port");
    drop(held);
}

#[test]
fn a_resumed_session_installs_live_attendance_and_login_notices() {
    let setup = Setup::new();
    let server = provider();
    let oauth = OauthServer::start(vec![OauthReply::token("resumed", "refresh", 3600)]);
    install(&setup, &server, &oauth, None);
    let hub = Arc::new(Mutex::new(None));
    let client = connect_hub(&setup, &hub);
    feed(&client);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start(&client, &workspace, true);
    refused(&setup, &server, &oauth);
    full(&client, &session, "c_full");
    refusal_snapshot(&client);
    close(&client, &session, "c_close_first");
    until(&client, "first session left the hub feed", |line| {
        line["kind"] == "session_left" && line["payload"]["session_id"] == session
    });
    drop(client);
    let client = connect_hub(&setup, &hub);
    oauth.hold();
    full(&client, &session, "c_resumed");
    // A status after the fold proves the old turn has been replayed.
    until(&client, "resumed stream is live", |line| {
        line["kind"] == "session_status"
    });
    prompt(&client, &session, "c_resumed_prompt");
    let seen = login_url(&client, &oauth);
    assert!(
        oauth.await_requests(1, setup.deadline),
        "resumed login poll held"
    );
    turn(&client, seen);
    oauth.release();
    close(&client, &session, "c_close_last");
    drop(client);
    stop(&hub, guard);
}
