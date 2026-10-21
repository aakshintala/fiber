//! Mid-session credentials: attendance, ephemeral login URLs and bounded waits.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod common;

use contract::emit::Emit;
use contract::events::Event;
use contract::extension::ExtensionDoor;
use extensions::SessionExtensions;
use fakes::NoLock;
use serde_json::json;
use std::net::{Ipv4Addr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use common::{Setup, pair, write};
use contract::clock::Clock;
use contract::{ErrorCode, signing};
use extensions::{Browser, Error, LuaExtension, LuaProvider};
use fakes::clock::FakeClock;
use fakes::{Deadline, OauthReply, OauthServer};

const WAIT: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Recording {
    opened: Mutex<Vec<String>>,
    shown: Mutex<Vec<(String, String)>>,
    listener: Mutex<Option<TcpListener>>,
}

impl Browser for Recording {
    fn open(&self, url: &str) {
        self.opened.lock().unwrap().push(url.into());
    }
    fn show(&self, url: &str, code: &str) {
        self.shown.lock().unwrap().push((url.into(), code.into()));
    }
    fn attended(&self) -> bool {
        true
    }
    fn callback_listener(&self, port: u16) -> std::io::Result<TcpListener> {
        if let Some(listener) = self.listener.lock().unwrap().take() {
            return Ok(listener);
        }
        TcpListener::bind((Ipv4Addr::LOCALHOST, port))
    }
}

fn bare(
    setup: &Setup,
    clock: &Arc<FakeClock>,
    browser: Arc<Recording>,
    body: &str,
    name: &str,
) -> Arc<LuaProvider> {
    let dir = setup.root().join("acme");
    write(
        &dir.join("providers/acme.lua"),
        &format!(
            r#"
fiber.provider("acme", {{ credential = {{ timeout = 3000, run = function()
  pcall(host.oauth.refresh, function() error("refresh failed") end)
  {body}
end }} }})
"#
        ),
    );
    let ext =
        Arc::new(LuaExtension::new(name, dir, setup.home(), clock.clone()).with_browser(browser));
    let loaded = ext.clone();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        tx.send(loaded.provider_functions("acme")).unwrap_or(());
    });
    assert_eq!(
        Deadline::after(WAIT)
            .recv_or_fail(&rx, "provider registered")
            .unwrap(),
        ["credential"]
    );
    LuaProvider::new(ext, "acme")
}

fn login_wait(name: &str) -> mpsc::Receiver<std::thread::ThreadId> {
    let (tx, rx) = mpsc::channel();
    extensions::observe_login_wait(
        name.into(),
        Box::new(move |thread| tx.send(thread).unwrap_or(())),
    );
    rx
}

fn advance_login_deadline(
    clock: &FakeClock,
    deadline: std::time::Instant,
    ready: &mpsc::Receiver<std::thread::ThreadId>,
) {
    let wait = Deadline::after(WAIT);
    let thread = wait.recv_or_fail(ready, "the callback's login wait recorded");
    clock.advance_when_parked(
        Some(deadline),
        Some(thread),
        deadline.saturating_duration_since(clock.now()),
        &wait,
    );
}

#[test]
fn callback_login_expires_as_authentication_failed_and_frees_its_port() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let browser = Arc::new(Recording {
        listener: Mutex::new(Some(listener)),
        ..Recording::default()
    });
    let provider = bare(
        &setup,
        &clock,
        browser.clone(),
        &format!(
            "host.oauth.open('https://login.example/verify'); host.oauth.callback({{port={port}}}); return 'unused'"
        ),
        "callback-login",
    );
    let ready = login_wait("callback-login");
    let deadline = clock.now().checked_add(Duration::from_secs(3)).unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        tx.send(provider.credential_token(&pair("acme", "default")))
            .unwrap_or(());
    });
    advance_login_deadline(&clock, deadline, &ready);
    let error = Deadline::after(WAIT)
        .recv_or_fail(&rx, "credential callback timed out")
        .unwrap_err();
    assert!(
        matches!(error, signing::Error::LoginTimedOut { .. }),
        "{error:?}"
    );
    assert_eq!(
        *browser.opened.lock().unwrap(),
        ["https://login.example/verify"]
    );
    let freed = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    loop {
        if TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok() {
            break;
        }
        assert!(!freed.left().is_zero(), "callback released its port");
        std::thread::yield_now();
    }
}

