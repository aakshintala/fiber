//! Unit tests for filling a `base_url` template
//! (`docs/model-routing.md`, "A per-account host").

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use std::cell::Cell;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use config::{Config, ConfigError, Placeholder, ProjectKey, ProviderData, Sources};

use super::{Filled, Source, Unconfigured, fill, host, hosts, not_a_host, unconfigured, value};
use crate::Providers;

/// A lookup that serves `value` from the setting for `name` and nothing
/// for anything else.
fn lookup_of(
    name: &'static str,
    value: &'static str,
) -> impl Fn(&str) -> Result<Option<(String, Source)>, ConfigError> {
    move |at: &str| {
        if at == name {
            Ok(Some((value.to_owned(), Source::Setting)))
        } else {
            Ok(None)
        }
    }
}

#[test]
fn a_template_with_no_placeholder_is_unchanged_and_looks_nothing_up() {
    let calls = Cell::new(0);
    let filled = fill("http://127.0.0.1:1/v1", &|_name| {
        calls.set(calls.get() + 1);
        Ok(None)
    })
    .unwrap();
    assert_eq!(filled, Filled::Url("http://127.0.0.1:1/v1".to_owned()));
    assert_eq!(calls.get(), 0);
}

#[test]
fn a_repeated_name_gets_the_same_value_every_time() {
    let lookup = lookup_of("r", "us");
    assert_eq!(
        fill("https://{r}-x/{r}/y", &lookup).unwrap(),
        Filled::Url("https://us-x/us/y".to_owned())
    );
}

#[test]
fn underscores_dashes_digits_and_capitals_are_name_characters() {
    for (template, name, value) in [
        ("{a_b}", "a_b", "one"),
        ("{a-b}", "a-b", "two"),
        ("{A9}", "A9", "three"),
    ] {
        let lookup = lookup_of(name, value);
        assert_eq!(
            fill(template, &lookup).unwrap(),
            Filled::Url(value.to_owned()),
            "{template}"
        );
    }
}

#[test]
fn other_braces_stay_as_written_and_are_not_looked_up() {
    let calls = Cell::new(0);
    let lookup = |_name: &str| {
        calls.set(calls.get() + 1);
        Ok(None)
    };
    for template in ["{a.b}", "{a b}", "{}", "x}y", "{a"] {
        assert_eq!(
            fill(template, &lookup).unwrap(),
            Filled::Url(template.to_owned()),
            "{template}"
        );
    }
    assert_eq!(calls.get(), 0);
}

#[test]
fn a_lone_brace_before_a_placeholder_stays_as_written() {
    let lookup = lookup_of("a", "h");
    assert_eq!(
        fill("{{a}}", &lookup).unwrap(),
        Filled::Url("{h}".to_owned())
    );
}

#[test]
fn host_strips_one_scheme_and_slash_and_checks_the_grammar() {
    for (value, expected) in [
        ("adb-1.example", "adb-1.example"),
        ("https://adb-1.example/", "adb-1.example"),
        ("adb-1.example/", "adb-1.example"),
        ("https://adb-1.example", "adb-1.example"),
        ("a:0", "a:0"),
        ("adb-1.example:8443", "adb-1.example:8443"),
        ("a:65535", "a:65535"),
    ] {
        assert_eq!(host(value), Some(expected), "{value}");
    }
    for value in [
        "",
        "https://",
        "/",
        ":443",
        "a:",
        "a:+1",
        "a:65536",
        "a:1:2",
        "a_b",
        "a/b",
        "a b",
        "[::1]",
        "HTTPS://a",
        "https://https://a",
        "a//",
    ] {
        assert_eq!(host(value), None, "{value}");
    }
}

#[test]
fn a_valid_host_is_filled_stripped() {
    let lookup = lookup_of("a", "https://h.example/");
    assert_eq!(
        fill("{a}", &lookup).unwrap(),
        Filled::Url("h.example".to_owned())
    );
}

