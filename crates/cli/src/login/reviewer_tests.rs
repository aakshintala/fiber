//! The login reviewer seam (`docs/model-routing.md`, "Logging in"): discovery,
//! preselection, parsing and saving through `reviewer_models`,
//! `set_reviewer` and the terminal prompt, against isolated homes. No
//! network beyond loopback, no other extension started.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, Cursor, ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError, Weak, atomic, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use config::{Secret, store_credential};
use contract::clock::{Clock, Wake};

use crate::CliError;
use extensions::{Browser, Providers};
use fakes::{ProviderServer, Response};
use serde_json::{Value, json};

use super::{Pick, ReviewerModels, prompt_choice, reviewer_models, set_reviewer};
use crate::test_support::write_record;

/// How long a test waits for one discovery callback.
const WAIT: Duration = Duration::from_secs(10);

const SECRET: &str = "sk-live-7f3a9c0d1e2b";

struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        Self {
            root: fakes::TempDir::new("fiber-login-reviewer"),
        }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn clock(&self) -> Arc<dyn contract::clock::Clock> {
        fakes::clock::FakeClock::new()
    }

    /// Installs `extension` with one provider `name`: `data` as its data
    /// file, `urls` as the manifest's declared endpoints, and `lua` as
    /// its optional provider file.
    fn install(&self, extension: &str, name: &str, data: Value, urls: &[&str], lua: Option<&str>) {
        let dir = self.home().join("extensions").join(extension);
        fs::create_dir_all(dir.join("providers")).unwrap();
        let mut manifest =
            json!({"name": extension, "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
        manifest["providers"] = json!({name: urls});
        fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
        write_record(&dir);
        fs::write(
            dir.join("providers").join(format!("{name}.json")),
            data.to_string(),
        )
        .unwrap();
        if let Some(lua) = lua {
            fs::write(dir.join("providers").join(format!("{name}.lua")), lua).unwrap();
        }
    }

    /// Installs a data-only provider with `models`.
    fn install_data(&self, name: &str, models: &[&str]) {
        self.install(
            name,
            name,
            json!({"name": name, "models": model_list(models)}),
            &["http://x/v1"],
            None,
        );
    }

    fn providers(&self) -> Providers {
        let (providers, notices) = Providers::load(&self.home()).unwrap();
        assert!(notices.is_empty(), "{notices:?}");
        providers
    }

    /// Stores `SECRET` for `stored` under `default`, as a login would.
    fn store(&self, stored: &str) {
        store_credential(
            &self.home(),
            stored,
            "default",
            &Secret::new(SECRET.to_owned()),
        )
        .unwrap();
    }

    fn choices(&self, name: &str) -> ReviewerModels {
        let providers = self.providers();
        reviewer_models(&self.home(), &providers, name, self.clock()).unwrap()
    }

    fn global(&self) -> Value {
        let text = fs::read_to_string(self.home().join("config.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }
}

fn model_data(id: &str) -> Value {
    json!({"id": id, "protocol": "openai-responses", "base_url": "http://x/v1", "context_window": 1000})
}

fn model_list(ids: &[&str]) -> Value {
    Value::Array(ids.iter().map(|id| model_data(id)).collect())
}

/// A `models()` provider file returning `body` as its list.
fn models_lua(name: &str, body: &str) -> String {
    format!(
        "fiber.provider(\"{name}\", {{models = {{timeout = 5000, run = function() {body} end}}}})\n"
    )
}

/// A `models()` body returning these IDs.
fn returns(ids: &[&str]) -> String {
    let items: Vec<String> = ids
        .iter()
        .map(|id| {
            format!(
                "{{id = \"{id}\", protocol = \"openai-responses\", \
                 base_url = \"http://x/v1\", context_window = 1000}}"
            )
        })
        .collect();
    format!("return {{{}}}", items.join(", "))
}

/// A `models()` body pinging `ping` before returning `ids`.
fn ping_then(url: &str, ping: &str, ids: &[&str]) -> String {
    format!("host.http({{url = \"{url}{ping}\"}}) {}", returns(ids))
}

fn ok() -> Response {
    Response::status(200, "{}")
}

#[test]
fn the_saved_global_value_is_preselected_over_the_package_default() {
    let setup = Setup::new();
    setup.install_data("acme", &["m1", "m2"]);
    fs::write(
        setup.home().join("config.json"),
        r#"{"providers": {"acme": {"reviewer_model": "saved-m"}}}"#,
    )
    .unwrap();
    let choices = setup.choices("acme");
    assert_eq!(choices.models, ["m1", "m2"]);
    assert_eq!(choices.selected.as_deref(), Some("saved-m"));
    assert!(choices.notices.is_empty(), "{:?}", choices.notices);
}

#[test]
fn without_a_saved_value_the_package_default_is_preselected() {
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        json!({
            "name": "acme",
            "reviewer_model": "pkg-m",
            "models": model_list(&["m1", "m2"]),
        }),
        &["http://x/v1"],
        None,
    );
    let choices = setup.choices("acme");
    assert_eq!(choices.models, ["m1", "m2"]);
    assert_eq!(choices.selected.as_deref(), Some("pkg-m"));
}

#[test]
fn with_neither_saved_nor_default_nothing_is_preselected() {
    let setup = Setup::new();
    setup.install_data("acme", &["m1", "m2"]);
    let choices = setup.choices("acme");
    assert_eq!(choices.models, ["m1", "m2"]);
    assert_eq!(choices.selected, None);
}

#[test]
fn neither_the_override_nor_a_project_value_preselects() {
    // Preselection reads the global key being edited: the every-provider
    // override and the project layer never stand in for it.
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        json!({
            "name": "acme",
            "reviewer_model": "pkg-m",
            "models": model_list(&["m1", "m2"]),
        }),
        &["http://x/v1"],
        None,
    );
    fs::create_dir_all(setup.home().join("projects/test")).unwrap();
    fs::write(
        setup.home().join("projects/test/config.json"),
        r#"{"reviewer": {"model": "all/x"}, "providers": {"acme": {"reviewer_model": "project-m"}}}"#,
    )
    .unwrap();
    let choices = setup.choices("acme");
    assert_eq!(choices.selected.as_deref(), Some("pkg-m"));
}

#[test]
fn a_stale_saved_pick_is_kept_and_enter_keeps_it() {
    let setup = Setup::new();
    setup.install_data("acme", &["m1", "m2"]);
    fs::write(
        setup.home().join("config.json"),
        r#"{"providers": {"acme": {"reviewer_model": "gone-m"}}}"#,
    )
    .unwrap();
    let choices = setup.choices("acme");
    assert_eq!(choices.models, ["m1", "m2"], "the list is never padded");
    assert_eq!(choices.selected.as_deref(), Some("gone-m"));
    let (pick, out) = ask("acme", &choices, "\n");
    assert_eq!(pick, Pick::Chosen("gone-m".to_owned()));
    assert!(
        out.contains("Saved reviewer `gone-m` is not listed; Enter keeps it."),
        "{out}"
    );
}

#[test]
fn an_empty_data_list_skips_with_a_provider_naming_notice() {
    let setup = Setup::new();
    setup.install_data("acme", &[]);
    fs::write(
        setup.home().join("config.json"),
        r#"{"providers": {"acme": {"reviewer_model": "saved-m"}}}"#,
    )
    .unwrap();
    let before = fs::read(setup.home().join("config.json")).unwrap();
    let choices = setup.choices("acme");
    assert!(choices.models.is_empty());
    assert_eq!(choices.selected.as_deref(), Some("saved-m"));
    assert_eq!(choices.notices.len(), 1);
    assert!(
        choices.notices[0].contains("`acme`"),
        "the notice names the provider: {}",
        choices.notices[0]
    );
    let (pick, _) = ask("acme", &choices, "1\n");
    assert_eq!(pick, Pick::Cancelled, "an empty list asks nothing");
    assert_eq!(
        fs::read(setup.home().join("config.json")).unwrap(),
        before,
        "skipping writes no reviewer"
    );
}

#[test]
fn an_uncached_discovery_runs_once_caches_and_serves_the_cache() {
    let server = ProviderServer::start([ok(), ok()]).unwrap();
    let url = server.url();
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        json!({"name": "acme", "models": [model_data("data-m")]}),
        &["http://x/v1"],
        Some(&models_lua(
            "acme",
            &ping_then(&url, "/models", &["live-m"]),
        )),
    );
    setup.store("acme");
    assert!(
        config::read_model_cache(&setup.home(), "acme")
            .unwrap()
            .is_none(),
        "no cached copy before the first discovery"
    );
    let first = setup.choices("acme");
    assert_eq!(first.models, ["live-m"]);
    assert!(
        server.await_requests(1, fakes::Deadline::after(WAIT)),
        "the callback ran for the uncached list"
    );
    let cached = config::read_model_cache(&setup.home(), "acme").unwrap();
    assert!(cached.is_some_and(|models| models.iter().any(|m| m.id == "live-m")));
    let second = setup.choices("acme");
    assert_eq!(second.models, ["live-m"]);
    assert_eq!(
        server.requests().len(),
        1,
        "the cache answers without a second callback, and no refresher runs one"
    );
}

#[test]
fn a_cached_list_answers_without_running_the_callback() {
    let server = ProviderServer::start([ok()]).unwrap();
    let url = server.url();
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        json!({"name": "acme", "models": [model_data("data-m")]}),
        &["http://x/v1"],
        Some(&models_lua(
            "acme",
            &ping_then(&url, "/models", &["live-m"]),
        )),
    );
    config::write_model_cache(
        &setup.home(),
        "acme",
        &json!([{"id": "cached-m", "protocol": "openai-responses",
                 "base_url": "http://x/v1", "context_window": 1000}]),
    )
    .unwrap();
    let choices = setup.choices("acme");
    assert_eq!(choices.models, ["cached-m"]);
    assert!(
        server.requests().is_empty(),
        "the cached copy is served without Lua"
    );
}

