//! `docs/configuration.md`, "Secrets": a credential command runs once per
//! process. Startup discovery checks presence without running it, and the
//! session reads the key afterwards; actual discovery and the session share
//! one run.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]

mod common;

use std::sync::Arc;

use common::{Setup, install, manifest, provider, write};
use config::{Config, ProjectKey, Sources};
use extensions::{LuaExtension, LuaProvider, Providers};
use fakes::Deadline;
use serde_json::json;

#[test]
fn discovery_and_the_session_share_one_run_of_the_command() {
    let setup = Setup::new();
    let marker = setup.root().join("runs");
    let script = format!("echo run >> '{}'; printf key", marker.display());
    let mut data = provider("acme", &["a"]);
    data["credential"] = json!({ "command": ["sh", "-c", script] });
    let source = setup.source("acme", &manifest("acme-ext"), &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (mut providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");

    let ext = setup.home().join("ext");
    write(
        &{
            std::fs::create_dir_all((ext.join("providers/acme.lua")).parent().unwrap()).unwrap();
            ext.join("providers/acme.lua")
        },
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

    // Presence checks run nothing: the configured command counts without
    // running, through the configuration and through Lua alike.
    let data = providers.data("acme");
    assert!(config.credentials().has_credential(&data, &|_| None));
    assert!(lua.has_credential(&config, &data));
    assert!(!marker.exists(), "presence checks run no command");
    // Discovery: no model cache, so a present credential lets `add_lua`
    // read the key for real.
    let (discovered, config) = fakes::within(
        "providers.add_lua",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            let notices = providers.add_lua(
                "acme-ext",
                &lua,
                &config,
                &fakes::children::grouped_output,
                &|_| None,
            );
            assert!(notices.is_empty(), "{notices:?}");
            (providers, config)
        },
    );
    // The session's own read.
    let key = fakes::within(
        "credential_with",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            let data = discovered.data("acme");
            config
                .credentials()
                .credential_with(&data, "default", &fakes::children::grouped_output)
                .map(|read| read.secret)
                .unwrap()
        },
    );
    assert_eq!(key.expose(), "key");
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1);
}

#[test]
fn presence_then_a_session_read_runs_the_command_once() {
    let setup = Setup::new();
    let marker = setup.root().join("runs");
    let script = format!("echo run >> '{}'; printf key", marker.display());
    let mut data = provider("acme", &["a"]);
    data["credential"] = json!({ "command": ["sh", "-c", script] });
    let source = setup.source("acme", &manifest("acme-ext"), &[data]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    let config = Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();

    // Presence without discovery runs nothing.
    let data = providers.data("acme");
    assert!(config.credentials().has_credential(&data, &|_| None));
    assert!(!marker.exists(), "presence checks run no command");
    // The session's first real read runs it once, and a repeated read on
    // the same configuration reuses the run.
    let key = config
        .credentials()
        .credential_with(&data, "default", &fakes::children::grouped_output)
        .map(|read| read.secret)
        .unwrap();
    assert_eq!(key.expose(), "key");
    let again = config
        .credentials()
        .credential_with(&data, "default", &fakes::children::grouped_output)
        .map(|read| read.secret)
        .unwrap();
    assert_eq!(again.expose(), "key");
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1);
}