#[test]
fn a_value_that_is_not_a_host_is_reported_with_its_source() {
    let lookup = lookup_of("a", "x/y");
    assert_eq!(
        fill("{a}", &lookup).unwrap(),
        Filled::NotHost {
            name: "a".to_owned(),
            source: Source::Setting,
        }
    );
    let env = |at: &str| {
        if at == "a" {
            Ok(Some((
                "x/y".to_owned(),
                Source::Env("ACME_HOST".to_owned()),
            )))
        } else {
            Ok(None)
        }
    };
    assert_eq!(
        fill("{a}", &env).unwrap(),
        Filled::NotHost {
            name: "a".to_owned(),
            source: Source::Env("ACME_HOST".to_owned()),
        }
    );
}

#[test]
fn the_first_placeholder_with_no_value_is_reported_and_an_error_passes_on() {
    let none = |_name: &str| Ok(None);
    assert_eq!(
        fill("{a}{b}", &none).unwrap(),
        Filled::Missing("a".to_owned())
    );
    let lookup = lookup_of("a", "h");
    assert_eq!(
        fill("{a}{b}", &lookup).unwrap(),
        Filled::Missing("b".to_owned())
    );
    let err = fill("{a}", &|_name| {
        Err::<Option<(String, Source)>, ConfigError>(ConfigError::Override { arg: "bad".into() })
    })
    .unwrap_err();
    assert_eq!(err.code(), contract::ErrorCode::Usage);
}

/// Declared placeholders with the environment variable `env`, if any.
fn declared(names: &[(&str, Option<&str>)]) -> BTreeMap<String, Placeholder> {
    names
        .iter()
        .map(|(name, env)| {
            (
                (*name).to_owned(),
                Placeholder {
                    env: env.map(str::to_owned),
                },
            )
        })
        .collect()
}

#[test]
fn hosts_with_no_declared_names_reads_nothing_and_returns_empty() {
    let called = Cell::new(false);
    assert_eq!(
        hosts(&BTreeMap::new(), &|_name| {
            called.set(true);
            Ok(None)
        })
        .unwrap(),
        Some(BTreeMap::new())
    );
    assert!(!called.get());
}

#[test]
fn hosts_reads_each_declared_name_and_strips_it_as_a_host() {
    let one = declared(&[("workspace", None)]);
    for (value, want) in [
        ("h.example", "h.example"),
        ("https://h.example/", "h.example"),
        ("127.0.0.1:8080", "127.0.0.1:8080"),
    ] {
        assert_eq!(
            hosts(&one, &lookup_of("workspace", value)).unwrap(),
            Some(BTreeMap::from([("workspace".to_owned(), want.to_owned())])),
            "{value}"
        );
    }
    // Two declared names: both hosts come back, in name order.
    let two = declared(&[("other", None), ("workspace", None)]);
    let lookup = |name: &str| match name {
        "workspace" => Ok(Some(("h.example".to_owned(), Source::Setting))),
        _ => Ok(Some(("o.example".to_owned(), Source::Setting))),
    };
    assert_eq!(
        hosts(&two, &lookup).unwrap(),
        Some(BTreeMap::from([
            ("other".to_owned(), "o.example".to_owned()),
            ("workspace".to_owned(), "h.example".to_owned()),
        ]))
    );
}

#[test]
fn hosts_returns_none_when_a_name_has_no_value_or_is_not_a_host() {
    // Two declared, one with no value: no `models()` argument.
    let two = declared(&[("other", None), ("workspace", None)]);
    assert_eq!(
        hosts(&two, &lookup_of("workspace", "h.example")).unwrap(),
        None
    );
    // A value that is not a host reads the same: nothing is built.
    let one = declared(&[("workspace", None)]);
    assert_eq!(hosts(&one, &lookup_of("workspace", "a/b")).unwrap(), None);
}

#[test]
fn hosts_passes_a_lookup_error_on() {
    let one = declared(&[("workspace", None)]);
    let err = hosts(&one, &|_name| {
        Err::<Option<(String, Source)>, ConfigError>(ConfigError::Override { arg: "bad".into() })
    })
    .unwrap_err();
    assert_eq!(err.code(), contract::ErrorCode::Usage);
}