#[test]
fn a_poll_sleep_past_the_credential_deadline_is_not_wrapped() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let server = OauthServer::start(vec![OauthReply::pending()]);
    let provider = bare(
        &setup,
        &clock,
        Arc::new(Recording::default()),
        &format!(
            "host.oauth.poll({{url='{}', body='device_code=d'}}); return 'unused'",
            server.url()
        ),
        "poll-login",
    );
    let ready = login_wait("poll-login");
    let deadline = clock.now().checked_add(Duration::from_secs(3)).unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        tx.send(provider.token(&pair("acme", "default")))
            .unwrap_or(());
    });
    assert!(
        server.await_requests(1, Deadline::after(WAIT)),
        "poll request arrived; early result: {:?}",
        rx.try_recv()
    );
    advance_login_deadline(&clock, deadline, &ready);
    let error = Deadline::after(WAIT)
        .recv_or_fail(&rx, "credential poll timed out")
        .unwrap_err();
    assert!(matches!(error, Error::LoginTimedOut { .. }), "{error:?}");
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed);
    assert_eq!(server.request_count(), 1);
}

struct Notices {
    events: Mutex<Vec<Event>>,
    tx: mpsc::Sender<Event>,
}
impl Emit for Notices {
    fn emit(&self, event: &Event) {
        self.events.lock().unwrap().push(event.clone());
        self.tx.send(event.clone()).unwrap_or(());
    }
}
struct SessionLogin {
    _setup: Setup,
    config: config::Config,
    clock: Arc<FakeClock>,
    session: SessionExtensions,
    browser: Arc<Recording>,
    yes: Arc<AtomicBool>,
    notices: Arc<Notices>,
    events: mpsc::Receiver<Event>,
}
impl SessionLogin {
    fn new(body: &str, attended: bool) -> Self {
        let setup = Setup::new();
        let clock = FakeClock::new();
        let src = setup.root().join("source");
        write(&src.join("extension.json"), &json!({"name":"acme", "version":"v1.0.0", "fiber":"0.1.0", "api":1, "providers":{"acme":[]}}).to_string());
        write(
            &src.join("providers/acme.lua"),
            &format!(
                r#"
fiber.provider("acme", {{credential={{timeout=60000,run=function()
    pcall(host.oauth.refresh, function() host.status("refresh ran"); error("refresh failed") end)
    {body}
end}}}})
"#
            ),
        );
        extensions::plan(
            &setup.home(),
            &extensions::Request::Path(src),
            "0.1.0",
            &extensions::Origin::github(),
            &*clock,
        )
        .unwrap()
        .commit()
        .unwrap();
        let config = config::Config::load(config::Sources {
            home: setup.home(),
            workspace: setup.workspace(),
            project: config::ProjectKey::new("p").unwrap(),
            overrides: vec![],
        })
        .unwrap();
        let cfg = config.clone();
        let home = setup.home();
        let time = clock.clone();
        let session = fakes::within(
            "SessionExtensions::load",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || SessionExtensions::load(&home, &cfg, time, Arc::new(NoLock), None),
        );
        assert!(session.notices().is_empty(), "{:?}", session.notices());
        let browser = Arc::new(Recording::default());
        let yes = Arc::new(AtomicBool::new(attended));
        if attended {
            let read = yes.clone();
            session.browse_with(
                browser.clone(),
                Arc::new(move || read.load(Ordering::SeqCst)),
            );
        }
        let (tx, events) = mpsc::channel();
        let notices = Arc::new(Notices {
            events: Mutex::new(vec![]),
            tx,
        });
        session.emit_to(notices.clone());
        Self {
            _setup: setup,
            config,
            clock,
            session,
            browser,
            yes,
            notices,
            events,
        }
    }
    fn provider(&self) -> Arc<LuaProvider> {
        self.session
            .lua_providers()
            .iter()
            .find(|(_, p)| p.name() == "acme")
            .unwrap()
            .1
            .clone()
    }
    fn start(&self) -> mpsc::Receiver<Result<config::Secret, Error>> {
        let provider = self.provider();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            tx.send(provider.token(&pair("acme", "default")))
                .unwrap_or(())
        });
        rx
    }
    fn notice(&self) -> contract::events::Notice {
        let wait = Deadline::after(WAIT);
        loop {
            if let Event::Notice(notice) = wait.recv_or_fail(&self.events, "login URL notice") {
                return notice;
            }
        }
    }
    fn fresh(&self) -> Arc<LuaProvider> {
        // A VM start does no Lua work; credential execution below has its own deadline.
        self.session
            .start_provider(
                &self.config,
                self.clock.clone(),
                Arc::new(NoLock),
                "acme",
                "acme",
            )
            .unwrap()
    }
}
fn device(server: &OauthServer) -> String {
    format!(
        "host.oauth.open('{0}/verify'); local token=host.oauth.poll({{url='{0}/token',body='device_code=d'}}); return {{token=token.access_token,expires_at=1800000000}}",
        server.url()
    )
}

