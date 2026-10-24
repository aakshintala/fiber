//! Pairing, expiry and revocation over a real loopback websocket port.
use super::*;
use crate::pairing::{Outcome, Who};
use fakes::MUST_SUCCEED_WITHIN;
use serde_json::json;

fn mint(hub: &Hub, device: &str) -> String {
    match hub
        .pairing
        .mint(
            hub,
            &Who::Local,
            &crate::testkit::args(json!({"device":device})),
        )
        .unwrap()
    {
        Outcome::Done(value) => value["code"].as_str().unwrap().to_owned(),
        Outcome::Dropped => panic!("local mint dropped"),
    }
}
fn text(deadline: Deadline, client: &Ws) -> Value {
    match deadline.recv_or_fail(&client.rx, "the next websocket line") {
        Item::Text(bytes) => serde_json::from_slice(&bytes).unwrap(),
        other @ (Item::Response(_) | Item::Close(..) | Item::Pong(_) | Item::Eof) => {
            panic!("expected text, got {other:?}")
        }
    }
}

fn pair(client: &Ws, code: &str) -> String {
    client.text(&json!({"id":"p1","command":"pair","args":{"code":code}}).to_string());
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    // docs/invocation.md:1232-1241 promises the `device_changed` announcement but not its
    // order relative to `command_accepted`, so accept either arrival order.
    let first = text(deadline, client);
    let second = text(deadline, client);
    let (accepted, notice) = if first["kind"] == "command_accepted" {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(accepted["kind"], "command_accepted");
    assert_eq!(accepted["payload"]["command_id"], "p1");
    let token = accepted["payload"]["result"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(token.len(), 43);
    assert_eq!(notice["kind"], "device_changed");
    assert_eq!(notice["payload"]["change"], "paired");
    token
}

fn test_hub(temp: &Temp, clock: &Arc<FakeClock>) -> Arc<Hub> {
    Arc::new(crate::testkit::hub(
        &temp.dir,
        Arc::new(FakeStarter::hang(&temp.dir)),
        Arc::clone(clock) as Arc<dyn Clock>,
    ))
}

fn authenticated(
    hub: Arc<Hub>,
    n: u64,
    who: Who,
) -> (
    UnixStream,
    mpsc::Receiver<Option<String>>,
    mpsc::Receiver<()>,
) {
    let (service, client) = UnixStream::pair().unwrap();
    let (lines_tx, lines_rx) = mpsc::channel();
    let reader = client.try_clone().unwrap();
    thread::spawn(move || {
        let mut read = BufReader::new(reader);
        loop {
            let mut line = String::new();
            match read.read_line(&mut line) {
                Ok(0) => {
                    lines_tx.send(None).unwrap_or(());
                    break;
                }
                Ok(_) => {
                    if lines_tx.send(Some(line)).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    lines_tx.send(None).unwrap_or(());
                    break;
                }
            }
        }
    });
    let (served_tx, served_rx) = mpsc::channel();
    thread::spawn(move || {
        crate::connection::serve_authenticated(service, hub, n, who, None);
        served_tx.send(()).unwrap_or(());
    });
    (client, lines_rx, served_rx)
}

fn wire_value(deadline: Deadline, lines: &mpsc::Receiver<Option<String>>, what: &str) -> Value {
    serde_json::from_str(&deadline.recv_or_fail(lines, what).unwrap()).unwrap()
}
fn stop(hub: &Arc<Hub>, got: &AtomicI32, done: &mpsc::Receiver<i32>) {
    await_zero(hub);
    signal(hub, got);
    assert_eq!(
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(done, "hub stops"),
        128 + signal_hook::consts::SIGTERM
    );
}

#[test]
fn pair_returns_a_token_named_after_the_device_and_writes_its_record() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, got, done) = serve_with_port(&temp, vec![], clock);
    let client = helloed(port, None);
    let code = mint(&hub, "phone");
    let token = pair(&client, &code);
    let record: Value =
        serde_json::from_slice(&fs::read(temp.dir.join("hub/devices/phone")).unwrap()).unwrap();
    assert_eq!(record["name"], "phone");
    assert_eq!(
        record["token_sha256"],
        crate::devices::sha256_hex(token.as_bytes())
    );
    assert_eq!(record["paired_at"], record["last_connected_at"]);
    client.text(r#"{"id":"status","command":"status"}"#);
    assert_eq!(
        text(Deadline::after(MUST_SUCCEED_WITHIN), &client)["payload"]["result"]["clients"],
        1
    );
    let log = temp.log();
    assert!(log.contains("device_paired") && log.contains("phone was paired by local"));
    assert!(!log.contains(&token) && !log.contains(&code));
    let again = helloed(port, None);
    again.text(&json!({"id":"a","command":"authenticate","args":{"token":token}}).to_string());
    assert_eq!(
        text(Deadline::after(MUST_SUCCEED_WITHIN), &again)["kind"],
        "command_accepted"
    );
    let reused = helloed(port, None);
    reused.text(&json!({"id":"bad","command":"pair","args":{"code":code}}).to_string());
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    assert_eq!(text(deadline, &reused)["payload"]["code"], "pairing_failed");
    assert_eq!(
        deadline.recv_or_fail(&reused.rx, "used code closes"),
        Item::Close(1008, "pairing_failed".to_owned())
    );
    assert_eq!(
        deadline.recv_or_fail(&reused.rx, "used code EOF"),
        Item::Eof
    );
    drop((client, again, reused));
    stop(&hub, &got, &done);
}

#[test]
fn an_uppercased_code_pairs() {
    let temp = Temp::new();
    let (port, hub, got, done) = serve_with_port(&temp, vec![], FakeClock::new());
    let client = helloed(port, None);
    pair(&client, &mint(&hub, "phone").to_ascii_uppercase());
    drop(client);
    stop(&hub, &got, &done);
}

#[test]
fn a_code_exactly_ten_minutes_old_pairs() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, got, done) = serve_with_port(&temp, vec![], Arc::clone(&clock));
    let client = helloed(port, None);
    let code = mint(&hub, "phone");
    // The counted connection keeps the idle wait free of a deadline; no waiter is released.
    assert!(
        clock.await_parked_unbounded(fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the idle wait parks with no deadline"
    );
    clock.advance_when_parked(
        None,
        None,
        Duration::from_secs(600),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    pair(&client, &code);
    drop(client);
    stop(&hub, &got, &done);
}

#[test]
fn a_code_past_ten_minutes_fails() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let (port, hub, got, done) = serve_with_port(&temp, vec![], Arc::clone(&clock));
    let client = helloed(port, None);
    let code = mint(&hub, "phone");
    assert!(
        clock.await_parked_unbounded(fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the idle wait parks with no deadline"
    );
    clock.advance_when_parked(
        None,
        None,
        Duration::from_secs(600) + TICK,
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    client.text(&json!({"id":"p","command":"pair","args":{"code":code}}).to_string());
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    let refused = text(deadline, &client);
    assert_eq!(refused["payload"]["code"], "pairing_failed");
    assert_eq!(refused["payload"]["message"], PAIRING_FAILED);
    assert_eq!(
        deadline.recv_or_fail(&client.rx, "expired code closes"),
        Item::Close(1008, "pairing_failed".to_owned())
    );
    assert_eq!(
        deadline.recv_or_fail(&client.rx, "expired code EOF"),
        Item::Eof
    );
    drop(client);
    stop(&hub, &got, &done);
}

#[test]
fn a_wrong_shaped_pair_fails_pairing_failed() {
    let temp = Temp::new();
    let (port, hub, got, done) = serve_with_port(&temp, vec![], FakeClock::new());
    for args in [json!({}), json!({"code":1}), json!({"code":"x","extra":1})] {
        let client = helloed(port, None);
        client.text(&json!({"id":"p","command":"pair","args":args}).to_string());
        let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
        assert_eq!(text(deadline, &client)["payload"]["code"], "pairing_failed");
        assert_eq!(
            deadline.recv_or_fail(&client.rx, "wrong shape closes"),
            Item::Close(1008, "pairing_failed".to_owned())
        );
        assert_eq!(
            deadline.recv_or_fail(&client.rx, "wrong shape EOF"),
            Item::Eof
        );
    }
    stop(&hub, &got, &done);
}

#[test]
fn a_code_for_a_device_that_now_has_a_record_is_refused_and_consumed() {
    let temp = Temp::new();
    let (port, hub, got, done) = serve_with_port(&temp, vec![], FakeClock::new());
    let client = helloed(port, None);
    let code = mint(&hub, "phone");
    pair_device(&temp.dir, "phone", "manual");
    let before = fs::read(temp.dir.join("hub/devices/phone")).unwrap();
    client.text(&json!({"id":"p","command":"pair","args":{"code":code}}).to_string());
    assert_eq!(
        text(Deadline::after(MUST_SUCCEED_WITHIN), &client)["payload"]["code"],
        "pairing_failed"
    );
    assert_eq!(
        fs::read(temp.dir.join("hub/devices/phone")).unwrap(),
        before
    );
    assert_eq!(hub.pairing.pending(), 0);
    fs::remove_file(temp.dir.join("hub/devices/phone")).unwrap();
    assert!(
        hub.pairing
            .redeem(&hub, 99, &crate::testkit::args(json!({"code":code})))
            .is_err()
    );
    drop(client);
    stop(&hub, &got, &done);
}

#[test]
fn revoke_from_a_second_device_closes_the_first() {
    let temp = Temp::new();
    let (port, hub, got, done) = serve_with_port(&temp, vec![], FakeClock::new());
    let a = helloed(port, None);
    let token = pair(&a, &mint(&hub, "a"));
    let b = helloed(port, None);
    pair(&b, &mint(&hub, "b"));
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    assert_eq!(text(deadline, &a)["payload"]["device"], "b");
    b.text(r#"{"id":"r","command":"revoke","args":{"device":"a"}}"#);
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    assert_eq!(text(deadline, &b)["kind"], "command_accepted");
    let notice = text(deadline, &b);
    assert_eq!(
        notice["payload"],
        json!({"by":"b","change":"revoked","device":"a"})
    );
    assert_eq!(text(deadline, &a)["payload"]["change"], "revoked");
    assert_eq!(
        deadline.recv_or_fail(&a.rx, "revoked connection closes"),
        Item::Close(1000, String::new())
    );
    assert_eq!(
        deadline.recv_or_fail(&a.rx, "revoked connection EOF"),
        Item::Eof
    );
    let next = helloed(port, None);
    next.text(&json!({"id":"a","command":"authenticate","args":{"token":token}}).to_string());
    assert_eq!(
        text(Deadline::after(MUST_SUCCEED_WITHIN), &next)["payload"]["code"],
        "unauthenticated"
    );
    assert!(!temp.dir.join("hub/devices/a").exists());
    // The revoke line follows the client-visible notices and closes, so
    // wait for it under a deadline instead of reading the log once.
    crate::testkit::await_true("the hub logs the revocation", {
        let home = temp.dir.clone();
        move || super::log_has(&home, "a was revoked by b")
    });
    drop((a, b, next));
    stop(&hub, &got, &done);
}

#[test]
fn revoking_copied_records_refuses_the_token_after_a_restart() {
    let temp = Temp::new();
    let (port, hub, got, done) = serve_with_port(&temp, vec![], FakeClock::new());
    let client = helloed(port, None);
    let token = pair(&client, &mint(&hub, "phone"));
    let devices = temp.dir.join("hub/devices");
    fs::copy(devices.join("phone"), devices.join("tablet")).unwrap();
    pair_device(&temp.dir, "other", "other-token");
    fs::create_dir(devices.join("directory")).unwrap();
    std::os::unix::fs::symlink("other", devices.join("link")).unwrap();
    client.text(r#"{"id":"r","command":"revoke","args":{"device":"phone"}}"#);
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    assert_eq!(text(deadline, &client)["kind"], "command_accepted");
    assert_eq!(text(deadline, &client)["payload"]["change"], "revoked");
    assert_eq!(
        deadline.recv_or_fail(&client.rx, "copied token's live connection closes"),
        Item::Close(1000, String::new())
    );
    assert_eq!(
        deadline.recv_or_fail(&client.rx, "revoked connection EOF"),
        Item::Eof
    );
    for name in ["phone", "tablet"] {
        assert!(!devices.join(name).exists(), "{name} must be removed");
    }
    assert!(devices.join("other").is_file());
    assert!(devices.join("directory").is_dir());
    assert!(
        fs::symlink_metadata(devices.join("link"))
            .unwrap()
            .is_symlink()
    );
    drop(client);
    stop(&hub, &got, &done);
    drop(hub);

    let (port, hub, got, done) = serve_with_port(&temp, vec![], FakeClock::new());
    let next = helloed(port, None);
    assert_eq!(
        authenticate(&next, "a", &token)["payload"]["code"],
        "unauthenticated"
    );
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    assert_eq!(
        deadline.recv_or_fail(&next.rx, "revoked token is refused after restart"),
        Item::Close(1008, "unauthenticated".to_owned())
    );
    assert_eq!(deadline.recv_or_fail(&next.rx, "refusal EOF"), Item::Eof);
    let other = helloed(port, None);
    assert_eq!(
        authenticate(&other, "a", "other-token")["kind"],
        "command_accepted"
    );
    drop((next, other));
    stop(&hub, &got, &done);
}

#[test]
fn an_unreadable_record_refuses_revoke_without_unlinking_any_record() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = Temp::new();
    let (port, hub, got, done) = serve_with_port(&temp, vec![], FakeClock::new());
    let client = helloed(port, None);
    let token = pair(&client, &mint(&hub, "phone"));
    let devices = temp.dir.join("hub/devices");
    fs::copy(devices.join("phone"), devices.join("tablet")).unwrap();
    pair_device(&temp.dir, "sibling", "other-token");
    let sibling = devices.join("sibling");
    let unreadable = fs::read(&sibling).unwrap();
    fs::set_permissions(&sibling, fs::Permissions::from_mode(0o0)).unwrap();
    // Both a sibling and the named record must be readable before any unlink.
    for device in ["phone", "sibling"] {
        client.text(&json!({"id":"r","command":"revoke","args":{"device":device}}).to_string());
        let refused = text(Deadline::after(MUST_SUCCEED_WITHIN), &client);
        assert_eq!(refused["kind"], "command_rejected");
        assert_eq!(refused["payload"]["code"], "io_failed");
        assert!(
            refused["payload"]["message"]
                .as_str()
                .unwrap()
                .contains("sibling")
        );
        for name in ["phone", "tablet", "sibling"] {
            assert!(devices.join(name).exists(), "{name} must remain");
        }
    }
    fs::set_permissions(&sibling, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(fs::read(&sibling).unwrap(), unreadable);
    // Malformed JSON and a missing/non-string hash are unreadable records too.
    for bytes in ["not json", "{}", r#"{"token_sha256":42}"#] {
        fs::write(&sibling, bytes).unwrap();
        client.text(r#"{"id":"r","command":"revoke","args":{"device":"phone"}}"#);
        let refused = text(Deadline::after(MUST_SUCCEED_WITHIN), &client);
        assert_eq!(refused["payload"]["code"], "io_failed");
        assert!(
            refused["payload"]["message"]
                .as_str()
                .unwrap()
                .contains("sibling")
        );
        assert!(devices.join("phone").exists() && devices.join("tablet").exists());
        assert_eq!(fs::read(&sibling).unwrap(), bytes.as_bytes());
    }
    let next = helloed(port, None);
    assert_eq!(authenticate(&next, "a", &token)["kind"], "command_accepted");
    drop((client, next));
    stop(&hub, &got, &done);
}

#[test]
fn a_device_revoking_itself_gets_the_answer_then_the_close() {
    let temp = Temp::new();
    let (port, hub, got, done) = serve_with_port(&temp, vec![], FakeClock::new());
    let client = helloed(port, None);
    pair(&client, &mint(&hub, "phone"));
    client.text(r#"{"id":"r","command":"revoke","args":{"device":"phone"}}"#);
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    let accepted = text(deadline, &client);
    assert_eq!(accepted["kind"], "command_accepted");
    assert_eq!(accepted["payload"]["command_id"], "r");
    let notice = text(deadline, &client);
    assert_eq!(notice["kind"], "device_changed");
    assert_eq!(
        notice["payload"],
        json!({"by":"phone","change":"revoked","device":"phone"})
    );
    assert_eq!(
        deadline.recv_or_fail(&client.rx, "self revoke closes"),
        Item::Close(1000, String::new())
    );
    assert_eq!(
        deadline.recv_or_fail(&client.rx, "self revoke EOF"),
        Item::Eof
    );
    drop(client);
    stop(&hub, &got, &done);
}

#[test]
fn a_code_minted_on_the_port_names_its_device_as_issuer() {
    let temp = Temp::new();
    let (port, hub, got, done) = serve_with_port(&temp, vec![], FakeClock::new());
    let b = helloed(port, None);
    pair(&b, &mint(&hub, "b"));
    b.text(r#"{"id":"mint","command":"pairing_code","args":{"device":"c"}}"#);
    let code = text(Deadline::after(MUST_SUCCEED_WITHIN), &b)["payload"]["result"]["code"]
        .as_str()
        .unwrap()
        .to_owned();
    let c = helloed(port, None);
    pair(&c, &code);
    let notice = text(Deadline::after(MUST_SUCCEED_WITHIN), &b);
    assert_eq!(notice["payload"]["by"], "b");
    assert_eq!(notice["payload"]["device"], "c");
    drop((b, c));
    stop(&hub, &got, &done);
}

#[test]
fn a_failed_record_write_closes_1011() {
    let temp = Temp::new();
    let (port, hub, got, done) = serve_with_port(&temp, vec![], FakeClock::new());
    let client = helloed(port, None);
    let code = mint(&hub, "phone");
    fs::create_dir_all(temp.dir.join("hub")).unwrap();
    fs::write(temp.dir.join("hub/devices"), "obstacle").unwrap();
    client.text(&json!({"id":"p","command":"pair","args":{"code":code}}).to_string());
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    assert_eq!(text(deadline, &client)["payload"]["code"], "io_failed");
    assert_eq!(
        deadline.recv_or_fail(&client.rx, "write failure closes"),
        Item::Close(1011, String::new())
    );
    assert_eq!(
        deadline.recv_or_fail(&client.rx, "write failure EOF"),
        Item::Eof
    );
    assert_eq!(hub.pairing.live(), 0);
    drop(client);
    stop(&hub, &got, &done);
}

#[test]
fn self_revocation_waits_for_the_output_pump_before_shutting_tcp() {
    // Holding Web's sink forces the drain thread to finish while the pump
    // still owes the acknowledgement, announcement and close frame.
    let temp = Temp::new();
    let clock = FakeClock::new();
    let hub = Arc::new(crate::testkit::hub(
        &temp.dir,
        Arc::new(FakeStarter::hang(&temp.dir)),
        Arc::clone(&clock) as Arc<dyn Clock>,
    ));
    pair_device(&temp.dir, "phone", "token");
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    let web = Arc::new(Web {
        control: server.try_clone().unwrap(),
        sink: Mutex::new(Sink {
            stream: server,
            closed: false,
        }),
        drained: AtomicBool::new(false),
    });
    let who = hub.pairing.authenticate(&hub, 1, "token").unwrap();
    hub.pairing.attach_web(&who, Arc::clone(&web));
    let (writer, bridge) = UnixStream::pair().unwrap();
    let writer = Arc::new(Mutex::new(writer));
    hub.pairing.attach(&who, &writer);
    let sink = web.hold();
    let (frames_tx, frames_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut read = BufReader::new(peer);
        while let Some(frame) = server_frame(&mut read) {
            frames_tx.send(frame).unwrap_or(());
        }
        frames_tx.send(Item::Eof).unwrap_or(());
    });
    let pumping = Arc::clone(&web);
    let tick = Arc::clone(&hub.tick);
    thread::spawn(move || {
        pump_lines(&pumping, bridge);
        pumping.drained.store(true, Ordering::SeqCst);
        tick.wake();
    });
    let end = clock.now() + STOP_CLOSE_WAIT;
    let revoked = match hub
        .pairing
        .revoke(&hub, &who, &crate::testkit::args(json!({"device":"phone"})))
        .unwrap()
    {
        Outcome::Done(r) => r,
        Outcome::Dropped => panic!("self revoke dropped"),
    };
    let mut accepted = connection::line(
        &hub,
        "command_accepted",
        serde_json::Map::from_iter([("command_id".to_owned(), Value::from("r"))]),
    );
    accepted.push(b'\n');
    lock(&writer).write_all(&accepted).unwrap();
    let (tx, rx) = mpsc::channel();
    let working = Arc::clone(&hub);
    thread::spawn(move || {
        working.pairing.finish(&working, revoked);
        tx.send(()).unwrap_or(());
    });
    assert!(clock.await_parked_count(end, 2, fakes::Deadline::after(MUST_SUCCEED_WITHIN)));
    assert!(!web.drained());
    let mark = clock
        .mark_parked(end, fakes::Deadline::after(MUST_SUCCEED_WITHIN))
        .expect("finish waits for the pump");
    drop(sink);
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    let first = deadline.recv_or_fail(&frames_rx, "acknowledgement after releasing the pump");
    let second = deadline.recv_or_fail(&frames_rx, "revocation after acknowledgement");
    for (item, kind) in [(first, "command_accepted"), (second, "device_changed")] {
        match item {
            Item::Text(bytes) => assert_eq!(
                serde_json::from_slice::<Value>(&bytes).unwrap()["kind"],
                kind
            ),
            other @ (Item::Response(_) | Item::Close(..) | Item::Pong(_) | Item::Eof) => {
                panic!("expected {kind}, got {other:?}")
            }
        }
    }
    assert_eq!(
        deadline.recv_or_fail(&frames_rx, "pump sends normal close"),
        Item::Close(1000, String::new())
    );
    assert_eq!(deadline.recv_or_fail(&frames_rx, "pump EOF"), Item::Eof);
    let finished_before_deadline = Deadline::after(Duration::from_millis(250))
        .recv(&rx)
        .is_ok();
    if !finished_before_deadline {
        let reparked = clock.await_parked_since(
            &mark,
            Some(end),
            fakes::Deadline::after(MUST_SUCCEED_WITHIN),
        );
        if reparked {
            clock.advance_when_parked_threads(
                Some(end),
                2,
                STOP_CLOSE_WAIT,
                &Deadline::after(MUST_SUCCEED_WITHIN),
            );
        }
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "finish observes pump completion");
    }
    assert!(
        finished_before_deadline,
        "finish returns when the pump drains"
    );
    assert!(web.drained());
}

#[test]
fn a_local_revoke_announces_without_waiting_for_the_clock() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let hub = test_hub(&temp, &clock);
    pair_device(&temp.dir, "phone", "token");
    let end = clock.now() + STOP_CLOSE_WAIT;
    let (mut client, lines, served) = authenticated(Arc::clone(&hub), 1, Who::Local);
    client
        .write_all(b"{\"id\":\"r\",\"command\":\"revoke\",\"args\":{\"device\":\"phone\"}}\n")
        .unwrap();
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    let accepted = wire_value(deadline, &lines, "local revoke acceptance");
    assert_eq!(accepted["kind"], "command_accepted");
    assert_eq!(accepted["payload"]["command_id"], "r");

    let early = Deadline::after(Duration::from_millis(250)).recv(&lines);
    let announced_without_clock = matches!(early, Ok(Some(_)));
    let event = match early {
        Ok(Some(line)) => serde_json::from_str::<Value>(&line).unwrap(),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            let parked =
                clock.await_parked_count(end, 2, fakes::Deadline::after(MUST_SUCCEED_WITHIN));
            if parked {
                clock.advance_when_parked_threads(
                    Some(end),
                    2,
                    STOP_CLOSE_WAIT,
                    &Deadline::after(MUST_SUCCEED_WITHIN),
                );
            }
            wire_value(
                Deadline::after(MUST_SUCCEED_WITHIN),
                &lines,
                "local revoke announcement",
            )
        }
        Ok(None) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("local revoke connection closed before its announcement")
        }
    };
    assert_eq!(event["kind"], "device_changed");
    assert_eq!(event["payload"]["change"], "revoked");
    assert_eq!(event["payload"]["device"], "phone");
    client.shutdown(Shutdown::Both).unwrap_or(());
    drop(client);
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&served, "local revoke connection exits");
    assert!(
        announced_without_clock,
        "a revoke with no live device does not wait for fake time"
    );
}

#[test]
fn finishing_a_revoke_returns_at_its_deadline_with_an_undrained_web() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let hub = test_hub(&temp, &clock);
    pair_device(&temp.dir, "phone", "token");
    let who = hub.pairing.authenticate(&hub, 1, "token").unwrap();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    let web = Arc::new(Web {
        control: server.try_clone().unwrap(),
        sink: Mutex::new(Sink {
            stream: server,
            closed: false,
        }),
        drained: AtomicBool::new(false),
    });
    hub.pairing.attach_web(&who, Arc::clone(&web));
    let end = clock.now() + STOP_CLOSE_WAIT;
    let (mut client, lines, served) = authenticated(Arc::clone(&hub), 1, who);
    client
        .write_all(b"{\"id\":\"r\",\"command\":\"revoke\",\"args\":{\"device\":\"phone\"}}\n")
        .unwrap();
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    assert_eq!(
        wire_value(deadline, &lines, "device revoke acceptance")["kind"],
        "command_accepted"
    );
    assert_eq!(
        wire_value(deadline, &lines, "device revoke notice")["payload"]["change"],
        "revoked"
    );
    assert!(clock.await_parked_count(end, 2, fakes::Deadline::after(MUST_SUCCEED_WITHIN)));
    client.shutdown(Shutdown::Write).unwrap_or(());

    clock.advance_when_parked_threads(
        Some(end),
        2,
        STOP_CLOSE_WAIT,
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    let returned_at_deadline = Deadline::after(Duration::from_millis(250))
        .recv(&served)
        .is_ok();
    if !returned_at_deadline {
        web.drained.store(true, Ordering::SeqCst);
        hub.tick.wake();
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&served, "finish exits after releasing the drain");
    }
    drop((client, peer));
    assert!(
        returned_at_deadline,
        "finish returns when its deadline expires"
    );
}