/// The `acme` provider with `placeholders`, owned by extension `acme`.
fn gap_providers(placeholders: BTreeMap<String, Placeholder>) -> Providers {
    let mut by_name = BTreeMap::new();
    by_name.insert(
        "acme".to_owned(),
        ProviderData {
            name: "acme".to_owned(),
            credential_from: config::CredentialFrom::Stored {
                credential: None,
                login: None,
            },
            credential_name: None,
            headers: BTreeMap::new(),
            placeholders,
            models: Vec::new(),
            reviewer_model: None,
        },
    );
    let mut extension_of = BTreeMap::new();
    extension_of.insert("acme".to_owned(), "acme".to_owned());
    Providers {
        by_name,
        extension_of,
        ..Default::default()
    }
}

/// The configuration over `home` with the `p` project and no overrides.
fn gap_config(home: &Path) -> Config {
    Config::load(Sources {
        home: home.to_path_buf(),
        workspace: home.to_path_buf(),
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap()
}

/// Writes `text` to `home`'s global `config/acme.json`.
fn write_global(home: &Path, text: &str) {
    let dir = home.join("config");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("acme.json"), text).unwrap();
}

/// Writes `text` to `home`'s `projects/p/config/acme.json`.
fn write_project(home: &Path, text: &str) {
    let dir = home.join("projects").join("p").join("config");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("acme.json"), text).unwrap();
}

/// An environment holding `pairs`.
fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn gaps_lists_a_placeholder_with_no_setting_and_no_environment() {
    let root = fakes::TempDir::new("fiber-gaps-missing");
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let providers = gap_providers(declared(&[("workspace", Some("ACME_HOST"))]));
    let config = gap_config(&home);
    let env = env_of(&[]);
    assert_eq!(
        providers
            .unset_placeholders(&config, &|name| env.get(name).cloned(), "acme")
            .unwrap(),
        [("workspace".to_owned(), Some("ACME_HOST".to_owned()))]
    );
}

