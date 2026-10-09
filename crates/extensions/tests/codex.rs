//! The real codex package through `LuaExtension`
//! (`docs/model-routing.md`, "Logging in"): the browser login, the device
//! login, the refresh and the session path, against the OAuth fake with the
//! package copied to a temp dir and its origin and port rewritten.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]
#![allow(
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod common;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::{Setup, copy_package};
use config::CredentialFile;
use contract::ErrorCode;
use contract::signing::SignRequest;
use extensions::{
    Browser, CredentialPair, Error, LoggedIn, LoginMethod, LuaExtension, LuaProvider,
    login_provider,
};
use fakes::OauthReply;
use fakes::OauthServer;
use fakes::clock::FakeClock;
use fakes::jwt;
use serde_json::{Value, json};

/// How long a test waits for one call or one background refresh.
const WAIT: Duration = Duration::from_secs(10);

/// How long a test waits for the package to open the authorize URL or for
/// its callback listener to bind: one wall-clock deadline for both waits.
const BROWSER_WAIT: Duration = Duration::from_secs(4);

/// The fake clock's wall at construction, in Unix seconds.
const WALL: u64 = 1_700_000_000;

/// The test access token's expiry, in Unix seconds.
const EXP: u64 = 4_102_444_800;

/// The test account id and email.
const ACCOUNT: &str = "acct_1";
const EMAIL: &str = "alice@example.com";

/// The client id the package sends, from pi's flow and the codex CLI.
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

struct Env {
    setup: Setup,
    clock: Arc<FakeClock>,
}

impl Env {
    fn new() -> Self {
        Self {
            setup: Setup::new(),
            clock: FakeClock::new(),
        }
    }

    fn home(&self) -> std::path::PathBuf {
        self.setup.home()
    }

    /// Installs the package copy pointing at `server`, with its callback
    /// port rewritten to `port`.
    fn install(&self, server: &OauthServer, port: u16) {
        copy_package(
            &self.home().join("extensions").join("codex"),
            "codex",
            &[
                ("https://auth.openai.com", &server.url()),
                ("local PORT = 1455", &format!("local PORT = {port}")),
            ],
        );
    }

    fn providers(&self) -> extensions::Providers {
        let (providers, notices) = extensions::Providers::load(&self.home()).unwrap();
        assert!(notices.is_empty(), "{notices:?}");
        providers
    }

    /// The installed package's provider, logging in with `browser`.
    fn logging_in(&self, browser: Arc<dyn Browser>) -> Arc<LuaProvider> {
        login_provider(
            &self.home(),
            &self.providers(),
            "codex",
            browser,
            self.clock.clone(),
        )
        .unwrap()
    }

    /// The installed package's provider for sessions, with nobody attached.
    fn session(&self) -> Arc<LuaProvider> {
        let dir = self.home().join("extensions").join("codex");
        let extension = Arc::new(LuaExtension::new(
            "codex",
            dir,
            self.home(),
            self.clock.clone(),
        ));
        LuaProvider::new(extension, "codex")
    }

    fn pair() -> CredentialPair {
        CredentialPair {
            credential: "codex".to_owned(),
            label: "default".to_owned(),
        }
    }

    fn store(&self, value: &Value) {
        let lock = CredentialFile::new(&self.home(), "codex", "default")
            .unwrap()
            .try_lock()
            .unwrap()
            .unwrap();
        lock.write(value).unwrap();
    }
}

/// Starts `provider.login` on its own thread; the result arrives on the
/// receiver, so the test thread can move the fake clock meanwhile.
fn start_login(
    provider: &Arc<LuaProvider>,
    label: Option<&str>,
    method: LoginMethod,
) -> mpsc::Receiver<Result<LoggedIn, Error>> {
    let (tx, rx) = mpsc::channel();
    let (provider, label) = (Arc::clone(provider), label.map(str::to_owned));
    std::thread::spawn(move || tx.send(provider.login("codex", label.as_deref(), method)));
    rx
}

