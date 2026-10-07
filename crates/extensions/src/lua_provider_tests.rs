#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::panic, reason = "the test's wait deadline is its failure")]

use std::fs;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::signing::{SignRequest, Signer};
use fakes::clock::FakeClock;

use crate::{
    LuaExtension,
    lua_provider::{LuaProvider, LuaSigner},
};

const WAIT: Duration = Duration::from_secs(5);

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

    let with_credential = provider.signer().unwrap().unwrap();
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
        credential: false,
        sign: false,
    };
    assert!(
        without_credential.credentials().is_empty(),
        "a cached token must not be reported by a signer without credential()"
    );
}