#[test]
fn gaps_is_empty_when_the_setting_is_set() {
    let root = fakes::TempDir::new("fiber-gaps-setting");
    let home = root.path().join("home");
    write_global(&home, r#"{"workspace":"adb-1.example"}"#);
    let providers = gap_providers(declared(&[("workspace", Some("ACME_HOST"))]));
    let config = gap_config(&home);
    let env = env_of(&[]);
    assert!(
        providers
            .unset_placeholders(&config, &|name| env.get(name).cloned(), "acme")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn gaps_is_empty_when_the_environment_is_set() {
    let root = fakes::TempDir::new("fiber-gaps-env");
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let providers = gap_providers(declared(&[("workspace", Some("ACME_HOST"))]));
    let config = gap_config(&home);
    let env = env_of(&[("ACME_HOST", "adb-2.example")]);
    assert!(
        providers
            .unset_placeholders(&config, &|name| env.get(name).cloned(), "acme")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn gaps_is_empty_for_a_present_but_invalid_setting() {
    // Presence, not validity, decides: an invalid setting is no gap, and
    // the model list still leaves the model out with `model_unconfigured`.
    let root = fakes::TempDir::new("fiber-gaps-invalid-setting");
    let home = root.path().join("home");
    write_global(&home, r#"{"workspace":"not a host!!"}"#);
    let providers = gap_providers(declared(&[("workspace", Some("ACME_HOST"))]));
    let config = gap_config(&home);
    let env = env_of(&[]);
    assert!(
        providers
            .unset_placeholders(&config, &|name| env.get(name).cloned(), "acme")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn gaps_is_empty_for_a_present_but_invalid_environment_value() {
    // Presence, not validity, decides: an invalid environment value is no
    // gap, and the model list still leaves the model out.
    let root = fakes::TempDir::new("fiber-gaps-invalid-env");
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let providers = gap_providers(declared(&[("workspace", Some("ACME_HOST"))]));
    let config = gap_config(&home);
    let env = env_of(&[("ACME_HOST", "not a host!!")]);
    assert!(
        providers
            .unset_placeholders(&config, &|name| env.get(name).cloned(), "acme")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_project_empty_over_a_global_answer_still_reads_as_no_value() {
    // The mask the login names: the global answer stays written, but the
    // project layer's empty string wins the merge, so `value` finds no
    // usable value.
    let root = fakes::TempDir::new("fiber-gaps-mask");
    let home = root.path().join("home");
    write_global(&home, r#"{"workspace":"adb-1.example"}"#);
    write_project(&home, r#"{"workspace":""}"#);
    let config = gap_config(&home);
    let declared = declared(&[("workspace", Some("ACME_HOST"))]);
    let env = env_of(&[]);
    assert_eq!(
        value(&config, "acme", &declared, "workspace", &|name| env
            .get(name)
            .cloned())
        .unwrap(),
        None
    );
}

#[test]
fn validate_host_delegates_to_the_model_list_rule() {
    // Every input the model list fills, the prompt accepts, and every one
    // it refuses, the prompt refuses: the same rule, the same result.
    for value in [
        "adb-1.example",
        "https://adb-1.example/",
        "https://adb-1.example:443/",
        "adb-1.example:0",
        "adb-1.example:65535",
        "not a host!!",
        "adb-1.example:65536",
        "adb-1.example:",
        "https://https://adb-1.example",
        "adb-\u{00e9}.example",
        "https://",
        "",
    ] {
        assert_eq!(Providers::validate_host(value), host(value), "{value:?}");
    }
}

#[test]
fn unconfigured_without_a_variable_names_the_login_and_config_remedies() {
    let notice = unconfigured("acme", "m", "acme", "workspace", None, true);
    assert_eq!(
        notice.message,
        "The model `acme/m` needs the setting `workspace` for its base URL, \
         which has no value. Run `fiber login acme`, or set `workspace` in \
         `config/acme.json` in Fiber home."
    );
}

#[test]
fn unconfigured_without_login_names_the_config_remedy_alone() {
    let notice = unconfigured("acme", "m", "acme", "workspace", None, false);
    assert_eq!(
        notice.message,
        "The model `acme/m` needs the setting `workspace` for its base URL, \
         which has no value. Set `workspace` in `config/acme.json` in Fiber home."
    );
}

#[test]
fn unconfigured_with_an_absent_variable_names_the_hub_environment_and_both_remedies() {
    let notice = unconfigured(
        "databricks",
        "model",
        "databricks",
        "workspace",
        Some("DATABRICKS_HOST"),
        true,
    );
    assert_eq!(
        notice.message,
        "The model `databricks/model` needs the setting `workspace` for its base URL, \
         which has no value, and `DATABRICKS_HOST` is not set in the hub's environment \
         (your login shell). Run `fiber login databricks`, or set `workspace` in \
         `config/databricks.json` in Fiber home, or export it in your shell profile and \
         run `fiber hub refresh`."
    );
}

#[test]
fn not_a_host_from_a_setting_names_the_source_without_the_value() {
    let notice = not_a_host("acme", "m", "acme", "workspace", &Source::Setting);
    assert_eq!(
        notice.message,
        "The model `acme/m` needs the setting `workspace` for its base URL, \
         whose value is not a host."
    );
    assert!(!notice.message.contains("evil"));
}

#[test]
fn not_a_host_from_the_environment_names_the_variable_without_the_value() {
    let notice = not_a_host(
        "acme",
        "m",
        "acme",
        "workspace",
        &Source::Env("ACME_HOST".to_owned()),
    );
    assert_eq!(
        notice.message,
        "The model `acme/m` needs the setting `workspace` for its base URL, \
         which has no value, and the value of `ACME_HOST` is not a host."
    );
    assert!(!notice.message.contains("evil"));
}

/// Provider `acme`'s models `ids` left out for its unset `workspace`.
fn left_out(ids: &[&str], variable: Option<&str>, login: bool) -> Unconfigured {
    Unconfigured {
        provider: "acme".to_owned(),
        extension: "ext-acme".to_owned(),
        setting: "workspace".to_owned(),
        variable: variable.map(str::to_owned),
        login,
        models: ids.iter().map(|id| (*id).to_owned()).collect(),
    }
}

/// One wording case: the hidden ids, the declared variable, whether
/// login asks for the setting, and the expected message.
type Case<'a> = (&'a [&'a str], Option<&'a str>, bool, Option<&'a str>);

/// Admits every reference.
fn every(_: &str) -> bool {
    true
}

#[test]
fn the_aggregate_notice_counts_the_models_it_leaves_out() {
    let ids: Vec<String> = (1..=136).map(|n| format!("m-{n}")).collect();
    let many: Vec<&str> = ids.iter().map(String::as_str).collect();
    let cases: [Case<'_>; 6] = [
        (&[], None, true, None),
        (
            &["m"],
            None,
            true,
            Some(
                "`acme`: 1 model needs the setting `workspace` for its base URL, \
                 which has no value. Run `fiber login acme`, or set `workspace` in \
                 `config/ext-acme.json` in Fiber home.",
            ),
        ),
        (
            &["m", "n"],
            None,
            true,
            Some(
                "`acme`: 2 models need the setting `workspace` for their base URL, \
                 which has no value. Run `fiber login acme`, or set `workspace` in \
                 `config/ext-acme.json` in Fiber home.",
            ),
        ),
        (
            &many,
            None,
            false,
            Some(
                "`acme`: 136 models need the setting `workspace` for their base URL, \
                 which has no value. Set `workspace` in `config/ext-acme.json` in Fiber home.",
            ),
        ),
        (
            &["m", "n"],
            Some("ACME_HOST"),
            true,
            Some(
                "`acme`: 2 models need the setting `workspace` for their base URL, \
                 which has no value, and `ACME_HOST` is not set in the hub's environment \
                 (your login shell). Run `fiber login acme`, or set `workspace` in \
                 `config/ext-acme.json` in Fiber home, or export it in your shell profile \
                 and run `fiber hub refresh`.",
            ),
        ),
        (
            &["m", "n"],
            Some("ACME_HOST"),
            false,
            Some(
                "`acme`: 2 models need the setting `workspace` for their base URL, \
                 which has no value, and `ACME_HOST` is not set in the hub's environment \
                 (your login shell). Set `workspace` in `config/ext-acme.json` in Fiber home, \
                 or export it in your shell profile and run `fiber hub refresh`.",
            ),
        ),
    ];
    for (ids, variable, login, expected) in cases {
        let notice = left_out(ids, variable, login).notice(&every);
        assert_eq!(
            notice.as_ref().map(|notice| notice.message.as_str()),
            expected,
            "{} models, {variable:?}, login {login}",
            ids.len()
        );
        if let Some(notice) = notice {
            assert_eq!(notice.code, contract::ErrorCode::ModelUnconfigured);
            assert_eq!(notice.extension.as_deref(), Some("ext-acme"));
        }
    }
}

#[test]
fn the_aggregate_notice_counts_only_the_models_listed_admits() {
    let group = left_out(&["alpha-1", "alpha-2", "beta-1"], None, true);
    assert_eq!(group.provider(), "acme");
    assert_eq!(group.setting(), "workspace");
    assert_eq!(group.models(), ["alpha-1", "alpha-2", "beta-1"]);
    let alpha = group
        .notice(&|reference: &str| reference.starts_with("acme/alpha"))
        .unwrap();
    assert!(
        alpha.message.starts_with("`acme`: 2 models need"),
        "{}",
        alpha.message
    );
    let beta = group
        .notice(&|reference: &str| reference == "acme/beta-1")
        .unwrap();
    assert!(
        beta.message.starts_with("`acme`: 1 model needs"),
        "{}",
        beta.message
    );
    assert_eq!(group.notice(&|_: &str| false), None);
}
