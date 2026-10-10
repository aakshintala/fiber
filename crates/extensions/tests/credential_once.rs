//! `docs/configuration.md`, "Secrets": a credential command runs once per
//! process. Startup discovery asks whether the provider has a credential,
//! and the session reads the key afterwards; both share one run.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]

mod common;

use std::sync::{Arc, mpsc};
use std::time::Duration;

use common::{Setup, install, manifest, provider, write};
use config::{Config, ProjectKey, Sources};
use extensions::{LuaExtension, LuaProvider, Providers};
use serde_json::json;

const WAIT: Duration = Duration::from_secs(10);

fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(f()));
    rx.recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("the call did not return within {WAIT:?}"))
}

#[test]
fn discovery_and_the_session_share_one_run_of_the_command() {
    let setup = Setup::new();
    let marker = setup.root().join("runs");
    let script = format!("echo run >> '{}'; printf key", marker.display());
    let mut data = provider("acme", &["a"]);
    data["credential"] = json!({ "command": ["sh", "-c", script] });
    let source = setup.source("acme", &manifest("acme"), &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (mut providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");

    let ext = setup.home().join("ext");
    write(
        &ext.join("init.lua"),
        "fiber.provider(\"acme\", { models = { timeout = 1000, run = function() \
         return { { id = \"a\", protocol = \"openai-responses\", \
         base_url = \"http://127.0.0.1:1/v1\", context_window = 1000 } } end } })\n",
    );
    let lua: Arc<LuaProvider> = LuaProvider::new(
        Arc::new(LuaExtension::new(
            "acme-ext",
            ext,
            setup.home(),
            fakes::clock::FakeClock::new(),
        )),
        "acme",
    );
    let config = Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();

    // Discovery: no model cache, so `add_lua` asks for the credential.
    let (discovered, config) = within(move || {
        let notices = providers.add_lua("acme-ext", &lua, &config);
        assert!(notices.is_empty(), "{notices:?}");
        (providers, config)
    });
    // The session's own read.
    let key = within(move || {
        let data = discovered.data("acme");
        config.credentials().credential(&data, "default").unwrap()
    });
    assert_eq!(key.expose(), "key");
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1);
}