#[test]
fn without_a_credential_discovery_never_runs_and_data_stands() {
    let server = ProviderServer::start([ok()]).unwrap();
    let url = server.url();
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        json!({"name": "acme", "models": [model_data("data-m")]}),
        &["http://x/v1"],
        Some(&models_lua(
            "acme",
            &ping_then(&url, "/models", &["live-m"]),
        )),
    );
    // No stored credential: the callback must not run.
    let choices = setup.choices("acme");
    assert_eq!(choices.models, ["data-m"]);
    assert!(choices.notices.is_empty(), "{:?}", choices.notices);
    assert!(
        server.requests().is_empty(),
        "no credential, no discovery callback"
    );
}

#[test]
fn a_returned_empty_list_writes_its_cache_and_skips() {
    let server = ProviderServer::start([ok()]).unwrap();
    let url = server.url();
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        json!({"name": "acme", "models": [model_data("data-m")]}),
        &["http://x/v1"],
        Some(&models_lua("acme", &ping_then(&url, "/models", &[]))),
    );
    setup.store("acme");
    let choices = setup.choices("acme");
    assert!(choices.models.is_empty());
    assert!(
        server.await_requests(1, fakes::Deadline::after(WAIT)),
        "the empty discovery still ran"
    );
    let cached = config::read_model_cache(&setup.home(), "acme").unwrap();
    assert!(
        cached.is_some_and(|models| models.is_empty()),
        "even an empty discovery writes the provider's cache"
    );
    assert_eq!(choices.notices.len(), 1);
    assert!(
        choices.notices[0].contains("`acme`"),
        "{}",
        choices.notices[0]
    );
}

#[test]
fn a_failing_callback_keeps_the_data_list_behind_a_redacted_notice() {
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        json!({"name": "acme", "models": [model_data("data-m")]}),
        &["http://x/v1"],
        Some(&models_lua("acme", "error('boom-marker-1')")),
    );
    setup.store("acme");
    let choices = setup.choices("acme");
    assert_eq!(choices.models, ["data-m"]);
    assert_eq!(choices.notices.len(), 1);
    assert!(
        choices.notices[0].contains("`acme`"),
        "the notice names the provider and its extension: {}",
        choices.notices[0]
    );
    assert!(
        !choices.notices[0].contains("boom-marker-1"),
        "no callback text leaks: {}",
        choices.notices[0]
    );
    assert!(
        config::read_model_cache(&setup.home(), "acme")
            .unwrap()
            .is_none(),
        "a failed discovery caches nothing"
    );
}

#[test]
fn a_malformed_callback_return_is_redacted_the_same_way() {
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        json!({"name": "acme", "models": [model_data("data-m")]}),
        &["http://x/v1"],
        Some(&models_lua("acme", "return 'oops-not-a-list'")),
    );
    setup.store("acme");
    let choices = setup.choices("acme");
    assert_eq!(choices.models, ["data-m"]);
    assert_eq!(choices.notices.len(), 1);
    for forbidden in ["oops-not-a-list", "something other than a model list"] {
        assert!(
            !choices.notices[0].contains(forbidden),
            "no raw return text leaks: {}",
            choices.notices[0]
        );
    }
}

#[test]
fn an_invalid_discovered_model_is_left_out_with_its_typed_notice() {
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        json!({"name": "acme", "models": []}),
        &["http://x/v1"],
        Some(&models_lua(
            "acme",
            &format!(
                "return {{{}, {}}}",
                "{id = \"good-m\", protocol = \"openai-responses\", \
                 base_url = \"http://x/v1\", context_window = 1000}",
                "{id = \"bad-m\", protocol = \"openai-responses\", \
                 base_url = \"http://x/v1\", context_window = 1000, \
                 web_search = \"web_search_20250305\"}",
            ),
        )),
    );
    setup.store("acme");
    let choices = setup.choices("acme");
    assert_eq!(
        choices.models,
        ["good-m"],
        "the invalid entry is not selectable"
    );
    assert_eq!(choices.notices.len(), 1);
    assert!(
        choices.notices[0].contains("left out a model"),
        "the model_invalid classification is retained: {}",
        choices.notices[0]
    );
    for forbidden in ["bad-m", "web_search_20250305"] {
        assert!(
            !choices.notices[0].contains(forbidden),
            "no entry detail leaks: {}",
            choices.notices[0]
        );
    }
}

#[test]
fn only_the_admitted_owners_endpoints_are_listed() {
    // Two packages declare `p`; `bbb` loads last and owns it. Discovery
    // through `bbb` admits its endpoint and rejects `aaa`'s.
    let setup = Setup::new();
    setup.install(
        "aaa",
        "p",
        json!({"name": "p", "models": [{
            "id": "m-a", "protocol": "openai-responses",
            "base_url": "https://a.invalid/v1", "context_window": 1000}]}),
        &["https://a.invalid/v1"],
        None,
    );
    setup.install(
        "bbb",
        "p",
        json!({"name": "p", "models": [{
            "id": "m-b", "protocol": "openai-responses",
            "base_url": "https://b.invalid/v1", "context_window": 1000}]}),
        &["https://b.invalid/v1"],
        Some(&models_lua(
            "p",
            &format!(
                "return {{{}, {}}}",
                "{id = \"m-b\", protocol = \"openai-responses\", \
                 base_url = \"https://b.invalid/v1\", context_window = 1000}",
                "{id = \"m-a\", protocol = \"openai-responses\", \
                 base_url = \"https://a.invalid/v1\", context_window = 1000}",
            ),
        )),
    );
    setup.store("p");
    let providers = setup.providers();
    assert_eq!(
        providers.extension_of("p"),
        Some("bbb"),
        "owner bbb is retained"
    );
    let choices = reviewer_models(&setup.home(), &providers, "p", setup.clock()).unwrap();
    assert_eq!(choices.models, ["m-b"]);
    assert_eq!(choices.notices.len(), 1);
    assert!(
        choices.notices[0].contains("`p`") && choices.notices[0].contains("`bbb`"),
        "{}",
        choices.notices[0]
    );
    assert!(
        !choices.notices[0].contains("a.invalid"),
        "no unlisted endpoint leaks: {}",
        choices.notices[0]
    );
}

