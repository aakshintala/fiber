#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::panic, reason = "the test's wait deadline is its failure")]

use std::fs;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::Secret;
use contract::signing::{SignRequest, Signer};
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use crate::{
    CredentialPair, LuaExtension,
    lua_provider::{LuaProvider, LuaSigner, UsedState, parse_credential_headers, redact_values},
};

const WAIT: Duration = Duration::from_secs(5);

#[test]
fn redaction_replaces_each_non_empty_call_value() {
    let cases: &[(&[&str], &str, &str)] = &[
        (&[""], "untouched", "untouched"),
        (&["tok", "tok-1"], "saw tok-1", "saw [redacted]"),
        (
            &["token"],
            "token and token again",
            "[redacted] and [redacted] again",
        ),
        (&["absent"], "no credential here", "no credential here"),
    ];
    for (values, message, expected) in cases {
        let values: Vec<Secret> = values
            .iter()
            .map(|value| Secret::new((*value).to_owned()))
            .collect();
        assert_eq!(redact_values(message, &values), *expected, "{values:?}");
    }
}

#[test]
fn used_history_keeps_sixteen_completed_values_at_the_boundary() {
    for (count, want) in [(15_usize, 15_usize), (16, 16), (17, 16)] {
        let values: Vec<Secret> = (1..=count)
            .map(|n| Secret::new(format!("tok-{n}")))
            .collect();
        let mut used = UsedState::default();
        used.start(&values);
        used.finish(&values);

        let completed: Vec<String> = used
            .completed
            .iter()
            .map(|secret| secret.expose().to_owned())
            .collect();
        let first = count.saturating_sub(16) + 1;
        let expected: Vec<String> = (first..=count).map(|n| format!("tok-{n}")).collect();
        assert_eq!(completed, expected, "{count} completed calls");
        assert!(used.active.is_empty(), "{count} calls have finished");
        assert_eq!(completed.len(), want, "{count} completed values");
    }
}

#[test]
fn credential_header_parser_distinguishes_empty_and_non_empty_arrays() {
    type HeaderCase = (Option<Value>, &'static [(&'static str, &'static str)], bool);
    let cases: &[HeaderCase] = &[
        (None, &[], true),
        (Some(Value::Null), &[], true),
        (Some(json!({})), &[], true),
        (Some(json!([])), &[], true),
        (Some(json!({"x-key": "value"})), &[("x-key", "value")], true),
        (Some(json!(["value"])), &[], false),
        (Some(json!({"x-key": 1})), &[], false),
        (Some(json!("value")), &[], false),
    ];
    for (headers, expected, valid) in cases {
        let parsed = parse_credential_headers(headers.as_ref());
        assert_eq!(parsed.is_ok(), *valid, "{headers:?}");
        if let Ok(parsed) = parsed {
            let mut parsed: Vec<(String, String)> = parsed
                .into_iter()
                .map(|(name, value)| (name, value.expose().to_owned()))
                .collect();
            parsed.sort();
            let mut expected: Vec<(String, String)> = expected
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect();
            expected.sort();
            assert_eq!(parsed, expected, "{headers:?}");
        }
    }
}

fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(f()));
    rx.recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("the call did not return within {WAIT:?}"))
}

#[test]
fn credentials_is_empty_for_a_signer_without_credentials_even_with_a_cached_token() {
    let root = fakes::TempDir::new("fiber-lua-provider");
    let home = root.path().join("home");
    let dir = root.path().join("ext");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("init.lua"),
        r#"fiber.provider("p", {
            credential = {
                timeout = 5000,
                run = function()
                    return { token = "tok-1", expires_at = 4102444800 }
                end
            }
        })"#,
    )
    .unwrap();
    let extension = Arc::new(LuaExtension::new("ext", dir, home, FakeClock::new()));
    let provider = LuaProvider::new(extension, "p");

    let with_credential = provider
        .signer(CredentialPair {
            credential: "p".to_owned(),
            label: "default".to_owned(),
        })
        .unwrap()
        .unwrap();
    let url = "http://127.0.0.1:1/v1/responses".to_owned();
    let headers = within({
        let signer = Arc::clone(&with_credential);
        move || {
            signer.sign(&SignRequest {
                method: "POST",
                url: &url,
                headers: &[],
                body: b"{}",
            })
        }
    })
    .unwrap();
    assert_eq!(
        headers,
        [("authorization".to_owned(), "Bearer tok-1".to_owned())]
    );

    // A registered credential makes every public signer enable it, so use
    // the cached provider with a signer whose credential flag is false.
    let without_credential = LuaSigner {
        provider,
        pair: CredentialPair {
            credential: "p".to_owned(),
            label: "default".to_owned(),
        },
        credential: false,
        sign: false,
    };
    assert!(
        without_credential.credentials().is_empty(),
        "a cached token must not be reported by a signer without credential()"
    );
}

