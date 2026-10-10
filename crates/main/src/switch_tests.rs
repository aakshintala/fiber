//! `prepare` on a `Switching` built as `parts_in` builds it
//! (`docs/model-routing.md`, "Naming a model" and "Thinking"), reading a
//! credential or starting a Lua provider the switch needs
//! (`docs/configuration.md`, "Secrets").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use crate::test_support::install_extension;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use config::Config;
use contract::commands::ModelArgs;
use contract::{ErrorCode, ThinkingLevel};
use extensions::{LuaProvider, Providers};
use serde_json::json;

use super::{Credentials, Door, Loader, Switching, hosted_stands, prepare};

/// The deadline of each preparation: one may start a Lua provider or run a
/// credential command.
const DEADLINE: Duration = Duration::from_secs(20);

/// The fixture home, workspace and config: `fake` with `m`, `n` (low and
/// high, defaulting low, with an addendum) and `r`; `claude` with the
/// hosted-search model `w`; `other` with `m` and `m2` behind a `command`
/// credential that appends a line to `marker`; `bad` with `m` behind a
/// `command` that appends to `bad_marker` and fails; `filed` with `m`
/// behind the `file` source `key_file`, absent at first; `bed` with a
/// bedrock model behind the `command` source `other` uses.
struct Fixture {
    root: fakes::TempDir,
    home: PathBuf,
    workspace: PathBuf,
    marker: PathBuf,
    bad_marker: PathBuf,
    key_file: PathBuf,
}

/// The lines `marker` holds: one per credential command run.
fn runs(marker: &Path) -> usize {
    std::fs::read_to_string(marker).map_or(0, |text| text.lines().count())
}

/// `sh -c <script>` as a credential source's argv.
fn sh(script: &str) -> serde_json::Value {
    json!(["sh", "-c", script])
}

/// Writes the data-only extension `name` whose provider data is `data`.
fn data_extension(home: &Path, name: &str, data: &serde_json::Value) {
    install_extension(
        home,
        &format!("extensions/{name}"),
        json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        &[(name, data.clone())],
    );
}