#[test]
fn only_the_requested_providers_callback_runs() {
    let server = ProviderServer::start([ok(), ok(), ok()]).unwrap();
    let url = server.url();
    let setup = Setup::new();
    let dir = setup.home().join("extensions").join("shared");
    fs::create_dir_all(dir.join("providers")).unwrap();
    let mut manifest = json!({"name": "shared", "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
    manifest["providers"] = json!({"foo": ["http://x/v1"], "bar": ["http://x/v1"]});
    fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
    write_record(&dir);
    for name in ["foo", "bar"] {
        fs::write(
            dir.join("providers").join(format!("{name}.json")),
            json!({"name": name, "models": [model_data("data-m")]}).to_string(),
        )
        .unwrap();
        fs::write(
            dir.join("providers").join(format!("{name}.lua")),
            models_lua(name, &ping_then(&url, &format!("/{name}"), &["live-m"])),
        )
        .unwrap();
    }
    setup.install(
        "other",
        "other",
        json!({"name": "other", "models": [model_data("data-m")]}),
        &["http://x/v1"],
        Some(&models_lua(
            "other",
            &ping_then(&url, "/other", &["live-m"]),
        )),
    );
    setup.store("foo");
    let providers = setup.providers();
    let choices = reviewer_models(&setup.home(), &providers, "foo", setup.clock()).unwrap();
    assert_eq!(choices.models, ["live-m"]);
    assert!(
        server.await_requests(1, fakes::Deadline::after(WAIT)),
        "the requested provider's callback ran"
    );
    assert_eq!(
        server
            .requests()
            .iter()
            .map(|r| r.path.clone())
            .collect::<Vec<_>>(),
        ["/foo"],
        "neither the sibling nor the unrelated extension's callback ran"
    );
}

#[test]
fn a_callback_reading_the_credential_leaks_neither_path_nor_secret() {
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        json!({"name": "acme", "models": [model_data("data-m")]}),
        &["http://x/v1"],
        None,
    );
    setup.store("acme");
    let credential = setup
        .home()
        .join("credentials")
        .join("acme")
        .join("default");
    let path = credential.to_str().unwrap().replace('\\', "/");
    let lua = models_lua("acme", &format!("error(host.fs.read(\"{path}\"))"));
    fs::write(setup.home().join("extensions/acme/providers/acme.lua"), lua).unwrap();
    let providers = setup.providers();
    let choices = reviewer_models(&setup.home(), &providers, "acme", setup.clock()).unwrap();
    assert_eq!(choices.models, ["data-m"], "the data list stands in");
    assert_eq!(choices.notices.len(), 1);
    let shown = format!("{:?}", choices);
    for forbidden in [SECRET, &path] {
        assert!(
            !shown.contains(forbidden),
            "neither the secret nor its path leaks: {shown}"
        );
    }
    assert!(shown.contains("`acme`"), "{shown}");
}

#[test]
fn a_malformed_configuration_is_an_error_not_a_guess() {
    let setup = Setup::new();
    setup.install_data("acme", &["m1"]);
    setup.store("acme");
    let credential_before = fs::read(setup.home().join("credentials/acme/default")).unwrap();
    fs::write(setup.home().join("config.json"), "{no json").unwrap();
    let providers = setup.providers();
    let failure = reviewer_models(&setup.home(), &providers, "acme", setup.clock()).unwrap_err();
    assert!(matches!(failure, CliError::Config(_)), "{failure:?}");
    assert_eq!(
        fs::read(setup.home().join("credentials/acme/default")).unwrap(),
        credential_before,
        "the stored credential stays"
    );
}

#[test]
fn an_unknown_provider_is_a_usage_error() {
    let setup = Setup::new();
    setup.install_data("acme", &["m1"]);
    let providers = setup.providers();
    let failure = reviewer_models(&setup.home(), &providers, "nope", setup.clock()).unwrap_err();
    assert!(
        matches!(failure, CliError::ProviderNotInstalled { .. }),
        "{failure:?}"
    );
}

/// Asks `input` of `choices` on a pipe, returning the pick and the terminal text.
fn ask(name: &str, choices: &ReviewerModels, input: &str) -> (Pick, String) {
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let stdin = std::io::stdin();
    let mut err = Vec::new();
    let pick = prompt_choice(
        name,
        choices,
        &mut Cursor::new(input.as_bytes().to_vec()),
        &mut err,
        &|| false,
        false,
        &clock,
        stdin.as_fd(),
    )
    .unwrap();
    (pick, String::from_utf8(err).unwrap())
}

fn listed() -> ReviewerModels {
    ReviewerModels {
        models: ["m1".to_owned(), "org/fast".to_owned(), "v:2".to_owned()].to_vec(),
        selected: None,
        notices: Vec::new(),
    }
}

#[test]
fn the_prompt_lists_numbers_names_and_slash_colon_ids() {
    for (input, expected) in [
        ("1\n", "m1"),
        ("2\n", "org/fast"),
        ("3\n", "v:2"),
        ("m1\n", "m1"),
        ("org/fast\n", "org/fast"),
        ("v:2\n", "v:2"),
        ("  2  \n", "org/fast"),
    ] {
        let (pick, out) = ask("acme", &listed(), input);
        assert_eq!(pick, Pick::Chosen(expected.to_owned()), "{input:?}");
        assert!(out.contains("Reviewer for acme:"), "{out}");
        assert!(out.contains("1) m1"), "{out}");
    }
}

#[test]
fn invalid_answers_ask_again_until_a_listed_choice_eof_or_cancel() {
    // Each bad line prints one problem line and asks again; the good line
    // then lands.
    let (pick, out) = ask(
        "acme",
        &listed(),
        "0\n99\nnope\n99999999999999999999999\n\n2\n",
    );
    assert_eq!(pick, Pick::Chosen("org/fast".to_owned()));
    assert_eq!(out.matches("is not a listed reviewer").count(), 4, "{out}");
    assert_eq!(
        out.matches("No reviewer is selected; choose a number or name.")
            .count(),
        1,
        "{out}"
    );
    assert_eq!(
        out.matches("Reviewer, by number or name: ").count(),
        6,
        "every retry re-prompts: {out}"
    );
    let (pick, _) = ask("acme", &listed(), "");
    assert_eq!(pick, Pick::Cancelled, "EOF saves nothing");
    let mut err = Vec::new();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let stdin = std::io::stdin();
    let pick = prompt_choice(
        "acme",
        &listed(),
        &mut Cursor::new(b"1\n".to_vec()),
        &mut err,
        &|| true,
        false,
        &clock,
        stdin.as_fd(),
    )
    .unwrap();
    assert_eq!(pick, Pick::Cancelled, "cancellation saves nothing");
}

#[test]
fn enter_keeps_the_preselected_row() {
    let mut choices = listed();
    choices.selected = Some("org/fast".to_owned());
    let (pick, out) = ask("acme", &choices, "\n");
    assert_eq!(pick, Pick::Chosen("org/fast".to_owned()));
    assert!(out.contains("2) org/fast (saved)"), "{out}");
}

#[test]
fn saving_targets_the_provider_name_not_its_credential_name() {
    let setup = Setup::new();
    setup.install(
        "acme",
        "acme",
        json!({
            "name": "acme",
            "credential_name": "shared",
            "models": [model_data("m1")],
        }),
        &["http://x/v1"],
        None,
    );
    setup.install_data("other", &["m1"]);
    set_reviewer(&setup.home(), "acme", "org/fast").unwrap();
    set_reviewer(&setup.home(), "other", "m1").unwrap();
    assert_eq!(
        setup.global(),
        json!({"providers": {
            "acme": {"reviewer_model": "org/fast"},
            "other": {"reviewer_model": "m1"},
        }}),
        "each provider keeps its own pick as the exact local ID"
    );
}

#[test]
fn saving_keeps_unrelated_configuration() {
    let setup = Setup::new();
    setup.install_data("acme", &["m1"]);
    fs::write(
        setup.home().join("config.json"),
        r#"{"reviewer": {"model": "all/a"},
            "providers": {"acme": {"credential": "default"}}}"#,
    )
    .unwrap();
    set_reviewer(&setup.home(), "acme", "m1").unwrap();
    assert_eq!(
        setup.global(),
        json!({"reviewer": {"model": "all/a"},
               "providers": {"acme": {"credential": "default", "reviewer_model": "m1"}}}),
    );
}

