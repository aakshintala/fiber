//! Pairing, expiry and revocation over a real loopback websocket port.
use super::*;
use crate::pairing::{Outcome, Who};
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
    let deadline = Deadline::after(DEADLINE);
    let accepted = text(deadline, client);
    assert_eq!(accepted["kind"], "command_accepted");
    assert_eq!(accepted["payload"]["command_id"], "p1");
    let token = accepted["payload"]["result"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(token.len(), 43);
    let notice = text(deadline, client);
    assert_eq!(notice["kind"], "device_changed");
    assert_eq!(notice["payload"]["change"], "paired");
    token
}
fn stop(hub: &Arc<Hub>, got: &AtomicI32, done: &mpsc::Receiver<i32>) {
    await_zero(hub);
    signal(hub, got);
    assert_eq!(Deadline::after(DEADLINE).recv_or_fail(done, "hub stops"), 0);
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
        text(Deadline::after(DEADLINE), &client)["payload"]["result"]["clients"],
        1
    );
    let log = temp.log();
    assert!(log.contains("device_paired") && log.contains("phone was paired by local"));
    assert!(!log.contains(&token) && !log.contains(&code));
    let again = helloed(port, None);
    again.text(&json!({"id":"a","command":"authenticate","args":{"token":token}}).to_string());
    assert_eq!(
        text(Deadline::after(DEADLINE), &again)["kind"],
        "command_accepted"
    );
    let reused = helloed(port, None);
    reused.text(&json!({"id":"bad","command":"pair","args":{"code":code}}).to_string());
    let deadline = Deadline::after(DEADLINE);
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
    clock.advance(Duration::from_secs(600));
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
    clock.advance(Duration::from_secs(600) + TICK);
    client.text(&json!({"id":"p","command":"pair","args":{"code":code}}).to_string());
    let deadline = Deadline::after(DEADLINE);
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
        let deadline = Deadline::after(DEADLINE);
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
        text(Deadline::after(DEADLINE), &client)["payload"]["code"],
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
    let deadline = Deadline::after(DEADLINE);
    assert_eq!(text(deadline, &a)["payload"]["device"], "b");
    b.text(r#"{"id":"r","command":"revoke","args":{"device":"a"}}"#);
    let deadline = Deadline::after(DEADLINE);
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
        text(Deadline::after(DEADLINE), &next)["payload"]["code"],
        "unauthenticated"
    );
    assert!(!temp.dir.join("hub/devices/a").exists());
    assert!(temp.log().contains("a was revoked by b"));
    drop((a, b, next));
    stop(&hub, &got, &done);
}

#[test]
fn a_device_revoking_itself_gets_the_answer_then_the_close() {
    let temp = Temp::new();
    let (port, hub, got, done) = serve_with_port(&temp, vec![], FakeClock::new());
    let client = helloed(port, None);
    pair(&client, &mint(&hub, "phone"));
    client.text(r#"{"id":"r","command":"revoke","args":{"device":"phone"}}"#);
    let deadline = Deadline::after(DEADLINE);
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
    let code = text(Deadline::after(DEADLINE), &b)["payload"]["result"]["code"]
        .as_str()
        .unwrap()
        .to_owned();
    let c = helloed(port, None);
    pair(&c, &code);
    let notice = text(Deadline::after(DEADLINE), &b);
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
    let deadline = Deadline::after(DEADLINE);
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
    lock(&writer)
        .write_all(&connection::line(
            &hub,
            "command_accepted",
            serde_json::Map::from_iter([("command_id".to_owned(), Value::from("r"))]),
        ))
        .unwrap();
    let (tx, rx) = mpsc::channel();
    let working = Arc::clone(&hub);
    thread::spawn(move || {
        working.pairing.finish(&working, revoked);
        tx.send(()).unwrap_or(());
    });
    assert!(clock.await_parked(end, WITHIN));
    assert!(
        Deadline::after(Duration::from_millis(100))
            .recv(&rx)
            .is_err()
    );
    assert!(!web.drained());
    drop(sink);
    let deadline = Deadline::after(DEADLINE);
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
    deadline.recv_or_fail(&rx, "finish observes pump completion");
    assert!(web.drained());
}