fn fixture(name: &str) -> Fixture {
    let root = fakes::TempDir::new(name);
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let marker = root.path().join("credential-ran");
    let bad_marker = root.path().join("bad-ran");
    let key_file = root.path().join("keys/filed");
    let fake = home.join("extensions/fake");
    install_extension(
        &home,
        "extensions/fake",
        json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        &[(
            "fake",
            json!({
                "name": "fake",
                "models": [
                    {"id": "m", "protocol": "openai-responses",
                     "base_url": "http://127.0.0.1:9/v1", "context_window": 100000},
                    {"id": "n", "protocol": "openai-responses",
                     "base_url": "http://127.0.0.1:9/v1", "context_window": 1000,
                     "thinking_levels": ["low", "high"], "thinking_default": "low",
                     "prompt_addendum": "extra.md"},
                    {"id": "r", "protocol": "openai-responses",
                     "base_url": "http://127.0.0.1:9/v1", "context_window": 1000},
                ],
            }),
        )],
    );
    std::fs::write(fake.join("extra.md"), "The extra paragraph.\n").unwrap();
    install_extension(
        &home,
        "extensions/claude",
        json!({"name": "claude", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        &[(
            "claude",
            json!({
                "name": "claude",
                "models": [
                    {"id": "w", "protocol": "anthropic-messages",
                     "base_url": "http://127.0.0.1:9/v1", "context_window": 500,
                     "web_search": "web_search_20250305"},
                ],
            }),
        )],
    );
    install_extension(
        &home,
        "extensions/other",
        json!({"name": "other", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        &[(
            "other",
            json!({
                "name": "other",
                "credential": {"command": sh(&format!(
                    "echo x >> '{}'; echo other-key", marker.display()
                ))},
                "models": [
                    {"id": "m", "protocol": "openai-responses",
                     "base_url": "http://127.0.0.1:9/v1", "context_window": 1000},
                    {"id": "m2", "protocol": "openai-responses",
                     "base_url": "http://127.0.0.1:9/v1", "context_window": 1000,
                     "thinking_levels": ["low"], "thinking_default": "low"},
                ],
            }),
        )],
    );
    install_extension(
        &home,
        "extensions/bed",
        json!({"name": "bed", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        &[(
            "bed",
            json!({
                "name": "bed",
                "credential": {"command": sh(&format!(
                    "echo x >> '{}'; echo bed-key", marker.display()
                ))},
                "models": [
                    {"id": "bk", "protocol": "bedrock-converse",
                     "base_url": "http://127.0.0.1:9/v1", "context_window": 1000},
                ],
            }),
        )],
    );
    data_extension(
        &home,
        "bad",
        &json!({
            "name": "bad",
            "credential": {"command": sh(&format!(
                "echo x >> '{}'; exit 1", bad_marker.display()
            ))},
            "models": [{"id": "bm", "protocol": "openai-responses",
                        "base_url": "http://127.0.0.1:9/v1", "context_window": 1000}],
        }),
    );
    data_extension(
        &home,
        "filed",
        &json!({
            "name": "filed",
            "credential": {"file": key_file},
            "models": [{"id": "fm", "protocol": "openai-responses",
                        "base_url": "http://127.0.0.1:9/v1", "context_window": 1000}],
        }),
    );
    Fixture {
        root,
        home,
        workspace,
        marker,
        bad_marker,
        key_file,
    }
}

fn config(fixture: &Fixture, overrides: &[&str]) -> Config {
    crate::test_support::load(&fixture.home, &fixture.workspace, "test", overrides)
}

fn keyed(label: &str) -> (String, crate::lua_providers::KeyAndSigner) {
    (
        label.to_owned(),
        (Some(contract::Secret::new("k1".to_owned())), None),
    )
}

/// The startup credential map holding each of `names` under `default`.
fn startup(names: &[&str]) -> Credentials {
    names
        .iter()
        .map(|name| ((*name).to_owned(), keyed("default")))
        .collect()
}

/// The `Switching` over the fixture with `overrides` and the credential
/// map holding `fake` and `claude` under `default`, as `parts_in` builds
/// it.
fn switching(fixture: &Fixture, overrides: &[&str]) -> Arc<Switching> {
    assembled(
        &fixture.home,
        config(fixture, overrides),
        startup(&["fake", "claude"]),
        &[],
    )
}

/// `Switching` as `parts_in` builds it over `home`: the registry after
/// `add_lua`, every Lua provider's extension, and `keep`'s Lua providers
/// loaded, `SessionExtensions` holding none.
fn assembled(
    home: &Path,
    config: Config,
    credentials: Credentials,
    keep: &[&str],
) -> Arc<Switching> {
    let (mut providers, _) = Providers::load(home).unwrap();
    let mut extensions = extensions::SessionExtensions::load(
        home,
        &config,
        fakes::clock::FakeClock::new(),
        Arc::new(tools::PathLocks::new()),
        None,
    );
    let naming = crate::lua_providers::add_lua(&extensions, &mut providers, &config).unwrap();
    let owners = (extensions.lua_providers().iter())
        .map(|(extension, lua)| (lua.name().to_owned(), extension.clone()))
        .collect();
    extensions.retain_lua_providers(keep);
    let loaded = (extensions.lua_providers().iter())
        .map(|(_, lua)| (lua.name().to_owned(), Arc::clone(lua)))
        .collect();
    extensions.retain_lua_providers(&[]);
    Arc::new(Switching::new(
        providers,
        naming,
        config,
        credentials,
        loaded,
        loader(Arc::new(extensions), owners),
    ))
}

fn loader(
    extensions: Arc<extensions::SessionExtensions>,
    owners: BTreeMap<String, String>,
) -> Loader {
    Loader {
        extensions,
        clock: fakes::clock::FakeClock::new(),
        locks: Arc::new(tools::PathLocks::new()),
        owners,
        workspace: PathBuf::new(),
    }
}

/// `Switching` over a registry and naming list a test made by hand, with
/// placeholders filled as `add_lua` fills them and no Lua provider.
fn over(
    home: &Path,
    mut providers: Providers,
    naming: Vec<(String, String)>,
    config: Config,
    credentials: Credentials,
) -> Arc<Switching> {
    let _notices = providers
        .fill_placeholders(&config, &|name| std::env::var(name).ok())
        .unwrap();
    let extensions = extensions::SessionExtensions::load(
        home,
        &config,
        fakes::clock::FakeClock::new(),
        Arc::new(tools::PathLocks::new()),
        None,
    );
    Arc::new(Switching::new(
        providers,
        naming,
        config,
        credentials,
        Vec::new(),
        loader(Arc::new(extensions), BTreeMap::new()),
    ))
}

/// The provider names `switching` holds a key for.
fn keys(switching: &Switching) -> Vec<String> {
    let remembered = switching.remembered.lock().unwrap();
    let mut names: Vec<String> = remembered
        .keys
        .keys()
        .map(|(name, _)| name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The label `switching` holds `provider` on, if any.
fn selected(switching: &Switching, provider: &str) -> Option<String> {
    switching
        .remembered
        .lock()
        .unwrap()
        .selected
        .get(provider)
        .cloned()
}

/// The provider names `switching` holds loaded.
fn loaded(switching: &Switching) -> Vec<String> {
    switching.loaded.lock().unwrap().keys().cloned().collect()
}

fn args(model: &str) -> ModelArgs {
    ModelArgs {
        model: model.into(),
        thinking: None,
    }
}

/// A door whose `tools` answer goes nowhere.
fn quiet() -> Door {
    Door {
        declare: Arc::new(|_, _| {}),
        hosted_stands: false,
        backend: None,
    }
}

/// A door recording every `tools` change, by name and hosted type.
type Declared = Arc<std::sync::Mutex<Vec<(String, Option<String>)>>>;

fn recording(hosted_stands: bool) -> (Door, Declared) {
    let declared: Declared = Arc::default();
    let seen = Arc::clone(&declared);
    let door = Door {
        declare: Arc::new(move |name, info| {
            seen.lock()
                .unwrap()
                .push((name.to_owned(), info.map(|info| info.name)));
        }),
        hosted_stands,
        backend: None,
    };
    (door, declared)
}

/// The tool a `Declare` registers, its registrant left to the loop.
fn declared_type(hosted: &r#loop::Hosted) -> Option<Option<String>> {
    match hosted {
        r#loop::Hosted::Declare(tool) => {
            let definition = tool.definition();
            assert_eq!(definition.name, "web_search");
            Some(definition.hosted)
        }
        r#loop::Hosted::Keep | r#loop::Hosted::Withdraw(_) => None,
    }
}

/// `prepare` on its own thread under [`DEADLINE`], keeping the provider's
/// selected label.
fn bounded(
    switching: &Arc<Switching>,
    args: &ModelArgs,
    chosen: Option<ThinkingLevel>,
) -> Result<r#loop::Prepared, contract::inbox::Rejection> {
    let (switching, args) = (Arc::clone(switching), args.clone());
    fakes::within("the preparation", DEADLINE, move || {
        prepare(&switching, &quiet(), &args, None, chosen)
    })
}

/// `prepare` on its own thread under [`DEADLINE`], switching to `label`.
fn bounded_with(
    switching: &Arc<Switching>,
    args: &ModelArgs,
    label: Option<&str>,
    chosen: Option<ThinkingLevel>,
) -> Result<r#loop::Prepared, contract::inbox::Rejection> {
    let (switching, args, label) = (
        Arc::clone(switching),
        args.clone(),
        label.map(str::to_owned),
    );
    fakes::within("the preparation", DEADLINE, move || {
        prepare(&switching, &quiet(), &args, label.as_deref(), chosen)
    })
}

/// A prepared switch: `prepare` succeeds, and `Prepared` is no `Debug`, so
/// no `unwrap`.
fn prepared(
    switching: &Arc<Switching>,
    args: &ModelArgs,
    chosen: Option<ThinkingLevel>,
) -> r#loop::Prepared {
    match bounded(switching, args, chosen) {
        Ok(prepared) => prepared,
        Err(rejection) => panic!("the switch rejected: {}", rejection.message),
    }
}

/// A prepared switch to `label`: `prepare` succeeds, and `Prepared` is no
/// `Debug`, so no `unwrap`.
fn prepared_with(
    switching: &Arc<Switching>,
    args: &ModelArgs,
    label: Option<&str>,
    chosen: Option<ThinkingLevel>,
) -> r#loop::Prepared {
    match bounded_with(switching, args, label, chosen) {
        Ok(prepared) => prepared,
        Err(rejection) => panic!("the switch rejected: {}", rejection.message),
    }
}

/// A rejected switch: `prepare` fails, and `Prepared` is no `Debug`, so no
/// `unwrap_err`.
fn rejected(
    switching: &Arc<Switching>,
    args: &ModelArgs,
    chosen: Option<ThinkingLevel>,
) -> contract::inbox::Rejection {
    match bounded(switching, args, chosen) {
        Ok(_) => panic!("the switch prepared"),
        Err(rejection) => rejection,
    }
}

/// A rejected switch to `label`: `prepare` fails, and `Prepared` is no
/// `Debug`, so no `unwrap_err`.
fn rejected_with(
    switching: &Arc<Switching>,
    args: &ModelArgs,
    label: Option<&str>,
    chosen: Option<ThinkingLevel>,
) -> contract::inbox::Rejection {
    match bounded_with(switching, args, label, chosen) {
        Ok(_) => panic!("the switch prepared"),
        Err(rejection) => rejection,
    }
}

/// A failed reviewer: the loop gets it, and `Reviewer` is no `Debug`, so no
/// `expect_err`.
fn reviewer_failed(
    reviewer: Result<r#loop::Reviewer, contract::shapes::Failure>,
) -> contract::shapes::Failure {
    match reviewer {
        Ok(_) => panic!("the reviewer resolved"),
        Err(failure) => failure,
    }
}

fn args_thinking(model: &str, thinking: &str) -> ModelArgs {
    ModelArgs {
        model: model.into(),
        thinking: Some(thinking.into()),
    }
}

#[test]
fn the_session_and_reviewers_providers_resolve_with_the_maps_label() {
    let fixture = fixture("fiber-switch-session");
    let switching = switching(&fixture, &["reviewer.model=fake/r"]);
    let made = prepared(&switching, &args("fake/n"), None);
    assert_eq!(made.model.reference, "fake/n");
    assert_eq!(made.credential, Some("default".to_owned()));
    assert_eq!(made.thinking, Some(ThinkingLevel::Low));
    let reviewer = made.reviewer.expect("the reviewer resolved");
    assert_eq!(reviewer.model.reference, "fake/r");
}

/// A provider outside the startup map reads its `command` source when the
/// switch is prepared, once per process: a second switch to it runs
/// nothing.
#[test]
fn another_installed_provider_reads_its_command_once() {
    let fixture = fixture("fiber-switch-outside");
    let switching = switching(&fixture, &[]);
    let made = prepared(&switching, &args("other/m"), None);
    assert_eq!(made.model.reference, "other/m");
    assert_eq!(made.credential, Some("default".to_owned()));
    assert!(made.credential_files.is_empty());
    assert_eq!(runs(&fixture.marker), 1);
    assert!(keys(&switching).contains(&"other".to_owned()));
    prepared(&switching, &args("other/m"), None);
    assert_eq!(
        runs(&fixture.marker),
        1,
        "a key read once is not read again"
    );
}

/// A failed read rejects with the credential's own code, names the
/// command by its program alone, and caches nothing: the next switch reads
/// again.
#[test]
fn a_failing_command_rejects_with_its_code_and_caches_nothing() {
    let fixture = fixture("fiber-switch-bad");
    let switching = switching(&fixture, &[]);
    let rejection = rejected(&switching, &args("bad/bm"), None);
    assert_eq!(rejection.code, ErrorCode::CredentialMissing);
    assert!(rejection.message.contains("`sh`"), "{}", rejection.message);
    assert!(
        !rejection.message.contains("exit 1"),
        "{}",
        rejection.message
    );
    assert_eq!(runs(&fixture.bad_marker), 1);
    assert!(!keys(&switching).contains(&"bad".to_owned()));
    rejected(&switching, &args("bad/bm"), None);
    assert_eq!(runs(&fixture.bad_marker), 2, "a failed read is read again");
}

/// A missing `file` source rejects `credential_missing`; once the file
/// exists the switch reads it, and names the file it read for the deny. A
/// key read before adds no file.
#[test]
fn a_missing_file_rejects_until_the_file_exists() {
    let fixture = fixture("fiber-switch-file");
    let switching = switching(&fixture, &[]);
    let rejection = rejected(&switching, &args("filed/fm"), None);
    assert_eq!(rejection.code, ErrorCode::CredentialMissing);
    assert!(
        rejection.message.contains("does not exist"),
        "{}",
        rejection.message
    );
    assert!(!keys(&switching).contains(&"filed".to_owned()));
    std::fs::create_dir_all(fixture.key_file.parent().unwrap()).unwrap();
    std::fs::write(&fixture.key_file, "sk-filed").unwrap();
    let made = prepared(&switching, &args("filed/fm"), None);
    assert_eq!(
        made.credential_files,
        vec![fixture.key_file.canonicalize().unwrap()]
    );
    let again = prepared(&switching, &args("filed/fm"), None);
    assert!(
        again.credential_files.is_empty(),
        "a cached key reads no file"
    );
}

/// A `file` source behind a symlink names the link's target: the file
/// whose bytes were read.
#[test]
fn a_file_behind_a_symlink_names_its_target() {
    let fixture = fixture("fiber-switch-file-link");
    let target = fixture.root.path().join("keys/target");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, "sk-target").unwrap();
    std::os::unix::fs::symlink(&target, &fixture.key_file).unwrap();
    let switching = switching(&fixture, &[]);
    let made = prepared(&switching, &args("filed/fm"), None);
    assert_eq!(made.credential_files, vec![target.canonicalize().unwrap()]);
}

/// Every check that can reject runs before the read: an unsupported
/// level, an unparseable one, an unknown model, a protocol this Fiber does
/// not speak, and the new model being the reviewer's. None runs the
/// `command`, and none caches a key.
#[test]
fn every_rejection_comes_before_the_read() {
    let fixture = fixture("fiber-switch-pre-read");
    let switching = switching(&fixture, &["reviewer.model=other/m"]);
    let cases = [
        (args_thinking("other/m2", "high"), "unsupported level"),
        (args_thinking("other/m", "sideways"), "unparseable level"),
        (args("other/x"), "unknown model"),
        (args("bed/bk"), "bedrock-converse"),
        (args("other/m"), "the reviewer's model"),
    ];
    for (asked, case) in &cases {
        let rejection = rejected(&switching, asked, None);
        assert_eq!(rejection.code, ErrorCode::InvalidArguments, "{case}");
    }
    let collision = rejected(&switching, &args("other/m"), None);
    assert_eq!(
        collision.message,
        "`other/m` is this session's reviewer model; set `reviewer.model` to another model first."
    );
    assert_eq!(runs(&fixture.marker), 0, "no read ran");
    assert_eq!(
        keys(&switching),
        vec!["claude".to_owned(), "fake".to_owned()]
    );
}

/// A reviewer on the new session provider reuses the session's read: the
/// `command` runs once.
#[test]
fn a_reviewer_on_the_new_provider_runs_no_second_read() {
    let fixture = fixture("fiber-switch-reviewer-same");
    let switching = switching(&fixture, &["reviewer.model=other/m2"]);
    let made = prepared(&switching, &args("other/m"), None);
    let reviewer = made.reviewer.expect("the reviewer resolved");
    assert_eq!(reviewer.model.reference, "other/m2");
    assert_eq!(runs(&fixture.marker), 1);
}

/// The startup provider's key is in the map, so a switch back to it runs
/// nothing.
#[test]
fn the_startup_providers_command_does_not_run_again() {
    let fixture = fixture("fiber-switch-startup");
    let switching = assembled(
        &fixture.home,
        config(&fixture, &[]),
        startup(&["fake", "claude", "other"]),
        &[],
    );
    let made = prepared(&switching, &args("other/m"), None);
    assert_eq!(made.model.reference, "other/m");
    assert_eq!(runs(&fixture.marker), 0);
}

#[test]
fn unknown_models_are_invalid_arguments() {
    let fixture = fixture("fiber-switch-unknown");
    let switching = switching(&fixture, &[]);
    for typed in ["fake/x", "nope/x", "x", ""] {
        let rejection = rejected(&switching, &args(typed), None);
        assert_eq!(rejection.code, ErrorCode::InvalidArguments, "{typed}");
    }
    let missing = rejected(&switching, &args("fake/x"), None);
    assert!(
        missing.message.contains("has no model"),
        "{}",
        missing.message
    );
}

#[test]
fn an_ambiguous_bare_id_lists_both_matches() {
    // `fake/m` and `other/m` share the id.
    let fixture = fixture("fiber-switch-ambiguous");
    let switching = switching(&fixture, &[]);
    let rejection = rejected(&switching, &args("m"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert!(
        rejection.message.contains("fake/m"),
        "{}",
        rejection.message
    );
    assert!(
        rejection.message.contains("other/m"),
        "{}",
        rejection.message
    );
}

#[test]
fn the_naming_list_counts_an_unloaded_lua_provider() {
    let fixture = fixture("fiber-switch-naming");
    let config = config(&fixture, &[]);
    let (providers, _) = Providers::load(&fixture.home).unwrap();
    let credentials: Credentials = [("fake", keyed("default"))]
        .into_iter()
        .map(|(name, entry)| (name.to_owned(), entry))
        .collect();
    // `lua` is unloaded: in the naming list only, sharing `m` with `fake`.
    let naming = vec![
        ("fake".to_owned(), "m".to_owned()),
        ("lua".to_owned(), "m".to_owned()),
    ];
    let switching = over(&fixture.home, providers, naming, config, credentials);
    let rejection = rejected(&switching, &args("m"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert!(
        rejection.message.contains("fake/m"),
        "{}",
        rejection.message
    );
    assert!(rejection.message.contains("lua/m"), "{}", rejection.message);
    // A provider the registry does not hold is an unknown model.
    let unknown = rejected(&switching, &args("lua/m"), None);
    assert_eq!(unknown.code, ErrorCode::InvalidArguments);
    assert!(unknown.message.contains("lua"), "{}", unknown.message);
    // An exact reference is never ambiguous.
    assert_eq!(
        prepared(&switching, &args("fake/m"), None).model.reference,
        "fake/m"
    );
    // The union is exact: no duplicate when the registry and the naming
    // list name the same reference. The fixture registry already holds
    // `other/m`, so all three list.
    assert_eq!(
        rejected(&switching, &args("m"), None).message,
        "The model `m` is offered by more than one provider: fake/m, lua/m, other/m. \
         Name one as `provider/model`."
    );
}

#[test]
fn a_bare_id_only_the_naming_list_names_is_invalid_arguments() {
    let fixture = fixture("fiber-switch-bare-unloaded");
    let config = config(&fixture, &[]);
    let (providers, _) = Providers::load(&fixture.home).unwrap();
    // Only `lua`, which the registry does not hold, names `rm`.
    let naming = vec![("lua".to_owned(), "rm".to_owned())];
    let switching = over(&fixture.home, providers, naming, config, startup(&["fake"]));
    let rejection = rejected(&switching, &args("rm"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert!(rejection.message.contains("rm"), "{}", rejection.message);
}

#[test]
fn a_naming_match_joins_a_resolved_match_in_ambiguity() {
    // The registry resolves bare `n` to `fake/n`; the naming list adds the
    // unloaded `lua/n`.
    let fixture = fixture("fiber-switch-resolved-naming");
    let config = config(&fixture, &[]);
    let (providers, _) = Providers::load(&fixture.home).unwrap();
    let credentials: Credentials = [("fake", keyed("default"))]
        .into_iter()
        .map(|(name, entry)| (name.to_owned(), entry))
        .collect();
    let naming = vec![
        ("fake".to_owned(), "n".to_owned()),
        ("lua".to_owned(), "n".to_owned()),
    ];
    let switching = over(&fixture.home, providers, naming, config, credentials);
    let rejection = rejected(&switching, &args("n"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert_eq!(
        rejection.message,
        "The model `n` is offered by more than one provider: fake/n, lua/n. \
         Name one as `provider/model`."
    );
}

/// Adds extension `lit` naming `n` (low and high, defaulting low) and the
/// literal `n:high`: a model id ending in a recognised thinking level.
fn with_literal_provider(fixture: &Fixture) {
    install_extension(
        &fixture.home,
        "extensions/lit",
        json!({"name": "lit", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        &[(
            "lit",
            json!({
                "name": "lit",
                "models": [
                    {"id": "n", "protocol": "openai-responses",
                     "base_url": "http://127.0.0.1:9/v1", "context_window": 1000,
                     "thinking_levels": ["low", "high"], "thinking_default": "low"},
                    {"id": "n:high", "protocol": "openai-responses",
                     "base_url": "http://127.0.0.1:9/v1", "context_window": 1000},
                ],
            }),
        )],
    );
}

#[test]
fn a_literal_id_ending_in_a_thinking_level_matches_before_the_suffix() {
    let fixture = fixture("fiber-switch-literal");
    with_literal_provider(&fixture);
    let config = config(&fixture, &[]);
    let (providers, _) = Providers::load(&fixture.home).unwrap();
    let credentials: Credentials = [("lit", keyed("default"))]
        .into_iter()
        .map(|(name, entry)| (name.to_owned(), entry))
        .collect();
    let naming = vec![
        ("lit".to_owned(), "n".to_owned()),
        ("lit".to_owned(), "n:high".to_owned()),
    ];
    let switching = over(&fixture.home, providers, naming, config, credentials);
    // `n:high` selects the literal model, with no thinking level: it is
    // neither `n` with `high` nor ambiguous with it.
    let made = prepared(&switching, &args("n:high"), None);
    assert_eq!(made.model.reference, "lit/n:high");
    assert_eq!(made.thinking, None);
    // The suffix still applies to `n` itself (`fake/n` shares the bare
    // id, so the suffix case names `lit/n` exactly).
    let made = prepared(&switching, &args_thinking("lit/n", "high"), None);
    assert_eq!(made.model.reference, "lit/n");
    assert_eq!(made.thinking, Some(ThinkingLevel::High));
}

#[test]
fn an_unloaded_literal_id_counts_toward_ambiguity() {
    let fixture = fixture("fiber-switch-literal-ambiguity");
    with_literal_provider(&fixture);
    let config = config(&fixture, &[]);
    let (providers, _) = Providers::load(&fixture.home).unwrap();
    let credentials: Credentials = [("lit", keyed("default"))]
        .into_iter()
        .map(|(name, entry)| (name.to_owned(), entry))
        .collect();
    // The unloaded `lua` names the same literal `n:high`.
    let naming = vec![
        ("lit".to_owned(), "n".to_owned()),
        ("lit".to_owned(), "n:high".to_owned()),
        ("lua".to_owned(), "n:high".to_owned()),
    ];
    let switching = over(&fixture.home, providers, naming, config, credentials);
    let rejection = rejected(&switching, &args("n:high"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert_eq!(
        rejection.message,
        "The model `n:high` is offered by more than one provider: lit/n:high, lua/n:high. \
         Name one as `provider/model`."
    );
}

#[test]
fn a_literal_only_the_naming_list_names_falls_back_to_the_suffix() {
    let fixture = fixture("fiber-switch-literal-unloaded");
    let config = config(&fixture, &[]);
    let (providers, _) = Providers::load(&fixture.home).unwrap();
    // Only `lua`, which the registry does not hold, names the literal
    // `n:high`, so `fake/n` takes the `high` suffix.
    let naming = vec![
        ("fake".to_owned(), "n".to_owned()),
        ("lua".to_owned(), "n:high".to_owned()),
    ];
    let switching = over(&fixture.home, providers, naming, config, startup(&["fake"]));
    let made = prepared(&switching, &args("n:high"), None);
    assert_eq!(made.model.reference, "fake/n");
    assert_eq!(made.thinking, Some(ThinkingLevel::High));
}

#[test]
fn an_unconfigured_literal_id_beats_stripped_id_ambiguity() {
    let fixture = fixture("fiber-switch-unconfigured-literal");
    install_extension(
        &fixture.home,
        "extensions/lit",
        json!({"name": "lit", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        &[(
            "lit",
            json!({
                "name": "lit",
                "placeholders": {"workspace": {}},
                "models": [
                    {"id": "n", "protocol": "openai-responses",
                     "base_url": "http://127.0.0.1:9/v1", "context_window": 1000},
                    {"id": "n:high", "protocol": "openai-responses",
                     "base_url": "https://{workspace}/v1", "context_window": 1000},
                ],
            }),
        )],
    );
    let config = config(&fixture, &[]);
    let (providers, _) = Providers::load(&fixture.home).unwrap();
    let credentials: Credentials = [("fake", keyed("default")), ("lit", keyed("default"))]
        .into_iter()
        .map(|(name, entry)| (name.to_owned(), entry))
        .collect();
    let naming = vec![
        ("fake".to_owned(), "n".to_owned()),
        ("lit".to_owned(), "n".to_owned()),
        ("lit".to_owned(), "n:high".to_owned()),
    ];
    let switching = over(
        &fixture.home,
        providers.clone(),
        naming.clone(),
        config.clone(),
        credentials.clone(),
    );
    let rejection = rejected(&switching, &args("n:high"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert_eq!(
        rejection.message,
        "The model `lit/n:high` needs the setting `workspace` for its base URL, which has no value."
    );

    // The exact unconfigured literal also wins when it is the only full-ID
    // naming match; no stripped-id entry is needed to preserve its error.
    let naming = vec![("lit".to_owned(), "n:high".to_owned())];
    let switching = over(&fixture.home, providers, naming, config, credentials);
    let rejection = rejected(&switching, &args("n:high"), None);
    assert_eq!(
        rejection.message,
        "The model `lit/n:high` needs the setting `workspace` for its base URL, which has no value."
    );
}

#[test]
fn an_unconfigured_model_counts_toward_ambiguity() {
    let fixture = fixture("fiber-switch-unconfigured");
    install_extension(
        &fixture.home,
        "extensions/acme",
        json!({"name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        &[(
            "acme",
            json!({
                "name": "acme",
                "placeholders": {"workspace": {}},
                "models": [{"id": "m", "protocol": "openai-responses",
                            "base_url": "https://{workspace}/v1", "context_window": 1000}],
            }),
        )],
    );
    let config = config(&fixture, &[]);
    let (mut providers, _) = Providers::load(&fixture.home).unwrap();
    let snapshot = providers.clone();
    let extensions = extensions::SessionExtensions::load(
        &fixture.home,
        &config,
        fakes::clock::FakeClock::new(),
        Arc::new(tools::PathLocks::new()),
        None,
    );
    let naming = crate::lua_providers::add_lua(&extensions, &mut providers, &config).unwrap();
    assert!(
        naming.contains(&("acme".to_owned(), "m".to_owned())),
        "the naming list keeps the unconfigured model: {naming:?}"
    );
    let credentials: Credentials = [("fake", keyed("default"))]
        .into_iter()
        .map(|(name, entry)| (name.to_owned(), entry))
        .collect();
    let switching = over(&fixture.home, snapshot, naming, config, credentials);
    // Configured `fake/m` and unconfigured `acme/m` make the bare `m`
    // ambiguous.
    let rejection = rejected(&switching, &args("m"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert!(
        rejection.message.contains("fake/m"),
        "{}",
        rejection.message
    );
    assert!(
        rejection.message.contains("acme/m"),
        "{}",
        rejection.message
    );
}

#[test]
fn the_suffix_beats_args_thinking_which_beats_chosen() {
    let fixture = fixture("fiber-switch-thinking");
    let switching = switching(&fixture, &[]);
    let made = prepared(
        &switching,
        &args_thinking("fake/n:high", "low"),
        Some(ThinkingLevel::Low),
    );
    assert_eq!(made.thinking, Some(ThinkingLevel::High));
    assert_eq!(made.chosen, Some(ThinkingLevel::High));
    let made = prepared(
        &switching,
        &args_thinking("fake/n", "high"),
        Some(ThinkingLevel::Low),
    );
    assert_eq!(made.thinking, Some(ThinkingLevel::High));
    assert_eq!(made.chosen, Some(ThinkingLevel::High));
    let made = prepared(&switching, &args("fake/n"), Some(ThinkingLevel::Low));
    assert_eq!(made.thinking, Some(ThinkingLevel::Low));
    assert_eq!(made.chosen, Some(ThinkingLevel::Low));
    let made = prepared(&switching, &args("fake/n"), None);
    assert_eq!(made.thinking, Some(ThinkingLevel::Low));
    assert_eq!(made.chosen, None);
}

#[test]
fn a_configured_unsupported_level_gives_the_default_and_a_notice() {
    let fixture = fixture("fiber-switch-configured-thinking");
    let switching = switching(&fixture, &[r#"models."fake/n".thinking=medium"#]);
    let made = prepared(&switching, &args("fake/n"), None);
    assert_eq!(made.thinking, Some(ThinkingLevel::Low));
    let notice = made.notice.expect("the ignored key is noticed");
    assert_eq!(notice.code, ErrorCode::ConfigKeyIgnored);
}

#[test]
fn a_chosen_unsupported_level_is_invalid_arguments() {
    let fixture = fixture("fiber-switch-chosen-thinking");
    let switching = switching(&fixture, &[]);
    let rejection = rejected(&switching, &args("fake/n"), Some(ThinkingLevel::Medium));
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
}

#[test]
fn an_unparseable_args_thinking_rejects_before_any_lookup() {
    let fixture = fixture("fiber-switch-bad-thinking");
    let switching = switching(&fixture, &[]);
    // `other/m` would run its `command`; the thinking level rejects first.
    let rejection = rejected(&switching, &args_thinking("other/m", "sideways"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert!(
        rejection.message.contains("sideways"),
        "{}",
        rejection.message
    );
    assert_eq!(runs(&fixture.marker), 0);
}

#[test]
fn a_bedrock_model_is_rejected() {
    let fixture = fixture("fiber-switch-bedrock");
    let config = config(&fixture, &[]);
    let (providers, _) = Providers::load(&fixture.home).unwrap();
    let credentials: Credentials = [("bed", keyed("default"))]
        .into_iter()
        .map(|(name, entry)| (name.to_owned(), entry))
        .collect();
    let naming = vec![("bed".to_owned(), "bk".to_owned())];
    let switching = over(&fixture.home, providers, naming, config, credentials);
    let rejection = rejected(&switching, &args("bed/bk"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert!(
        rejection.message.contains("does not speak yet"),
        "{}",
        rejection.message
    );
}

#[test]
fn the_reviewer_is_rechosen_for_the_new_provider() {
    let fixture = fixture("fiber-switch-reviewer");
    let switching = switching(&fixture, &["reviewer.model=fake/r"]);
    let made = prepared(&switching, &args("fake/n"), None);
    let reviewer = made.reviewer.expect("the reviewer resolved");
    assert_eq!(reviewer.model.reference, "fake/r");
    assert_eq!(reviewer.context_window, 1000);
}

/// A reviewer on another provider is read in the same job as the session's
/// provider, and its key is kept.
#[test]
fn a_reviewer_on_another_provider_is_read_in_the_same_job() {
    let fixture = fixture("fiber-switch-reviewer-outside");
    let switching = switching(&fixture, &["reviewer.model=other/m"]);
    let made = prepared(&switching, &args("fake/n"), None);
    assert_eq!(made.model.reference, "fake/n");
    let reviewer = made.reviewer.expect("the reviewer resolved");
    assert_eq!(reviewer.model.reference, "other/m");
    assert_eq!(runs(&fixture.marker), 1);
    assert!(keys(&switching).contains(&"other".to_owned()));
}

/// A reviewer whose read fails is the loop's to escalate: the switch
/// stands, and the reviewer's provider keeps no key.
#[test]
fn a_reviewer_whose_read_fails_leaves_the_switch_standing() {
    let fixture = fixture("fiber-switch-reviewer-bad");
    let switching = switching(&fixture, &["reviewer.model=bad/bm"]);
    let made = prepared(&switching, &args("fake/n"), None);
    assert_eq!(made.model.reference, "fake/n");
    let failure = reviewer_failed(made.reviewer);
    assert_eq!(failure.code, ErrorCode::CredentialMissing);
    assert!(!keys(&switching).contains(&"bad".to_owned()));
}

#[test]
fn a_reviewer_naming_no_installed_model_fails_and_the_switch_stands() {
    let fixture = fixture("fiber-switch-reviewer-lua");
    let switching = switching(&fixture, &["reviewer.model=lua/rm"]);
    let made = prepared(&switching, &args("fake/n"), None);
    assert_eq!(made.model.reference, "fake/n");
    let failure = reviewer_failed(made.reviewer);
    assert!(failure.message.contains("lua"), "{}", failure.message);
}

#[test]
fn without_a_reviewer_the_loop_gets_its_failure() {
    let fixture = fixture("fiber-switch-no-reviewer");
    let switching = switching(&fixture, &[]);
    let made = prepared(&switching, &args("fake/n"), None);
    let failure = reviewer_failed(made.reviewer);
    assert_eq!(failure.code, ErrorCode::NoModel);
    assert_eq!(failure.message, r#loop::NO_MODEL_MESSAGE);
}

#[test]
fn the_prepared_fields_follow_the_new_model() {
    let fixture = fixture("fiber-switch-fields");
    let switching = switching(&fixture, &["cache.lifetime=5m", "handoff.enabled=false"]);
    let made = prepared(&switching, &args("fake/n"), None);
    assert_eq!(made.context_window, 1000);
    assert_eq!(made.addendum, Some("The extra paragraph.\n".to_owned()));
    assert_eq!(
        made.cache_lifetime,
        contract::events::CacheLifetime::FiveMinutes
    );
    assert!(!made.handoff.enabled);
    let searched = prepared(&switching, &args("claude/w"), None);
    assert_eq!(searched.context_window, 500);
}

#[test]
fn a_data_file_written_after_the_snapshot_does_not_resolve() {
    let fixture = fixture("fiber-switch-snapshot");
    let switching = switching(&fixture, &["reviewer.model=fake/r"]);
    std::fs::write(
        fixture.home.join("extensions/fake/providers/fake.json"),
        json!({
            "name": "fake",
            "models": [
                {"id": "m", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 100000},
                {"id": "n", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 1000},
                {"id": "r", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 1000},
                {"id": "late", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:9/v1", "context_window": 1000},
            ],
        })
        .to_string(),
    )
    .unwrap();
    let rejection = rejected(&switching, &args("fake/late"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert!(
        rejection.message.contains("has no model"),
        "{}",
        rejection.message
    );
}

/// The fixture Lua extension installed in a new home whose `fixture.url`
/// is `server`, with `cache` as `fixture`'s cached model list when given.
fn lua_fixture_home(
    root: &fakes::TempDir,
    server: &fakes::ProviderServer,
    cache: Option<serde_json::Value>,
) -> (PathBuf, Config) {
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    extensions::plan(
        &home,
        &extensions::Request::Path(fakes::lua_fixture()),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    config::store_secret(&home, "fixture.url", &config::Secret::new(server.url())).unwrap();
    config::store_secret(&home, "fixture.api_key", &config::Secret::new("k1".into())).unwrap();
    if let Some(cache) = cache {
        config::write_model_cache(&home, "fixture", &cache).unwrap();
    }
    let config = crate::test_support::load(&home, &workspace, "test", Vec::<String>::new());
    (home, config)
}

/// A retained Lua provider with a model cache resolves its cached models
/// with no listing request: the switch registry is the startup one.
#[test]
fn a_retained_lua_provider_with_a_cache_resolves_without_a_listing_request() {
    let server = fakes::ProviderServer::start_routed(
        [(
            "/token",
            fakes::Response::status(
                200,
                json!({"access_token": "tok", "expires_at": 4_102_444_800_u64}).to_string(),
            ),
        )],
        fakes::Response::status(500, "no"),
    )
    .unwrap();
    let root = fakes::TempDir::new("fiber-switch-lua-cache");
    let cached = json!([{"id": "cached", "protocol": "openai-responses",
                         "base_url": format!("{}/v1", server.url()), "context_window": 1000}]);
    let (home, config) = lua_fixture_home(&root, &server, Some(cached));
    let credentials: Credentials = [("fixture".to_owned(), ("default".to_owned(), (None, None)))]
        .into_iter()
        .collect();
    let switching = assembled(&home, config, credentials, &["fixture"]);
    let made = prepared(&switching, &args("fixture/cached"), None);
    assert_eq!(made.model.reference, "fixture/cached");
    assert!(
        server
            .requests()
            .iter()
            .all(|request| request.path != "/v1/models"),
        "no listing request ran: {:?}",
        server.requests()
    );
}

/// A Lua provider whose discovery failed at startup lists no models, so a
/// switch to one of its ids is an unknown model, and no listing runs again.
#[test]
fn a_retained_lua_provider_without_a_cache_does_not_resolve() {
    let server = fakes::ProviderServer::start([]).unwrap();
    let root = fakes::TempDir::new("fiber-switch-lua-live");
    let (home, config) = lua_fixture_home(&root, &server, None);
    let credentials: Credentials = [("fixture".to_owned(), ("default".to_owned(), (None, None)))]
        .into_iter()
        .collect();
    // Discovery runs here, for the naming list, and fails.
    let switching = assembled(&home, config, credentials, &["fixture"]);
    let listed = server
        .requests()
        .iter()
        .filter(|request| request.path == "/v1/models")
        .count();
    let rejection = rejected(&switching, &args("fixture/live"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|request| request.path == "/v1/models")
            .count(),
        listed,
        "no listing ran again"
    );
}

/// A bare id one provider names resolves; pushing the reference again
/// would list it twice and reject as ambiguous, as would treating one
/// match as many.
#[test]
fn a_bare_id_one_provider_names_resolves() {
    let fixture = fixture("fiber-switch-bare-single");
    let switching = switching(&fixture, &[]);
    let made = prepared(&switching, &args("r"), None);
    assert_eq!(made.model.reference, "fake/r");
}

/// A `:<level>` id no literal names reports the stripped id's ambiguity,
/// not the full id's: always taking the full-id ambiguity arm would name
/// `m:high`, and reporting ambiguity on an empty match list would too.
#[test]
fn a_suffixed_id_without_a_literal_reports_the_stripped_ambiguity() {
    let fixture = fixture("fiber-switch-stripped-suffix");
    let switching = switching(&fixture, &[]);
    let rejection = rejected(&switching, &args("m:high"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert_eq!(
        rejection.message,
        "The model `m` is offered by more than one provider: fake/m, other/m. \
         Name one as `provider/model`."
    );
}

/// Adds extensions `lit1` and `lit2` each naming the literal `n:high`: a
/// model id ending in a recognised thinking level that two registry
/// providers share.
fn with_two_literal_providers(fixture: &Fixture) {
    for name in ["lit1", "lit2"] {
        install_extension(
            &fixture.home,
            &format!("extensions/{name}"),
            json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
            &[(
                name,
                json!({
                    "name": name,
                    "models": [
                        {"id": "n:high", "protocol": "openai-responses",
                         "base_url": "http://127.0.0.1:9/v1", "context_window": 1000},
                    ],
                }),
            )],
        );
    }
}

/// Two registry literals sharing one id are ambiguous on the full id, even
/// when the naming list holds only one: skipping the guard would fall back
/// to the stripped id, and pushing only duplicates would drop the other.
#[test]
fn two_registry_literals_sharing_one_id_are_ambiguous_on_the_full_id() {
    let fixture = fixture("fiber-switch-literal-registry-ambiguity");
    with_two_literal_providers(&fixture);
    let config = config(&fixture, &[]);
    let (providers, _) = Providers::load(&fixture.home).unwrap();
    let credentials: Credentials = [("lit1", keyed("default"))]
        .into_iter()
        .map(|(name, entry)| (name.to_owned(), entry))
        .collect();
    // The naming list holds only one of the two literals.
    let naming = vec![("lit1".to_owned(), "n:high".to_owned())];
    let switching = over(&fixture.home, providers, naming, config, credentials);
    let rejection = rejected(&switching, &args("n:high"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert_eq!(
        rejection.message,
        "The model `n:high` is offered by more than one provider: lit1/n:high, lit2/n:high. \
         Name one as `provider/model`."
    );
}

/// The OpenRouter package installed in a new home: a Lua provider that
/// registers `cost` and `models`, beside an empty data file. The cached
/// `z-ai/glm-5.3-flash` entry stands in for a discovery run, so resolving
/// it never touches the network.
fn openrouter_home(root: &fakes::TempDir) -> (PathBuf, Config) {
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../providers/openrouter");
    extensions::plan(
        &home,
        &extensions::Request::Path(package),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    config::write_model_cache(
        &home,
        "openrouter",
        &json!([{
            "id": "z-ai/glm-5.3-flash",
            "protocol": "openai-completions",
            "base_url": "https://openrouter.ai/api/v1",
            "compat": {"cache_key_field": "session_id", "reasoning_object": true},
            "context_window": 1048576,
            "max_output_tokens": 943717,
            "input": ["text", "image", "video"],
            "cost": {"input": 0.15, "output": 0.5, "cache_read": 0.03},
        }]),
    )
    .unwrap();
    let config = crate::test_support::load(&home, &workspace, "test", Vec::<String>::new());
    (home, config)
}

const OPENROUTER_MODEL: &str = "openrouter/z-ai/glm-5.3-flash";

/// A switch to a cost-only Lua provider carries its lookup, whether the
/// session holds it loaded or the switch starts it.
#[test]
fn a_switch_to_a_cost_only_provider_carries_its_lookup() {
    for keep in [&["openrouter"] as &[&str], &[]] {
        let root = fakes::TempDir::new("fiber-switch-lua-cost");
        let (home, config) = openrouter_home(&root);
        let switching = assembled(&home, config, startup(&["openrouter"]), keep);
        assert_eq!(loaded(&switching).len(), keep.len());
        let made = prepared(&switching, &args(OPENROUTER_MODEL), None);
        assert_eq!(made.model.reference, OPENROUTER_MODEL);
        assert!(made.provider.cost_lookup().is_some(), "{keep:?}");
    }
}

/// An owed cost lookup keeps the provider switched away from alive until
/// the last one is released, though `Switching` no longer holds it.
#[test]
fn an_owed_cost_lookup_holds_the_old_provider_until_released() {
    let root = fakes::TempDir::new("fiber-switch-lua-owed");
    let (home, config) = openrouter_home(&root);
    data_extension(
        &home,
        "fake",
        &json!({"name": "fake", "models": [{"id": "m", "protocol": "openai-responses",
            "base_url": "http://127.0.0.1:9/v1", "context_window": 1000}]}),
    );
    let switching = assembled(
        &home,
        config,
        startup(&["openrouter", "fake"]),
        &["openrouter"],
    );
    let old = Arc::downgrade(&switching.loaded.lock().unwrap()["openrouter"]);
    let started = prepared(&switching, &args(OPENROUTER_MODEL), None);
    // Two calls owed a lookup, as two generations' pending costs.
    let first = started.provider.cost_lookup().expect("a lookup");
    let second = started.provider.cost_lookup().expect("a lookup");
    drop(started);
    let away = prepared(&switching, &args("fake/m"), None);
    (away.applied.expect("applying unloads"))();
    assert!(loaded(&switching).is_empty());
    assert!(old.upgrade().is_some(), "the owed lookups hold it");
    drop(first);
    assert!(old.upgrade().is_some(), "one lookup still holds it");
    drop(second);
    assert!(old.upgrade().is_none(), "unloaded once both are released");
}

#[test]
fn a_model_with_hosted_search_declares_it_and_applying_publishes_it() {
    let fixture = fixture("fiber-switch-hosted-declare");
    let switching = switching(&fixture, &["tools.web_search.max_result_bytes=100"]);
    let (door, declared) = recording(false);
    let Ok(made) = prepare(&switching, &door, &args("claude/w"), None, None) else {
        panic!("the switch rejected");
    };
    assert_eq!(
        declared_type(&made.web_search),
        Some(Some("web_search_20250305".to_owned()))
    );
    assert!(
        declared.lock().unwrap().is_empty(),
        "nothing during preparation"
    );
    (made.applied.expect("applying publishes"))();
    assert_eq!(
        *declared.lock().unwrap(),
        vec![("web_search".to_owned(), Some("web_search".to_owned()))]
    );
}

#[test]
fn a_model_without_hosted_search_withdraws_it_and_applying_removes_it() {
    let fixture = fixture("fiber-switch-hosted-withdraw");
    let switching = switching(&fixture, &[]);
    let (door, declared) = recording(false);
    let Ok(made) = prepare(&switching, &door, &args("fake/m"), None, None) else {
        panic!("the switch rejected");
    };
    assert!(matches!(&made.web_search, r#loop::Hosted::Withdraw(name) if name == "web_search"));
    (made.applied.expect("applying publishes"))();
    assert_eq!(
        *declared.lock().unwrap(),
        vec![("web_search".to_owned(), None)]
    );
}

#[test]
fn a_standing_web_search_is_kept_and_nothing_is_published() {
    let fixture = fixture("fiber-switch-hosted-keep");
    let switching = switching(&fixture, &[]);
    for model in ["claude/w", "fake/m"] {
        let (door, declared) = recording(true);
        let Ok(made) = prepare(&switching, &door, &args(model), None, None) else {
            panic!("the switch rejected");
        };
        assert!(matches!(made.web_search, r#loop::Hosted::Keep), "{model}");
        (made.applied.expect("applying keeps the loaded set"))();
        assert!(declared.lock().unwrap().is_empty(), "{model}");
    }
}

#[test]
fn only_a_web_search_from_another_registrant_stands() {
    let hosted = || -> Arc<dyn contract::tool::Tool> {
        let (tool, _) = crate::builtin::hosted("web_search_20250305").unwrap();
        tool
    };
    let other: Arc<dyn contract::tool::Tool> = Arc::new(tools::Handoff);
    assert!(!hosted_stands(&[]));
    assert!(!hosted_stands(&[("builtin".to_owned(), hosted())]));
    assert!(!hosted_stands(&[("search-ext".to_owned(), other)]));
    assert!(hosted_stands(&[
        ("builtin".to_owned(), hosted()),
        ("search-ext".to_owned(), hosted()),
    ]));
}

/// Installs the Lua extension `fiber.test/luax` registering provider `lp`,
/// whose data file names `lm` and whose `credential()` returns `tok-lp`.
/// Its entry script appends `x` to `counter` each time a VM runs it, after
/// erroring when `flag` exists.
fn lua_extension(fixture: &Fixture, counter: &Path, flag: &Path) {
    let src = fixture.root.path().join("src/luax");
    install_extension(
        &fixture.root.path().join("src"),
        "luax",
        json!({"name": "fiber.test/luax", "version": "v1.2.3", "fiber": "0.1.0", "api": 1}),
        &[(
            "lp",
            json!({"name": "lp", "models": [{"id": "lm", "protocol": "openai-responses",
            "base_url": "http://127.0.0.1:9/v1", "context_window": 1000}]}),
        )],
    );
    std::fs::write(
        src.join("init.lua"),
        format!(
            "local failing = pcall(host.fs.read, \"{flag}\")\n\
             if failing then error(\"broken\") end\n\
             local ok, seen = pcall(host.fs.read, \"{counter}\")\n\
             if not ok or seen == nil then seen = \"\" end\n\
             host.fs.write(\"{counter}\", seen .. \"x\")\n\
             fiber.provider(\"lp\", {{ credential = {{ timeout = 1000,\n\
               run = function() return {{ token = \"tok-lp\", expires_at = 4102444800 }} end }} }})\n",
            flag = flag.display(),
            counter = counter.display(),
        ),
    )
    .unwrap();
    extensions::plan(
        &fixture.home,
        &extensions::Request::Path(src),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
}

/// The fixture with `fiber.test/luax` installed: its counter and flag.
fn lua_fixture(name: &str) -> (Fixture, PathBuf, PathBuf) {
    let fixture = fixture(name);
    let counter = fixture.root.path().join("vm-runs");
    let flag = fixture.root.path().join("vm-fails");
    lua_extension(&fixture, &counter, &flag);
    (fixture, counter, flag)
}

fn vm_runs(counter: &Path) -> usize {
    std::fs::read_to_string(counter).map_or(0, |text| text.len())
}

/// A Lua-only provider the session unloaded at start is started for the
/// switch: a bare id only it names resolves, its token is read, and once
/// the switch applies it stays loaded, so a second switch starts no VM.
#[test]
fn an_unloaded_lua_provider_is_started_for_the_switch_once() {
    let (fixture, counter, _) = lua_fixture("fiber-switch-lua-start");
    let switching = switching(&fixture, &[]);
    assert_eq!(vm_runs(&counter), 1, "startup ran the entry script");
    assert!(loaded(&switching).is_empty());
    let made = prepared(&switching, &args("lm"), None);
    assert_eq!(made.model.reference, "lp/lm");
    assert_eq!(vm_runs(&counter), 2, "the switch started one VM");
    assert!(loaded(&switching).is_empty(), "loaded only when it applies");
    assert_eq!(
        switching
            .remembered
            .lock()
            .unwrap()
            .keys
            .get(&("lp".to_owned(), "default".to_owned()))
            .map(Option::is_none),
        Some(true),
        "`credential()` supplies the token, so no key is kept"
    );
    assert_eq!(selected(&switching, "lp").as_deref(), Some("default"));
    (made.applied.expect("applying keeps it"))();
    assert_eq!(loaded(&switching), vec!["lp".to_owned()]);
    prepared(&switching, &args("lp/lm"), None);
    assert_eq!(vm_runs(&counter), 2, "a loaded provider starts no VM");
}

/// Applying a switch away from a Lua provider unloads it, unless the
/// reviewer uses it.
#[test]
fn applying_a_switch_unloads_the_old_provider_unless_the_reviewer_uses_it() {
    for (overrides, kept) in [
        (&[] as &[&str], false),
        (&["reviewer.model=lp/lm"] as &[&str], true),
    ] {
        let (fixture, _, _) = lua_fixture("fiber-switch-lua-unload");
        let mut credentials = startup(&["fake", "claude"]);
        credentials.insert("lp".to_owned(), ("default".to_owned(), (None, None)));
        let switching = assembled(
            &fixture.home,
            config(&fixture, overrides),
            credentials,
            &["lp"],
        );
        let old: Weak<LuaProvider> = Arc::downgrade(&switching.loaded.lock().unwrap()["lp"]);
        let made = prepared(&switching, &args("fake/m"), None);
        (made.applied.expect("applying keeps the set"))();
        assert_eq!(!loaded(&switching).is_empty(), kept, "{overrides:?}");
        drop(made.reviewer);
        assert_eq!(old.upgrade().is_some(), kept, "{overrides:?}");
    }
}

/// A reviewer on a Lua provider the session does not hold is started in
/// the read job, and stays loaded once the switch applies.
#[test]
fn a_reviewer_on_an_unloaded_lua_provider_is_started_on_demand() {
    let (fixture, counter, _) = lua_fixture("fiber-switch-lua-reviewer");
    let switching = switching(&fixture, &["reviewer.model=lp/lm"]);
    let made = prepared(&switching, &args("fake/m"), None);
    assert_eq!(made.model.reference, "fake/m");
    let reviewer = made.reviewer.as_ref().expect("the reviewer resolved");
    assert_eq!(reviewer.model.reference, "lp/lm");
    assert_eq!(vm_runs(&counter), 2);
    (made.applied.expect("applying keeps it"))();
    assert_eq!(loaded(&switching), vec!["lp".to_owned()]);
}

/// A reviewer whose Lua provider fails to start is the loop's to escalate,
/// with `extension_failed`; the switch stands.
#[test]
fn a_reviewer_whose_provider_fails_to_start_leaves_the_switch_standing() {
    let (fixture, _, flag) = lua_fixture("fiber-switch-lua-broken");
    let switching = switching(&fixture, &["reviewer.model=lp/lm"]);
    std::fs::write(&flag, "").unwrap();
    let made = prepared(&switching, &args("fake/m"), None);
    assert_eq!(made.model.reference, "fake/m");
    let failure = reviewer_failed(made.reviewer);
    assert_eq!(failure.code, ErrorCode::ExtensionFailed);
    assert!(!keys(&switching).contains(&"lp".to_owned()));
}

/// Shutdown during a read rejects the switch `closing` and caches nothing.
#[test]
fn a_cancelled_read_rejects_closing_and_caches_nothing() {
    let fixture = fixture("fiber-switch-cancelled");
    let switching = switching(&fixture, &[]);
    switching.reads().cancel();
    let rejection = rejected(&switching, &args("other/m"), None);
    assert_eq!(rejection.code, ErrorCode::Closing);
    assert_eq!(rejection.message, "The session is shutting down.");
    assert_eq!(runs(&fixture.marker), 0);
    assert!(!keys(&switching).contains(&"other".to_owned()));
}

/// A provider read at startup keeps the label it was read under, such as
/// a resumed session's recorded one, rather than the configured label.
#[test]
fn a_key_read_before_keeps_its_label() {
    let fixture = fixture("fiber-switch-label");
    let credentials: Credentials = [("fake".to_owned(), keyed("recorded"))]
        .into_iter()
        .collect();
    let switching = assembled(&fixture.home, config(&fixture, &[]), credentials, &[]);
    let made = prepared(&switching, &args("fake/n"), None);
    assert_eq!(made.credential, Some("recorded".to_owned()));
}

/// The `file` source a reviewer's read reads joins the deny too.
#[test]
fn a_reviewers_file_source_joins_the_credential_files() {
    let fixture = fixture("fiber-switch-reviewer-file");
    std::fs::create_dir_all(fixture.key_file.parent().unwrap()).unwrap();
    std::fs::write(&fixture.key_file, "sk-filed").unwrap();
    let switching = switching(&fixture, &["reviewer.model=filed/fm"]);
    let made = prepared(&switching, &args("fake/n"), None);
    assert!(made.reviewer.is_ok());
    assert_eq!(
        made.credential_files,
        vec![fixture.key_file.canonicalize().unwrap()]
    );
}

/// `Switching` as `parts_in` builds it for a session started with
/// `overrides`, its scripted references added, over a workspace holding
/// `s.json` and `r.json`, with `fake`'s key read at startup.
fn scripted_switching(fixture: &Fixture, overrides: &[&str]) -> Arc<Switching> {
    for name in ["s.json", "r.json"] {
        std::fs::write(
            fixture.workspace.join(name),
            r#"{"steps": [{"text": "Hi."}]}"#,
        )
        .unwrap();
    }
    let config = config(fixture, overrides);
    let (mut providers, _) = Providers::load(&fixture.home).unwrap();
    crate::scripted::prepare(&mut providers, &config, None);
    let extensions = extensions::SessionExtensions::load(
        &fixture.home,
        &config,
        fakes::clock::FakeClock::new(),
        Arc::new(tools::PathLocks::new()),
        None,
    );
    let mut loader = loader(Arc::new(extensions), BTreeMap::new());
    loader.workspace = fixture.workspace.clone();
    Arc::new(Switching::new(
        providers,
        Vec::new(),
        config,
        startup(&["fake"]),
        Vec::new(),
        loader,
    ))
}

/// The registry a session switches within is fixed at start, so a scripted
/// model it did not start with is rejected with how to choose one
/// (`docs/model-routing.md`, "The scripted provider").
#[test]
fn a_scripted_model_the_session_did_not_start_with_is_rejected() {
    let fixture = fixture("fiber-switch-scripted-new");
    for overrides in [&["model=scripted/s.json"][..], &[]] {
        let switching = scripted_switching(&fixture, overrides);
        let rejection = rejected(&switching, &args("scripted/r.json"), None);
        assert_eq!(rejection.code, ErrorCode::InvalidArguments);
        assert_eq!(rejection.message, crate::scripted::START_ONLY);
    }
    // Any other unknown reference keeps its own message.
    let switching = scripted_switching(&fixture, &["model=scripted/s.json"]);
    let rejection = rejected(&switching, &args("nobody/m"), None);
    assert_ne!(rejection.message, crate::scripted::START_ONLY);
}

#[test]
fn a_switch_to_the_startup_scripted_model_reads_no_credential() {
    let fixture = fixture("fiber-switch-scripted-back");
    let switching = scripted_switching(&fixture, &["model=scripted/s.json"]);
    let made = prepared(&switching, &args("scripted/s.json"), None);
    assert_eq!(made.model.reference, "scripted/s.json");
    assert!(made.model.cost.is_none());
    // A credential read would fail: the scripted provider declares none.
    assert!(made.credential_files.is_empty());
}

/// A scripted reviewer's read is bypassed on a switch as at startup: the
/// reviewer resolves to its own script with no credential.
#[test]
fn a_scripted_reviewer_stands_across_a_switch() {
    let fixture = fixture("fiber-switch-scripted-reviewer");
    let switching = scripted_switching(&fixture, &["reviewer.model=scripted/r.json"]);
    let made = prepared(&switching, &args("fake/m"), None);
    let reviewer = made.reviewer.expect("the reviewer resolved");
    assert_eq!(reviewer.model.reference, "scripted/r.json");
}

/// A `credential` switch to a stored label prepares under it: the label and
/// the key read under it (`docs/model-routing.md`, "Which credential a
/// session uses").
#[test]
fn a_credential_switch_to_a_stored_label_reads_under_it() {
    let fixture = fixture("fiber-switch-credential-stored");
    config::store_credential(
        &fixture.home,
        "fake",
        "other",
        &config::Secret::new("k-other".into()),
    )
    .unwrap();
    let switching = switching(&fixture, &[]);
    let made = prepared_with(&switching, &args("fake/n"), Some("other"), None);
    assert_eq!(made.model.reference, "fake/n");
    assert_eq!(made.credential, Some("other".to_owned()));
    assert_eq!(selected(&switching, "fake").as_deref(), Some("other"));
}

/// A label the provider does not have is `credential_missing`, naming its
/// labels, before any configured source is read or any command runs: no
/// read, nothing cached (`docs/model-routing.md`, "Which credential a
/// session uses").
#[test]
fn a_credential_switch_to_an_absent_label_reads_nothing() {
    let fixture = fixture("fiber-switch-credential-absent");
    let switching = switching(&fixture, &[]);
    let rejection = rejected_with(&switching, &args("other/m"), Some("nope"), None);
    assert_eq!(rejection.code, ErrorCode::CredentialMissing);
    assert_eq!(
        rejection.message,
        "`other` has no credential label `nope`. The labels for `other` are: default"
    );
    assert_eq!(runs(&fixture.marker), 0, "no command ran");
    assert!(!keys(&switching).contains(&"other".to_owned()));
    assert_eq!(selected(&switching, "other"), None);
}

/// A configured `command` label whose source cannot be read rejects with
/// the read's own code and caches nothing, so the next switch reads again.
#[test]
fn a_failing_configured_command_label_reads_again() {
    let fixture = fixture("fiber-switch-credential-command-fails");
    let script = format!("echo x >> '{}'; exit 1", fixture.bad_marker.display());
    std::fs::write(
        fixture.home.join("config.json"),
        json!({"providers": {"bad": {"credentials": {"retry": {"command": sh(&script)}}}}})
            .to_string(),
    )
    .unwrap();
    let switching = switching(&fixture, &[]);
    let rejection = rejected_with(&switching, &args("bad/bm"), Some("retry"), None);
    assert_eq!(rejection.code, ErrorCode::CredentialMissing);
    assert!(rejection.message.contains("`sh`"), "{}", rejection.message);
    assert!(!keys(&switching).contains(&"bad".to_owned()));
    assert_eq!(selected(&switching, "bad"), None);
    rejected_with(&switching, &args("bad/bm"), Some("retry"), None);
    assert_eq!(runs(&fixture.bad_marker), 2, "a failed read is read again");
}

/// A return to a label already read reuses its key without a read: after A
/// and B are read, A's file rewritten (or removed) changes nothing, and no
/// read of A's file happens.
#[test]
fn a_label_read_before_is_reused_without_a_read() {
    for (name, remove) in [
        ("fiber-switch-credential-reuse-rewritten", false),
        ("fiber-switch-credential-reuse-removed", true),
    ] {
        let fixture = fixture(name);
        let dir = fixture.root.path().join("keys");
        std::fs::create_dir_all(&dir).unwrap();
        let fa = dir.join("a");
        let fb = dir.join("b");
        std::fs::write(&fa, "key-a\n").unwrap();
        std::fs::write(&fb, "key-b\n").unwrap();
        std::fs::write(
            fixture.home.join("config.json"),
            json!({"providers": {"fake": {"credentials": {
                "a": {"file": fa},
                "b": {"file": fb},
            }}}})
            .to_string(),
        )
        .unwrap();
        let switching = switching(&fixture, &[]);
        let first = prepared_with(&switching, &args("fake/m"), Some("a"), None);
        assert_eq!(first.credential, Some("a".to_owned()));
        assert_eq!(
            first.credential_files,
            vec![fa.canonicalize().unwrap()],
            "{name}"
        );
        let second = prepared_with(&switching, &args("fake/m"), Some("b"), None);
        assert_eq!(second.credential, Some("b".to_owned()));
        if remove {
            std::fs::remove_file(&fa).unwrap();
        } else {
            std::fs::write(&fa, "key-a2\n").unwrap();
        }
        let again = prepared_with(&switching, &args("fake/m"), Some("a"), None);
        assert_eq!(again.credential, Some("a".to_owned()));
        assert!(
            again.credential_files.is_empty(),
            "{name}: the `(provider, label)` key is reused"
        );
    }
}

/// After a credential switch, a model switch with no label prepares under
/// the selected one.
#[test]
fn a_model_switch_after_a_credential_switch_keeps_its_label() {
    let fixture = fixture("fiber-switch-credential-kept");
    config::store_credential(
        &fixture.home,
        "fake",
        "other",
        &config::Secret::new("k-other".into()),
    )
    .unwrap();
    let switching = switching(&fixture, &[]);
    let made = prepared_with(&switching, &args("fake/n"), Some("other"), None);
    assert_eq!(made.credential, Some("other".to_owned()));
    let again = prepared(&switching, &args("fake/m"), None);
    assert_eq!(again.credential, Some("other".to_owned()));
}

/// A read cancelled while in progress rejects `closing` and publishes
/// nothing: no `(provider, label)` entry, the provider's selected label
/// still the old one.
#[test]
fn a_read_cancelled_in_progress_publishes_nothing() {
    let fixture = fixture("fiber-switch-credential-cancel");
    let gate = fixture.root.path().join("gate");
    let started = fixture.root.path().join("started");
    let script = format!(
        "echo started >> '{}'; while [ ! -e '{}' ]; do sleep 0.05; done; echo x >> '{}'; echo other-key",
        started.display(),
        gate.display(),
        fixture.marker.display(),
    );
    std::fs::write(
        fixture.home.join("config.json"),
        json!({"providers": {"other": {"credentials": {"waiting": {"command": sh(&script)}}}}})
            .to_string(),
    )
    .unwrap();
    let switching = switching(&fixture, &[]);
    // The watchdog kills the gate-waiting command if the test dies: its
    // command line holds the fixture's directory.
    let watchdog = fakes::Watchdog::matching("fiber-switch-credential-cancel");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn({
        let switching = Arc::clone(&switching);
        move || {
            tx.send(bounded_with(
                &switching,
                &args("other/m"),
                Some("waiting"),
                None,
            ))
            .unwrap_or(());
        }
    });
    // The command signals it runs by writing `started`: the test cancels
    // only then, so the read is in progress.
    let (_tick, tock) = std::sync::mpsc::channel::<()>();
    for _ in 0..200 {
        if started.exists() {
            break;
        }
        let _waited = tock.recv_timeout(Duration::from_millis(50));
    }
    assert!(started.exists(), "the credential command started");
    switching.reads().cancel();
    let answered = rx.recv_timeout(DEADLINE).expect("the preparation answered");
    let rejection = match answered {
        Ok(_) => panic!("the cancelled read prepared"),
        Err(rejection) => rejection,
    };
    assert_eq!(rejection.code, ErrorCode::Closing);
    assert!(!keys(&switching).contains(&"other".to_owned()));
    assert_eq!(selected(&switching, "other"), None);
    assert_eq!(
        runs(&fixture.marker),
        0,
        "the killed command printed no key"
    );
    // Releases a survivor, if the cancel missed it.
    std::fs::write(&gate, "").unwrap();
    watchdog.stand_down(Duration::from_secs(5));
}

/// A scripted session's switch with no label fails at `connect` when the
/// script cannot be read, publishing neither key nor selected label; with
/// the script restored the same switch prepares with no credential.
#[test]
fn a_scripted_credential_switch_publishes_only_after_connect() {
    let fixture = fixture("fiber-switch-credential-scripted-connect");
    let switching = scripted_switching(&fixture, &["model=scripted/s.json"]);
    std::fs::remove_file(fixture.workspace.join("s.json")).unwrap();
    let rejection = rejected(&switching, &args("scripted/s.json"), None);
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert!(!keys(&switching).contains(&"scripted".to_owned()));
    assert_eq!(selected(&switching, "scripted"), None);
    std::fs::write(
        fixture.workspace.join("s.json"),
        r#"{"steps": [{"text": "Hi."}]}"#,
    )
    .unwrap();
    let made = prepared(&switching, &args("scripted/s.json"), None);
    assert_eq!(made.credential, None);
}

/// A scripted provider takes no credential, so any asked label is
/// `credential_missing` naming no labels, publishing nothing and reading
/// nothing (`docs/model-routing.md`, "The scripted provider").
#[test]
fn a_scripted_provider_rejects_any_label() {
    let fixture = fixture("fiber-switch-credential-scripted-any");
    let switching = scripted_switching(&fixture, &["model=scripted/s.json"]);
    let rejection = rejected_with(&switching, &args("scripted/s.json"), Some("anything"), None);
    assert_eq!(rejection.code, ErrorCode::CredentialMissing);
    assert!(
        rejection.message.ends_with("are: none"),
        "{}",
        rejection.message
    );
    assert!(!keys(&switching).contains(&"scripted".to_owned()));
    assert_eq!(selected(&switching, "scripted"), None);
}

/// On a Lua `credential()` provider the label reaches `credential()`: the
/// current label prepares, and so does one no source names.
#[test]
fn a_lua_credential_provider_takes_any_label_to_credential() {
    let (fixture, _, _) = lua_fixture("fiber-switch-credential-lua");
    let switching = switching(&fixture, &[]);
    let current = prepared_with(&switching, &args("lp/lm"), Some("default"), None);
    assert_eq!(current.credential, Some("default".to_owned()));
    let other = prepared_with(&switching, &args("lp/lm"), Some("other"), None);
    assert_eq!(other.credential, Some("other".to_owned()));
}

/// On a provider with no `credential()`, the current label is accepted even
/// when no source names it, such as a startup map label.
#[test]
fn the_current_label_needs_no_source() {
    let fixture = fixture("fiber-switch-credential-current");
    let credentials: Credentials = [("fake".to_owned(), keyed("recorded"))]
        .into_iter()
        .collect();
    let switching = assembled(&fixture.home, config(&fixture, &[]), credentials, &[]);
    assert!(
        config(&fixture, &[])
            .credentials()
            .labels(&fake_data())
            .is_empty()
    );
    let made = prepared_with(&switching, &args("fake/n"), Some("recorded"), None);
    assert_eq!(made.credential, Some("recorded".to_owned()));
    let rejection = rejected_with(&switching, &args("fake/n"), Some("nope"), None);
    assert_eq!(rejection.code, ErrorCode::CredentialMissing);
}

/// The missing-label check reads the provider's selected label at read
/// time: when another `prepare` for the same provider publishes `selected`
/// between `want()` and the read, the read answers without a read instead
/// of rejecting the label it just moved onto.
#[test]
fn the_missing_label_check_reads_selected_at_read_time() {
    let fixture = fixture("fiber-switch-stale-label");
    let switching = switching(&fixture, &[]);
    let want = switching.want("other", Some("bogus"));
    switching
        .remembered
        .lock()
        .unwrap()
        .selected
        .insert("other".to_owned(), "bogus".to_owned());
    let failure = match switching.read(want) {
        Ok(_) => panic!("the read succeeded"),
        Err(failure) => failure,
    };
    assert!(
        !failure.message.contains("has no credential label"),
        "{}",
        failure.message
    );
}

fn fake_data() -> config::ProviderData {
    config::ProviderData {
        name: "fake".into(),
        credential: None,
        credential_name: None,
        headers: Default::default(),
        placeholders: Default::default(),
        models: Vec::new(),
        reviewer_model: None,
        login: None,
    }
}

/// A search backend answering nothing, for switching with one installed.
struct StubBackend;

impl contract::search::SearchBackend for StubBackend {
    fn search(
        &self,
        _query: &str,
        _domains: &contract::search::Domains,
        _cancel: &dyn contract::tool::Cancel,
    ) -> Result<Option<Vec<contract::search::SearchResult>>, contract::shapes::Failure> {
        Ok(Some(Vec::new()))
    }
}

fn stub_backend() -> Arc<dyn contract::search::SearchBackend> {
    Arc::new(StubBackend)
}

#[test]
fn a_model_without_hosted_search_declares_fibers_own_over_the_backend() {
    let fixture = fixture("fiber-switch-hosted-withdraw");
    let switching = switching(&fixture, &[]);
    let (mut door, declared) = recording(false);
    door.backend = Some(stub_backend());
    let Ok(made) = prepare(&switching, &door, &args("fake/m"), None, None) else {
        panic!("the switch rejected");
    };
    assert_eq!(declared_type(&made.web_search), Some(None));
    (made.applied.expect("applying publishes"))();
    assert_eq!(
        *declared.lock().unwrap(),
        vec![("web_search".to_owned(), Some("web_search".to_owned()))]
    );
}

#[test]
fn a_model_with_hosted_search_declares_it_despite_the_backend() {
    let fixture = fixture("fiber-switch-hosted-declare");
    let switching = switching(&fixture, &[]);
    let (mut door, _) = recording(false);
    door.backend = Some(stub_backend());
    let Ok(made) = prepare(&switching, &door, &args("claude/w"), None, None) else {
        panic!("the switch rejected");
    };
    assert_eq!(
        declared_type(&made.web_search),
        Some(Some("web_search_20250305".to_owned()))
    );
}

#[test]
fn a_standing_web_search_is_kept_whatever_the_backend() {
    let fixture = fixture("fiber-switch-hosted-keep");
    let switching = switching(&fixture, &[]);
    for model in ["claude/w", "fake/m"] {
        let (mut door, declared) = recording(true);
        door.backend = Some(stub_backend());
        let Ok(made) = prepare(&switching, &door, &args(model), None, None) else {
            panic!("the switch rejected");
        };
        assert!(matches!(made.web_search, r#loop::Hosted::Keep), "{model}");
        (made.applied.expect("applying keeps the loaded set"))();
        assert!(declared.lock().unwrap().is_empty(), "{model}");
    }
}