fn finish_login(rx: &mpsc::Receiver<Result<LoggedIn, Error>>) -> Result<LoggedIn, Error> {
    rx.recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("the login did not return within {WAIT:?}"))
}

/// A port nothing listens on, for the package's callback listener.
fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// How the injected browser answers thearct callback.
#[derive(Clone, Copy)]
enum Answer {
    /// The authorize URL's code and state.
    Code,
    /// A state that does not match.
    WrongState,
    /// An `error` parameter instead of a code.
    Denied,
    /// A state with no code.
    MissingCode,
}

/// A browser that records the authorize URL and opens nothing: the test
/// thread performs the redirect once the callback listens, since the
/// listener starts only after `open` returns. Each `open` also reports on
/// the channel, so the wait for it is a notification under [`BROWSER_WAIT`]
/// instead of an attempt count.
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

/// Waits for the package's `open` notification, without reading the
/// clock: the login parks on the fake clock, so a wait on it would never
/// end. The bound is the wall-clock [`BROWSER_WAIT`], so a login that
/// never opens fails there instead of hanging.
fn await_opened(opened: &mpsc::Receiver<String>) -> String {
    opened.recv_timeout(BROWSER_WAIT).unwrap_or_else(|_| {
        panic!("the package never opened the authorize URL within {BROWSER_WAIT:?}")
    })
}

/// GETs the callback's `target` on `port`, retrying a refused connection
/// until the listener binds. The callback serves one request; the bound is
/// the wall-clock [`BROWSER_WAIT`], so a listener that never binds fails
/// there instead of hanging.
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
                Err(_) => std::thread::yield_now(),
            }
        }
    });
}

/// The authorize URL's query parameters, percent-decoded.
fn split_query(query: &str) -> std::collections::BTreeMap<String, String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(name), decode(value))
        })
        .collect()
}