#[test]
fn a_dotted_provider_name_stays_one_path_segment() {
    let setup = Setup::new();
    setup.install_data("acme", &["m1"]);
    set_reviewer(&setup.home(), "acme.corp", "m1").unwrap();
    assert_eq!(
        setup.global(),
        json!({"providers": {"acme.corp": {"reviewer_model": "m1"}}}),
    );
}

#[test]
fn a_name_the_path_syntax_cannot_hold_is_refused_before_any_write() {
    let setup = Setup::new();
    setup.install_data("acme", &["m1"]);
    setup.store("acme");
    fs::write(
        setup.home().join("config.json"),
        r#"{"providers": {"acme": {"credential": "default"}}}"#,
    )
    .unwrap();
    let before = fs::read(setup.home().join("config.json")).unwrap();
    let credential_before = fs::read(setup.home().join("credentials/acme/default")).unwrap();
    let failure = set_reviewer(&setup.home(), "a.\"b", "m1").unwrap_err();
    assert!(
        matches!(failure, CliError::ReviewerNameUnrepresentable { .. }),
        "{failure:?}"
    );
    assert_eq!(fs::read(setup.home().join("config.json")).unwrap(), before);
    assert_eq!(
        fs::read(setup.home().join("credentials/acme/default")).unwrap(),
        credential_before
    );
}

#[test]
fn a_broken_existing_document_fails_naming_the_key_and_keeps_the_credential() {
    for existing in ["[]", "{no json"] {
        let setup = Setup::new();
        setup.install_data("acme", &["m1"]);
        setup.store("acme");
        let credential_before = fs::read(setup.home().join("credentials/acme/default")).unwrap();
        fs::write(setup.home().join("config.json"), existing).unwrap();
        let failure = set_reviewer(&setup.home(), "acme", "m1").unwrap_err();
        assert!(
            matches!(failure, CliError::ReviewerNotSaved { .. }),
            "{failure:?}"
        );
        assert!(
            failure
                .to_string()
                .contains("providers.acme.reviewer_model"),
            "{failure:?}"
        );
        assert_eq!(
            fs::read(setup.home().join("credentials/acme/default")).unwrap(),
            credential_before,
            "{existing}: the stored credential stays"
        );
    }
}

#[test]
fn a_provider_name_that_parses_to_the_wrong_segments_is_refused_before_any_write() {
    // `a"."b` displays as `providers."a"."b".reviewer_model`, which parses
    // to four segments instead of the key's three: the guard must refuse
    // it, not accept every path the parser returns.
    let setup = Setup::new();
    setup.install_data("acme", &["m1"]);
    fs::write(
        setup.home().join("config.json"),
        r#"{"providers": {"acme": {"reviewer_model": "old-m"}}}"#,
    )
    .unwrap();
    let before = fs::read(setup.home().join("config.json")).unwrap();
    let failure = set_reviewer(&setup.home(), "a\".\"b", "m1").unwrap_err();
    assert!(
        matches!(failure, CliError::ReviewerNameUnrepresentable { .. }),
        "{failure:?}"
    );
    assert_eq!(
        fs::read(setup.home().join("config.json")).unwrap(),
        before,
        "a refused name writes nothing"
    );
}

#[test]
fn the_discovery_browser_reports_unattended() {
    // Discovery never opens a browser: the seam answers unattended, so a
    // callback that branches on it takes the headless path.
    assert!(!super::NoBrowser.attended());
}

/// What `run_command` sleeps between child polls: every gated sleep is this.
const COMMAND_POLL: Duration = Duration::from_millis(10);

/// How long one gated wait lasts.
const GATE_WAIT: Duration = Duration::from_secs(10);

/// How long the guarded cleanup waits for the gated group to empty and
/// for its watchdog to stand down.
const GROUP_WAIT: Duration = Duration::from_secs(10);

/// A clock the test advances by hand: each sleep signals its length, then
/// waits for the test's advance, so time moves only after the operation
/// shows it is waiting on it. Past the test's disconnect the requested
/// sleep lands at once, so a stranded worker still ends instead of hanging
/// the suite.
struct GateClock {
    origin: Instant,
    offset: Mutex<Duration>,
    slept: Mutex<mpsc::Sender<Duration>>,
    release: Mutex<mpsc::Receiver<Duration>>,
}

impl GateClock {
    fn clock(
        slept: mpsc::Sender<Duration>,
        release: mpsc::Receiver<Duration>,
    ) -> Arc<dyn contract::clock::Clock> {
        Arc::new(Self {
            origin: fakes::clock::FakeClock::new().origin(),
            offset: Mutex::new(Duration::ZERO),
            slept: Mutex::new(slept),
            release: Mutex::new(release),
        })
    }
}

impl contract::clock::Clock for GateClock {
    fn now(&self) -> Instant {
        let offset = *self.offset.lock().unwrap_or_else(PoisonError::into_inner);
        self.origin.checked_add(offset).unwrap_or(self.origin)
    }

    fn wall(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }

    fn sleep(&self, duration: Duration) {
        self.slept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(duration)
            .unwrap_or(());
        let advance = self
            .release
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .recv()
            .unwrap_or(duration);
        *self.offset.lock().unwrap_or_else(PoisonError::into_inner) += advance;
    }

