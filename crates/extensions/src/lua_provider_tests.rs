#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::panic, reason = "the test's wait deadline is its failure")]

use std::fs;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::Secret;
use contract::signing::{SignRequest, Signer};
use fakes::clock::FakeClock;

use crate::{
    CredentialPair, LuaExtension,
    lua_provider::{LuaProvider, LuaSigner, redact_values},
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