#[test]
fn revoke_watchdog_closes_a_live_device_at_the_deadline_while_answer_is_blocked() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let hub = test_hub(&temp, &clock);
    pair_device(&temp.dir, "phone", "token");
    let who = hub.pairing.authenticate(&hub, 1, "token").unwrap();
    let (target, peer) = UnixStream::pair().unwrap();
    hub.pairing.attach(&who, &Arc::new(Mutex::new(target)));
    let (closed_tx, closed_rx) = mpsc::channel();
    let mut target_read = peer.try_clone().unwrap();
    thread::spawn(move || {
        let mut bytes = [0; 1024];
        loop {
            match target_read.read(&mut bytes) {
                Ok(0) | Err(_) => {
                    closed_tx.send(()).unwrap_or(());
                    break;
                }
                Ok(_) => {}
            }
        }
    });

    let end = clock.now() + STOP_CLOSE_WAIT;
    let (service, mut client) = UnixStream::pair().unwrap();
    let filler = service.try_clone().unwrap();
    filler.set_nonblocking(true).unwrap();
    while (&filler).write(&[0; 4096]).is_ok() {}
    filler.set_nonblocking(false).unwrap();
    drop(filler);
    let (served_tx, served_rx) = mpsc::channel();
    let working = Arc::clone(&hub);
    thread::spawn(move || {
        crate::connection::serve_authenticated(service, working, 2, Who::Local, None);
        served_tx.send(()).unwrap_or(());
    });
    client
        .write_all(b"{\"id\":\"r\",\"command\":\"revoke\",\"args\":{\"device\":\"phone\"}}\n")
        .unwrap();

    let watchdog_waited = clock.await_parked(end, fakes::Deadline::after(MUST_SUCCEED_WITHIN));
    let closed_before_deadline = closed_rx.try_recv().is_ok();
    let closed_at_deadline = if watchdog_waited {
        clock.advance_when_parked(
            Some(end),
            None,
            STOP_CLOSE_WAIT,
            &Deadline::after(MUST_SUCCEED_WITHIN),
        );
        Deadline::after(Duration::from_millis(250))
            .recv(&closed_rx)
            .is_ok()
    } else {
        false
    };
    client.shutdown(Shutdown::Both).unwrap_or(());
    drop(client);
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&served_rx, "blocked revoke answer is released");
    if !closed_at_deadline {
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&closed_rx, "finish closes the revoked device");
    }
    hub.pairing.forget(&hub, &who);
    drop(peer);
    assert!(
        watchdog_waited,
        "the revoke watchdog waits before its deadline"
    );
    assert!(
        !closed_before_deadline,
        "the device remains open before the deadline"
    );
    assert!(
        closed_at_deadline,
        "the watchdog shuts the device at the deadline"
    );
}

