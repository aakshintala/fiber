#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::panic, reason = "the test's wait deadline is its failure")]

use fakes::MUST_SUCCEED_WITHIN;
use std::collections::BTreeMap;
use std::fs;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use config::{Config, ProjectKey, Sources};
use contract::Secret;
use contract::signing::{SignRequest, Signer};
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use crate::{
    CredentialPair, LuaExtension, ModelsArg,
    lua_provider::{LuaProvider, LuaSigner, UsedState, parse_credential_headers, redact_values},
};

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

#[test]
fn credentials_is_empty_for_a_signer_without_credentials_even_with_a_cached_token() {
    let root = fakes::TempDir::new("fiber-lua-provider");
    let home = root.path().join("home");
    let dir = root.path().join("ext");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("providers/p.lua"),
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
    let headers = fakes::within(
        "signer.sign",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let signer = Arc::clone(&with_credential);
            move || {
                signer.sign(&SignRequest {
                    method: "POST",
                    url: &url,
                    headers: &[],
                    body: b"{}",
                })
            }
        },
    )
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
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("providers/p.lua"),
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
    let (models, returned) = fakes::within(
        "provider.list_models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let provider = Arc::clone(&provider);
            move || provider.list_models(&ModelsArg::default())
        },
    )
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
    let err = fakes::within(
        "provider.list_models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let provider = Arc::clone(&provider);
            move || provider.list_models(&ModelsArg::default())
        },
    )
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
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("providers/p.lua"),
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
    assert!(provider.await_idle(&pair, Duration::ZERO));
    let other = Arc::clone(&provider);
    let other_pair = pair.clone();
    let (sent, received) = mpsc::channel();
    let fetch = std::thread::spawn(move || {
        drop(sent.send(other.token(&other_pair)));
    });
    assert!(
        tokens.await_requests(1, Deadline::after(MUST_SUCCEED_WITHIN)),
        "fetch reaches the held server"
    );
    assert!(!provider.await_idle(&pair, Duration::ZERO));
    assert!(!provider.await_idle(&pair, Duration::from_millis(10)));
    let (parked_tx, parked_rx) = mpsc::channel();
    *provider.waiting.lock().unwrap() = Some(parked_tx);
    let idle_provider = Arc::clone(&provider);
    let idle_pair = pair.clone();
    let (sent, idle) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let _sent = sent.send(idle_provider.await_idle(&idle_pair, MUST_SUCCEED_WITHIN));
    });
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&parked_rx)
        .expect("idle waiter reaches its condition wait");
    tokens.release();
    assert_eq!(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&received)
            .unwrap()
            .unwrap()
            .expose(),
        "tok-idle"
    );
    assert!(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&idle)
            .expect("fetch completion wakes the idle waiter")
    );
    fetch.join().unwrap();
    waiter.join().unwrap();
    assert!(provider.await_idle(&pair, Duration::ZERO));
}