fn decode(text: &str) -> String {
    let mut out = Vec::with_capacity(text.len());
    let mut bytes = text.bytes();
    while let Some(byte) = bytes.next() {
        match byte {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = |b: Option<u8>| b.and_then(|b| char::from(b).to_digit(16));
                match (hex(bytes.next()), hex(bytes.next())) {
                    (Some(high), Some(low)) => {
                        out.push(u8::try_from(high * 16 + low).unwrap_or(b'%'));
                    }
                    _ => out.push(b'%'),
                }
            }
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The S256 challenge of `verifier`.
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(ring::digest::digest(
        &ring::digest::SHA256,
        verifier.as_bytes(),
    ))
}

/// An access token carrying the account id and the expiry, and an id token
/// carrying the email.
fn tokens() -> (String, String) {
    let access = jwt(&json!({
        "https://api.openai.com/auth": { "chatgpt_account_id": ACCOUNT },
        "exp": EXP,
    }));
    let id = jwt(&json!({ "email": EMAIL, "exp": EXP }));
    (access, id)
}

/// A 200 token exchange reply.
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

/// Logs in by browser against `replies`, answering the callback as `answer`
/// says, and returns the login, the authorize URL and every request the
/// fake saw: a refused callback sends none.
fn browser_login(
    answer: Answer,
    replies: Vec<OauthReply>,
) -> (
    Env,
    OauthServer,
    Result<LoggedIn, Error>,
    String,
    Vec<fakes::OauthRequest>,
) {
    let env = Env::new();
    let server = OauthServer::start(replies);
    let port = free_port();
    env.install(&server, port);
    let (browser, opened) = RedirectBrowser::notified();
    let provider = env.logging_in(browser.clone());
    let rx = start_login(&provider, None, LoginMethod::Browser);
    // The listener starts after `open` returns, so the redirect goes out
    // from here, once the authorize URL is recorded.
    let url = await_opened(&opened);
    let query = url.split_once('?').map_or("", |(_, query)| query);
    let state = split_query(query).get("state").cloned().unwrap_or_default();
    let target = match answer {
        Answer::Code => format!("/auth/callback?code=authcode-1&state={state}"),
        Answer::WrongState => format!("/auth/callback?code=authcode-1&state={state}-wrong"),
        Answer::Denied => format!("/auth/callback?error=access_denied&state={state}"),
        Answer::MissingCode => format!("/auth/callback?state={state}"),
    };
    redirect(port, &target);
    let logged = finish_login(&rx);
    let requests = server.requests();
    (env, server, logged, url, requests)
}

// ------------------------------------------------------------------ browser

#[test]
fn a_browser_login_sends_the_wire_shape_and_stores_the_four_fields() {
    let (access, id) = tokens();
    let (_env, server, logged, url, requests) =
        browser_login(Answer::Code, vec![exchange(&access, &id)]);
    let logged = logged.unwrap();
    let token = requests.last().unwrap();

    // The authorize URL carries pi's parameters, with Fiber as originator.
    let (base, query) = url.split_once('?').unwrap();
    assert_eq!(base, format!("{}/oauth/authorize", server.url()));
    let params = split_query(query);
    assert_eq!(params["response_type"], "code");
    assert_eq!(params["client_id"], CLIENT_ID);
    assert_eq!(
        params["redirect_uri"],
        format!("http://localhost:{}/auth/callback", free_port_url(&url))
    );
    assert_eq!(params["scope"], "openid profile email offline_access");
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["id_token_add_organizations"], "true");
    assert_eq!(params["codex_cli_simplified_flow"], "true");
    assert_eq!(params["originator"], "fiber");
    assert_eq!(params["state"].len(), 43);
    assert_eq!(params["code_challenge"].len(), 43);

    // The exchange names the code, and its verifier matches the challenge.
    assert_eq!(token.path, "/oauth/token");
    let form: std::collections::BTreeMap<String, String> = token.form.clone().into_iter().collect();
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["client_id"], CLIENT_ID);
    assert_eq!(form["code"], "authcode-1");
    assert_eq!(challenge(&form["code_verifier"]), params["code_challenge"]);
    assert_eq!(form["redirect_uri"], params["redirect_uri"]);

    // The slot holds the four fields; the email rides the return.
    assert_eq!(
        logged.stored.as_value(),
        &json!({
            "token": access,
            "expires_at": EXP,
            "refresh_token": "rt_1",
            "account_id": ACCOUNT,
        })
    );
    assert_eq!(logged.email.as_deref(), Some(EMAIL));
}

/// The redirect port the authorize URL names.
fn free_port_url(url: &str) -> u16 {
    let params = split_query(url.split_once('?').unwrap().1);
    params["redirect_uri"]
        .trim_start_matches("http://localhost:")
        .split_once('/')
        .unwrap()
        .0
        .parse()
        .unwrap()
}

#[test]
fn a_browser_login_refused_at_the_callback_is_credential_failed() {
    for answer in [Answer::WrongState, Answer::Denied, Answer::MissingCode] {
        let (access, id) = tokens();
        let (_env, server, logged, _url, requests) =
            browser_login(answer, vec![exchange(&access, &id)]);
        let error = logged.unwrap_err();
        assert_eq!(error.code(), ErrorCode::CredentialFailed, "{error:?}");
        // No token request follows a refused callback.
        assert!(requests.is_empty(), "{requests:?}");
        let _ = server;
    }
}

#[test]
fn a_browser_login_whose_exchange_fails_is_credential_failed() {
    let (access, id) = tokens();
    // A rejected exchange.
    let (_env, _server, logged, _url, _requests) =
        browser_login(Answer::Code, vec![OauthReply::raw(400, "{}")]);
    assert_eq!(logged.unwrap_err().code(), ErrorCode::CredentialFailed);
    // An exchange without a refresh token.
    let server = OauthServer::start(vec![OauthReply::raw(
        200,
        &json!({ "access_token": access, "id_token": id, "expires_in": 864000 }).to_string(),
    )]);
    let env = Env::new();
    let port = free_port();
    env.install(&server, port);
    let (browser, opened) = RedirectBrowser::notified();
    let provider = env.logging_in(browser.clone());
    let rx = start_login(&provider, None, LoginMethod::Browser);
    let url = await_opened(&opened);
    let state = split_query(url.split_once('?').unwrap().1)
        .get("state")
        .cloned()
        .unwrap_or_default();
    redirect(
        port,
        &format!("/auth/callback?code=authcode-1&state={state}"),
    );
    assert_eq!(
        finish_login(&rx).unwrap_err().code(),
        ErrorCode::CredentialFailed
    );
}

