//! Device login answers and cancellation, with provider progress held by channels.
use crate::connection::lock;
use crate::error::DeviceLoginError;
use crate::fake::{FakeLogin, FakeMachine};
use crate::file_search::tests::Client;
use crate::testkit::DEADLINE;
use contract::{ErrorCode, shapes::Failure};
use fakes::Deadline;
use serde_json::{Value, json};
use std::sync::{Arc, mpsc};

fn failure() -> DeviceLoginError {
    DeviceLoginError::Forwarded(Failure {
        code: ErrorCode::AuthenticationFailed,
        message: "cancelled".into(),
        provider: None,
        retry_after_ms: None,
    })
}
fn login(
    shows: bool,
    outcome: Result<String, DeviceLoginError>,
    hold_cancel: bool,
) -> (
    Arc<FakeLogin>,
    mpsc::Receiver<()>,
    mpsc::Sender<()>,
    mpsc::Receiver<()>,
) {
    let (waiting, waits) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let (cancelled, cancels) = mpsc::channel();
    (
        Arc::new(FakeLogin {
            shows,
            outcome: outcome.map_err(|error| Failure::from(&error)),
            hold_cancel,
            waiting,
            gate: std::sync::Mutex::new(gate),
            release: std::sync::Mutex::new(Some(release.clone())),
            cancelled,
            stopped: std::sync::atomic::AtomicBool::new(false),
        }),
        waits,
        release,
        cancels,
    )
}
fn install(machine: &FakeMachine, login: Arc<FakeLogin>) {
    lock(&machine.logins).push_back(login);
}
fn url(client: &mut Client, id: &str) {
    client.send(id, "device_login", json!({"name":"test"}));
    let answer = client.next();
    assert_eq!(answer["kind"], "command_accepted");
    assert_eq!(answer["payload"]["command_id"], id);
    assert_eq!(
        answer["payload"]["result"],
        json!({"url":"https://device","code":"CODE"})
    );
}
#[test]
fn a_device_code_is_answered_before_the_waiting_result() {
    let machine = Arc::new(FakeMachine::default());
    let (login, waiting, release, _) = login(true, Ok("credentials/test/me".into()), false);
    install(&machine, login);
    let mut client = Client::new(machine, false);
    url(&mut client, "start");
    Deadline::after(DEADLINE).recv_or_fail(&waiting, "provider waiting");
    client.send("result", "device_login_result", json!({}));
    client.barrier();
    release.send(()).unwrap();
    let answer = client.next();
    assert_eq!(answer["payload"]["command_id"], "result");
    assert_eq!(
        answer["payload"]["result"],
        json!({"path":"credentials/test/me"})
    );
}
#[test]
fn a_result_asked_after_finish_takes_the_stored_outcome_once() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;
    let machine = Arc::new(FakeMachine::default());
    let (login, waiting, release, _) = login(true, Ok("stored".into()), false);
    install(&machine, login);
    let client = Client::new(machine, false);
    let (write, read) = UnixStream::pair().unwrap();
    read.set_read_timeout(Some(DEADLINE)).unwrap();
    let writer = Arc::new(std::sync::Mutex::new(write));
    let mut reader = BufReader::new(read);
    let mut slot = super::Login::default();
    let (finished, finish) = mpsc::channel();
    *lock(&slot.after_finish) = Some(Box::new(move || {
        finished.send(()).unwrap();
    }));
    slot.start(
        &contract::CommandId("start".into()),
        &crate::testkit::args(json!({"name":"test"})),
        &client.hub,
        &writer,
    );
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["payload"]["result"]["code"],
        "CODE"
    );
    Deadline::after(DEADLINE).recv_or_fail(&waiting, "provider waiting");
    release.send(()).unwrap();
    Deadline::after(DEADLINE).recv_or_fail(&finish, "slot stored completed outcome");
    slot.result(
        &contract::CommandId("result".into()),
        &Default::default(),
        &client.hub,
        &writer,
    );
    line.clear();
    reader.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["payload"]["result"],
        json!({"path":"stored"})
    );
    slot.result(
        &contract::CommandId("again".into()),
        &Default::default(),
        &client.hub,
        &writer,
    );
    line.clear();
    reader.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["payload"]["code"],
        "stale_request"
    );
}