#[test]
fn detail_is_the_first_line_of_the_extension_s_own_text() {
    use crate::Error;
    use crate::lua_provider::detail;

    fn lua(message: &str) -> Error {
        Error::Lua {
            extension: "ext".to_owned(),
            message: message.to_owned(),
        }
    }
    let cases: Vec<(Error, &str)> = vec![
        (lua(""), ""),
        (
            Error::RefreshRejected {
                extension: "ext".to_owned(),
                message: "\n".to_owned(),
            },
            "",
        ),
        (
            Error::RefreshUnreachable {
                extension: "ext".to_owned(),
                message: "a\r\nb".to_owned(),
            },
            "a",
        ),
        (Error::Credential(Box::new(lua("x\ny"))), "x"),
        (
            Error::Credential(Box::new(Error::Credential(Box::new(
                Error::RefreshRejected {
                    extension: "ext".to_owned(),
                    message: "r".to_owned(),
                },
            )))),
            "r",
        ),
    ];
    for (error, expected) in &cases {
        assert_eq!(detail(error), *expected, "{error:?}");
    }
    let bad = Error::BadReturn {
        extension: "ext".to_owned(),
        callback: "p.credential".to_owned(),
        why: "no `token`".to_owned(),
    };
    assert_eq!(detail(&bad), bad.to_string());
}

fn models_provider(root: &fakes::TempDir, run: &str) -> (std::path::PathBuf, Arc<LuaProvider>) {
    let home = root.path().join("home");
    let dir = root.path().join("ext");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("init.lua"),
        format!(
            "fiber.provider(\"p\", {{ models = {{ timeout = 5000, run = function() return {run} end }} }})"
        ),
    )
    .unwrap();
    let extension = Arc::new(LuaExtension::new(
        "ext",
        dir,
        home.clone(),
        FakeClock::new(),
    ));
    let provider = LuaProvider::new(extension, "p");
    (home, provider)
}

fn model_entry(id: &str) -> String {
    format!(
        "{{ id = \"{id}\", protocol = \"openai-responses\", \
         base_url = \"http://127.0.0.1:1/v1\", context_window = 1000 }}"
    )
}

#[test]
fn list_models_returns_the_parsed_list_in_order_and_writes_no_cache() {
    let root = fakes::TempDir::new("fiber-list-models");
    let run = format!("{{ {}, {} }}", model_entry("b"), model_entry("a"));
    let (home, provider) = models_provider(&root, &run);
    let (models, returned) = within({
        let provider = Arc::clone(&provider);
        move || provider.list_models()
    })
    .unwrap();
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["b", "a"]
    );
    assert_eq!(returned[0]["id"], serde_json::json!("b"));
    assert_eq!(returned[1]["id"], serde_json::json!("a"));
    assert!(
        config::read_model_cache(&home, "p").unwrap().is_none(),
        "list_models writes no cache"
    );
}

#[test]
fn list_models_with_a_non_list_return_is_bad_return() {
    let root = fakes::TempDir::new("fiber-list-models-bad");
    let (_home, provider) = models_provider(&root, "\"x\"");
    let err = within({
        let provider = Arc::clone(&provider);
        move || provider.list_models()
    })
    .unwrap_err();
    let crate::Error::BadReturn { callback, .. } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(callback, "p.models");
}

/// A provider `p` whose `credential()` reads its token over HTTP from
/// `url`, failing the fetch on any status but 200, so the test holds and
/// counts the fetches through the server.
fn http_token_provider(root: &fakes::TempDir, url: &str) -> Arc<LuaProvider> {
    let home = root.path().join("home");
    let dir = root.path().join("ext");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("init.lua"),
        format!(
            "fiber.provider(\"p\", {{ credential = {{ timeout = 60000, run = function()\n\
             local reply = host.http({{ url = \"{url}\", method = \"POST\" }})\n\
             if reply.status ~= 200 then error(\"denied\") end\n\
             local got = json.decode(reply.body)\n\
             return {{ token = got.token, expires_at = got.expires_at, headers = got.headers }}\n\
             end }} }})"
        ),
    )
    .unwrap();
    let extension = Arc::new(LuaExtension::new("ext", dir, home, FakeClock::new()));
    LuaProvider::new(extension, "p")
}

