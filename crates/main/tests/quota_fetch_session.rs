//! A model call through a session held open through the hub starts the
//! Lua provider's quota fetch off the request path (`docs/tools.md`,
//! "Provider quota"): the fake provider server records a `/lua-quota`
//! request while the session stays open. `fiber ask` exits without
//! waiting for that fetch, so no `ask` test asserts it (see #2593).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::sync::{Arc, Mutex, mpsc};

use contract::clock::Clock;
use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use support::*;

/// Installs the Lua extension `test-ext` with a `test` provider whose
/// models, credential and quota functions each make one real HTTP request.
/// Model URLs come from the test server, so no secret is configured
/// anywhere.
// debt: two copies of the test Lua provider installer and router (extension_diagnostics.rs), upgrade trigger: a third test file that needs them, which moves both into support/
fn install_test_provider(setup: &Setup, server_url: &str) {
    let source = setup.root.path().join("luasrc");
    // A reset time the quota view draws as `resets in 3h...`: far enough
    // out that the minute's truncation never drops the `3h` prefix.
    let reset_secs = SystemClock
        .wall()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3 * 3600
        + 600;
    write_json(
        &source.join("extension.json"),
        &json!({"name": "test-ext", "version": "0.1.0", "fiber": "0.1.0", "api": 1,
                "providers": {"test": [format!("{server_url}/v1")]}}),
    );
    fs::create_dir_all(source.join("providers")).unwrap();
    fs::write(
        source.join("providers/test.lua"),
        format!(
            "fiber.provider(\"test\", {{\n\
               models = {{ timeout = 60000, run = function()\n\
                 local r = host.http({{ url = \"{server_url}/lua-models\" }})\n\
                 return {{{{ id = \"m1\", protocol = \"openai-responses\", base_url = \"{server_url}/v1\", context_window = 100000 }}}}\n\
               end }},\n\
               credential = {{ timeout = 60000, run = function()\n\
                 local r = host.http({{ method = \"POST\", url = \"{server_url}/lua-token\" }})\n\
                 return {{ token = \"tok\", expires_at = 1893456000 }}\n\
               end }},\n\
               quota = {{ timeout = 60000, run = function(q)\n\
                 local r = host.http({{ url = \"{server_url}/lua-quota\" }})\n\
                 return {{ windows = {{ {{ name = \"w\", used_percent = tonumber(r.body) or 1, resets_at = {reset_secs} }} }} }}\n\
               end }},\n\
               sign = {{ timeout = 60000, run = function(request) return {{}} end }},\n\
             }})\n"
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
}

/// A server answering every Lua purpose plus native model calls, without
/// limit.
fn start_routed() -> ProviderServer {
    ProviderServer::start_responding(|request| match request.path.as_str() {
        "/lua-models" | "/lua-token" | "/lua-quota" => Response::status(200, "{}"),
        "/v1/responses" => hello(),
        _ => Response::status(500, "unrouted"),
    })
    .unwrap()
}

fn configure(setup: &Setup, model: &str) {
    // Debug, so the quota fetch's `provider_request` line is written.
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": model, "diagnostics": {"level": "debug"}}),
    );
}

/// The held-open session's diagnostic file, parsed: a trailing fragment
/// without its newline is still being written, so it is dropped.
fn session_lines(setup: &Setup, id: &str) -> Vec<Value> {
    let path = setup.home().join("logs").join(format!("session-{id}.log"));
    let text = fs::read_to_string(&path).unwrap_or_default();
    complete_lines(&text)
}

#[test]
fn a_model_call_through_an_open_session_fetches_lua_quota() {
    let setup = Setup::new();
    let server = start_routed();
    let url = server.url();
    install_test_provider(&setup, &url);
    configure(&setup, "test/m1");
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, "hi");
    subscribe(&client, &session);
    until(&client, "the first turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    // The quota fetch runs off the request path, so it can land after the
    // turn does: each wait below takes what remains of the test's one
    // deadline, and expiry fails naming the missing request.
    let mut seen = server.requests().len();
    loop {
        if server
            .requests()
            .iter()
            .any(|request| request.path == "/lua-quota")
        {
            break;
        }
        assert!(
            server.await_requests(seen + 1, setup.deadline),
            "waited until the deadline for the /lua-quota request after the turn"
        );
        seen = server.requests().len();
    }
    // The fetch thread writes the record synchronously before it stores
    // the fetch, so once the fetch ends the line is in the session's
    // file: poll the file to the test's one deadline, with no sleep, and
    // expiry fails naming the missing record.
    let quota = loop {
        if let Some(quota) = session_lines(&setup, &session)
            .into_iter()
            .find(|line| line["code"] == "provider_request" && line["data"]["purpose"] == "quota")
        {
            break quota;
        }
        assert!(
            !setup.deadline.left().is_zero(),
            "waited until the deadline for the quota provider_request record in the session log"
        );
        std::thread::yield_now();
    };
    assert_eq!(quota["message"], "A provider request ended.");
    assert_eq!(quota["data"]["provider"], "test");
    assert_eq!(quota["data"]["purpose"], "quota");
    assert_eq!(quota["data"]["attempt"], 1);
    assert_eq!(quota["data"]["status"], 200);
    drop(client);
    // SIGKILL the hub: the session keeps running on its own socket.
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.kill_and_wait();
    let direct = Socket::connect(setup.deadline, &setup.session_socket(&session));
    close_session(&direct);
    drop(direct);
    guard.wait_gone();
}

#[test]
fn a_hot_window_notices_rearms_and_notices_again_across_resumes() {
    let setup = Setup::new();
    let percent = Arc::new(Mutex::new("90".to_owned()));
    let response = percent.clone();
    let server = ProviderServer::start_responding(move |request| match request.path.as_str() {
        "/lua-quota" => Response::status(200, response.lock().unwrap().clone()),
        "/lua-models" | "/lua-token" => Response::status(200, "{}"),
        "/v1/responses" => hello(),
        _ => Response::status(500, "unrouted"),
    })
    .unwrap();
    install_test_provider(&setup, &server.url());
    // The quota loader enumerates configured credential labels, including
    // when the provider supplies its token through Lua.
    fs::create_dir_all(setup.home().join("credentials/test")).unwrap();
    fs::write(setup.home().join("credentials/test/work"), "test-key").unwrap();
    configure(&setup, "test/m1");
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let mut guard = Some(SessionGuard::arm(setup.deadline, &workspace));
    let session = start_session(&client, &workspace, "hi");
    subscribe(&client, &session);
    until(&client, "the initial turn", |line| {
        line["kind"] == "turn_completed"
    });
    let mut expected_quota = Vec::new();
    let mut attempt = 0;
    for (index, used) in [90.0, 40.0, 90.0].into_iter().enumerate() {
        *percent.lock().unwrap() = used.to_string();
        if index != 0 {
            guard = Some(SessionGuard::arm(setup.deadline, &workspace));
        }
        loop {
            assert!(
                !setup.deadline.left().is_zero(),
                "waited for quota window w at {used}"
            );
            attempt += 1;
            let id = format!("q_{attempt}");
            client.send(&json!({"id":id, "session_id":session, "command":"quota"}).to_string());
            let answer = recv_answer(&client, &id, "the quota answer");
            assert_eq!(answer["kind"], "command_accepted", "{answer}");
            if answer["payload"]["result"]["quota"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["provider"] == "test" && row["windows"][0]["percent_used"] == used)
            {
                break;
            }
            assert!(
                !setup.deadline.left().is_zero(),
                "quota window w never reached {used}: {answer}"
            );
        }
        let direct = Socket::connect(setup.deadline, &setup.session_socket(&session));
        direct.send(r#"{"id":"sub","command":"subscribe","args":{"level":"full"}}"#);
        assert_eq!(
            recv_answer(&direct, "sub", "the subscription")["kind"],
            "command_accepted"
        );
        let prompt_id = format!("next_{index}");
        direct.send(&json!({"id":prompt_id,"command":"prompt","args":{"content":[{"type":"text","text":"next"}]}}).to_string());
        // A full subscription replays the log. Wait for this prompt's own
        // start, not a completed turn from the replay.
        let starts = until(&direct, "this prompt's turn_started", |line| {
            line["kind"] == "turn_started" && line["payload"]["input"][0]["command_id"] == prompt_id
        });
        let mut lines = vec![starts.last().unwrap().clone()];
        let turn_id = lines[0]["turn_id"].clone();
        lines.extend(until(&direct, "this quota turn's completion", |line| {
            line["kind"] == "turn_completed" && line["turn_id"] == turn_id
        }));
        let started = &lines[0];
        let kind = if used == 40.0 {
            "quota_rearmed"
        } else {
            "quota_noticed"
        };
        let durable_kinds: Vec<_> = lines
            .iter()
            .filter(|line| line.get("seq").is_some())
            .map(|line| line["kind"].as_str().unwrap())
            .collect();
        assert_eq!(
            durable_kinds,
            [
                "turn_started",
                kind,
                "step_started",
                "assistant_message_started",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
                "turn_completed"
            ]
        );
        let quota: Vec<_> = lines
            .iter()
            .filter(|line| line["kind"].as_str().unwrap().starts_with("quota_"))
            .collect();
        assert_eq!(quota.len(), 1, "phase {index}");
        assert_eq!(quota[0]["kind"], kind);
        assert_eq!(quota[0]["turn_id"], started["turn_id"]);
        let credential = quota[0]["payload"]["credential"].as_str().unwrap();
        assert_eq!(credential, "work");
        let mut payload = quota[0]["payload"].as_object().unwrap().clone();
        let resets = payload.remove("resets_at");
        assert_eq!(
            Value::Object(payload),
            json!({"provider":"test","credential":credential,"window":"w","percent_used":used,"notice_at":80.0})
        );
        if kind == "quota_noticed" {
            // The window carries the fake's future reset time through the
            // notice; the rearm carries none.
            let resets_ms = resets.unwrap().as_u64().unwrap();
            let now_secs = SystemClock
                .wall()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            assert!(
                resets_ms / 1000 > now_secs + 3 * 3600,
                "the reset is the fake's ~3h future time, not now: {resets_ms}"
            );
        } else {
            assert!(resets.is_none(), "a rearm carries no reset time");
        }
        let input = started["payload"]["input"].as_array().unwrap();
        assert_eq!(input.len(), if used == 40.0 { 1 } else { 2 });
        if used != 40.0 {
            // The window now carries the fake's reset time, so the notice
            // names it after the base text.
            let text = input.last().unwrap()["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_owned();
            assert!(
                text.starts_with(&format!(
                    "Fiber: test credential {credential} has used 90% of its w quota, which resets at "
                )) && text.ends_with('.'),
                "unexpected notice text: {text}"
            );
            assert_eq!(
                input.last().unwrap(),
                &json!({"type":"message","source":"fiber","content":[{"type":"text","text":text}]})
            );
        }
        expected_quota.push(kind);
        direct.send(r#"{"id":"close","command":"close"}"#);
        until_close(&direct);
        drop(direct);
        guard.take().unwrap().wait_gone();
        let durable: Vec<Value> = log::lines(&session_dir(&setup, &session))
            .unwrap()
            .map(|line| serde_json::to_value(line.unwrap()).unwrap())
            .collect();
        let actual: Vec<_> = durable
            .iter()
            .filter_map(|line| {
                line["kind"]
                    .as_str()
                    .filter(|kind| kind.starts_with("quota_"))
            })
            .collect();
        assert_eq!(actual, expected_quota);
    }
    drop(client);
    hub.lock().unwrap().take().unwrap().kill_and_wait();
}

/// The `/lua-quota` gate: armed after the turn answers, holding the
/// view's later fetch until the test's release, counting the fetches
/// held in between.
struct QuotaGate {
    armed: bool,
    gated: usize,
}

/// Quits the run's terminal and waits for it, as `panel_look.rs` does.
fn quit(run: support::pty::Run) {
    let mut run = run;
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// `/quota` opens the quota view, marks the in-flight fetch, and shows
/// the fetched values: the view's fetch waits on a gate the test
/// releases, so the mark draws deterministically (`docs/tui.md`,
/// "The quota view").
#[test]
fn slash_quota_opens_the_view_and_shows_the_fetched_values() {
    let setup = Setup::new();
    // Armed after the turn answers: quota hits before that (the session
    // start's fetch, the turn's own read) answer at once, while the
    // view's later fetch waits for the test's release, so the fetching
    // mark stays up until the test has seen it.
    let quota_gate = Arc::new(Mutex::new(QuotaGate {
        armed: false,
        gated: 0,
    }));
    // The test owns the release sender: dropping it, even by a failure
    // before the release, unblocks the held fetch, so the server can
    // always shut down instead of hanging a responder.
    let (release, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let release_at = setup.deadline;
    // The server signals each fetch it holds: the test releases only
    // after that signal, so the gated count proves the request was held
    // before release.
    let (held, held_rx) = mpsc::channel();
    let server = ProviderServer::start_responding({
        let quota_gate = Arc::clone(&quota_gate);
        let release_rx = Arc::clone(&release_rx);
        move |request| match request.path.as_str() {
            "/lua-models" | "/lua-token" => Response::status(200, "{}"),
            "/lua-quota" => {
                let mut guard = quota_gate.lock().unwrap();
                if guard.armed {
                    // The view's fetch: hold it until the release. The
                    // wait is bounded by the test's deadline, and the
                    // dropped sender releases it: on a release, a failure
                    // before it, or an expired deadline the fetch is
                    // answered, so the server always shuts down instead of
                    // hanging a responder.
                    guard.gated += 1;
                    let _sent = held.send(());
                    drop(guard);
                    let gate = release_rx.lock().unwrap();
                    let _released = release_at.recv(&gate).is_ok();
                }
                Response::status(200, "42")
            }
            "/v1/responses" => hello(),
            _ => Response::status(500, "unrouted"),
        }
    })
    .unwrap();
    let url = server.url();
    install_test_provider(&setup, &url);
    // The loader fetches only configured credential labels.
    fs::create_dir_all(setup.home().join("credentials/test")).unwrap();
    fs::write(setup.home().join("credentials/test/work"), "test-key").unwrap();
    configure(&setup, "test/m1");
    // The hub exits at once once the terminal quits, so the run's
    // wait does not sit out the default thirty idle minutes.
    let config_path = setup.home().join("config.json");
    let mut config: Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    config["hub"] = json!({"idle_exit_ms": 200});
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
    let mut run = support::pty::Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    // The box starts a session and the turn answers at once; the
    // session start's fetch takes longer and stays in flight.
    run.write(b"hi\r");
    run.wait_screen("the answered turn", |grid| grid.contents.contains("Hello."));
    // The session start's fetch can still be in flight here: the view's
    // read would share that flight (`docs/invocation.md`, `quota`) and
    // never reach the gate, so the hold below would time out. Focus in
    // (the terminal asks for cached quota on focus-in, answered from the
    // caches without starting a fetch) and wait for the fetched value
    // before arming: a drawn value means `finish_quota` stored it and
    // cleared the flight under one lock, and the fresh cache starts no
    // new flight on its own, so only the view's fetch can arrive now.
    run.write(b"\x1b[I");
    run.wait_screen("the fetched quota value", |grid| {
        grid.contents.contains("58%")
    });
    quota_gate.lock().unwrap().armed = true;
    run.write(b"/quota\r");
    run.wait_screen("the view's fetching mark", |grid| {
        grid.contents.contains("fetching")
    });
    // The mark is up, but the fetch may not have reached the server yet:
    // wait for the server to hold it before releasing.
    setup
        .deadline
        .recv_or_fail(&held_rx, "the fake server holding the view's fetch");
    release.send(()).unwrap();
    let shown = run.wait_screen("the fetched quota", |grid| {
        !grid.contents.contains("fetching")
    });
    assert!(shown.contents.contains("Quota"), "{}", shown.contents);
    assert!(shown.contents.contains("test · work"), "{}", shown.contents);
    assert!(shown.contents.contains("58%"), "{}", shown.contents);
    assert!(
        shown.contents.contains("resets in 3h"),
        "{}",
        shown.contents
    );
    // The gate held the view's fetch until the release: the mark the
    // test saw was the fetch in flight, not a frame's flicker.
    assert_eq!(quota_gate.lock().unwrap().gated, 1, "one gated fetch");
    quit(run);
}