#[test]
fn a_cancel_answers_only_after_the_provider_is_cancelled_and_resolves_its_waiter() {
    let machine = Arc::new(FakeMachine::default());
    let (login, waiting, _release, cancelled) = login(true, Ok("path".into()), false);
    install(&machine, login);
    let mut client = Client::new(machine, false);
    url(&mut client, "start");
    Deadline::after(DEADLINE).recv_or_fail(&waiting, "provider waiting");
    client.send("result", "device_login_result", json!({}));
    client.send("cancel", "device_login_cancel", json!({}));
    let deadline = Deadline::after(DEADLINE);
    deadline.recv_or_fail(&cancelled, "provider cancelled");
    let mut answers = [
        deadline.recv_or_fail(&client.answers, "cancel answer"),
        deadline.recv_or_fail(&client.answers, "result answer"),
    ];
    answers.sort_by_key(|v: &Value| v["payload"]["command_id"].as_str().unwrap().to_owned());
    assert_eq!(answers[0]["payload"]["command_id"], "cancel");
    assert_eq!(answers[0]["kind"], "command_accepted");
    assert!(answers[0]["payload"].get("result").is_none());
    assert_eq!(answers[1]["payload"]["command_id"], "result");
    assert_eq!(answers[1]["payload"]["code"], "authentication_failed");
}
#[test]
fn a_superseded_login_cannot_take_its_replacements_waiter() {
    let machine = Arc::new(FakeMachine::default());
    let (a, a_waiting, a_release, a_cancelled) = login(true, Ok("a".into()), true);
    let (b, b_waiting, b_release, _) = login(true, Ok("b".into()), false);
    install(&machine, a);
    install(&machine, b);
    let mut client = Client::new(machine, false);
    url(&mut client, "a");
    Deadline::after(DEADLINE).recv_or_fail(&a_waiting, "A waiting");
    client.send("ar", "device_login_result", json!({}));
    url(&mut client, "b");
    let deadline = Deadline::after(DEADLINE);
    deadline.recv_or_fail(&a_cancelled, "A cancelled");
    deadline.recv_or_fail(&b_waiting, "B waiting");
    client.send("br", "device_login_result", json!({}));
    client.barrier();
    a_release.send(()).unwrap();
    let answer = client.next();
    assert_eq!(answer["payload"]["command_id"], "ar");
    assert_eq!(answer["payload"]["code"], "authentication_failed");
    b_release.send(()).unwrap();
    let answer = client.next();
    assert_eq!(answer["payload"]["command_id"], "br");
    assert_eq!(answer["payload"]["result"], json!({"path":"b"}));
}
#[test]
fn a_disconnect_cancels_the_connections_login() {
    let machine = Arc::new(FakeMachine::default());
    let (login, waiting, _release, cancelled) = login(true, Ok("path".into()), false);
    install(&machine, login);
    let mut client = Client::new(machine, false);
    url(&mut client, "start");
    Deadline::after(DEADLINE).recv_or_fail(&waiting, "provider waiting");
    client.socket.shutdown(std::net::Shutdown::Both).unwrap();
    Deadline::after(DEADLINE).recv_or_fail(&cancelled, "disconnect cancelled login");
}
#[test]
fn commands_without_a_login_and_a_second_waiter_are_stale() {
    let machine = Arc::new(FakeMachine::default());
    let (login, waiting, release, _) = login(true, Ok("path".into()), false);
    install(&machine, login);
    let mut client = Client::new(machine, false);
    for command in ["device_login_result", "device_login_cancel"] {
        client.send("none", command, json!({}));
        assert_eq!(client.next()["payload"]["code"], "stale_request");
    }
    url(&mut client, "start");
    Deadline::after(DEADLINE).recv_or_fail(&waiting, "provider waiting");
    client.send("r1", "device_login_result", json!({}));
    client.send("r2", "device_login_result", json!({}));
    let answer = client.next();
    assert_eq!(answer["payload"]["command_id"], "r2");
    assert_eq!(answer["payload"]["code"], "stale_request");
    release.send(()).unwrap();
    assert_eq!(client.next()["payload"]["command_id"], "r1");
}
#[test]
fn a_provider_that_never_shows_a_code_rejects_start_exactly_once() {
    for outcome in [Ok("must-not-leak".into()), Err(failure())] {
        let machine = Arc::new(FakeMachine::default());
        let (login, waiting, release, _) = login(false, outcome.clone(), false);
        install(&machine, login);
        let mut client = Client::new(machine, false);
        client.send("start", "device_login", json!({"name":"test"}));
        Deadline::after(DEADLINE).recv_or_fail(&waiting, "provider waiting");
        release.send(()).unwrap();
        let answer = client.next();
        assert_eq!(answer["payload"]["command_id"], "start");
        assert_eq!(answer["payload"]["code"], "authentication_failed");
        let message = match outcome {
            Ok(_) => "`test` has no device-code form.".to_owned(),
            Err(error) => error.to_string(),
        };
        assert_eq!(answer["payload"]["message"], message);
        client.send("result", "device_login_result", json!({}));
        assert_eq!(client.next()["payload"]["code"], "stale_request");
        client.barrier();
    }
}
#[test]
fn login_arguments_are_strict_and_spawn_failure_leaves_no_slot() {
    let mut client = Client::new(Arc::new(FakeMachine::default()), false);
    for args in [
        json!({}),
        json!({"name":null}),
        json!({"name":3}),
        json!({"name":"test","extra":true}),
    ] {
        client.send("bad", "device_login", args);
        assert_eq!(client.next()["payload"]["code"], "invalid_arguments");
    }
    for command in ["device_login_result", "device_login_cancel"] {
        client.send("bad", command, json!({"extra":true}));
        assert_eq!(client.next()["payload"]["code"], "invalid_arguments");
    }
    *lock(&client.hub.fail_spawn) = Some("hub-login");
    client.send("spawn", "device_login", json!({"name":"test"}));
    assert_eq!(client.next()["payload"]["code"], "io_failed");
    client.send("result", "device_login_result", json!({}));
    assert_eq!(client.next()["payload"]["code"], "stale_request");
}
