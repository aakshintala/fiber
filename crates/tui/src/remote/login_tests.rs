use super::*;
use contract::ErrorCode;
use fakes::Deadline;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

struct Show(Sender<(String, String)>);
impl LoginShow for Show {
    fn open(&self, _: &str) {
        panic!("remote login must never open a browser");
    }
    fn show(&self, url: &str, code: &str) {
        self.0.send((url.into(), code.into())).unwrap();
    }
}
struct Hub {
    router: Arc<Router>,
    lines: Receiver<Value>,
}
impl Hub {
    fn new() -> Self {
        let router = Router::new();
        let lines = Self::attach(&router);
        Self { router, lines }
    }
    fn attach(router: &Router) -> Receiver<Value> {
        let (client, hub) = UnixStream::pair().unwrap();
        router.attach(client);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(hub).lines() {
                let Ok(line) = line else {
                    break;
                };
                if tx.send(serde_json::from_str(&line).unwrap()).is_err() {
                    break;
                }
            }
        });
        rx
    }
    fn reply(&self, line: &Value, result: Value) {
        self.route(line, "command_accepted", json!({"result":result}));
    }
    fn route(&self, line: &Value, kind: &str, mut payload: Value) {
        payload["command_id"] = line["id"].clone();
        let answer = serde_json::from_value(
            json!({"kind":kind,"ts":1,"schema_version":1,"payload":payload}),
        )
        .unwrap();
        assert!(self.router.route(&crate::link::Line::Hub(answer)));
    }
    fn login(&self) -> (Arc<RemoteLogin>, Receiver<(String, String)>) {
        let (tx, rx) = mpsc::channel();
        (
            Arc::new(RemoteLogin::new(
                Arc::clone(&self.router),
                "acme".into(),
                Arc::new(Show(tx)),
            )),
            rx,
        )
    }
    fn barrier(&self, deadline: &Deadline) {
        let router = Arc::clone(&self.router);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            tx.send(router.ask("sentinel", json!({}), crate::remote::Caller::Loop))
                .unwrap();
        });
        let sentinel = self.line(deadline, "sentinel");
        self.reply(&sentinel, json!(null));
        assert!(deadline.recv_or_fail(&rx, "sentinel answer").is_ok());
    }
    fn line(&self, deadline: &Deadline, command: &str) -> Value {
        let line = deadline.recv_or_fail(&self.lines, command);
        assert_eq!(line["command"], command);
        line
    }
}
impl Drop for Hub {
    fn drop(&mut self) {
        self.router.detach();
    }
}
fn run(login: &Arc<RemoteLogin>) -> Receiver<Result<Stored, ConfigureError>> {
    let login = Arc::clone(login);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(login.run()).unwrap();
    });
    rx
}
fn cancel(login: &Arc<RemoteLogin>) -> Receiver<()> {
    let login = Arc::clone(login);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        login.cancel();
        tx.send(()).unwrap();
    });
    rx
}
fn hold(pause: &Mutex<Option<Pause>>) -> (Receiver<()>, Sender<()>) {
    let (tx, rx) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    *support::lock(pause) = Some((tx, resume));
    (rx, release)
}
fn waiting(router: &Router, deadline: &Deadline) {
    while router.link_waiters() != 1 {
        assert!(
            !deadline.left().is_zero(),
            "login did not park waiting for link"
        );
        thread::yield_now();
    }
}
fn no_answer<T>(rx: &Receiver<T>) {
    assert!(matches!(
        Deadline::after(Duration::from_millis(100)).recv(rx),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
}

#[test]
fn malformed_device_answers_fail_without_sending_a_later_command() {
    for answer in [
        json!({"code":"code"}),
        json!({"url":"url"}),
        json!({"url":7,"code":"code"}),
        json!({"url":"url","code":7}),
    ] {
        let d = Deadline::start();
        let hub = Hub::new();
        let (login, _shown) = hub.login();
        let result = run(&login);
        let start = hub.line(&d, "device_login");
        hub.reply(&start, answer);
        let error = d.recv_or_fail(&result, "malformed start").unwrap_err();
        assert_eq!(error.code, ErrorCode::IoFailed);
        assert!(error.message.contains("device_login"));
        assert!(hub.lines.try_recv().is_err());
    }
    let d = Deadline::start();
    let hub = Hub::new();
    let (login, shown) = hub.login();
    let result = run(&login);
    let start = hub.line(&d, "device_login");
    hub.reply(&start, json!({"url":"url","code":"code"}));
    d.recv_or_fail(&shown, "code");
    let finish = hub.line(&d, "device_login_result");
    hub.reply(&finish, json!({"path":7}));
    assert!(
        d.recv_or_fail(&result, "malformed stored path")
            .unwrap_err()
            .message
            .contains("device_login_result")
    );
}

#[test]
fn login_shows_device_code_and_stores_the_hubs_path() {
    let d = Deadline::start();
    let hub = Hub::new();
    let (login, shown) = hub.login();
    let result = run(&login);
    let start = hub.line(&d, "device_login");
    assert_eq!(start["args"], json!({"name":"acme"}));
    hub.reply(
        &start,
        json!({"url":"https://example.test/device","code":"ABCD"}),
    );
    assert_eq!(
        d.recv_or_fail(&shown, "device code"),
        ("https://example.test/device".into(), "ABCD".into())
    );
    let finish = hub.line(&d, "device_login_result");
    assert!(finish.get("args").is_none());
    hub.reply(&finish, json!({"path":"credentials/acme/default"}));
    assert_eq!(
        d.recv_or_fail(&result, "stored login").unwrap(),
        Stored {
            path: "credentials/acme/default".into(),
            replaced: false
        }
    );
}

#[test]
fn rejected_start_and_cancel_before_run_write_no_result() {
    let d = Deadline::start();
    let hub = Hub::new();
    let (login, _) = hub.login();
    d.recv_or_fail(&cancel(&login), "early cancel");
    assert_eq!(
        d.recv_or_fail(&run(&login), "cancelled run")
            .unwrap_err()
            .code,
        ErrorCode::Cancelled
    );
    assert!(hub.lines.try_recv().is_err());
    let (login, _) = hub.login();
    let result = run(&login);
    let start = hub.line(&d, "device_login");
    hub.route(
        &start,
        "command_rejected",
        json!({"code":"authentication_failed","message":"refused"}),
    );
    assert_eq!(
        d.recv_or_fail(&result, "refused login").unwrap_err(),
        ConfigureError {
            code: ErrorCode::AuthenticationFailed,
            message: "refused".into()
        }
    );
    assert!(hub.lines.try_recv().is_err());
}

#[test]
fn cancellation_waits_for_its_answer_without_holding_login_state() {
    let d = Deadline::start();
    let hub = Hub::new();
    let (login, shown) = hub.login();
    let result = run(&login);
    let start = hub.line(&d, "device_login");
    hub.reply(&start, json!({"url":"url","code":"code"}));
    d.recv_or_fail(&shown, "shown");
    let finish = hub.line(&d, "device_login_result");
    let cancelled = cancel(&login);
    let command = hub.line(&d, "device_login_cancel");
    assert!(login.l1_free());
    no_answer(&cancelled);
    hub.route(
        &finish,
        "command_rejected",
        json!({"code":"cancelled","message":"cancelled"}),
    );
    hub.reply(&command, json!(null));
    d.recv_or_fail(&cancelled, "cancel answer");
    assert_eq!(
        d.recv_or_fail(&result, "cancelled result")
            .unwrap_err()
            .code,
        ErrorCode::Cancelled
    );
}

#[test]
fn detached_start_retries_outside_l1_and_can_be_cancelled_there() {
    for cancelled in [false, true] {
        let d = Deadline::start();
        let mut hub = Hub::new();
        let (login, shown) = hub.login();
        let (paused, release) = hold(&login.pause_before_start);
        let result = run(&login);
        d.recv_or_fail(&paused, "start under L1");
        hub.router.detach();
        release.send(()).unwrap();
        waiting(&hub.router, &d);
        assert!(login.l1_free());
        assert!(hub.lines.try_recv().is_err());
        if cancelled {
            d.recv_or_fail(&cancel(&login), "cancel detached start");
            assert_eq!(
                d.recv_or_fail(&result, "cancelled link wait")
                    .unwrap_err()
                    .code,
                ErrorCode::Cancelled
            );
            let (tx, rx) = mpsc::channel();
            let router = Arc::clone(&hub.router);
            thread::spawn(move || {
                let lines = Hub::attach(&router);
                tx.send(lines).unwrap();
            });
            let _lines = d.recv_or_fail(&rx, "attach after cancellation");
        } else {
            hub.lines = Hub::attach(&hub.router);
            let start = hub.line(&d, "device_login");
            hub.reply(&start, json!({"url":"url","code":"code"}));
            assert_eq!(
                d.recv_or_fail(&shown, "retry device code"),
                ("url".into(), "code".into())
            );
            let finish = hub.line(&d, "device_login_result");
            hub.reply(&finish, json!({"path":"stored"}));
            assert_eq!(
                d.recv_or_fail(&result, "retry stored").unwrap().path,
                "stored"
            );
        }
    }
}

#[test]
fn cancel_serializes_with_start_and_result_writes() {
    for at_result in [false, true] {
        let d = Deadline::start();
        let hub = Hub::new();
        let (login, shown) = hub.login();
        let pause = if at_result {
            &login.pause_before_result_send
        } else {
            &login.pause_before_start
        };
        let (paused, release) = hold(pause);
        let result = run(&login);
        if at_result {
            let start = hub.line(&d, "device_login");
            hub.reply(&start, json!({"url":"url","code":"code"}));
            d.recv_or_fail(&shown, "shown before result pause");
        }
        d.recv_or_fail(&paused, "write under L1");
        let cancelled = cancel(&login);
        no_answer(&cancelled);
        release.send(()).unwrap();
        let first = hub.line(
            &d,
            if at_result {
                "device_login_result"
            } else {
                "device_login"
            },
        );
        let command = hub.line(&d, "device_login_cancel");
        assert!(login.l1_free());
        hub.route(
            &first,
            "command_rejected",
            json!({"code":"cancelled","message":"cancelled"}),
        );
        hub.reply(&command, json!(null));
        d.recv_or_fail(&cancelled, "serialized cancel");
        assert_eq!(
            d.recv_or_fail(&result, "serialized run").unwrap_err().code,
            ErrorCode::Cancelled
        );
    }
}

#[test]
fn cancelling_a_before_b_starts_prevents_as_result_from_reaching_b() {
    let d = Deadline::start();
    let hub = Hub::new();
    let (a, shown) = hub.login();
    let (paused, release) = hold(&a.pause_before_result);
    let a_result = run(&a);
    let start = hub.line(&d, "device_login");
    hub.reply(&start, json!({"url":"a","code":"A"}));
    d.recv_or_fail(&shown, "A code");
    d.recv_or_fail(&paused, "A before result");
    let cancelled = cancel(&a);
    let command = hub.line(&d, "device_login_cancel");
    hub.reply(&command, json!(null));
    d.recv_or_fail(&cancelled, "A cancelled");
    let (b, shown) = hub.login();
    let b_result = run(&b);
    let start = hub.line(&d, "device_login");
    hub.reply(&start, json!({"url":"b","code":"B"}));
    d.recv_or_fail(&shown, "B code");
    release.send(()).unwrap();
    assert_eq!(
        d.recv_or_fail(&a_result, "A cannot send result")
            .unwrap_err()
            .code,
        ErrorCode::Cancelled
    );
    let finish = hub.line(&d, "device_login_result");
    hub.reply(&finish, json!({"path":"B stored"}));
    assert_eq!(
        d.recv_or_fail(&b_result, "B stored").unwrap().path,
        "B stored"
    );
    hub.barrier(&d);
}

#[test]
fn reconnect_before_result_never_writes_to_the_new_connection() {
    let d = Deadline::start();
    let mut hub = Hub::new();
    let (login, shown) = hub.login();
    let (paused, release) = hold(&login.pause_before_result);
    let result = run(&login);
    let start = hub.line(&d, "device_login");
    hub.reply(&start, json!({"url":"url","code":"code"}));
    d.recv_or_fail(&shown, "shown");
    d.recv_or_fail(&paused, "before reconnect");
    hub.router.detach();
    hub.lines = Hub::attach(&hub.router);
    release.send(()).unwrap();
    assert_eq!(
        d.recv_or_fail(&result, "connection changed")
            .unwrap_err()
            .message,
        "connection changed"
    );
    assert_eq!(hub.router.registered(), 0);
    hub.barrier(&d);
    d.recv_or_fail(&cancel(&login), "cancel older epoch");
    hub.barrier(&d);
}

#[test]
fn lost_start_and_lost_result_end_run_and_cancel_without_retry() {
    for during_result in [false, true] {
        let d = Deadline::start();
        let hub = Hub::new();
        let (login, shown) = hub.login();
        let result = run(&login);
        let start = hub.line(&d, "device_login");
        if during_result {
            hub.reply(&start, json!({"url":"url","code":"code"}));
            d.recv_or_fail(&shown, "shown");
            hub.line(&d, "device_login_result");
            let cancelled = cancel(&login);
            hub.line(&d, "device_login_cancel");
            hub.router.detach();
            d.recv_or_fail(&cancelled, "lost cancel");
        } else {
            hub.router.detach();
        }
        assert_eq!(
            d.recv_or_fail(&result, "lost login").unwrap_err().message,
            "connection lost"
        );
        d.recv_or_fail(&cancel(&login), "detached cancel");
        let reconnected = Hub {
            router: Arc::clone(&hub.router),
            lines: Hub::attach(&hub.router),
        };
        d.recv_or_fail(&cancel(&login), "reattached cancelled login");
        reconnected.barrier(&d);
    }
}