#[test]
fn attach_web_stores_the_handle_finish_waits_for_its_drain() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let hub = test_hub(&temp, &clock);
    pair_device(&temp.dir, "phone", "token");
    let who = hub.pairing.authenticate(&hub, 1, "token").unwrap();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    let web = Arc::new(Web {
        control: server.try_clone().unwrap(),
        sink: Mutex::new(Sink {
            stream: server,
            closed: false,
        }),
        drained: AtomicBool::new(false),
    });
    hub.pairing.attach_web(&who, Arc::clone(&web));
    let end = clock.now() + STOP_CLOSE_WAIT;
    let revoked = match hub
        .pairing
        .revoke(
            &hub,
            &Who::Local,
            &crate::testkit::args(json!({"device":"phone"})),
        )
        .unwrap()
    {
        Outcome::Done(revoked) => revoked,
        Outcome::Dropped => panic!("local revoke dropped"),
    };
    let (tx, rx) = mpsc::channel();
    let working = Arc::clone(&hub);
    thread::spawn(move || {
        working.pairing.finish(&working, revoked);
        tx.send(()).unwrap_or(());
    });
    // The watchdog and `finish` are both parked on the clock while the
    // stored web never drains, so `finish` cannot have returned yet.
    assert!(clock.await_parked_count(end, 2, fakes::Deadline::after(MUST_SUCCEED_WITHIN)));
    assert!(
        rx.try_recv().is_err(),
        "finish waits for the attached web to drain"
    );
    clock.advance_when_parked_threads(
        Some(end),
        2,
        STOP_CLOSE_WAIT,
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "finish returns at its deadline");
    drop((peer, web));
}