#[test]
fn credential_idle_distinguishes_idle_fetching_and_completed() {
    let root = fakes::TempDir::new("fiber-credential-idle");
    let tokens = fakes::ProviderServer::start([token_response("tok-idle")]).unwrap();
    tokens.hold();
    let provider = http_token_provider(&root, &format!("{}/token", tokens.url()));
    let pair = token_pair();
    assert!(!provider.fetching(&pair));
    assert!(provider.await_idle(&pair, Duration::ZERO));
    let other = Arc::clone(&provider);
    let other_pair = pair.clone();
    let (sent, received) = mpsc::channel();
    let fetch = std::thread::spawn(move || {
        drop(sent.send(other.token(&other_pair)));
    });
    assert!(
        tokens.await_requests(1, WAIT),
        "fetch reaches the held server"
    );
    assert!(provider.fetching(&pair));
    assert!(!provider.await_idle(&pair, Duration::ZERO));
    assert!(!provider.await_idle(&pair, Duration::from_millis(10)));
    let (parked_tx, parked_rx) = mpsc::channel();
    *provider.waiting.lock().unwrap() = Some(parked_tx);
    let idle_provider = Arc::clone(&provider);
    let idle_pair = pair.clone();
    let (sent, idle) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let _sent = sent.send(idle_provider.await_idle(&idle_pair, WAIT));
    });
    parked_rx
        .recv_timeout(WAIT)
        .expect("idle waiter reaches its condition wait");
    tokens.release();
    assert_eq!(
        received.recv_timeout(WAIT).unwrap().unwrap().expose(),
        "tok-idle"
    );
    assert!(
        idle.recv_timeout(Duration::from_secs(1))
            .expect("fetch completion wakes the idle waiter")
    );
    fetch.join().unwrap();
    waiter.join().unwrap();
    assert!(!provider.fetching(&pair));
    assert!(provider.await_idle(&pair, Duration::ZERO));
}

#[test]
fn credential_value_returns_the_cached_token_expiry_and_headers() {
    let root = fakes::TempDir::new("fiber-credential-value");
    let tokens = fakes::ProviderServer::start([fakes::Response::status(200, json!({"token": "json-token", "expires_at": 4102444800_u64, "headers": {"x-account": "json-account"}}).to_string())]).unwrap();
    let provider = http_token_provider(&root, &format!("{}/token", tokens.url()));
    let result = within(move || {
        let first = provider.credential_value(&token_pair()).unwrap();
        let second = provider.credential_value(&token_pair()).unwrap();
        (first, second)
    });
    assert_eq!(
        result.0,
        json!({"token": "json-token", "expires_at": 4102444800_u64, "headers": {"x-account": "json-account"}})
    );
    assert_eq!(result.0, result.1);
    assert_eq!(tokens.requests().len(), 1);
}

fn token_pair() -> CredentialPair {
    CredentialPair {
        credential: "p".to_owned(),
        label: "default".to_owned(),
    }
}

/// A token response with this value, expiring far in the future.
fn token_response(value: &str) -> fakes::Response {
    fakes::Response::status(
        200,
        json!({"token": value, "expires_at": 4102444800_u64}).to_string(),
    )
}

/// A second `token()` while one fetch runs waits for it: after the release
/// both callers hold the same token, and `credential()` ran once.
#[test]
fn a_second_token_waits_for_the_fetch_in_flight() {
    let root = fakes::TempDir::new("fiber-credential-wait");
    let tokens = fakes::ProviderServer::start([token_response("tok-1")]).unwrap();
    tokens.hold();
    let provider = http_token_provider(&root, &format!("{}/token", tokens.url()));
    let pair = token_pair();
    let (wait_tx, wait_rx) = mpsc::channel();
    *provider.waiting.lock().unwrap() = Some(wait_tx);
    let (first_tx, first_rx) = mpsc::channel();
    let first_provider = Arc::clone(&provider);
    let first_pair = pair.clone();
    std::thread::spawn(move || {
        let _sent = first_tx.send(first_provider.token(&first_pair));
    });
    assert!(
        tokens.await_requests(1, WAIT),
        "the first fetch reaches the server"
    );
    let (second_tx, second_rx) = mpsc::channel();
    let second_provider = Arc::clone(&provider);
    let second_pair = pair.clone();
    std::thread::spawn(move || {
        let _sent = second_tx.send(second_provider.token(&second_pair));
    });
    // The second caller reached its wait before the first fetch lands.
    wait_rx
        .recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("the second caller waits within {WAIT:?}"));
    tokens.release();
    let first = first_rx
        .recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("the first token returns within {WAIT:?}"))
        .unwrap();
    let second = second_rx
        .recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("the second token returns within {WAIT:?}"))
        .unwrap();
    assert_eq!(first.expose(), "tok-1");
    assert_eq!(second.expose(), "tok-1");
    assert_eq!(
        tokens.requests().len(),
        1,
        "one waiting caller runs no second fetch"
    );
}

