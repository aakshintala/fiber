//! The session's credential pair through the provider API
//! (`docs/model-routing.md`, "Keys, tokens and OAuth"): `credential()`
//! receives the session's label and the stored credential's name, the token
//! cache is keyed by that pair, and `host.oauth.refresh` locks and reads the
//! pair's own credential file.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use common::{Setup, write};
use config::{CredentialFile, Secret, store_secret};
use contract::ErrorCode;
use contract::signing::SignRequest;
use extensions::{CredentialPair, LuaExtension, LuaProvider, REFRESH_BEFORE};
use fakes::clock::FakeClock;
use fakes::{OauthReply, OauthServer};
use serde_json::json;

/// How long a test waits for one call, or for a background refresh.
const WAIT: Duration = Duration::from_secs(5);

const INIT: &str = r#"
local echo_calls = 0

fiber.provider("echo", {
  credential = { timeout = 60000, run = function(who)
    echo_calls = echo_calls + 1
    return { token = who.credential .. "/" .. who.label .. "/" .. echo_calls, expires_at = 4102444800 }
  end },
  sign = { timeout = 60000, run = function() return {} end },
})

fiber.provider("oauth", {
  credential = { timeout = 60000, run = function(who)
    return host.oauth.refresh(function(stored)
      return { token = "fresh-" .. who.label, expires_at = 4102444800 }
    end)
  end },
  models = { timeout = 60000, run = function()
    host.oauth.refresh(function() return { token = "x", expires_at = 4102444800 } end)
    return {}
  end },
})

local soon_calls = 0
local late_calls = 0

fiber.provider("timed", {
  credential = { timeout = 60000, run = function(who)
    if who.label == "soon" then
      soon_calls = soon_calls + 1
      local reply = host.http({ url = host.secret("url") .. "/token", method = "POST" })
      local expires = soon_calls == 1 and 1700003600 or 1700007200
      return { token = json.decode(reply.body).access_token, expires_at = expires }
    end
    late_calls = late_calls + 1
    return { token = "late/" .. late_calls, expires_at = 4102444800 }
  end },
})
"#;

/// Runs `f` on its own thread under `WAIT`, so a call that never returns
/// fails the test instead of hanging it.
fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(f()));
    rx.recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("the call did not return within {WAIT:?}"))
}

fn pair(credential: &str, label: &str) -> CredentialPair {
    CredentialPair {
        credential: credential.to_owned(),
        label: label.to_owned(),
    }
}

/// The fixture extension, registering `echo`, `oauth` and `timed`, in a
/// fresh Fiber home on a fresh fake clock.
fn fixture() -> (Setup, Arc<FakeClock>, Arc<LuaExtension>) {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let dir = setup.root().join("extensions").join("pair");
    write(&dir.join("init.lua"), INIT);
    let extension = Arc::new(
        LuaExtension::new("pair", dir, setup.home(), clock.clone())
            .with_secrets(vec!["url".into()]),
    );
    (setup, clock, extension)
}

fn provider(extension: &Arc<LuaExtension>, name: &str) -> Arc<LuaProvider> {
    LuaProvider::new(Arc::clone(extension), name)
}

/// `provider.token()` for the pair, on its own thread under `WAIT`.
fn token(provider: &Arc<LuaProvider>, credential: &str, label: &str) -> String {
    let provider = Arc::clone(provider);
    let owned = pair(credential, label);
    within(move || provider.token(&owned))
        .unwrap()
        .expose()
        .to_owned()
}

fn data(name: &str, credential_name: Option<&str>) -> config::ProviderData {
    config::ProviderData {
        name: name.to_owned(),
        credential: None,
        credential_name: credential_name.map(str::to_owned),
        headers: BTreeMap::new(),
        placeholders: BTreeMap::new(),
        models: Vec::new(),
        reviewer_model: None,
    }
}

#[test]
fn for_provider_names_the_shared_credential_or_the_provider() {
    assert_eq!(
        CredentialPair::for_provider(&data("echo", Some("shared")), "work"),
        pair("shared", "work")
    );
    assert_eq!(
        CredentialPair::for_provider(&data("echo", None), "work"),
        pair("echo", "work")
    );
}

#[test]
fn credential_receives_the_label_and_the_stored_credential_name() {
    let (_setup, _clock, extension) = fixture();
    let echo = provider(&extension, "echo");
    assert_eq!(token(&echo, "shared", "work"), "shared/work/1");
}

#[test]
fn the_token_cache_is_keyed_by_the_pair() {
    let (_setup, _clock, extension) = fixture();
    let echo = provider(&extension, "echo");
    assert_eq!(token(&echo, "echo", "default"), "echo/default/1");
    assert_eq!(token(&echo, "echo", "work"), "echo/work/2");
    assert_eq!(token(&echo, "echo", "default"), "echo/default/1");
    assert_eq!(token(&echo, "shared", "default"), "shared/default/3");
    assert_eq!(token(&echo, "echo", "work"), "echo/work/2");
}

