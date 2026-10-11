//! `/login` through the seam: the rows it lists and the keys it stores
//! (`docs/tui.md`, "Logging in").

use crate::test_support::install_extension;
use std::fs;
use std::path::PathBuf;

use serde_json::json;
use tui::{Configure, LoginKind};

use super::super::Seam;

struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-configure-login");
        fs::create_dir_all(root.path().join("home")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// Installs a provider `name`.
    fn install(&self, name: &str) {
        install_extension(
            &self.home(),
            &format!("extensions/{name}"),
            json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
            &[(
                name,
                json!({
                    "name": name,
                    "models": [{"id": "m", "protocol": "openai-responses",
                        "base_url": "http://x/v1", "context_window": 1000}],
                }),
            )],
        );
    }

    /// Installs the extension `extension` with no provider, declaring
    /// `secrets`.
    fn declare(&self, extension: &str, secrets: &[&str]) {
        install_extension(
            &self.home(),
            &format!("extensions/{extension}"),
            json!({"name": extension, "version": "v0.0.0", "fiber": "0.0.0", "api": 1,
                "secrets": secrets}),
            &[],
        );
    }
}

#[test]
fn login_targets_maps_providers_to_key_and_secrets_to_secret() {
    let setup = Setup::new();
    setup.install("acme");
    setup.declare("acme-secrets", &["acme.api_key"]);
    let rows = Seam::new(setup.home()).login_targets().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "acme");
    assert_eq!(rows[0].kind, LoginKind::Key);
    assert_eq!(rows[1].name, "acme.api_key");
    assert_eq!(rows[1].kind, LoginKind::Secret);
}

#[test]
fn store_key_lands_in_credentials_and_names_the_first_label() {
    let setup = Setup::new();
    setup.install("acme");
    let seam = Seam::new(setup.home());
    let stored = seam
        .store_key(
            "acme",
            Some("work"),
            contract::Secret::new("sk-a".to_owned()),
        )
        .unwrap();
    assert_eq!(stored.path, "credentials/acme/work");
    assert!(!stored.replaced);
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/acme/work")).unwrap(),
        "sk-a"
    );
    let global: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(
        global,
        json!({"providers": {"acme": {"credential": "work"}}})
    );
}

#[test]
fn store_key_refusal_is_usage_with_the_cli_message() {
    let setup = Setup::new();
    setup.install("acme");
    let seam = Seam::new(setup.home());
    seam.store_key("acme", None, contract::Secret::new("old".to_owned()))
        .unwrap();
    let error = seam
        .store_key("acme", None, contract::Secret::new("new".to_owned()))
        .err()
        .unwrap();
    assert_eq!(error.code, contract::ErrorCode::Usage);
    assert!(
        error.message.contains(
            "credentials/acme/default is already stored; log in under another label with --as <label>"
        ),
        "{}",
        error.message
    );
    assert!(
        !error.message.contains("fiber --help"),
        "no CLI hint: {}",
        error.message
    );
}

#[test]
fn store_key_with_an_empty_key_stores_nothing() {
    let setup = Setup::new();
    setup.install("acme");
    let error = Seam::new(setup.home())
        .store_key("acme", None, contract::Secret::new(String::new()))
        .err()
        .unwrap();
    assert_eq!(error.code, contract::ErrorCode::Usage);
    assert_eq!(error.message, "No key was given; nothing was stored.");
    assert!(!setup.home().join("credentials/acme/default").exists());
    assert!(!setup.home().join("config.json").exists());
}

#[test]
fn login_targets_maps_a_browser_provider_to_browser() {
    let setup = Setup::new();
    setup.install("acme");
    setup.install("codex");
    let path = setup.home().join("extensions/codex/providers/codex.json");
    let mut data: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    data["login"] = serde_json::json!("browser");
    fs::write(path, data.to_string()).unwrap();
    let rows = Seam::new(setup.home()).login_targets().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "acme");
    assert_eq!(rows[0].kind, LoginKind::Key);
    assert_eq!(rows[1].name, "codex");
    assert_eq!(rows[1].kind, LoginKind::Browser);
}

/// A `LoginShow` that records each `show` and nothing else.
struct ShowCode {
    shown: std::sync::mpsc::Sender<(String, String)>,
}

impl tui::LoginShow for ShowCode {
    fn open(&self, _url: &str) {}

    fn show(&self, url: &str, code: &str) {
        drop(self.shown.send((url.to_owned(), code.to_owned())));
    }
}

#[test]
fn the_browser_shows_the_url_and_code_through_the_login_show() {
    let (tx, rx) = std::sync::mpsc::channel();
    let browser = super::ShownBrowser {
        shown: std::sync::Arc::new(ShowCode { shown: tx }),
    };
    extensions::Browser::show(&browser, "https://auth.example/device", "ABCD-1234");
    assert_eq!(
        rx.try_recv().ok(),
        Some((
            "https://auth.example/device".to_owned(),
            "ABCD-1234".to_owned()
        ))
    );
}

#[cfg(test)]
mod browser {
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::Duration;

    use fakes::Deadline;
    use fakes::clock::FakeClock;
    use fakes::{OauthReply, OauthServer, jwt};
    use serde_json::{Value, json};
    use tui::{BrowserLogin, Configure, LoginShow};

    use super::super::super::Seam;

