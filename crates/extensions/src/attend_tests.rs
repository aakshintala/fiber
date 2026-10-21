#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use std::net::{Ipv4Addr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use contract::emit::Emit;
use contract::events::Event;
use contract::extension::ExtensionDoor;
use fakes::clock::FakeClock;
use fakes::{Deadline, NoLock};
use serde_json::json;

use crate::attend::{Noticing, SessionBrowser, login_notice};
use crate::hooks::{LatePoint, SessionExtensions};
use crate::{Browser, CredentialPair, LuaProvider};

const WAIT: Duration = Duration::from_secs(10);

#[derive(Default)]
struct BrowserLog {
    open: Mutex<Vec<String>>,
    show: Mutex<Vec<(String, String)>>,
    yes: AtomicBool,
    listener: Mutex<Option<TcpListener>>,
}
impl Browser for BrowserLog {
    fn open(&self, url: &str) {
        self.open.lock().unwrap().push(url.into());
    }
    fn show(&self, url: &str, code: &str) {
        self.show.lock().unwrap().push((url.into(), code.into()));
    }
    fn attended(&self) -> bool {
        self.yes.load(Ordering::SeqCst)
    }
    fn callback_listener(&self, _: u16) -> std::io::Result<TcpListener> {
        Ok(self.listener.lock().unwrap().take().unwrap())
    }
}

#[test]
fn unset_browser_is_unattended() {
    assert!(!SessionBrowser::default().attended());
}

#[test]
fn session_browser_forwards_and_reads_attendance_each_time() {
    let browser = Arc::new(BrowserLog::default());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    *browser.listener.lock().unwrap() = Some(listener);
    let yes = Arc::new(AtomicBool::new(false));
    let read = yes.clone();
    let session = SessionBrowser::default();
    session.set(
        browser.clone(),
        Arc::new(move || read.load(Ordering::SeqCst)),
    );
    assert!(!session.attended());
    yes.store(true, Ordering::SeqCst);
    assert!(session.attended());
    session.open("https://login.example");
    session.show("https://device.example", "CODE");
    assert_eq!(*browser.open.lock().unwrap(), ["https://login.example"]);
    assert_eq!(
        *browser.show.lock().unwrap(),
        [("https://device.example".into(), "CODE".into())]
    );
    assert_eq!(
        session.callback_listener(0).unwrap().local_addr().unwrap(),
        addr
    );
}

#[test]
fn noticing_forwards_and_emits_one_notice_for_each_open_and_show() {
    let browser = Arc::new(BrowserLog::default());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    *browser.listener.lock().unwrap() = Some(listener);
    let hub = crate::lua::Hub::new(FakeClock::new());
    let recorder = Arc::new(fakes::emit::Recorder::default());
    hub.set_emit(recorder.clone());
    let noticing = Noticing::new(browser.clone(), hub, "acme".into());
    assert!(!noticing.attended());
    browser.yes.store(true, Ordering::SeqCst);
    assert!(noticing.attended());
    noticing.open("https://login.example");
    noticing.show("https://device.example", "CODE");
    assert_eq!(*browser.open.lock().unwrap(), ["https://login.example"]);
    assert_eq!(
        *browser.show.lock().unwrap(),
        [("https://device.example".into(), "CODE".into())]
    );
    assert_eq!(
        noticing.callback_listener(0).unwrap().local_addr().unwrap(),
        addr
    );
    let events = recorder.events();
    assert_eq!(events.len(), 2);
    for (event, text) in events.iter().zip([
        "Open https://login.example to log in.",
        "Open https://device.example to log in, and enter the code CODE.",
    ]) {
        let Event::Notice(notice) = event else {
            panic!("{event:?}")
        };
        assert_eq!(notice.code, contract::ErrorCode::LoginUrl);
        assert_eq!(notice.extension.as_deref(), Some("acme"));
        assert_eq!(notice.message, text);
    }
}