#[test]
fn a_browser_login_whose_access_token_is_unusable_is_credential_failed() {
    for (name, access) in [
        ("missing_claim", jwt(&json!({ "exp": EXP }))),
        ("malformed", "not-a-jwt".to_owned()),
        (
            "float_expiry",
            jwt(&json!({
                "https://api.openai.com/auth": { "chatgpt_account_id": ACCOUNT },
                "exp": 1_791_403_200.5,
            })),
        ),
    ] {
        let id = jwt(&json!({ "email": EMAIL }));
        let (_env, _server, logged, _url, _requests) =
            browser_login(Answer::Code, vec![exchange(&access, &id)]);
        assert_eq!(
            logged.unwrap_err().code(),
            ErrorCode::CredentialFailed,
            "{name}"
        );
    }
}

// ------------------------------------------------------------------- device

/// A browser that records what `show` shows; the device flow opens nothing.
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

/// A user-code reply with `interval`.
fn usercode(interval: Value) -> OauthReply {
    OauthReply::raw(
        200,
        &json!({
            "device_auth_id": "da_1",
            "user_code": "ABCD-1234",
            "interval": interval,
        })
        .to_string(),
    )
}

/// A device-token poll reply with the authorization code.
fn polled() -> OauthReply {
    OauthReply::raw(
        200,
        &json!({ "authorization_code": "authcode-2", "code_verifier": "verifier-2" }).to_string(),
    )
}

/// Logs in by device code, moving the fake clock past each poll sleep, and
/// returns the login and every request the fake saw.
fn device_login(
    interval: Value,
) -> (
    OauthServer,
    Result<LoggedIn, Error>,
    Vec<fakes::OauthRequest>,
) {
    let env = Env::new();
    let server = OauthServer::start(vec![
        usercode(interval),
        OauthReply::raw(
            403,
            r#"{"error":{"code":"deviceauth_authorization_pending"}}"#,
        ),
        OauthReply::raw(404, "not here"),
        polled(),
        exchange(&tokens().0, &tokens().1),
    ]);
    let port = free_port();
    env.install(&server, port);
    let browser = Arc::new(ShowBrowser {
        shown: Mutex::new(Vec::new()),
    });
    let provider = env.logging_in(browser.clone());
    let rx = start_login(&provider, Some("work"), LoginMethod::Device);
    // Two pending polls, five seconds apart; the clock moves only once a
    // poll has parked on it.
    // A ready result is kept, never dropped: `try_recv` consumes it.
    let mut done = None;
    for step in [5, 10] {
        if let Ok(result) = rx.try_recv() {
            done = Some(result);
            break;
        }
        assert!(
            env.clock
                .await_parked(env.clock.origin() + Duration::from_secs(step), WAIT),
            "the device poll never parked for its {step}-second sleep within {WAIT:?}"
        );
        env.clock.advance(Duration::from_secs(5));
    }
    let logged = match done {
        Some(result) => result,
        None => finish_login(&rx),
    };
    let shown = browser.shown.lock().unwrap().clone();
    assert_eq!(
        shown,
        [(
            format!("{}/codex/device", server.url()),
            "ABCD-1234".to_owned()
        )]
    );
    let requests = server.requests();
    (server, logged, requests)
}

