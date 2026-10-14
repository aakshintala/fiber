//! The login reviewer seam (`docs/model-routing.md`, "Logging in"): discovery,
//! preselection, parsing and saving through `reviewer_models`,
//! `set_reviewer` and the terminal prompt, against isolated homes. No
//! network beyond loopback, no other extension started.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use config::{Secret, store_credential};
use contract::ErrorCode;
use extensions::Providers;
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
        server.await_requests(1, WAIT),
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
        server.await_requests(1, WAIT),
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
        server.await_requests(1, WAIT),
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
    assert_eq!(failure.code, ErrorCode::ConfigInvalid);
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
    assert_eq!(failure.code, ErrorCode::Usage);
    assert!(failure.message.contains("`nope`"), "{}", failure.message);
}

/// Asks `input` of `choices`, returning the pick and the terminal text.
fn ask(name: &str, choices: &ReviewerModels, input: &str) -> (Pick, String) {
    let mut err = Vec::new();
    let pick = prompt_choice(
        name,
        choices,
        &mut Cursor::new(input.as_bytes().to_vec()),
        &mut err,
        &|| false,
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
    let pick = prompt_choice(
        "acme",
        &listed(),
        &mut Cursor::new(b"1\n".to_vec()),
        &mut err,
        &|| true,
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
    assert_eq!(failure.code, ErrorCode::ConfigInvalid);
    assert!(
        failure.message.contains("providers."),
        "the failure names the key: {}",
        failure.message
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
            failure.message.contains("providers.acme.reviewer_model"),
            "{failure:?}"
        );
        assert_eq!(
            fs::read(setup.home().join("credentials/acme/default")).unwrap(),
            credential_before,
            "{existing}: the stored credential stays"
        );
    }
}