#[test]
fn unset_session_refuses_before_any_interactive_side_effect() {
    let server = OauthServer::start(vec![OauthReply::pending()]);
    let env = SessionLogin::new(&device(&server), false);
    let provider = env.provider();
    let error = fakes::within(
        "provider.token",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || provider.token(&pair("acme", "default")),
    )
    .unwrap_err();
    assert!(matches!(error, Error::Unattended { .. }));
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed);
    let provider = env.provider();
    assert!(matches!(
        fakes::within(
            "provider.credential_token",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || provider.credential_token(&pair("acme", "other"))
        ),
        Err(signing::Error::Unattended { .. })
    ));
    assert!(
        env.notices
            .events
            .lock()
            .unwrap()
            .iter()
            .all(|event| !matches!(event, Event::Notice(_)))
    );
    assert_eq!(server.request_count(), 0);
    assert!(env.browser.opened.lock().unwrap().is_empty());
}

#[test]
fn attended_callback_login_exchanges_a_code_for_a_token() {
    use std::io::{Read, Write};
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let server = OauthServer::start(vec![OauthReply::token("exchanged", "refresh", 3600)]);
    let env = SessionLogin::new(
        &format!(
            "host.oauth.open('https://login.example/verify'); local query=host.oauth.callback({{port={}}}); assert(query.code=='returned'); local token=host.oauth.poll({{url='{}',body='code='..query.code}}); return {{token=token.access_token,expires_at=1800000000}}",
            addr.port(),
            server.url()
        ),
        true,
    );
    *env.browser.listener.lock().unwrap() = Some(listener);
    let ready = login_wait("acme");
    let rx = env.start();
    assert_eq!(
        env.notice().message,
        "Open https://login.example/verify to log in."
    );
    Deadline::after(WAIT).recv_or_fail(&ready, "callback recorded");
    let response = fakes::within(
        "callback.get",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            let mut socket = std::net::TcpStream::connect_timeout(&addr, WAIT).unwrap();
            socket.set_read_timeout(Some(WAIT)).unwrap();
            socket.set_write_timeout(Some(WAIT)).unwrap();
            socket
                .write_all(b"GET /?code=returned HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .unwrap();
            let mut response = String::new();
            socket.read_to_string(&mut response).unwrap();
            response
        },
    );
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert_eq!(
        Deadline::after(WAIT)
            .recv_or_fail(&rx, "exchanged token")
            .unwrap()
            .expose(),
        "exchanged"
    );
    assert_eq!(server.request_count(), 1);
}

#[test]
fn attended_device_login_delivers_url_before_the_held_poll() {
    let server = OauthServer::start(vec![OauthReply::token("polled", "refresh", 3600)]);
    server.hold();
    let env = SessionLogin::new(&device(&server), true);
    let rx = env.start();
    let notice = env.notice();
    assert_eq!(notice.code, ErrorCode::LoginUrl);
    assert_eq!(notice.extension.as_deref(), Some("acme"));
    assert_eq!(
        notice.message,
        format!("Open {}/verify to log in.", server.url())
    );
    assert!(server.await_requests(1, Deadline::after(WAIT)));
    server.release();
    assert_eq!(
        Deadline::after(WAIT)
            .recv_or_fail(&rx, "polled token")
            .unwrap()
            .expose(),
        "polled"
    );
    assert_eq!(
        *env.browser.opened.lock().unwrap(),
        [format!("{}/verify", server.url())]
    );
}

#[test]
fn attendance_is_read_again_before_the_next_poll() {
    let server = OauthServer::start(vec![OauthReply::pending()]);
    let env = SessionLogin::new(&device(&server), true);
    let ready = login_wait("acme");
    let wake = env.clock.now().checked_add(Duration::from_secs(5)).unwrap();
    let rx = env.start();
    env.notice();
    let wait = Deadline::after(WAIT);
    let thread = wait.recv_or_fail(&ready, "poll sleep recorded");
    env.yes.store(false, Ordering::SeqCst);
    env.clock
        .advance_when_parked(Some(wake), Some(thread), Duration::from_secs(5), &wait);
    assert!(matches!(
        Deadline::after(WAIT).recv_or_fail(&rx, "unattended poll refused"),
        Err(Error::Unattended { .. })
    ));
    assert_eq!(server.request_count(), 1);
}