#[test]
fn login_notices_keep_the_full_url_and_device_code() {
    let open = login_notice("acme", "https://example/verify?a=1", None);
    assert_eq!(open.message, "Open https://example/verify?a=1 to log in.");
    assert_eq!(open.extension.as_deref(), Some("acme"));
    assert_eq!(open.code, contract::ErrorCode::LoginUrl);
    let show = login_notice("acme", "https://example/device", Some("ABCD-1234"));
    assert_eq!(
        show.message,
        "Open https://example/device to log in, and enter the code ABCD-1234."
    );
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
fn notices() -> (Arc<Notices>, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    (
        Arc::new(Notices {
            events: Mutex::new(vec![]),
            tx,
        }),
        rx,
    )
}

struct Home {
    _temp: fakes::TempDir,
    config: config::Config,
    clock: Arc<FakeClock>,
    session: Arc<SessionExtensions>,
}
impl Home {
    fn new() -> Self {
        let temp = fakes::TempDir::new("late-login");
        let home = temp.path().join("home");
        let workspace = temp.path().join("workspace");
        let source = temp.path().join("source");
        std::fs::create_dir_all(source.join("providers")).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(source.join("extension.json"), json!({"name":"acme", "version":"v1.0.0", "fiber":"0.1.0", "api":1, "providers":{"acme":[]}}).to_string()).unwrap();
        std::fs::write(source.join("providers/acme.lua"), r#"fiber.provider("acme", {credential={timeout=60000, run=function() host.status("late"); return {token="ok", expires_at=1800000000} end}})"#).unwrap();
        let clock = FakeClock::new();
        crate::plan(
            &home,
            &crate::Request::Path(source),
            "0.1.0",
            &crate::Origin::github(),
            &*clock,
        )
        .unwrap()
        .commit()
        .unwrap();
        let config = config::Config::load(config::Sources {
            home: home.clone(),
            workspace,
            project: config::ProjectKey::new("p").unwrap(),
            overrides: vec![],
        })
        .unwrap();
        let cfg = config.clone();
        let time = clock.clone();
        let session = fakes::within(
            "SessionExtensions::load",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                let mut session =
                    SessionExtensions::load(&home, &cfg, time, Arc::new(NoLock), None);
                session.retain_lua_providers(&[]);
                Arc::new(session)
            },
        );
        Self {
            _temp: temp,
            config,
            clock,
            session,
        }
    }
    fn start(&self) -> Arc<LuaProvider> {
        let session = self.session.clone();
        let cfg = self.config.clone();
        let clock = self.clock.clone();
        fakes::within(
            "session.start_provider",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || session.start_provider(&cfg, clock, Arc::new(NoLock), "acme", "acme"),
        )
        .unwrap()
    }
}
fn credential(provider: Arc<LuaProvider>) {
    credential_for(provider, "default");
}
fn credential_for(provider: Arc<LuaProvider>, label: &str) {
    let label = label.to_owned();
    assert_eq!(
        fakes::within(
            "provider.token",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                provider.token(&CredentialPair {
                    credential: "acme".into(),
                    label,
                })
            }
        )
        .unwrap()
        .expose(),
        "ok"
    );
}
fn pause(
    session: &SessionExtensions,
    hold: LatePoint,
) -> (mpsc::Receiver<LatePoint>, mpsc::Sender<()>) {
    let (tx, rx) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    session.pause_at_late(Arc::new(move |point| {
        tx.send(point).unwrap_or(());
        if point == hold {
            Deadline::after(WAIT)
                .recv_or_fail(&released.lock().unwrap(), "late-provider pause released");
        }
    }));
    (rx, release)
}
fn point(rx: &mpsc::Receiver<LatePoint>, expected: LatePoint) {
    let wait = Deadline::after(WAIT);
    loop {
        if wait.recv_or_fail(rx, "late-provider pause point") == expected {
            return;
        }
    }
}
fn spawn_done(f: impl FnOnce() + Send + 'static) -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        f();
        tx.send(()).unwrap_or(());
    });
    rx
}

