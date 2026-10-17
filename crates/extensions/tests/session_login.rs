//! Mid-session credentials: attendance, ephemeral login URLs and bounded waits.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod common;

use std::net::{Ipv4Addr, TcpListener};
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
        Arc::new(LuaExtension::new("acme", dir, setup.home(), clock.clone()).with_browser(browser));
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

fn advance_login_deadline(clock: &FakeClock, deadline: std::time::Instant) {
    let wait = Deadline::after(WAIT);
    let grace = deadline.checked_add(Duration::from_secs(1)).unwrap();
    assert!(
        clock.await_parked_count(grace, 1, wait.left()),
        "caller waiting past the deadline"
    );
    let mark = clock.mark_parked(deadline, wait.left()).unwrap();
    let threads = mark.threads();
    assert_eq!(threads.len(), 1, "only the VM waits at the login deadline");
    clock.advance_when_parked(
        Some(deadline),
        threads.first().copied(),
        Duration::from_secs(3),
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
    );
    let deadline = clock.now().checked_add(Duration::from_secs(3)).unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        tx.send(provider.credential_token(&pair("acme", "default"))).unwrap_or(());
    });
    advance_login_deadline(&clock, deadline);
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
    );
    let deadline = clock.now().checked_add(Duration::from_secs(3)).unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        tx.send(provider.token(&pair("acme", "default"))).unwrap_or(());
    });
    assert!(
        server.await_requests(1, WAIT),
        "poll request arrived; early result: {:?}",
        rx.try_recv()
    );
    advance_login_deadline(&clock, deadline);
    let error = Deadline::after(WAIT)
        .recv_or_fail(&rx, "credential poll timed out")
        .unwrap_err();
    assert!(matches!(error, Error::LoginTimedOut { .. }), "{error:?}");
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed);
    assert_eq!(server.request_count(), 1);
}
