//! An extension's declared secrets (`docs/configuration.md`, "Secrets";
//! `docs/extensions.md`, "Host calls"): `host.secret` reads only a name the
//! manifest's `secrets` lists, and `Providers` lists every installed
//! extension's declared names for `fiber login`.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::expect_used, reason = "test helpers; a failure is the test's")]

mod common;

use common::write_record;
use fakes::MUST_SUCCEED_WITHIN;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};

use common::{Setup, install_lua, provider, write};
use config::{Config, ProjectKey, Secret, Sources, store_secret};
use contract::events::CallStatus;
use contract::hook::{AfterToolCall, AfterToolOutcome, Hooks};
use extensions::{Providers, SessionExtensions};
use fakes::clock::FakeClock;
use fakes::{Deadline, NoLock};
use serde_json::{Map, Value, json};

/// The string `host.secret` raises for `other.key`, which no manifest here
/// lists.
const UNDECLARED: &str = "host.secret: `other.key` is not in the manifest's `secrets`";

/// The `after_tool` hook under test: `body` runs on the call and its text
/// becomes the new content.
fn hook(body: &str) -> String {
    format!(
        "fiber.hook(\"after_tool\", {{ timeout = 5000, on_failure = \"blocking\",\n\
           run = function(call)\n\
           {body}\n\
         end }})\n"
    )
}

#[track_caller]
fn load(setup: &Setup) -> Arc<SessionExtensions> {
    let config = Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    // On its own thread under `MUST_SUCCEED_WITHIN`: the fake clock never ends a wait the
    // runtime does not end itself.
    let home = setup.home();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
        let clock: Arc<FakeClock> = FakeClock::new();
        let _sent = tx.send(SessionExtensions::load(&home, &config, clock, locks, None));
    });
    Arc::new(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&rx)
            .expect("waited for the extensions to load"),
    )
}

/// The content the hooks return for a completed call, asked on its own
/// thread under `MUST_SUCCEED_WITHIN`.
#[track_caller]
fn content(session: &Arc<SessionExtensions>) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    let session = Arc::clone(session);
    std::thread::spawn(move || {
        let arguments = Map::new();
        let answer = session.after_tool(&AfterToolCall {
            tool: "read",
            arguments: &arguments,
            status: CallStatus::Completed,
            content: "x",
            details: None,
            process: None,
            spilled: None,

            not_regular: false,
            delivery: None,
        });
        let _sent = tx.send(answer);
    });
    let answer = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("waited for the hook");
    match answer.outcome {
        AfterToolOutcome::Changed { content, .. } => content,
        AfterToolOutcome::Unchanged
        | AfterToolOutcome::Withheld { .. }
        | AfterToolOutcome::Failed(_) => None,
    }
}

fn secret(setup: &Setup, name: &str, value: &str) {
    store_secret(&setup.home(), name, &Secret::new(value.to_owned())).unwrap();
}

#[test]
fn a_declared_secret_reads_its_stored_value_and_nil_once_it_is_gone() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "acme",
        &json!({"secrets": ["acme.api_key"]}),
        &hook("return { content = tostring(host.secret(\"acme.api_key\")) }"),
    );
    secret(&setup, "acme.api_key", "sk-acme-1");
    let session = load(&setup);
    assert_eq!(content(&session).as_deref(), Some("sk-acme-1"));
    std::fs::remove_file(setup.home().join("credentials/acme.api_key")).unwrap();
    assert_eq!(content(&session).as_deref(), Some("nil"));
}

#[test]
fn an_undeclared_secret_is_a_lua_error_even_when_it_is_stored() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "acme",
        &json!({"secrets": ["acme.api_key"]}),
        &hook(
            "local ok, err = pcall(host.secret, \"other.key\")\n\
             return { content = tostring(ok) .. \"|\" .. type(err) .. \"|\" .. tostring(err) }",
        ),
    );
    secret(&setup, "acme.api_key", "sk-acme-1");
    secret(&setup, "other.key", "sk-other-1");
    let session = load(&setup);
    let content = content(&session).unwrap();
    assert!(content.starts_with("false|string|"), "{content}");
    assert!(content.ends_with(UNDECLARED), "{content}");
    assert!(!content.contains("sk-other-1"), "{content}");
}

/// Writes `extensions/<dir>/extension.json` straight into Fiber home, with
/// the API version `api` and the declared `secrets`, and returns the
/// extension's directory.
fn placed(setup: &Setup, dir: &str, api: u64, secrets: &[&str]) -> PathBuf {
    let path = setup.home().join("extensions").join(dir);
    write(
        &path.join("extension.json"),
        &json!({ "name": dir, "version": "v1.0.0", "fiber": "0.1.0", "api": api, "secrets": secrets })
            .to_string(),
    );
    write_record(&path);
    path
}