    fn wait_until(&self, _until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        wait(Some(Duration::ZERO));
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

/// A `sleep 5` child under the gated clock: `slept` signals each wait,
/// `release` advances it, and `done` carries the outcome. The child
/// outlives every correct run's kill, so a worker the test lets go still
/// ends on its own past the disconnect.
///
/// The child joins a group led by a `sleep 300` the test owns, under a
/// `fakes::Watchdog`: `finish` kills that group and reaps the leader on
/// the passing path, and `Drop` kills it when an assertion unwinds past
/// `finish`, so a failed test cannot leak either sleep. When the test
/// process itself dies, the watchdog kills the group.
struct Gated {
    clock: Arc<dyn contract::clock::Clock>,
    slept: mpsc::Receiver<Duration>,
    release: mpsc::Sender<Duration>,
    done: mpsc::Receiver<Result<(), ErrorKind>>,
    worker: Option<thread::JoinHandle<()>>,
    start: Instant,
    group: u32,
    watchdog: Option<fakes::Watchdog>,
    leader: Option<std::process::Child>,
}

/// Owns the gated group leader between its spawn and `Gated` taking it:
/// dropping it SIGKILLs the group and reaps the leader, so a panic before
/// `Gated` owns the leader (notably a `Watchdog::group` panic) cannot leak
/// the `sleep 300`. Disarmed by taking the leader out once `Gated` owns it.
/// Never panics, so dropping while unwinding cannot abort.
struct LeaderOwner {
    group: u32,
    leader: Option<std::process::Child>,
}

impl Drop for LeaderOwner {
    fn drop(&mut self) {
        if let Some(leader) = self.leader.take() {
            if self.group > 1 {
                match fakes::kill_group(self.group, "KILL") {
                    Ok(_) | Err(_) => {}
                }
            }
            let (waited, reaped) = mpsc::channel();
            let spawned = thread::Builder::new()
                .name("gated-leader-reap".to_owned())
                .spawn(move || {
                    let mut leader = leader;
                    let _sent = waited.send(leader.wait());
                });
            // A reaper that could not start sends nothing: skip the wait
            // instead of blocking the full bound on a dead channel.
            if spawned.is_ok() {
                match fakes::Deadline::after(GROUP_WAIT).recv(&reaped) {
                    Ok(_) | Err(_) => {}
                }
            }
        }
    }
}

impl Gated {
    fn run(timeout: Duration) -> Self {
        let (slept_tx, slept) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let clock = GateClock::clock(slept_tx, release_rx);
        let mut group_leader = Command::new("sleep");
        group_leader.arg("300");
        group_leader.process_group(0);
        let leader = group_leader
            .spawn()
            .expect("the gated group leader started");
        let group = leader.id();
        let mut owner = LeaderOwner {
            group,
            leader: Some(leader),
        };
        let watchdog = fakes::Watchdog::group(group);
        let worker_clock = Arc::clone(&clock);
        let start = clock.now();
        // `None` when the OS refuses the thread: the `done` sender drops
        // with the closure, so the tests fail on their bounded deadline
        // instead of panicking here while `LeaderOwner` owns the leader.
        let worker = thread::Builder::new()
            .name("gated-command".to_owned())
            .spawn(move || {
                let mut command = Command::new("sleep");
                command.arg("5");
                let group: i32 = match group.try_into() {
                    Ok(group) => group,
                    Err(_) => {
                        done_tx.send(Err(ErrorKind::InvalidInput)).unwrap_or(());
                        return;
                    }
                };
                command.process_group(group);
                let outcome = super::run_command(&mut command, timeout, &worker_clock);
                done_tx
                    .send(outcome.map(|_| ()).map_err(|e| e.kind()))
                    .unwrap_or(());
            })
            .ok();
        Self {
            clock,
            slept,
            release,
            done,
            worker,
            start,
            group,
            watchdog: Some(watchdog),
            leader: owner.leader.take(),
        }
    }

    /// Takes the next sleep signal and advances it by `advance`.
    fn advance(&self, within: &fakes::Deadline, advance: Duration) {
        let slept = within.recv_or_fail(&self.slept, "a sleep while the child runs");
        assert_eq!(
            slept, COMMAND_POLL,
            "run_command polls every {COMMAND_POLL:?}"
        );
        self.release.send(advance).unwrap();
    }

    fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.join().expect("the command worker joined");
        }
    }

    /// Joins the worker, then kills the gated group and reaps the leader:
    /// nothing the test started may outlive the passing path. The worker
    /// already reaped the `sleep 5` child; the SIGKILL below takes the
    /// leader, through the guarded helper, because the fixtures ignore
    /// SIGTERM on purpose.
    fn finish(&mut self) {
        self.join();
        let group = self.group;
        if group > 1 {
            match fakes::kill_group(group, "KILL") {
                Ok(_) | Err(_) => {}
            }
        }
        if let Some(leader) = self.leader.take() {
            let (waited, reaped) = mpsc::channel();
            let spawned = thread::Builder::new()
                .name("gated-leader-reap".to_owned())
                .spawn(move || {
                    let mut leader = leader;
                    let _sent = waited.send(leader.wait());
                });
            // A reaper that could not start sends nothing: fail fast
            // instead of waiting the full bound on a dead channel.
            let reaped =
                spawned.is_ok() && fakes::Deadline::after(GROUP_WAIT).recv(&reaped).is_ok();
            assert!(
                reaped,
                "waited {GROUP_WAIT:?} for the gated group leader to be reaped"
            );
        }
        assert!(
            fakes::group_empties(group, fakes::Deadline::after(GROUP_WAIT)),
            "the gated group {group} emptied within {GROUP_WAIT:?}"
        );
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.stand_down(fakes::Deadline::after(GROUP_WAIT));
        }
    }
}

impl Drop for Gated {
    fn drop(&mut self) {
        // An assertion unwound past `finish`: SIGKILL the group so
        // neither sleep leaks. The watchdog kills it too when the test
        // process dies. This never blocks or fails: a drop that panics
        // while unwinding aborts the test process.
        if (self.leader.is_some() || self.watchdog.is_some()) && self.group > 1 {
            match fakes::kill_group(self.group, "KILL") {
                Ok(_) | Err(_) => {}
            }
        }
    }
}

#[test]
fn run_command_times_out_after_the_deadline_not_before() {
    // Nine 10 ms advances leave 90 ms on a 100 ms deadline: the worker is
    // still waiting, with no result. The tenth lands exactly on it.
    let within = fakes::Deadline::after(GATE_WAIT);
    let timeout = Duration::from_millis(100);
    let mut gated = Gated::run(timeout);
    for _ in 0..9 {
        gated.advance(&within, COMMAND_POLL);
        assert!(
            gated.done.try_recv().is_err(),
            "no timeout before the deadline"
        );
    }
    gated.advance(&within, COMMAND_POLL);
    let outcome = within.recv_or_fail(&gated.done, "the timeout at the deadline");
    assert_eq!(outcome.unwrap_err(), ErrorKind::TimedOut);
    assert_eq!(
        gated.clock.now().duration_since(gated.start),
        timeout,
        "the timeout fires at exactly the deadline"
    );
    gated.finish();
}

#[test]
fn run_command_keeps_waiting_before_the_deadline() {
    // Thirty milliseconds in, the child is still awaited: another sleep
    // arrives and no result does. A jump past the deadline then ends it.
    let within = fakes::Deadline::after(GATE_WAIT);
    let mut gated = Gated::run(Duration::from_millis(100));
    for _ in 0..3 {
        gated.advance(&within, COMMAND_POLL);
    }
    assert!(
        gated.done.try_recv().is_err(),
        "no timeout before the deadline"
    );
    gated.advance(&within, Duration::from_secs(1));
    let outcome = within.recv_or_fail(&gated.done, "the timeout past the deadline");
    assert_eq!(outcome.unwrap_err(), ErrorKind::TimedOut);
    gated.finish();
}

#[test]
fn run_command_fires_just_past_a_near_boundary() {
    // Twenty milliseconds is under a 25 ms deadline, so the child is still
    // awaited; the next 10 ms slice crosses it and the timeout fires.
    let within = fakes::Deadline::after(GATE_WAIT);
    let mut gated = Gated::run(Duration::from_millis(25));
    gated.advance(&within, COMMAND_POLL);
    gated.advance(&within, COMMAND_POLL);
    assert!(
        gated.done.try_recv().is_err(),
        "just under the deadline the child is still awaited"
    );
    gated.advance(&within, COMMAND_POLL);
    let outcome = within.recv_or_fail(&gated.done, "the timeout just past the deadline");
    assert_eq!(outcome.unwrap_err(), ErrorKind::TimedOut);
    assert_eq!(
        gated.clock.now().duration_since(gated.start),
        Duration::from_millis(30)
    );
    gated.finish();
}

#[test]
fn dropping_the_stdin_guard_restores_its_flags() {
    // A real PTY slave: nonblocking goes on under the guard and the saved
    // flags come back when it drops.
    let (_master, slave_path) = fakes::pty::open().unwrap();
    let slave = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&slave_path)
        .unwrap();
    let before = rustix::fs::fcntl_getfl(&slave).unwrap();
    assert!(!before.contains(rustix::fs::OFlags::NONBLOCK));
    {
        let _guard = super::NonblockingStdin::open(slave.as_fd()).unwrap();
        assert!(
            rustix::fs::fcntl_getfl(&slave)
                .unwrap()
                .contains(rustix::fs::OFlags::NONBLOCK)
        );
    }
    assert_eq!(rustix::fs::fcntl_getfl(&slave).unwrap(), before);
}

#[test]
fn waiting_for_input_blocks_for_the_poll_budget() {
    // The slave stays open with nothing written: no event is ready, so the
    // wait lasts the whole budget on the fake clock.
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let (master, slave_path) = fakes::pty::open().unwrap();
    let _slave = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&slave_path)
        .unwrap();
    let start = clock.now();
    super::wait_ready(&master, &clock).unwrap();
    assert_eq!(
        clock.now().duration_since(start),
        super::PROMPT_POLL,
        "the wait blocks for the whole budget"
    );
}