#[test]
fn a_signer_sends_its_own_pairs_token() {
    let (_setup, _clock, extension) = fixture();
    let echo = provider(&extension, "echo");
    let signed = |pair: CredentialPair| {
        let echo = Arc::clone(&echo);
        let signer = within(move || echo.signer(pair)).unwrap().unwrap();
        let signing = Arc::clone(&signer);
        let headers = within(move || {
            signing.sign(&SignRequest {
                method: "POST",
                url: "http://127.0.0.1:1/v1/responses",
                headers: &[],
                body: b"{}",
            })
        })
        .unwrap();
        (headers, signer.credentials())
    };
    let (default_headers, default_credentials) = signed(pair("echo", "default"));
    let (work_headers, work_credentials) = signed(pair("echo", "work"));
    assert_eq!(
        default_headers,
        [(
            "authorization".to_owned(),
            "Bearer echo/default/1".to_owned()
        )]
    );
    assert_eq!(
        work_headers,
        [("authorization".to_owned(), "Bearer echo/work/2".to_owned())]
    );
    assert_eq!(
        default_credentials
            .iter()
            .map(|secret| secret.expose())
            .collect::<Vec<_>>(),
        ["echo/default/1"]
    );
    assert_eq!(
        work_credentials
            .iter()
            .map(|secret| secret.expose())
            .collect::<Vec<_>>(),
        ["echo/work/2"]
    );
}

#[test]
fn refresh_locks_and_reads_the_calls_pair() {
    let (setup, _clock, extension) = fixture();
    let home = setup.home();
    let stored = json!({ "token": "stored-work", "expires_at": 4102444800_u64 });
    CredentialFile::new(&home, "shared", "work")
        .unwrap()
        .try_lock()
        .unwrap()
        .unwrap()
        .write(&stored)
        .unwrap();
    let oauth = provider(&extension, "oauth");
    assert_eq!(token(&oauth, "shared", "work"), "stored-work");
    assert_eq!(token(&oauth, "shared", "personal"), "fresh-personal");
    let reread = CredentialFile::new(&home, "shared", "personal")
        .unwrap()
        .try_lock()
        .unwrap()
        .unwrap()
        .read()
        .unwrap()
        .unwrap();
    assert_eq!(reread.get("token"), Some(&json!("fresh-personal")));
    assert!(
        !home.join("credentials/oauth/default").exists(),
        "nothing locks or writes the provider's own default"
    );
}

#[test]
fn a_pair_the_credential_file_refuses_fails_and_creates_nothing() {
    let (setup, _clock, extension) = fixture();
    let home = setup.home();
    let oauth = provider(&extension, "oauth");
    for (credential, label) in [
        ("oauth", "work.lock"),
        ("oauth", "work.tmp"),
        ("oauth", "a/b"),
        ("../escape", "work"),
        ("..", "work"),
    ] {
        let provider = Arc::clone(&oauth);
        let owned = pair(credential, label);
        let err = within(move || provider.token(&owned)).unwrap_err();
        assert_eq!(
            err.code(),
            ErrorCode::CredentialFailed,
            "{credential}/{label}: {err:?}"
        );
        assert!(
            err.to_string().contains("host.oauth.refresh"),
            "{credential}/{label}: {err}"
        );
    }
    assert!(
        !home.join("credentials/oauth").exists(),
        "a refused label creates nothing"
    );
    assert!(
        !home.join("escape").exists(),
        "a refused credential name creates nothing"
    );
}

#[test]
fn a_background_refresh_of_one_pair_leaves_another_pair_alone() {
    let (setup, clock, extension) = fixture();
    let server = OauthServer::start(vec![
        OauthReply::token("s1", "r", 3600),
        OauthReply::token("s2", "r", 3600),
    ]);
    store_secret(&setup.home(), "url", &Secret::new(server.url().to_owned())).unwrap();
    let timed = provider(&extension, "timed");
    assert_eq!(token(&timed, "timed", "soon"), "s1");
    assert_eq!(token(&timed, "timed", "late"), "late/1");
    // Into the refresh window, still short of expiry.
    clock.advance(
        Duration::from_secs(3600)
            .checked_sub(REFRESH_BEFORE)
            .unwrap(),
    );
    server.hold();
    let soon = Arc::clone(&timed);
    assert_eq!(
        within(move || soon.token(&pair("timed", "soon")))
            .unwrap()
            .expose(),
        "s1",
        "the request still gets the token it has while the refresh runs"
    );
    assert!(
        server.await_requests(2, WAIT),
        "waited for the refresh request"
    );
    // While the refresh is held, the other pair answers from its own entry:
    // no new `credential()` call, and the held pair's token is untouched.
    let late = Arc::clone(&timed);
    assert_eq!(
        within(move || late.token(&pair("timed", "late")))
            .unwrap()
            .expose(),
        "late/1"
    );
    assert_eq!(
        timed.cached_token(&pair("timed", "soon")).unwrap().expose(),
        "s1"
    );
    server.release();
    let (done, refreshed) = mpsc::channel();
    let polling = Arc::clone(&timed);
    std::thread::spawn(move || {
        loop {
            match polling.token(&pair("timed", "soon")) {
                Ok(secret) if secret.expose() == "s2" => {
                    match done.send(()) {
                        Ok(()) | Err(mpsc::SendError(())) => {}
                    }
                    return;
                }
                _ => std::thread::yield_now(),
            }
        }
    });
    refreshed
        .recv_timeout(WAIT)
        .expect("waited for the refreshed token");
    assert_eq!(token(&timed, "timed", "late"), "late/1");
    assert_eq!(server.request_count(), 2);
}

#[test]
fn refresh_outside_credential_is_refused() {
    let (setup, _clock, extension) = fixture();
    let oauth = provider(&extension, "oauth");
    let err = within({
        let oauth = Arc::clone(&oauth);
        move || oauth.models()
    })
    .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("host.oauth.refresh"), "{text}");
    assert!(text.contains("oauth.models"), "{text}");
    assert!(
        !setup.home().join("credentials/oauth").exists(),
        "a refused refresh creates nothing"
    );
}