#[test]
fn emit_stored_before_late_start_reaches_the_vm_without_a_second_flush() {
    let home = Home::new();
    let (emit, rx) = notices();
    let (points, release) = pause(&home.session, LatePoint::Stored);
    let session = home.session.clone();
    let sink = emit.clone();
    let done = spawn_done(move || session.emit_to(sink));
    point(&points, LatePoint::Stored);
    credential(home.start());
    let event = Deadline::after(WAIT).recv_or_fail(&rx, "late status before emit_to finishes");
    assert!(matches!(event, Event::ExtensionUi(_)));
    release.send(()).unwrap();
    Deadline::after(WAIT).recv_or_fail(&done, "emit_to returned");
    assert_eq!(emit.events.lock().unwrap().len(), 1);
}

#[test]
fn late_start_before_emit_store_is_included_in_emit_to() {
    let home = Home::new();
    let (emit, rx) = notices();
    let (points, release) = pause(&home.session, LatePoint::Read);
    let session = home.session.clone();
    let cfg = home.config.clone();
    let clock = home.clock.clone();
    let (started, result) = mpsc::channel();
    std::thread::spawn(move || {
        started
            .send(session.start_provider(&cfg, clock, Arc::new(NoLock), "acme", "acme"))
            .unwrap_or(())
    });
    point(&points, LatePoint::Read);
    let session = home.session.clone();
    let sink = emit.clone();
    let done = spawn_done(move || session.emit_to(sink));
    point(&points, LatePoint::Stored);
    release.send(()).unwrap();
    let provider = Deadline::after(WAIT)
        .recv_or_fail(&result, "provider started")
        .unwrap();
    Deadline::after(WAIT).recv_or_fail(&done, "emit_to returned");
    credential(provider);
    Deadline::after(WAIT).recv_or_fail(&rx, "late status emitted");
    assert_eq!(emit.events.lock().unwrap().len(), 1);
}

#[test]
fn seal_cleared_before_late_start_seals_the_new_vm() {
    let home = Home::new();
    let (emit, _) = notices();
    home.session.emit_to(emit.clone());
    let (points, release) = pause(&home.session, LatePoint::Cleared);
    let session = home.session.clone();
    let done = spawn_done(move || session.seal());
    point(&points, LatePoint::Cleared);
    let provider = home.start();
    release.send(()).unwrap();
    Deadline::after(WAIT).recv_or_fail(&done, "seal returned");
    credential(provider.clone());
    let (second, _) = notices();
    for vm in home.session.attached_vms() {
        vm.set_emit(second.clone());
    }
    assert!(emit.events.lock().unwrap().is_empty());
    credential_for(provider, "other");
    assert!(second.events.lock().unwrap().is_empty());
}

#[test]
fn late_start_reading_an_emitter_is_still_included_in_seal() {
    let home = Home::new();
    let (emit, _) = notices();
    home.session.emit_to(emit.clone());
    let (points, release) = pause(&home.session, LatePoint::Read);
    let session = home.session.clone();
    let cfg = home.config.clone();
    let clock = home.clock.clone();
    let (started, result) = mpsc::channel();
    std::thread::spawn(move || {
        started
            .send(session.start_provider(&cfg, clock, Arc::new(NoLock), "acme", "acme"))
            .unwrap_or(())
    });
    point(&points, LatePoint::Read);
    let session = home.session.clone();
    let done = spawn_done(move || session.seal());
    point(&points, LatePoint::Cleared);
    release.send(()).unwrap();
    let provider = Deadline::after(WAIT)
        .recv_or_fail(&result, "provider started")
        .unwrap();
    Deadline::after(WAIT).recv_or_fail(&done, "seal returned");
    credential(provider);
    assert!(emit.events.lock().unwrap().is_empty());
}