#[test]
fn attach_web_after_revocation_shuts_the_web() {
    let temp = Temp::new();
    let clock = FakeClock::new();
    let hub = test_hub(&temp, &clock);
    pair_device(&temp.dir, "phone", "token");
    let who = hub.pairing.authenticate(&hub, 1, "token").unwrap();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    let web = Arc::new(Web {
        control: server.try_clone().unwrap(),
        sink: Mutex::new(Sink {
            stream: server,
            closed: false,
        }),
        drained: AtomicBool::new(false),
    });
    let revoked = match hub
        .pairing
        .revoke(
            &hub,
            &Who::Local,
            &crate::testkit::args(json!({"device":"phone"})),
        )
        .unwrap()
    {
        Outcome::Done(revoked) => revoked,
        Outcome::Dropped => panic!("local revoke dropped"),
    };
    hub.pairing.attach_web(&who, Arc::clone(&web));
    // A web attached after revocation is shut, so the peer reads EOF.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut peer = peer;
        let mut bytes = [0; 1024];
        loop {
            match peer.read(&mut bytes) {
                Ok(0) | Err(_) => {
                    tx.send(()).unwrap_or(());
                    break;
                }
                Ok(_) => {}
            }
        }
    });
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "revocation shuts the late web");
    // The revoke snapshot holds no web or writer, so finish returns at
    // once and releases the revoke watchdog.
    let (done_tx, done_rx) = mpsc::channel();
    let working = Arc::clone(&hub);
    thread::spawn(move || {
        working.pairing.finish(&working, revoked);
        done_tx.send(()).unwrap_or(());
    });
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&done_rx, "finish releases the revoke");
    drop(web);
}