/// A second `token()` whose fetch fails fetches for itself: after the
/// release the waiter holds its own fetch's token.
#[test]
fn a_waiter_woken_by_a_failure_fetches_for_itself() {
    let root = fakes::TempDir::new("fiber-credential-retry");
    let tokens =
        fakes::ProviderServer::start([fakes::Response::status(500, "{}"), token_response("tok-2")])
            .unwrap();
    tokens.hold();
    let provider = http_token_provider(&root, &format!("{}/token", tokens.url()));
    let pair = token_pair();
    let (wait_tx, wait_rx) = mpsc::channel();
    *provider.waiting.lock().unwrap() = Some(wait_tx);
    let (first_tx, first_rx) = mpsc::channel();
    let first_provider = Arc::clone(&provider);
    let first_pair = pair.clone();
    std::thread::spawn(move || {
        let _sent = first_tx.send(first_provider.token(&first_pair));
    });
    assert!(
        tokens.await_requests(1, WAIT),
        "the first fetch reaches the server"
    );
    let (second_tx, second_rx) = mpsc::channel();
    let second_provider = Arc::clone(&provider);
    let second_pair = pair.clone();
    std::thread::spawn(move || {
        let _sent = second_tx.send(second_provider.token(&second_pair));
    });
    // The second caller reached its wait before the first fetch lands.
    wait_rx
        .recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("the second caller waits within {WAIT:?}"));
    tokens.release();
    assert!(
        first_rx
            .recv_timeout(WAIT)
            .unwrap_or_else(|_| panic!("the first token returns within {WAIT:?}"))
            .is_err(),
        "the held fetch fails"
    );
    let second = second_rx
        .recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("the second token returns within {WAIT:?}"))
        .unwrap();
    assert_eq!(second.expose(), "tok-2");
    assert_eq!(
        tokens.requests().len(),
        2,
        "the woken waiter runs its own fetch"
    );
}

/// A woken waiter fetches for itself at once, even while another fetch it
/// never waited on is still held: its request reaches the server while the
/// third caller's is still unanswered.
#[test]
fn a_woken_waiter_never_waits_on_a_later_fetch() {
    let root = fakes::TempDir::new("fiber-credential-once");
    let tokens = fakes::ProviderServer::start([
        fakes::Response::status(500, "{}"),
        token_response("tok-3"),
        token_response("tok-2"),
    ])
    .unwrap();
    tokens.hold();
    let provider = http_token_provider(&root, &format!("{}/token", tokens.url()));
    let pair = token_pair();
    let (wait_tx, wait_rx) = mpsc::channel();
    *provider.waiting.lock().unwrap() = Some(wait_tx);
    let (arrived_tx, arrived_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *provider.woke.lock().unwrap() = Some(crate::lua_provider::WokeHook::for_tests(
        arrived_tx, release_rx,
    ));
    // Caller 1 fetches and is held.
    let (first_tx, first_rx) = mpsc::channel();
    let first_provider = Arc::clone(&provider);
    let first_pair = pair.clone();
    std::thread::spawn(move || {
        let _sent = first_tx.send(first_provider.token(&first_pair));
    });
    assert!(
        tokens.await_requests(1, WAIT),
        "the first fetch reaches the server"
    );
    // Caller 2 waits on it.
    let (second_tx, second_rx) = mpsc::channel();
    let second_provider = Arc::clone(&provider);
    let second_pair = pair.clone();
    std::thread::spawn(move || {
        let _sent = second_tx.send(second_provider.token(&second_pair));
    });
    wait_rx
        .recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("the second caller waits within {WAIT:?}"));
    // The first fetch fails; caller 2 wakes and is held before it looks.
    tokens.release_one();
    arrived_rx
        .recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("the woken caller parks within {WAIT:?}"));
    // Caller 3 starts its own fetch while caller 2 is held.
    let (third_tx, third_rx) = mpsc::channel();
    let third_provider = Arc::clone(&provider);
    let third_pair = pair.clone();
    std::thread::spawn(move || {
        let _sent = third_tx.send(third_provider.token(&third_pair));
    });
    assert!(
        tokens.await_requests(2, WAIT),
        "the third caller fetches without waiting on the held waiter"
    );
    // Caller 2 fetches for itself while caller 3's fetch is still held: no
    // release went out since, so the third request is still unanswered.
    release_tx.send(()).unwrap();
    assert!(
        tokens.await_requests(3, WAIT),
        "the woken waiter fetches without waiting on the third caller"
    );
    tokens.release();
    assert!(
        first_rx
            .recv_timeout(WAIT)
            .unwrap_or_else(|_| panic!("the first token returns within {WAIT:?}"))
            .is_err(),
        "the held fetch fails"
    );
    let second = second_rx
        .recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("the second token returns within {WAIT:?}"))
        .unwrap();
    let third = third_rx
        .recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("the third token returns within {WAIT:?}"))
        .unwrap();
    assert_eq!(second.expose(), "tok-2");
    assert_eq!(third.expose(), "tok-3");
    assert_eq!(tokens.requests().len(), 3);
}
