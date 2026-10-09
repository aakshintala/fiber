//! `fiber login codex` (`docs/model-routing.md`, "Logging in"): the real
//! codex package copied against the OAuth fake, with an injected browser on
//! a fake clock. No browser opens and no real port listens, except the
//! package's own callback listener on a free port.

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::panic, reason = "the test's wait deadline is its failure")]

use std::fs;
use std::io::{Cursor, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use config::CredentialFile;
use contract::ErrorCode;
use contract::shapes::Failure;
use extensions::{Browser, LoginMethod, Providers};
use fakes::clock::FakeClock;
use fakes::{OauthReply, OauthServer, jwt};
use serde_json::{Value, json};

use super::{Attended, browser_login, login_with};
use crate::login::{LoginIo, Plain, login};

/// How long a test waits for one call or one child.
const WAIT: Duration = Duration::from_secs(10);

/// How long a test waits for the package to open the authorize URL or for
/// its callback listener to bind: one wall-clock deadline for both waits.
const BROWSER_WAIT: Duration = Duration::from_secs(4);

/// The test account id and email.
const ACCOUNT: &str = "acct_1";
const EMAIL: &str = "alice@example.com";

struct Setup {
    root: fakes::TempDir,
    clock: Arc<FakeClock>,
}

impl Setup {
    fn new() -> Self {
        Self {
            root: fakes::TempDir::new("fiber-login-codex"),
            clock: FakeClock::new(),
        }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// Installs the codex package copy pointing at `server`, with its
    /// callback port rewritten to `port`.
    fn install(&self, server: &OauthServer, port: u16) {
        copy_package(
            &self.home().join("extensions").join("codex"),
            &[
                ("https://auth.openai.com", &server.url()),
                ("local PORT = 1455", &format!("local PORT = {port}")),
            ],
        );
    }

    /// Installs a key provider `name`.
    fn install_key(&self, name: &str) {
        let dir = self.home().join("extensions").join(name);
        fs::create_dir_all(dir.join("providers")).unwrap();
        fs::write(
            dir.join("extension.json"),
            json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
        )
        .unwrap();
        fs::write(
            dir.join("providers").join(format!("{name}.json")),
            json!({
                "name": name,
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": "http://x/v1", "context_window": 1000}],
            })
            .to_string(),
        )
        .unwrap();
    }

    fn providers(&self) -> Providers {
        let (providers, notices) = Providers::load(&self.home()).unwrap();
        assert!(notices.is_empty(), "{notices:?}");
        providers
    }

    fn stored(&self, label: &str) -> Option<Value> {
        let path = self.home().join("credentials").join("codex").join(label);
        fs::read(&path)
            .ok()
            .map(|bytes| serde_json::from_slice(&bytes).unwrap())
    }

    /// Stores a known credential under `label`, as another login would have.
    fn store_for_test(&self, label: &str) {
        let lock = CredentialFile::new(&self.home(), "codex", label)
            .unwrap()
            .try_lock()
            .unwrap()
            .unwrap();
        lock.write(&json!({
            "token": "old",
            "expires_at": 4_102_444_800u64,
            "refresh_token": "rt",
            "account_id": "acct_0",
        }))
        .unwrap();
    }

    fn mode(&self, label: &str) -> u32 {
        fs::metadata(self.home().join("credentials").join("codex").join(label))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    /// The clock as the login calls take it.
    fn clock(&self) -> Arc<dyn contract::clock::Clock> {
        Arc::clone(&self.clock) as Arc<dyn contract::clock::Clock>
    }

    fn global_credential(&self) -> Option<String> {
        let text = fs::read_to_string(self.home().join("config.json")).unwrap();
        serde_json::from_str::<Value>(&text).unwrap()["providers"]["codex"]["credential"]
            .as_str()
            .map(str::to_owned)
    }
}

fn copy_package(dest: &Path, replacements: &[(&str, &str)]) {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../providers/codex");
    copy_tree(&source, dest, replacements);
}

fn copy_tree(source: &Path, dest: &Path, replacements: &[(&str, &str)]) {
    fs::create_dir_all(dest).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = dest.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target, replacements);
        } else {
            let mut text = fs::read_to_string(entry.path()).unwrap();
            for (from, to) in replacements {
                text = text.replace(from, to);
            }
            fs::write(target, text).unwrap();
        }
    }
}