#[test]
fn a_device_login_shows_polls_and_exchanges_with_the_device_redirect() {
    for interval in [json!("5"), json!(5)] {
        let (server, logged, requests) = device_login(interval);
        let logged = logged.unwrap();
        assert_eq!(requests.len(), 5);

        // The user code goes out as JSON, and the polls carry it back.
        assert_eq!(requests[0].path, "/api/accounts/deviceauth/usercode");
        assert_eq!(
            serde_json::from_str::<Value>(&requests[0].body).unwrap(),
            json!({ "client_id": CLIENT_ID })
        );
        for request in &requests[1..3] {
            assert_eq!(request.path, "/api/accounts/deviceauth/token");
            assert_eq!(
                serde_json::from_str::<Value>(&request.body).unwrap(),
                json!({ "device_auth_id": "da_1", "user_code": "ABCD-1234" })
            );
        }

        // The exchange is the browser's, with the device redirect.
        let form: std::collections::BTreeMap<String, String> =
            requests[4].form.clone().into_iter().collect();
        assert_eq!(form["grant_type"], "authorization_code");
        assert_eq!(form["code"], "authcode-2");
        assert_eq!(form["code_verifier"], "verifier-2");
        assert_eq!(
            form["redirect_uri"],
            format!("{}/deviceauth/callback", server.url())
        );

        assert_eq!(logged.email.as_deref(), Some(EMAIL));
        assert_eq!(logged.stored.as_value()["account_id"], json!(ACCOUNT));
        let _ = server;
    }
}

#[test]
fn a_device_login_without_a_device_auth_id_fails_before_any_poll() {
    let env = Env::new();
    let server = OauthServer::start(vec![OauthReply::raw(
        200,
        &json!({ "user_code": "ABCD-1234", "interval": "5" }).to_string(),
    )]);
    let port = free_port();
    env.install(&server, port);
    let browser = Arc::new(ShowBrowser {
        shown: Mutex::new(Vec::new()),
    });
    let rx = start_login(&env.logging_in(browser), None, LoginMethod::Device);
    let error = finish_login(&rx).unwrap_err();
    assert_eq!(error.code(), ErrorCode::CredentialFailed, "{error:?}");
    assert_eq!(server.request_count(), 1);
}

// ------------------------------------------------------------------ session

/// Signs one request with `provider`'s session for the default label.
fn sign_with(provider: &Arc<LuaProvider>) -> Vec<(String, String)> {
    let signer = provider.signer(Env::pair()).unwrap().unwrap();
    let url = "https://chatgpt.com/backend-api/codex/responses".to_owned();
    signer
        .sign(&SignRequest {
            method: "POST",
            url: &url,
            headers: &[],
            body: b"{}",
        })
        .unwrap()
}

fn header(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.clone())
}

#[test]
fn a_stored_credential_signs_with_its_account_id_and_sends_no_request() {
    let env = Env::new();
    let server = OauthServer::start(vec![]);
    env.install(&server, free_port());
    let (access, _id) = tokens();
    env.store(&json!({
        "token": access,
        "expires_at": EXP,
        "refresh_token": "rt_1",
        "account_id": ACCOUNT,
    }));
    let provider = env.session();
    let token = provider.token(&Env::pair()).unwrap();
    assert_eq!(token.expose(), access);
    assert_eq!(server.request_count(), 0);

    let headers = sign_with(&provider);
    assert_eq!(
        headers[0],
        ("authorization".to_owned(), format!("Bearer {access}"))
    );
    assert_eq!(
        header(&headers, "chatgpt-account-id"),
        Some(ACCOUNT.to_owned())
    );
}