#[test]
fn ready_input_returns_before_the_poll_budget() {
    // A byte on the slave readies the master at once: the wait ends
    // without sleeping the clock.
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let (master, slave_path) = fakes::pty::open().unwrap();
    let mut slave = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&slave_path)
        .unwrap();
    slave.write_all(b"x").unwrap();
    let start = clock.now();
    super::wait_ready(&master, &clock).unwrap();
    assert_eq!(clock.now().duration_since(start), Duration::ZERO);
}

/// One scripted terminal read.
enum Step {
    /// `WouldBlock`: the prompt waits and reads on.
    Block,
    /// `Interrupted`: the prompt reads on.
    Intr,
    /// Any other error: the prompt surfaces it.
    Fail,
    /// A line, newline included.
    Line(&'static str),
}

/// A terminal reader playing `steps` in order, then EOF.
struct Script {
    steps: VecDeque<Step>,
}

impl std::io::Read for Script {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Ok(0)
    }
}

impl BufRead for Script {
    fn read_line(&mut self, buf: &mut String) -> std::io::Result<usize> {
        match self.steps.pop_front() {
            Some(Step::Block) => Err(std::io::Error::new(ErrorKind::WouldBlock, "busy")),
            Some(Step::Intr) => Err(std::io::Error::new(ErrorKind::Interrupted, "cut in")),
            Some(Step::Fail) => Err(std::io::Error::other("broken")),
            Some(Step::Line(line)) => {
                buf.push_str(line);
                Ok(line.len())
            }
            None => Ok(0),
        }
    }

    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        Ok(&[])
    }

    fn consume(&mut self, _amount: usize) {}
}

/// Asks `choices` on the terminal through `steps`, returning the outcome
/// and the terminal text.
fn ask_term(choices: &ReviewerModels, steps: Vec<Step>) -> (std::io::Result<Pick>, String) {
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let (_master, slave_path) = fakes::pty::open().unwrap();
    let slave = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&slave_path)
        .unwrap();
    let mut err = Vec::new();
    let mut input = Script {
        steps: steps.into_iter().collect(),
    };
    let pick = prompt_choice(
        "acme",
        choices,
        &mut input,
        &mut err,
        &|| false,
        true,
        &clock,
        slave.as_fd(),
    );
    (pick, String::from_utf8(err).unwrap())
}

#[test]
fn a_would_block_retries_until_a_listed_answer() {
    // A would-block is not an answer: the prompt waits on the terminal and
    // reads the listed number next.
    let (pick, out) = ask_term(&listed(), vec![Step::Block, Step::Line("2\n")]);
    assert_eq!(pick.unwrap(), Pick::Chosen("org/fast".to_owned()));
    assert!(out.contains("Reviewer for acme:"), "{out}");
}

#[test]
fn an_interrupted_read_retries_until_a_listed_answer() {
    let (pick, _) = ask_term(&listed(), vec![Step::Intr, Step::Line("1\n")]);
    assert_eq!(pick.unwrap(), Pick::Chosen("m1".to_owned()));
}

#[test]
fn an_unexpected_error_surfaces_instead_of_retrying() {
    // Any other error ends the prompt: it must not retry into the listed
    // answer waiting behind it.
    let (pick, _) = ask_term(&listed(), vec![Step::Fail, Step::Line("1\n")]);
    let error = pick.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Other);
}

/// An `err` writer that fails the first prompt write and the first of each
/// retry message write with `WouldBlock`, then accepts everything: the
/// prompt must wait through each and complete the whole text.
struct WouldBlockThenAccept {
    out: Vec<u8>,
    prompt_blocked: bool,
    retry_blocked: bool,
    blank_blocked: bool,
}

impl Write for WouldBlockThenAccept {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        if !self.prompt_blocked && text.contains("Reviewer, by number or name: ") {
            self.prompt_blocked = true;
            return Err(std::io::Error::new(ErrorKind::WouldBlock, "busy"));
        }
        if !self.retry_blocked && text.contains("is not a listed reviewer") {
            self.retry_blocked = true;
            return Err(std::io::Error::new(ErrorKind::WouldBlock, "busy"));
        }
        if !self.blank_blocked && text.contains("No reviewer is selected") {
            self.blank_blocked = true;
            return Err(std::io::Error::new(ErrorKind::WouldBlock, "busy"));
        }
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_would_block_write_retries_until_the_prompt_completes() {
    // The first prompt write and the first retry message each block once:
    // the prompt waits through both and still writes every byte, then the
    // listed answer lands.
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let (_pipe_read, pipe_write) = nonblocking_pipe();
    let mut input = Script {
        steps: [Step::Line("nope\n"), Step::Line("2\n")]
            .into_iter()
            .collect(),
    };
    let mut err = WouldBlockThenAccept {
        out: Vec::new(),
        prompt_blocked: false,
        retry_blocked: false,
        blank_blocked: true,
    };
    let pick = prompt_choice(
        "acme",
        &listed(),
        &mut input,
        &mut err,
        &|| false,
        true,
        &clock,
        pipe_write.as_fd(),
    )
    .unwrap();
    assert_eq!(pick, Pick::Chosen("org/fast".to_owned()));
    let out = String::from_utf8(err.out).unwrap();
    assert!(
        out.contains("Reviewer, by number or name: "),
        "the prompt lands whole: {out}"
    );
    assert!(
        out.contains("is not a listed reviewer"),
        "the retry message lands whole: {out}"
    );
}

#[test]
fn a_would_block_write_retries_until_the_blank_retry_completes() {
    // A blank answer with nothing preselected prints the "No reviewer is
    // selected" retry: the first write of that message blocks once, the
    // prompt waits through it and still writes every byte, then the listed
    // answer lands.
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let (_pipe_read, pipe_write) = nonblocking_pipe();
    let mut input = Script {
        steps: [Step::Line("\n"), Step::Line("2\n")].into_iter().collect(),
    };
    let mut err = WouldBlockThenAccept {
        out: Vec::new(),
        prompt_blocked: true,
        retry_blocked: true,
        blank_blocked: false,
    };
    let pick = prompt_choice(
        "acme",
        &listed(),
        &mut input,
        &mut err,
        &|| false,
        true,
        &clock,
        pipe_write.as_fd(),
    )
    .unwrap();
    assert_eq!(pick, Pick::Chosen("org/fast".to_owned()));
    let out = String::from_utf8(err.out).unwrap();
    assert!(
        out.contains("No reviewer is selected"),
        "the blank-answer retry lands whole: {out}"
    );
    assert!(
        out.contains("Reviewer, by number or name: "),
        "the prompt lands whole: {out}"
    );
}

/// How long one pipe write-wait test waits for a park or a worker result.
const PIPE_WAIT: Duration = Duration::from_secs(30);

/// Makes a nonblocking pipe: the test fills the write end until it
/// refuses, and the helper under test polls it through `term`.
fn nonblocking_pipe() -> (std::io::PipeReader, std::io::PipeWriter) {
    let (read, write) = std::io::pipe().unwrap();
    for end in [read.as_fd(), write.as_fd()] {
        let flags = rustix::fs::fcntl_getfl(end).unwrap();
        rustix::fs::fcntl_setfl(end, flags.union(rustix::fs::OFlags::NONBLOCK)).unwrap();
    }
    (read, write)
}

/// Fills `write` until a one-byte write refuses with `WouldBlock`, leaving
/// the pipe full with nobody reading. Bounds the fill so a bottomless pipe
/// fails instead of spinning.
fn fill_pipe(write: &mut fs::File) {
    let mut total = 0;
    loop {
        match write.write(&[0x78]) {
            Ok(1) => total += 1,
            Ok(n) => panic!("a one-byte pipe write took {n} bytes"),
            Err(e) if e.kind() == ErrorKind::WouldBlock => return,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => panic!("filling the pipe: {e:?}"),
        }
        assert!(
            total <= 1 << 20,
            "the pipe took {total} bytes without refusing"
        );
    }
}

/// Empties `read` until it refuses or ends: the pipe the helper blocked on
/// has room again. Bounds the drain so a refilling pipe fails instead of
/// spinning.
fn drain_pipe(read: &mut std::io::PipeReader) {
    let mut buf = [0u8; 1024];
    let mut total = 0;
    loop {
        match read.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => total += n,
            Err(e) if e.kind() == ErrorKind::WouldBlock => return,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => panic!("draining the pipe: {e:?}"),
        }
        assert!(
            total <= 1 << 20,
            "the pipe gave {total} bytes without refusing"
        );
    }
}