#[test]
fn show_delivers_one_notice_with_the_url_and_device_code() {
    let env = SessionLogin::new(
        "host.oauth.show('https://login.example/device', 'ABCD-1234'); return {token='ok', expires_at=1800000000}",
        true,
    );
    let provider = env.provider();
    assert_eq!(
        fakes::within(
            "provider.token",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || { provider.token(&pair("acme", "default")) }
        )
        .unwrap()
        .expose(),
        "ok"
    );
    let notice = env.notice();
    assert_eq!(notice.code, ErrorCode::LoginUrl);
    assert_eq!(notice.extension.as_deref(), Some("acme"));
    assert_eq!(
        notice.message,
        "Open https://login.example/device to log in, and enter the code ABCD-1234."
    );
    assert_eq!(
        env.notices
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| matches!(event, Event::Notice(_)))
            .count(),
        1
    );
    assert_eq!(
        *env.browser.shown.lock().unwrap(),
        [("https://login.example/device".into(), "ABCD-1234".into())]
    );
    assert!(env.browser.opened.lock().unwrap().is_empty());
}

#[test]
fn provider_only_status_is_emitted_until_the_session_is_sealed() {
    let env = SessionLogin::new(
        "host.status('credential ran'); return {token='ok',expires_at=1800000000}",
        true,
    );
    let provider = env.provider();
    fakes::within(
        "provider.token",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || provider.token(&pair("acme", "default")),
    )
    .unwrap();
    let before = env.notices.events.lock().unwrap().len();
    assert!(before > 0);
    env.session.seal();
    let provider = env.provider();
    fakes::within(
        "provider.token",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || provider.token(&pair("acme", "other")),
    )
    .unwrap();
    assert_eq!(env.notices.events.lock().unwrap().len(), before);
}

#[test]
fn failed_refresh_waits_for_interactive_login_before_returning_a_token() {
    let server = OauthServer::start(vec![OauthReply::token("polled", "refresh", 3600)]);
    server.hold();
    let env = SessionLogin::new(&device(&server), true);
    let rx = env.start();
    let notice = env.notice();
    assert_eq!(notice.code, ErrorCode::LoginUrl);
    assert!(server.await_requests(1, Deadline::after(WAIT)));
    assert!(
        matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "held login has not returned"
    );
    let events = env.notices.events.lock().unwrap();
    let serialized: Vec<String> = events
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
    let status = serialized
        .iter()
        .position(|s| s.contains("refresh ran"))
        .unwrap();
    let notice = events
        .iter()
        .position(|e| matches!(e, Event::Notice(_)))
        .unwrap();
    assert!(status < notice);
    drop(events);
    server.release();
    assert_eq!(
        Deadline::after(WAIT)
            .recv_or_fail(&rx, "interactive token")
            .unwrap()
            .expose(),
        "polled"
    );
    assert_eq!(
        *env.browser.opened.lock().unwrap(),
        [format!("{}/verify", server.url())]
    );
}

#[test]
fn a_provider_started_after_a_model_switch_gets_the_browser_and_emitter() {
    let server = OauthServer::start(vec![
        OauthReply::token("polled", "refresh", 3600),
        OauthReply::token("later", "refresh", 3600),
    ]);
    server.hold();
    let mut env = SessionLogin::new(&device(&server), true);
    env.session.retain_lua_providers(&[]);
    let provider = env.fresh();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        tx.send(provider.token(&pair("acme", "default")))
            .unwrap_or(())
    });
    assert_eq!(
        env.notice().message,
        format!("Open {}/verify to log in.", server.url())
    );
    assert!(server.await_requests(1, Deadline::after(WAIT)));
    server.release();
    assert_eq!(
        Deadline::after(WAIT)
            .recv_or_fail(&rx, "late provider token")
            .unwrap()
            .expose(),
        "polled"
    );
    env.session.seal();
    env.session.retain_lua_providers(&[]);
    let before = env.notices.events.lock().unwrap().len();
    let provider = env.fresh();
    fakes::within(
        "provider.token",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || provider.token(&pair("acme", "other")),
    )
    .unwrap();
    assert_eq!(env.notices.events.lock().unwrap().len(), before);
}