/// A port nothing listens on, for the package's callback listener.
fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A browser that records the authorize URL and opens nothing: the test
/// thread performs the redirect once the callback listens. Each `open` also
/// reports on the channel, so the wait for it is a notification under
/// [`BROWSER_WAIT`] instead of an attempt count.
struct RedirectBrowser {
    opened: Mutex<Vec<String>>,
    notify: mpsc::Sender<String>,
}

impl RedirectBrowser {
    /// A browser and the channel its `open` reports on.
    fn notified() -> (Arc<Self>, mpsc::Receiver<String>) {
        let (notify, opened) = mpsc::channel();
        (
            Arc::new(Self {
                opened: Mutex::new(Vec::new()),
                notify,
            }),
            opened,
        )
    }
}

impl Browser for RedirectBrowser {
    fn open(&self, url: &str) {
        self.opened.lock().unwrap().push(url.to_owned());
        match self.notify.send(url.to_owned()) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    }

    fn show(&self, _url: &str, _code: &str) {}

    fn attended(&self) -> bool {
        true
    }
}

/// Waits for the package's `open` notification, without reading the clock:
/// nothing here parks on it. The bound is the wall-clock [`BROWSER_WAIT`],
/// so a package that never opens fails there instead of hanging.
fn await_opened(opened: &mpsc::Receiver<String>) -> String {
    opened.recv_timeout(BROWSER_WAIT).unwrap_or_else(|_| {
        panic!("the package never opened the authorize URL within {BROWSER_WAIT:?}")
    })
}

/// GETs the callback's `target` on `port`, retrying a refused connection
/// until the listener binds. The bound is the wall-clock [`BROWSER_WAIT`],
/// so a listener that never binds fails there instead of hanging.
fn redirect(port: u16, target: &str) {
    let target = target.to_owned();
    fakes::within("the callback to listen", BROWSER_WAIT, move || {
        loop {
            match TcpStream::connect((Ipv4Addr::LOCALHOST, port)) {
                Ok(mut stream) => {
                    stream.set_read_timeout(Some(WAIT)).unwrap();
                    write!(stream, "GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
                    let mut reply = String::new();
                    stream.read_to_string(&mut reply).unwrap();
                    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
                    return;
                }
                Err(_) => thread::yield_now(),
            }
        }
    });
}

fn state_of(url: &str) -> String {
    url.split_once('?')
        .unwrap()
        .1
        .split('&')
        .find_map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (name == "state").then(|| value.to_owned())
        })
        .unwrap()
}

/// An access token carrying the account id and the expiry, and an id token
/// carrying `email`.
fn tokens(email: &str) -> (String, String) {
    let access = jwt(&json!({
        "https://api.openai.com/auth": { "chatgpt_account_id": ACCOUNT },
        "exp": 4_102_444_800u64,
    }));
    let id = jwt(&json!({ "email": email }));
    (access, id)
}

fn exchange(access: &str, id: &str) -> OauthReply {
    OauthReply::raw(
        200,
        &json!({
            "access_token": access,
            "refresh_token": "rt_1",
            "id_token": id,
            "expires_in": 864000,
        })
        .to_string(),
    )
}

