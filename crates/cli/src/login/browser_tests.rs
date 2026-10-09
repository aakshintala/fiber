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

use super::{Attended, browser_login};
use crate::login::{LoginIo, Plain, login};

/// How long a test waits for one call or one child.
const WAIT: Duration = Duration::from_secs(10);

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
/// thread performs the redirect once the callback listens.
struct RedirectBrowser {
    opened: Mutex<Vec<String>>,
}

impl Browser for RedirectBrowser {
    fn open(&self, url: &str) {
        self.opened.lock().unwrap().push(url.to_owned());
    }

    fn show(&self, _url: &str, _code: &str) {}

    fn attended(&self) -> bool {
        true
    }
}

fn await_opened(browser: &Arc<RedirectBrowser>) -> String {
    for _ in 0..1_000_000 {
        if let Some(url) = browser.opened.lock().unwrap().pop() {
            return url;
        }
        thread::yield_now();
    }
    panic!("the package never opened the authorize URL");
}

fn redirect(port: u16, target: &str) {
    for _ in 0..50_000 {
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
    panic!("the callback never listened on {port}");
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
    server: &OauthServer,
) -> Result<String, Failure> {
    let _ = server;
    thread::scope(|scope| {
        let (tx, rx) = mpsc::channel();
        scope.spawn(move || {
            tx.send(browser_login(
                &setup.home(),
                providers,
                "codex",
                label,
                LoginMethod::Browser,
                Arc::clone(browser) as Arc<dyn Browser>,
                setup.clock(),
            ))
            .unwrap();
        });
        let url = await_opened(browser);
        let port = free_port_of(&url);
        redirect(
            port,
            &format!("/auth/callback?code=authcode-1&state={}", state_of(&url)),
        );
        rx.recv_timeout(WAIT)
            .unwrap_or_else(|_| panic!("the login did not return within {WAIT:?}"))
    })
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
    let browser = Arc::new(RedirectBrowser {
        opened: Mutex::new(Vec::new()),
    });
    let path = browser_flow(&setup, &providers, None, &browser, &server).unwrap();
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
    let browser = Arc::new(RedirectBrowser {
        opened: Mutex::new(Vec::new()),
    });
    let path = browser_flow(&setup, &providers, Some("work"), &browser, &server).unwrap();
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
    let browser = Arc::new(RedirectBrowser {
        opened: Mutex::new(Vec::new()),
    });
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
    let browser = Arc::new(RedirectBrowser {
        opened: Mutex::new(Vec::new()),
    });
    let error = failed(browser_flow(&setup, &providers, None, &browser, &server));
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
    let browser = Arc::new(RedirectBrowser {
        opened: Mutex::new(Vec::new()),
    });
    let error = failed(browser_flow(&setup, &providers, None, &browser, &server));
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
    let browser = Arc::new(RedirectBrowser {
        opened: Mutex::new(Vec::new()),
    });
    let error = failed(browser_flow(&setup, &providers, None, &browser, &server));
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
    let browser = Arc::new(RedirectBrowser {
        opened: Mutex::new(Vec::new()),
    });
    let error = failed(browser_flow(&setup, &providers, None, &browser, &server));
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
fn fiber_login_is_attended_whatever_stdin_is() {
    assert!(Attended::attached().attended());
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