/// An `err` writer over a pipe that forwards every write, except once
/// when the drain hook arms it: a spurious writable report followed by
/// `WouldBlock`, which must cause another wait instead of spinning or
/// dropping bytes. Every other `WouldBlock` comes from the really-full
/// pipe, so a retry always re-derives the park the driver expects.
/// An `err` writer over a pipe that fakes one spurious `WouldBlock`:
/// the first write attempted while the pipe has room — right after a
/// writable report — refills the pipe and reports `WouldBlock` anyway,
/// so the helper must wait again instead of spinning or surfacing the
/// error. Writes attempted while the pipe is full go through to the
/// really-full pipe, so every retry re-derives the parked instant from
/// real pipe state. The refill's many syscalls also delay the worker
/// past the draining hook's advance, pinning the second park one slice
/// past the first on every interleaving.
struct SpuriousReadyThenFull {
    file: fs::File,
    filler: fs::File,
    faked: bool,
    out: Vec<u8>,
}

impl Write for SpuriousReadyThenFull {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if !self.faked && pipe_has_room(&self.file) {
            self.faked = true;
            fill_pipe(&mut self.filler);
            return Err(std::io::Error::new(ErrorKind::WouldBlock, "busy"));
        }
        let written = self.file.write(buf)?;
        self.out.extend_from_slice(&buf[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Whether `file`'s pipe currently takes a write without refusing: a
/// zero-timeout `OUT` poll, so the spurious fake fires only after a
/// real writable report. Any doubt answers no, leaving the write to
/// meet the pipe itself.
fn pipe_has_room(file: &fs::File) -> bool {
    let mut waiting = [rustix::event::PollFd::new(
        file,
        rustix::event::PollFlags::OUT,
    )];
    match rustix::event::poll(
        &mut waiting,
        Some(&rustix::event::Timespec::try_from(Duration::ZERO).unwrap()),
    ) {
        Ok(_) => waiting
            .first()
            .is_some_and(|polled| !polled.revents().is_empty()),
        Err(_) => false,
    }
}

#[test]
fn a_would_block_after_a_wait_waits_again_until_the_write_completes() {
    // The first write reports WouldBlock with the pipe full, so the
    // helper parks; the hook drains the pipe, earning the writable
    // report the retry follows, but that retry still says WouldBlock —
    // the writer refilled the pipe — so the helper parks again instead
    // of spinning or surfacing the error; draining again lets the whole
    // text through in order.
    let clock = fakes::clock::FakeClock::new();
    let start = clock.now();
    let (mut read, write) = nonblocking_pipe();
    let mut fill = fs::File::from(write.as_fd().try_clone_to_owned().unwrap());
    fill_pipe(&mut fill);
    let file = fs::File::from(write.as_fd().try_clone_to_owned().unwrap());
    let filler = fs::File::from(write.as_fd().try_clone_to_owned().unwrap());
    let text: &[u8] = b"Reviewer, by number or name: ";
    let (ids_tx, ids) = mpsc::channel();
    let (done_tx, done) = mpsc::channel();
    let worker_clock: Arc<dyn contract::clock::Clock> = clock.clone();
    let worker = thread::Builder::new()
        .name("pipe-write-wait".to_owned())
        .spawn(move || {
            ids_tx.send(thread::current().id()).unwrap_or(());
            let mut err = SpuriousReadyThenFull {
                file,
                filler,
                faked: false,
                out: Vec::new(),
            };
            let outcome =
                super::write_all_wait(&mut err, write.as_fd(), text, &|| false, &worker_clock);
            done_tx.send((outcome, err.out)).unwrap_or(());
        })
        .unwrap();
    let within = fakes::Deadline::after(PIPE_WAIT);
    let id = within.recv_or_fail(&ids, "the write worker reports its thread");
    let first = start + super::POLL_SLICE;
    clock.advance_when_parked_then(Some(first), Some(id), super::POLL_SLICE, &within, || {
        // Drain, earning the writable report the retry follows.
        drain_pipe(&mut read);
    });
    let second = first + super::POLL_SLICE;
    clock.advance_when_parked_then(Some(second), Some(id), super::POLL_SLICE, &within, || {
        // The retry met room yet still said WouldBlock, so the writer
        // refilled the pipe: draining again is what lets it complete.
        match fill.write(&[0x78]) {
            Err(e) if e.kind() == ErrorKind::WouldBlock => {}
            outcome => panic!("the spurious write refilled the pipe: {outcome:?}"),
        }
        drain_pipe(&mut read);
    });
    let (outcome, out) = within.recv_or_fail(&done, "the drained write completes");
    assert!(!outcome.unwrap(), "no cancellation interrupted the write");
    assert_eq!(out.as_slice(), text, "every byte lands in order");
    worker.join().unwrap();
}

/// An `err` writer that fails its first write with `Other`, then accepts:
/// the helper must surface the error at once instead of waiting and
/// retrying into the accepting write.
struct FailOnceThenAccept {
    failed: bool,
    out: Vec<u8>,
}

impl Write for FailOnceThenAccept {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if !self.failed {
            self.failed = true;
            return Err(std::io::Error::other("broken"));
        }
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_non_would_block_write_error_surfaces_at_once() {
    // Only WouldBlock waits: any other write error ends the write, even
    // though the retry would accept.
    let clock = fakes::clock::FakeClock::new();
    let worker_clock: Arc<dyn contract::clock::Clock> = clock.clone();
    let (_read, write) = nonblocking_pipe();
    let mut err = FailOnceThenAccept {
        failed: false,
        out: Vec::new(),
    };
    let outcome = super::write_all_wait(
        &mut err,
        write.as_fd(),
        b"Reviewer, by number or name: ",
        &|| false,
        &worker_clock,
    );
    let error = outcome.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Other, "{error:?}");
    assert!(
        err.out.is_empty(),
        "no retry follows a fatal write error: {:?}",
        err.out
    );
}

/// An `err` writer that accepts every write and fails its first flush
/// with `WouldBlock`: the helper must wait and retry the flush instead of
/// surfacing the error.
struct FlushWouldBlockOnce {
    flushed: bool,
    out: Vec<u8>,
}

impl Write for FlushWouldBlockOnce {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.flushed {
            self.flushed = true;
            return Err(std::io::Error::new(ErrorKind::WouldBlock, "busy"));
        }
        Ok(())
    }
}

#[test]
fn a_would_block_flush_waits_and_retries_instead_of_surfacing() {
    // A WouldBlock flush parks exactly like a WouldBlock write: the retry
    // flush lands, every byte stays, and nothing surfaces. The park the
    // test advances pins the flush-wait deadline one slice ahead.
    let clock = fakes::clock::FakeClock::new();
    let start = clock.now();
    let (mut read, write) = nonblocking_pipe();
    let mut fill = fs::File::from(write.as_fd().try_clone_to_owned().unwrap());
    fill_pipe(&mut fill);
    let text: &[u8] = b"Reviewer, by number or name: ";
    let (ids_tx, ids) = mpsc::channel();
    let (done_tx, done) = mpsc::channel();
    let worker_clock: Arc<dyn contract::clock::Clock> = clock.clone();
    let worker = thread::Builder::new()
        .name("pipe-flush-wait".to_owned())
        .spawn(move || {
            ids_tx.send(thread::current().id()).unwrap_or(());
            let mut err = FlushWouldBlockOnce {
                flushed: false,
                out: Vec::new(),
            };
            let outcome =
                super::write_all_wait(&mut err, write.as_fd(), text, &|| false, &worker_clock);
            done_tx.send((outcome, err.out)).unwrap_or(());
        })
        .unwrap();
    let within = fakes::Deadline::after(PIPE_WAIT);
    let id = within.recv_or_fail(&ids, "the flush worker reports its thread");
    let until = start + super::POLL_SLICE;
    clock.advance_when_parked_then(Some(until), Some(id), super::POLL_SLICE, &within, || {
        drain_pipe(&mut read);
    });
    let (outcome, out) = within.recv_or_fail(&done, "the retried flush completes");
    assert!(!outcome.unwrap(), "no cancellation interrupted the flush");
    assert_eq!(out.as_slice(), text, "every byte lands in order");
    worker.join().unwrap();
}

/// An `err` writer that accepts every write and fails its flush with
/// `Other`: the helper must surface the error at once instead of waiting
/// on a flush that will never clear.
struct FailFlushOnce {
    out: Vec<u8>,
}

impl Write for FailFlushOnce {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::other("broken"))
    }
}