/// Runs the browser login to completion, performing the redirect from a
/// scoped thread once the authorize URL is recorded, and returns what
/// `browser_login` returned.
fn browser_flow(
    setup: &Setup,
    providers: &Providers,
    label: Option<&str>,
    browser: &Arc<RedirectBrowser>,
    opened: &mpsc::Receiver<String>,
    server: &OauthServer,
) -> Result<String, Failure> {
    let _ = server;
    let (tx, rx) = mpsc::channel();
    let (home, providers, label) = (setup.home(), providers.clone(), label.map(str::to_owned));
    let (browser, clock) = (Arc::clone(browser) as Arc<dyn Browser>, setup.clock());
    thread::spawn(move || {
        let result = browser_login(
            &home,
            &providers,
            "codex",
            label.as_deref(),
            LoginMethod::Browser,
            browser,
            clock,
        );
        match tx.send(result) {
            Ok(()) | Err(_) => {}
        }
    });
    let url = await_opened(opened);
    let port = free_port_of(&url);
    redirect(
        port,
        &format!("/auth/callback?code=authcode-1&state={}", state_of(&url)),
    );
    rx.recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("the login did not return within {WAIT:?}"))
}

/// The redirect port the authorize URL names.
fn free_port_of(url: &str) -> u16 {
    let redirect = url
        .split_once('?')
        .unwrap()
        .1
        .split('&')
        .find_map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (name == "redirect_uri").then(|| value.to_owned())
        })
        .unwrap();
    redirect
        .trim_start_matches("http%3A%2F%2Flocalhost%3A")
        .split_once('%')
        .unwrap()
        .0
        .parse()
        .unwrap()
}

fn failed(result: Result<String, Failure>) -> Failure {
    result.unwrap_err()
}

#[test]
fn a_browser_login_stores_the_email_label_0600_and_names_it_globally() {
    let setup = Setup::new();
    let (access, id) = tokens(EMAIL);
    let server = OauthServer::start(vec![exchange(&access, &id)]);
    let port = free_port();
    setup.install(&server, port);
    let providers = setup.providers();
    let (browser, opened) = RedirectBrowser::notified();
    let path = browser_flow(&setup, &providers, None, &browser, &opened, &server).unwrap();
    assert_eq!(path, "credentials/codex/alice@example.com");

    assert_eq!(
        setup.stored(EMAIL).unwrap(),
        json!({
            "token": access,
            "expires_at": 4_102_444_800u64,
            "refresh_token": "rt_1",
            "account_id": ACCOUNT,
        })
    );
    assert_eq!(setup.mode(EMAIL), 0o600);
    assert_eq!(setup.global_credential().as_deref(), Some(EMAIL));
    // The redirect the test performed proves the authorize URL opened.
    assert_eq!(server.request_count(), 1);
}

#[test]
fn a_browser_login_with_as_stores_that_label_and_ignores_the_email() {
    let setup = Setup::new();
    let (access, id) = tokens(EMAIL);
    let server = OauthServer::start(vec![exchange(&access, &id)]);
    let port = free_port();
    setup.install(&server, port);
    let providers = setup.providers();
    let (browser, opened) = RedirectBrowser::notified();
    let path = browser_flow(&setup, &providers, Some("work"), &browser, &opened, &server).unwrap();
    assert_eq!(path, "credentials/codex/work");
    assert_eq!(setup.stored("work").unwrap()["account_id"], json!(ACCOUNT));
    assert!(setup.stored(EMAIL).is_none());
    assert_eq!(setup.global_credential().as_deref(), Some("work"));
}

#[test]
fn an_as_label_already_stored_is_refused_before_anything_opens() {
    let setup = Setup::new();
    let (access, id) = tokens(EMAIL);
    let server = OauthServer::start(vec![exchange(&access, &id)]);
    let port = free_port();
    setup.install(&server, port);
    let providers = setup.providers();
    setup.store_for_test("work");
    let (browser, _opened) = RedirectBrowser::notified();
    let error = failed(browser_login(
        &setup.home(),
        &providers,
        "codex",
        Some("work"),
        LoginMethod::Browser,
        browser.clone(),
        setup.clock(),
    ));
    assert_eq!(error.code, ErrorCode::Usage);
    assert!(
        error.message.contains("--as"),
        "{message}",
        message = error.message
    );
    assert!(browser.opened.lock().unwrap().is_empty());
    assert_eq!(server.request_count(), 0);
    assert_eq!(setup.stored("work").unwrap()["token"], json!("old"));
}