    const WAIT: Duration = Duration::from_secs(10);
    const BROWSER_WAIT: Duration = Duration::from_secs(4);
    const ACCOUNT: &str = "acct_1";
    const EMAIL: &str = "alice@example.com";

    struct Show {
        notify: mpsc::Sender<String>,
    }

    impl Show {
        fn recording() -> (Arc<Self>, mpsc::Receiver<String>) {
            let (notify, opened) = mpsc::channel();
            (Arc::new(Self { notify }), opened)
        }
    }

    impl LoginShow for Show {
        fn open(&self, url: &str) {
            match self.notify.send(url.to_owned()) {
                Ok(()) | Err(_) => {}
            }
        }

        fn show(&self, _url: &str, _code: &str) {}
    }

    fn free_port() -> u16 {
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
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

    fn tokens(email: &str) -> (String, String) {
        let access = jwt(&json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": ACCOUNT },
            "exp": 4_102_444_800u64,
        }));
        (access, jwt(&json!({ "email": email })))
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

    #[track_caller]
    fn await_opened(opened: &mpsc::Receiver<String>, wait: &Deadline) -> String {
        wait.recv_or_fail(opened, "the package to open the authorize URL")
    }

    #[track_caller]
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

    fn port_of(url: &str) -> u16 {
        url.split_once('?')
            .unwrap()
            .1
            .split('&')
            .find_map(|pair| {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                (name == "redirect_uri").then(|| value.to_owned())
            })
            .unwrap()
            .trim_start_matches("http%3A%2F%2Flocalhost%3A")
            .split_once('%')
            .unwrap()
            .0
            .parse()
            .unwrap()
    }

    fn install_codex(home: &Path, server: &OauthServer, port: u16) {
        let dest = home.join("extensions").join("codex");
        copy_package(
            &dest,
            &[
                ("https://auth.openai.com", &server.url()),
                ("local PORT = 1455", &format!("local PORT = {port}")),
            ],
        );
        crate::test_support::write_record(&dest);
    }

    #[test]
    fn a_browser_login_through_the_seam_stores_what_fiber_login_stores() {
        let root = fakes::TempDir::new("fiber-configure-login-browser");
        let home = root.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
        let (access, id) = tokens(EMAIL);
        let server = OauthServer::start(vec![exchange(&access, &id)]);
        install_codex(&home, &server, free_port());
        let seam = Seam::new(home.clone());
        let (shown, opened) = Show::recording();
        let login: Arc<dyn BrowserLogin> = seam.browser_login("codex", shown, Arc::clone(&clock));
        let (done, finished) = mpsc::channel();
        thread::spawn(move || match done.send(login.run()) {
            Ok(()) | Err(_) => {}
        });
        let url = await_opened(&opened, &Deadline::after(BROWSER_WAIT));
        redirect(
            port_of(&url),
            &format!("/auth/callback?code=authcode-1&state={}", state_of(&url)),
        );
        let stored = Deadline::after(WAIT)
            .recv(&finished)
            .unwrap_or_else(|_| panic!("the login did not return within {WAIT:?}"))
            .unwrap();
        assert_eq!(
            stored,
            tui::Stored {
                path: "credentials/codex/alice@example.com".to_owned(),
                replaced: false,
            }
        );
        let bytes = fs::read(home.join("credentials/codex").join(EMAIL)).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            json!({
                "token": access,
                "expires_at": 4_102_444_800u64,
                "refresh_token": "rt_1",
                "account_id": ACCOUNT,
            })
        );
        assert_eq!(
            fs::metadata(home.join("credentials/codex").join(EMAIL))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let global: Value =
            serde_json::from_str(&fs::read_to_string(home.join("config.json")).unwrap()).unwrap();
        assert_eq!(global["providers"]["codex"]["credential"], json!(EMAIL));
    }

    #[test]
    fn cancelling_a_waiting_browser_login_through_the_seam_stores_nothing() {
        let root = fakes::TempDir::new("fiber-configure-login-cancel");
        let home = root.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
        let (access, id) = tokens(EMAIL);
        let server = OauthServer::start(vec![exchange(&access, &id)]);
        install_codex(&home, &server, free_port());
        let seam = Seam::new(home.clone());
        let (shown, opened) = Show::recording();
        let login: Arc<dyn BrowserLogin> = seam.browser_login("codex", shown, Arc::clone(&clock));
        let (done, finished) = mpsc::channel();
        let running = Arc::clone(&login);
        thread::spawn(move || match done.send(running.run()) {
            Ok(()) | Err(_) => {}
        });
        let url = await_opened(&opened, &Deadline::after(BROWSER_WAIT));
        let callback_port = port_of(&url);
        login.cancel();
        let result = Deadline::after(WAIT)
            .recv(&finished)
            .unwrap_or_else(|_| panic!("the cancelled login did not return within {WAIT:?}"));
        assert!(result.is_err(), "{result:?}");
        assert!(!home.join("credentials/codex").join(EMAIL).exists());
        let global = fs::read_to_string(home.join("config.json")).unwrap_or_default();
        assert!(!global.contains("credential"), "{global}");
        drop(server);
        fakes::within("the callback port to bind again", BROWSER_WAIT, move || {
            loop {
                if TcpListener::bind((Ipv4Addr::LOCALHOST, callback_port)).is_ok() {
                    return;
                }
                thread::yield_now();
            }
        });
    }
}