#[test]
fn providers_list_every_installed_extensions_secrets_sorted_and_once() {
    let setup = Setup::new();
    placed(&setup, "a", 1, &["x.key", "shared"]);
    placed(&setup, "b", 1, &["shared"]);
    // Written for another API, so it never loads and declares nothing.
    placed(&setup, "c", 2, &["c.key"]);
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert_eq!(providers.secrets().collect::<Vec<_>>(), ["shared", "x.key"]);
    assert_eq!(notices.len(), 1, "{notices:?}");
}

#[test]
fn an_extension_with_a_provider_and_secrets_lists_both() {
    let setup = Setup::new();
    let dir = placed(&setup, "acme", 1, &["acme.api_key"]);
    write(
        &dir.join("providers/acme.json"),
        &provider("acme", &["m"]).to_string(),
    );
    let manifest_file = dir.join("extension.json");
    let mut installed: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_file).unwrap()).unwrap();
    installed["providers"] = json!({"acme": ["http://127.0.0.1:1/v1"]});
    std::fs::write(&manifest_file, installed.to_string()).unwrap();
    let (providers, _) = Providers::load(&setup.home()).unwrap();
    assert_eq!(providers.names().collect::<Vec<_>>(), ["acme"]);
    assert_eq!(providers.secrets().collect::<Vec<_>>(), ["acme.api_key"]);
}

#[test]
fn declared_secrets_lists_each_owners_rows_by_extension_then_name() {
    let setup = Setup::new();
    placed(&setup, "b", 1, &["shared", "b.key"]);
    placed(&setup, "a", 1, &["x.key", "shared"]);
    let (providers, _) = Providers::load(&setup.home()).unwrap();
    assert_eq!(
        providers.declared_secrets().collect::<Vec<_>>(),
        [
            ("a", "shared"),
            ("a", "x.key"),
            ("b", "b.key"),
            ("b", "shared"),
        ]
    );
    // The flattened list still sorts the shared name once.
    assert_eq!(
        providers.secrets().collect::<Vec<_>>(),
        ["b.key", "shared", "x.key"]
    );
}

#[test]
fn damaged_packages_declare_nothing() {
    let setup = Setup::new();
    let dir = placed(&setup, "d", 1, &["d.key"]);
    write(&dir.join(".fiber.json"), "not json");
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(!notices.is_empty());
    assert_eq!(
        providers.declared_secrets().collect::<Vec<_>>(),
        Vec::<(&str, &str)>::new()
    );
    assert_eq!(providers.secrets().collect::<Vec<_>>(), Vec::<&str>::new());
}

#[test]
fn a_provider_admission_failure_keeps_the_extensions_secrets() {
    let setup = Setup::new();
    let dir = placed(&setup, "acme", 1, &["acme.api_key"]);
    // The file names a provider the manifest does not declare, so none
    // of the extension's providers loads.
    write(
        &dir.join("providers/other.json"),
        &provider("other", &["m"]).to_string(),
    );
    let manifest_file = dir.join("extension.json");
    let mut installed: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_file).unwrap()).unwrap();
    installed["providers"] = json!({"acme": ["http://127.0.0.1:1/v1"]});
    std::fs::write(&manifest_file, installed.to_string()).unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(!notices.is_empty());
    assert_eq!(providers.names().collect::<Vec<_>>(), Vec::<&str>::new());
    assert_eq!(
        providers.declared_secrets().collect::<Vec<_>>(),
        [("acme", "acme.api_key")]
    );
    assert_eq!(providers.secrets().collect::<Vec<_>>(), ["acme.api_key"]);
}

#[test]
fn declared_secrets_follow_scoped_enabling() {
    let setup = Setup::new();
    placed(&setup, "acme", 1, &["acme.api_key"]);
    write(
        &setup.home().join("config.json"),
        r#"{"extensions":{"acme":{"enabled":false}}}"#,
    );
    let configuration = common::config(&setup, &[]);
    let (providers, _) = Providers::load_enabled(&setup.home(), &configuration).unwrap();
    assert_eq!(
        providers.declared_secrets().collect::<Vec<_>>(),
        Vec::<(&str, &str)>::new()
    );
    assert_eq!(providers.secrets().collect::<Vec<_>>(), Vec::<&str>::new());
    write(
        &setup.home().join("projects/p/config.json"),
        r#"{"extensions":{"acme":{"enabled":true}}}"#,
    );
    let configuration = common::config(&setup, &[]);
    let (providers, _) = Providers::load_enabled(&setup.home(), &configuration).unwrap();
    assert_eq!(
        providers.declared_secrets().collect::<Vec<_>>(),
        [("acme", "acme.api_key")]
    );
    assert_eq!(providers.secrets().collect::<Vec<_>>(), ["acme.api_key"]);
}