#[test]
fn an_email_label_already_stored_is_refused_after_the_flow_naming_as() {
    let setup = Setup::new();
    let (access, id) = tokens(EMAIL);
    let server = OauthServer::start(vec![exchange(&access, &id)]);
    let port = free_port();
    setup.install(&server, port);
    let providers = setup.providers();
    setup.store_for_test(EMAIL);
    let (browser, opened) = RedirectBrowser::notified();
    let error = failed(browser_flow(
        &setup, &providers, None, &browser, &opened, &server,
    ));
    assert_eq!(error.code, ErrorCode::Usage);
    assert!(
        error.message.contains("--as"),
        "{message}",
        message = error.message
    );
    assert_eq!(setup.stored(EMAIL).unwrap()["token"], json!("old"));
}

#[test]
fn a_browser_login_while_another_holds_the_label_lock_is_io_failed() {
    let setup = Setup::new();
    let (access, id) = tokens(EMAIL);
    let server = OauthServer::start(vec![exchange(&access, &id)]);
    let port = free_port();
    setup.install(&server, port);
    let providers = setup.providers();
    let held = config::CredentialFile::new(&setup.home(), "codex", EMAIL)
        .unwrap()
        .try_lock()
        .unwrap()
        .unwrap();
    let (browser, opened) = RedirectBrowser::notified();
    let error = failed(browser_flow(
        &setup, &providers, None, &browser, &opened, &server,
    ));
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert!(
        error.message.contains("another login"),
        "{message}",
        message = error.message
    );
    assert!(setup.stored(EMAIL).is_none());
    drop(held);
}

#[test]
fn an_email_that_is_no_label_is_a_usage_error_naming_as() {
    let setup = Setup::new();
    let (access, id) = tokens("a.lock");
    let server = OauthServer::start(vec![exchange(&access, &id)]);
    let port = free_port();
    setup.install(&server, port);
    let providers = setup.providers();
    let (browser, opened) = RedirectBrowser::notified();
    let error = failed(browser_flow(
        &setup, &providers, None, &browser, &opened, &server,
    ));
    assert_eq!(error.code, ErrorCode::Usage);
    assert!(
        error.message.contains("--as"),
        "{message}",
        message = error.message
    );
    assert!(setup.stored("a.lock").is_none());
}

#[test]
fn a_failed_global_write_deletes_the_stored_file() {
    let setup = Setup::new();
    let (access, id) = tokens(EMAIL);
    let server = OauthServer::start(vec![exchange(&access, &id)]);
    let port = free_port();
    setup.install(&server, port);
    let providers = setup.providers();
    fs::write(setup.home().join("config.json"), "{\"model\": ,").unwrap();
    let (browser, opened) = RedirectBrowser::notified();
    let error = failed(browser_flow(
        &setup, &providers, None, &browser, &opened, &server,
    ));
    assert_eq!(error.code, ErrorCode::ConfigInvalid, "{error:?}");
    assert!(setup.stored(EMAIL).is_none());
}