#[test]
fn a_credential_due_for_refresh_is_replaced_with_its_headers() {
    let env = Env::new();
    let fresh = jwt(&json!({
        "https://api.openai.com/auth": { "chatgpt_account_id": "acct_2" },
        "exp": EXP,
    }));
    let server = OauthServer::start(vec![OauthReply::raw(
        200,
        &json!({
            "access_token": fresh,
            "refresh_token": "rt_2",
            "id_token": jwt(&json!({ "email": "other@example.com" })),
            "expires_in": 864000,
        })
        .to_string(),
    )]);
    env.install(&server, free_port());
    env.store(&json!({
        "token": "old",
        "expires_at": WALL + 3600,
        "refresh_token": "rt_1",
        "account_id": ACCOUNT,
    }));
    let provider = env.session();
    // Outside the window the stored token serves with no request.
    assert_eq!(provider.token(&Env::pair()).unwrap().expose(), "old");
    assert_eq!(server.request_count(), 0);
    // Into the window, still short of expiry: the old token serves while
    // the refresh runs beside it.
    env.clock.advance(Duration::from_secs(3301));
    assert_eq!(provider.token(&Env::pair()).unwrap().expose(), "old");
    assert!(
        server.await_requests(1, WAIT),
        "the background refresh never ran within {WAIT:?}"
    );
    let request = server.requests().pop().unwrap();
    assert_eq!(request.path, "/oauth/token");
    let form: std::collections::BTreeMap<String, String> = request.form.into_iter().collect();
    assert_eq!(form["grant_type"], "refresh_token");
    assert_eq!(form["refresh_token"], "rt_1");
    assert_eq!(form["client_id"], CLIENT_ID);

    // Every sign is one consistent pair or the other, until the new one lands.
    let polling = Arc::clone(&provider);
    fakes::within("the refresh to land", WAIT, move || {
        loop {
            let headers = sign_with(&polling);
            let token = header(&headers, "authorization");
            let account = header(&headers, "chatgpt-account-id");
            assert!(
                (token.clone(), account.clone())
                    == (Some("Bearer old".to_owned()), Some(ACCOUNT.to_owned()))
                    || (token, account)
                        == (Some(format!("Bearer {fresh}")), Some("acct_2".to_owned())),
                "a request never pairs a token with another token's headers"
            );
            if header(&headers, "authorization") == Some(format!("Bearer {fresh}")) {
                return;
            }
            std::thread::yield_now();
        }
    });
}

#[test]
fn a_rejected_refresh_keeps_authentication_failed_and_the_stored_file() {
    let env = Env::new();
    let server = OauthServer::start(vec![OauthReply::raw(401, "{}")]);
    env.install(&server, free_port());
    env.store(&json!({
        "token": "old",
        "expires_at": WALL + 60,
        "refresh_token": "rt_1",
        "account_id": ACCOUNT,
    }));
    let before = std::fs::read(env.home().join("credentials/codex/default")).unwrap();
    let error = env.session().token(&Env::pair()).unwrap_err();
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed, "{error:?}");
    assert_eq!(
        std::fs::read(env.home().join("credentials/codex/default")).unwrap(),
        before
    );
}

#[test]
fn a_session_with_nothing_stored_and_nobody_attached_is_authentication_failed() {
    let env = Env::new();
    let server = OauthServer::start(vec![]);
    env.install(&server, free_port());
    let error = env.session().token(&Env::pair()).unwrap_err();
    assert!(matches!(error, Error::Unattended { .. }), "{error:?}");
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed);
    assert!(server.requests().is_empty());
}

#[test]
fn a_browser_login_with_its_port_busy_names_device_login() {
    let env = Env::new();
    let server = OauthServer::start(vec![]);
    let port = free_port();
    env.install(&server, port);
    // The codex CLI's own login holds the callback port.
    let _held = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
    let (browser, opened) = RedirectBrowser::notified();
    let rx = start_login(&env.logging_in(browser.clone()), None, LoginMethod::Browser);
    let url = await_opened(&opened);
    assert!(url.contains("/oauth/authorize"), "{url}");
    let error = finish_login(&rx).unwrap_err();
    assert_eq!(error.code(), ErrorCode::CredentialFailed, "{error:?}");
    assert!(error.to_string().contains("is in use"), "{error}");
    assert!(error.to_string().contains("--device"), "{error}");
    assert!(server.requests().is_empty());
}