#[test]
fn a_non_would_block_flush_error_surfaces_at_once() {
    // Only a WouldBlock flush waits: any other flush error ends the write
    // on a worker thread, so a wait instead of a surface would miss the
    // deadline rather than hang the test.
    let clock = fakes::clock::FakeClock::new();
    let worker_clock: Arc<dyn contract::clock::Clock> = clock.clone();
    let (_read, write) = nonblocking_pipe();
    let (done_tx, done) = mpsc::channel();
    let worker = thread::Builder::new()
        .name("pipe-flush-fail".to_owned())
        .spawn(move || {
            let mut err = FailFlushOnce { out: Vec::new() };
            let outcome = super::write_all_wait(
                &mut err,
                write.as_fd(),
                b"Reviewer, by number or name: ",
                &|| false,
                &worker_clock,
            );
            done_tx.send((outcome, err.out)).unwrap_or(());
        })
        .unwrap();
    let within = fakes::Deadline::after(PIPE_WAIT);
    let (outcome, out) = within.recv_or_fail(&done, "the fatal flush surfaces");
    let error = outcome.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Other, "{error:?}");
    assert_eq!(
        out.as_slice(),
        b"Reviewer, by number or name: ",
        "the writes before the fatal flush stay: {out:?}"
    );
    worker.join().unwrap();
}

#[test]
fn a_blocked_write_completes_whole_once_the_pipe_drains() {
    // The pipe starts full, so the first write refuses and the helper
    // parks; the test drains the pipe in the fake-clock hook before time
    // moves, and the whole text lands in order with no cancellation.
    let clock = fakes::clock::FakeClock::new();
    let start = clock.now();
    let (mut read, write) = nonblocking_pipe();
    let mut fill = fs::File::from(write.as_fd().try_clone_to_owned().unwrap());
    fill_pipe(&mut fill);
    let text: &[u8] = b"Reviewer, by number or name: ";
    let (ids_tx, ids) = mpsc::channel();
    let (done_tx, done) = mpsc::channel();
    let worker_clock: Arc<dyn contract::clock::Clock> = clock.clone();
    let worker = thread::Builder::new()
        .name("pipe-write-drain".to_owned())
        .spawn(move || {
            ids_tx.send(thread::current().id()).unwrap_or(());
            let mut err = fs::File::from(write.as_fd().try_clone_to_owned().unwrap());
            let outcome =
                super::write_all_wait(&mut err, write.as_fd(), text, &|| false, &worker_clock);
            done_tx.send(outcome).unwrap_or(());
        })
        .unwrap();
    let within = fakes::Deadline::after(PIPE_WAIT);
    let id = within.recv_or_fail(&ids, "the write worker reports its thread");
    let until = start + super::POLL_SLICE;
    clock.advance_when_parked_then(Some(until), Some(id), super::POLL_SLICE, &within, || {
        drain_pipe(&mut read);
    });
    let outcome = within.recv_or_fail(&done, "the drained write completes");
    assert!(!outcome.unwrap(), "no cancellation interrupted the write");
    // The worker wrote after the drain: the pipe holds exactly the text.
    let mut got = vec![0u8; text.len()];
    read.read_exact(&mut got).unwrap();
    assert_eq!(got.as_slice(), text, "every byte lands in order");
    worker.join().unwrap();
}

/// An `err` writer that captures the blocking list, then fills the pipe on
/// the first guarded prompt write and reports WouldBlock: the prompt must
/// cancel within the poll budget with the pipe still full.
struct CaptureThenFill {
    file: fs::File,
    filled: bool,
    out: Vec<u8>,
}

impl Write for CaptureThenFill {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        if !self.filled && text.contains("Reviewer, by number or name: ") {
            self.filled = true;
            fill_pipe(&mut self.file);
            return Err(std::io::Error::new(ErrorKind::WouldBlock, "busy"));
        }
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_blocked_prompt_write_returns_cancelled_within_the_poll_budget() {
    // The pipe fills exactly as the guarded prompt write begins, so the
    // helper parks with nowhere to write; setting the latch in the
    // fake-clock hook cancels within PROMPT_POLL, even with the prompt
    // unwritten and the pipe still full.
    let clock = fakes::clock::FakeClock::new();
    let start = clock.now();
    let (_read, write) = nonblocking_pipe();
    let file = fs::File::from(write.as_fd().try_clone_to_owned().unwrap());
    let term = fs::File::from(write.as_fd().try_clone_to_owned().unwrap());
    let stopped = Arc::new(atomic::AtomicBool::new(false));
    let worker_stopped = Arc::clone(&stopped);
    let (ids_tx, ids) = mpsc::channel();
    let (done_tx, done) = mpsc::channel();
    let worker_clock: Arc<dyn contract::clock::Clock> = clock.clone();
    let worker = thread::Builder::new()
        .name("pipe-prompt-cancel".to_owned())
        .spawn(move || {
            ids_tx.send(thread::current().id()).unwrap_or(());
            let mut input = Script {
                steps: [Step::Line("2\n")].into_iter().collect(),
            };
            let mut err = CaptureThenFill {
                file,
                filled: false,
                out: Vec::new(),
            };
            let halt = move || worker_stopped.load(atomic::Ordering::SeqCst);
            let pick = prompt_choice(
                "acme",
                &listed(),
                &mut input,
                &mut err,
                &halt,
                true,
                &worker_clock,
                term.as_fd(),
            );
            done_tx.send((pick, err.out)).unwrap_or(());
        })
        .unwrap();
    let within = fakes::Deadline::after(PIPE_WAIT);
    let id = within.recv_or_fail(&ids, "the prompt worker reports its thread");
    let until = start + super::POLL_SLICE;
    clock.advance_when_parked_then(Some(until), Some(id), super::POLL_SLICE, &within, || {
        stopped.store(true, atomic::Ordering::SeqCst);
    });
    let (pick, out) = within.recv_or_fail(&done, "the blocked prompt answers cancellation");
    assert_eq!(pick.unwrap(), Pick::Cancelled);
    assert!(
        clock.now().duration_since(start) <= super::PROMPT_POLL,
        "cancellation lands within the poll budget"
    );
    let captured = String::from_utf8(out).unwrap();
    assert!(
        captured.contains("Reviewer for acme:"),
        "the blocking list got through: {captured}"
    );
    // Nobody drained: the pipe the prompt blocked on is still full.
    let mut waiting = [rustix::event::PollFd::new(
        &write,
        rustix::event::PollFlags::OUT,
    )];
    rustix::event::poll(
        &mut waiting,
        Some(&rustix::event::Timespec::try_from(Duration::ZERO).unwrap()),
    )
    .unwrap();
    assert!(
        waiting
            .first()
            .is_some_and(|polled| polled.revents().is_empty()),
        "the pipe stays full"
    );
    worker.join().unwrap();
}