#[test]
fn a_device_login_shows_its_code_and_stores_the_email_label() {
    let setup = Setup::new();
    let (access, id) = tokens(EMAIL);
    let server = OauthServer::start(vec![
        OauthReply::raw(
            200,
            &json!({
                "device_auth_id": "da_1",
                "user_code": "ABCD-1234",
                "interval": "1",
            })
            .to_string(),
        ),
        OauthReply::raw(
            200,
            &json!({ "authorization_code": "authcode-2", "code_verifier": "verifier-2" })
                .to_string(),
        ),
        exchange(&access, &id),
    ]);
    let port = free_port();
    setup.install(&server, port);
    let providers = setup.providers();
    let browser = Arc::new(ShowBrowser {
        shown: Mutex::new(Vec::new()),
    });
    let (tx, rx) = mpsc::channel();
    let (home, owned) = (setup.home(), providers.clone());
    let (browser_in, clock) = (Arc::clone(&browser) as Arc<dyn Browser>, setup.clock());
    thread::spawn(move || {
        tx.send(browser_login(
            &home,
            &owned,
            "codex",
            None,
            LoginMethod::Device,
            browser_in,
            clock,
        ))
        .unwrap();
    });
    // The scripted poll answers 200 at once, so nothing parks on the
    // clock: the login lands on its own under the wall-clock bound.
    let path = rx
        .recv_timeout(WAIT)
        .unwrap_or_else(|_| {
            panic!(
                "the device login did not return within {WAIT:?}; requests: {}",
                server.request_count()
            )
        })
        .unwrap();
    assert_eq!(path, "credentials/codex/alice@example.com");
    assert_eq!(
        browser.shown.lock().unwrap().clone(),
        [(
            format!("{}/codex/device", server.url()),
            "ABCD-1234".to_owned()
        )]
    );
    assert_eq!(setup.stored(EMAIL).unwrap()["account_id"], json!(ACCOUNT));
}

/// A browser that records what `show` shows and opens nothing.
struct ShowBrowser {
    shown: Mutex<Vec<(String, String)>>,
}

impl Browser for ShowBrowser {
    fn open(&self, _url: &str) {}

    fn show(&self, url: &str, code: &str) {
        self.shown
            .lock()
            .unwrap()
            .push((url.to_owned(), code.to_owned()));
    }

    fn attended(&self) -> bool {
        true
    }
}

#[test]
fn device_on_a_declared_secret_is_a_usage_error() {
    let setup = Setup::new();
    let dir = setup.home().join("extensions").join("acme");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({"name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "secrets": ["api_key"]})
            .to_string(),
    )
    .unwrap();
    let providers = setup.providers();
    let mut err = Vec::new();
    let mut keys = Plain;
    let result = login(
        Some("api_key"),
        None,
        &mut LoginIo {
            home: &setup.home(),
            providers: &providers,
            terminal: false,
            stdin: &mut Cursor::new(String::new()),
            err: &mut err,
            keys: &mut keys,
            device: true,
            clock: setup.clock(),
        },
    );
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::Usage);
    assert!(
        error.message.contains("--device"),
        "{message}",
        message = error.message
    );
}

#[derive(Default)]
struct RecordingBrowser {
    calls: Mutex<Vec<String>>,
}

impl Browser for RecordingBrowser {
    fn open(&self, url: &str) {
        self.calls.lock().unwrap().push(format!("open:{url}"));
    }

    fn show(&self, url: &str, code: &str) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("show:{url}:{code}"));
    }

    fn attended(&self) -> bool {
        true
    }
}

#[test]
fn attended_browser_delegates_open_and_show_to_its_inner_browser() {
    let inner = Arc::new(RecordingBrowser::default());
    let attended = Attended(Arc::clone(&inner) as Arc<dyn Browser>);

    attended.open("https://auth.example/authorize");
    attended.show("https://auth.example/device", "ABCD-1234");

    assert_eq!(
        inner.calls.lock().unwrap().as_slice(),
        [
            "open:https://auth.example/authorize",
            "show:https://auth.example/device:ABCD-1234"
        ]
    );
}

#[test]
fn fiber_login_is_attended_whatever_stdin_is() {
    assert!(Attended::attached().attended());
}