#[test]
fn credential_value_returns_the_cached_token_expiry_and_headers() {
    let root = fakes::TempDir::new("fiber-credential-value");
    let tokens = fakes::ProviderServer::start([fakes::Response::status(200, json!({"token": "json-token", "expires_at": 4102444800_u64, "headers": {"x-account": "json-account"}}).to_string())]).unwrap();
    let provider = http_token_provider(&root, &format!("{}/token", tokens.url()));
    let result = fakes::within(
        "provider.credential_value",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            let first = provider.credential_value(&token_pair()).unwrap();
            let second = provider.credential_value(&token_pair()).unwrap();
            (first, second)
        },
    );
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
        tokens.await_requests(1, Deadline::after(MUST_SUCCEED_WITHIN)),
        "the first fetch reaches the server"
    );
    let (second_tx, second_rx) = mpsc::channel();
    let second_provider = Arc::clone(&provider);
    let second_pair = pair.clone();
    std::thread::spawn(move || {
        let _sent = second_tx.send(second_provider.token(&second_pair));
    });
    // The second caller reached its wait before the first fetch lands.
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&wait_rx)
        .unwrap_or_else(|_| panic!("the second caller waits within {MUST_SUCCEED_WITHIN:?}"));
    tokens.release();
    let first = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&first_rx)
        .unwrap_or_else(|_| panic!("the first token returns within {MUST_SUCCEED_WITHIN:?}"))
        .unwrap();
    let second = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&second_rx)
        .unwrap_or_else(|_| panic!("the second token returns within {MUST_SUCCEED_WITHIN:?}"))
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
        tokens.await_requests(1, Deadline::after(MUST_SUCCEED_WITHIN)),
        "the first fetch reaches the server"
    );
    let (second_tx, second_rx) = mpsc::channel();
    let second_provider = Arc::clone(&provider);
    let second_pair = pair.clone();
    std::thread::spawn(move || {
        let _sent = second_tx.send(second_provider.token(&second_pair));
    });
    // The second caller reached its wait before the first fetch lands.
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&wait_rx)
        .unwrap_or_else(|_| panic!("the second caller waits within {MUST_SUCCEED_WITHIN:?}"));
    tokens.release();
    assert!(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&first_rx)
            .unwrap_or_else(|_| panic!("the first token returns within {MUST_SUCCEED_WITHIN:?}"))
            .is_err(),
        "the held fetch fails"
    );
    let second = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&second_rx)
        .unwrap_or_else(|_| panic!("the second token returns within {MUST_SUCCEED_WITHIN:?}"))
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
        tokens.await_requests(1, Deadline::after(MUST_SUCCEED_WITHIN)),
        "the first fetch reaches the server"
    );
    // Caller 2 waits on it.
    let (second_tx, second_rx) = mpsc::channel();
    let second_provider = Arc::clone(&provider);
    let second_pair = pair.clone();
    std::thread::spawn(move || {
        let _sent = second_tx.send(second_provider.token(&second_pair));
    });
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&wait_rx)
        .unwrap_or_else(|_| panic!("the second caller waits within {MUST_SUCCEED_WITHIN:?}"));
    // The first fetch fails; caller 2 wakes and is held before it looks.
    tokens.release_one();
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&arrived_rx)
        .unwrap_or_else(|_| panic!("the woken caller parks within {MUST_SUCCEED_WITHIN:?}"));
    // Caller 3 starts its own fetch while caller 2 is held.
    let (third_tx, third_rx) = mpsc::channel();
    let third_provider = Arc::clone(&provider);
    let third_pair = pair.clone();
    std::thread::spawn(move || {
        let _sent = third_tx.send(third_provider.token(&third_pair));
    });
    assert!(
        tokens.await_requests(2, Deadline::after(MUST_SUCCEED_WITHIN)),
        "the third caller fetches without waiting on the held waiter"
    );
    // Caller 2 fetches for itself while caller 3's fetch is still held: no
    // release went out since, so the third request is still unanswered.
    release_tx.send(()).unwrap();
    assert!(
        tokens.await_requests(3, Deadline::after(MUST_SUCCEED_WITHIN)),
        "the woken waiter fetches without waiting on the third caller"
    );
    tokens.release();
    assert!(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&first_rx)
            .unwrap_or_else(|_| panic!("the first token returns within {MUST_SUCCEED_WITHIN:?}"))
            .is_err(),
        "the held fetch fails"
    );
    let second = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&second_rx)
        .unwrap_or_else(|_| panic!("the second token returns within {MUST_SUCCEED_WITHIN:?}"))
        .unwrap();
    let third = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&third_rx)
        .unwrap_or_else(|_| panic!("the third token returns within {MUST_SUCCEED_WITHIN:?}"))
        .unwrap();
    assert_eq!(second.expose(), "tok-2");
    assert_eq!(third.expose(), "tok-3");
    assert_eq!(tokens.requests().len(), 3);
}

/// Provider data naming no credential source: only a stored key could
/// serve it, and the test home stores none.
fn credentialless_data() -> config::ProviderData {
    config::ProviderData {
        name: "p".to_owned(),
        credential_from: config::CredentialFrom::Stored {
            credential: None,
            login: None,
        },
        credential_name: None,
        headers: BTreeMap::new(),
        placeholders: BTreeMap::new(),
        models: Vec::new(),
        reviewer_model: None,
    }
}

#[test]
fn has_credential_needs_a_registered_credential_or_a_stored_key() {
    let root = fakes::TempDir::new("fiber-has-credential");
    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let run = format!("{{ {} }}", model_entry("a"));
    let (home, bare) = models_provider(&root, &run);
    let config = Config::load(Sources {
        home: home.clone(),
        workspace,
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    let data = credentialless_data();
    // No `credential` function and no key source: nothing to refresh with.
    assert!(
        !fakes::within(
            "has_credential",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            {
                let provider = Arc::clone(&bare);
                let config = config.clone();
                let data = data.clone();
                move || provider.has_credential(&config, &data, &fakes::children::grouped_output)
            }
        ),
        "a provider with no credential and no stored key has none"
    );
    // A registered `credential` is one whatever the session stores.
    let dir = root.path().join("ext-cred");
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("providers/p.lua"),
        "fiber.provider(\"p\", { credential = { timeout = 5000, run = function() \
         return { token = \"tok-1\", expires_at = 4102444800 } end } })",
    )
    .unwrap();
    let extension = Arc::new(LuaExtension::new("ext", dir, home, FakeClock::new()));
    let credentialed = LuaProvider::new(extension, "p");
    assert!(
        fakes::within(
            "has_credential",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            {
                let config = config.clone();
                let data = data.clone();
                move || {
                    credentialed.has_credential(&config, &data, &fakes::children::grouped_output)
                }
            }
        ),
        "a provider registering credential has one"
    );
}
