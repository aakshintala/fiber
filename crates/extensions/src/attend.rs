//! Session attendance and login notices (`docs/model-routing.md`, "Credentials").

use std::io;
use std::net::{Ipv4Addr, TcpListener};
use std::sync::{Arc, Mutex};
use support::lock;

use crate::Browser;
use contract::events::{Event, Notice};

type Attended = Arc<dyn Fn() -> bool + Send + Sync>;
type Installed = (Arc<dyn Browser>, Attended);

/// Shared by every VM, including ones started before session setup finishes.
#[derive(Default)]
pub(crate) struct SessionBrowser(Mutex<Option<Installed>>);

impl SessionBrowser {
    pub(crate) fn set(&self, browser: Arc<dyn Browser>, attended: Attended) {
        *lock(&self.0) = Some((browser, attended));
    }
    fn installed(&self) -> Option<Installed> {
        lock(&self.0).clone()
    }
}
impl Browser for SessionBrowser {
    fn open(&self, url: &str) {
        if let Some((browser, _)) = self.installed() {
            browser.open(url);
        }
    }
    fn show(&self, url: &str, code: &str) {
        if let Some((browser, _)) = self.installed() {
            browser.show(url, code);
        }
    }
    fn attended(&self) -> bool {
        self.installed().is_some_and(|(_, attended)| attended())
    }
    fn callback_listener(&self, port: u16) -> io::Result<TcpListener> {
        match self.installed() {
            Some((browser, _)) => browser.callback_listener(port),
            None => TcpListener::bind((Ipv4Addr::LOCALHOST, port)),
        }
    }
}

/// Adds a copyable notice without changing how the browser opens a URL.
pub(crate) struct Noticing {
    browser: Arc<dyn Browser>,
    hub: Arc<crate::lua::Hub>,
    extension: String,
}
impl Noticing {
    pub(crate) fn new(
        browser: Arc<dyn Browser>,
        hub: Arc<crate::lua::Hub>,
        extension: String,
    ) -> Self {
        Self {
            browser,
            hub,
            extension,
        }
    }
}
impl Browser for Noticing {
    fn open(&self, url: &str) {
        self.browser.open(url);
        self.hub
            .emit(Event::Notice(login_notice(&self.extension, url, None)));
    }
    fn show(&self, url: &str, code: &str) {
        self.browser.show(url, code);
        self.hub.emit(Event::Notice(login_notice(
            &self.extension,
            url,
            Some(code),
        )));
    }
    fn attended(&self) -> bool {
        self.browser.attended()
    }
    fn callback_listener(&self, port: u16) -> io::Result<TcpListener> {
        self.browser.callback_listener(port)
    }
}

/// The login notice keeps the full URL and device code available to copy.
pub(crate) fn login_notice(extension: &str, url: &str, code: Option<&str>) -> Notice {
    Notice {
        code: contract::ErrorCode::LoginUrl,
        message: code.map_or_else(
            || format!("Open {url} to log in."),
            |code| format!("Open {url} to log in, and enter the code {code}."),
        ),
        extension: Some(extension.to_owned()),
    }
}