#[test]
fn login_with_runs_the_provider_and_prints_its_stored_result() {
    let setup = Setup::new();
    let home = setup.home();
    let dir = home.join("extensions").join("acme-ext");
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({ "name": "acme-ext", "version": "v1.0.0", "fiber": "0.1.0", "api": 1 }).to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("providers/acme.json"),
        json!({
            "name": "acme",
            "login": "browser",
            "models": [{
                "id": "m",
                "protocol": "openai-responses",
                "base_url": "http://127.0.0.1:1/v1",
                "context_window": 1000
            }]
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("init.lua"),
        r#"fiber.provider("acme", {
          credential = { timeout = 60000, run = function()
            local stored = host.oauth.refresh(function()
              return { token = "test-token", expires_at = 4102444800 }
            end)
            return { token = stored.token, expires_at = stored.expires_at, email = "alice@example.com" }
          end }
        })"#,
    )
    .unwrap();
    let providers = setup.providers();
    let mut output = Vec::new();
    let mut stdin = Cursor::new(String::new());
    let mut keys = Plain;

    login_with(
        &mut LoginIo {
            home: &home,
            providers: &providers,
            terminal: false,
            stdin: &mut stdin,
            err: &mut output,
            keys: &mut keys,
            device: false,
            clock: setup.clock(),
        },
        "acme",
        None,
    )
    .unwrap();

    assert_eq!(
        String::from_utf8(output).unwrap(),
        "fiber: stored credentials/acme/alice@example.com\n"
    );
    let credential: Value =
        serde_json::from_slice(&fs::read(home.join("credentials/acme/alice@example.com")).unwrap())
            .unwrap();
    assert_eq!(
        credential,
        json!({"token": "test-token", "expires_at": 4102444800u64})
    );
}

#[test]
fn device_on_a_key_provider_is_a_usage_error() {
    let setup = Setup::new();
    setup.install_key("acme");
    let providers = setup.providers();
    let mut err = Vec::new();
    let mut keys = Plain;
    let result = login(
        Some("acme"),
        None,
        &mut LoginIo {
            home: &setup.home(),
            providers: &providers,
            terminal: false,
            stdin: &mut Cursor::new(String::new()),
            err: &mut err,
            keys: &mut keys,
            device: true,
            clock: setup.clock(),
        },
    );
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::Usage);
    assert!(
        error.message.contains("--device"),
        "{message}",
        message = error.message
    );
}

/// The child's marker: set, the test runs `run_login` and exits with its code.
const CHILD: &str = "FIBER_CLI_TEST_CHILD";

/// How long the child may run before the test kills its group and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(60);

/// How long a reaped child may take to report after its group is killed.
const REAP_DEADLINE: Duration = Duration::from_secs(10);

#[test]
fn run_login_of_a_key_provider_with_device_is_a_usage_failure() {
    use std::os::unix::process::ExitStatusExt;
    // Fails before any prompt or read of stdin. It runs in a child with a
    // Fiber home holding a key provider and no stdin.
    if std::env::var_os(CHILD).is_some() {
        let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
        std::process::exit(super::super::run_login(Some("acme"), None, true, clock));
    }
    let setup = Setup::new();
    setup.install_key("acme");
    let name = module_path!().split_once("::").unwrap().1;
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{name}::run_login_of_a_key_provider_with_device_is_a_usage_failure"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FIBER_HOME", setup.home())
        .env(CHILD, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let group = child.id();
    let watchdog = fakes::Watchdog::group(group);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()));
    let Ok(received) = finished.recv_timeout(CHILD_DEADLINE) else {
        fakes::kill_group(group, "KILL").unwrap();
        let killed = finished
            .recv_timeout(REAP_DEADLINE)
            .map(|output| output.map(|output| output.status.signal()));
        assert!(
            matches!(killed, Ok(Ok(Some(9)))),
            "the device child was not reaped as killed within {REAP_DEADLINE:?}: {killed:?}"
        );
        panic!("waited {CHILD_DEADLINE:?} for the device child to exit");
    };
    watchdog.stand_down(REAP_DEADLINE);
    let output = received.unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}
