//! `providers.databricks.request_tags` (`docs/configuration.md`, "A
//! provider's data"): an object of string values, validated once after the
//! layers merge. A value that is not one is dropped with one nonfatal
//! `config_invalid` notice that names the key and never echoes the value.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

mod common;

use std::collections::BTreeMap;

use common::Setup;
use config::Config;
use contract::ErrorCode;

const KEY: &str = "providers.databricks.request_tags";

fn tags(config: &Config) -> Option<BTreeMap<String, String>> {
    config.databricks_request_tags().cloned()
}

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// The notices about this key, whatever their code.
fn about_key(config: &Config) -> Vec<(ErrorCode, String)> {
    config
        .notices()
        .iter()
        .filter(|n| n.message.contains("request_tags"))
        .map(|n| (n.code.clone(), n.message.clone()))
        .collect()
}

/// The one notice about this key.
fn only_notice(config: &Config) -> (ErrorCode, String) {
    let about = about_key(config);
    assert_eq!(about.len(), 1, "one notice: {about:?}");
    about.into_iter().next().unwrap()
}

/// One effective value that is invalid: a usable config, no tags and one
/// `config_invalid` notice naming the key, without the supplied text.
fn assert_invalid(config: &Config, secret: &str) {
    assert_eq!(tags(config), None);
    let (code, message) = only_notice(config);
    assert_eq!(code, ErrorCode::ConfigInvalid);
    assert!(message.contains(KEY), "names the key: {message}");
    assert!(
        !message.contains(secret),
        "never echoes the value: {message}"
    );
    assert_eq!(config.notices().len(), 1, "no other notice");
}

#[test]
fn request_tags_accept_native_objects_and_merge_layers() {
    let setup = Setup::new();
    // Unset: no tags and no notice.
    let config = setup.load(&[]).unwrap();
    assert_eq!(tags(&config), None);
    assert!(config.notices().is_empty());

    // An empty object is a usable default.
    let config = setup.load(&[&format!("{KEY}={{}}")]).unwrap();
    assert_eq!(tags(&config), Some(BTreeMap::new()));
    assert!(config.notices().is_empty());

    // The literal acceptance override, a work-unit string, a `source` entry
    // (kept as an ordinary entry here; routing pins it) and Unicode.
    let config = setup
        .load(&[&format!(
            r#"{KEY}={{"run":"r1","work-unit":"w \"1\"\\ \n","source":"other","név":"é𝔘"}}"#
        )])
        .unwrap();
    assert_eq!(
        tags(&config),
        Some(map(&[
            ("run", "r1"),
            ("work-unit", "w \"1\"\\ \n"),
            ("source", "other"),
            ("név", "é𝔘"),
        ]))
    );
    assert!(config.notices().is_empty(), "{:?}", config.notices());

    // The dotted form of one entry reaches the same key.
    let config = setup.load(&[&format!("{KEY}.run=r2")]).unwrap();
    assert_eq!(tags(&config), Some(map(&[("run", "r2")])));
    assert!(config.notices().is_empty());

    // Global, project and run objects merge key by key, highest last.
    setup.write(
        &setup.global(),
        r#"{"providers":{"databricks":{"request_tags":{"a":"g","b":"g","c":"g"}}}}"#,
    );
    setup.write(
        &setup.project(),
        r#"{"providers":{"databricks":{"request_tags":{"b":"p","c":"p"}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        tags(&config),
        Some(map(&[("a", "g"), ("b", "p"), ("c", "p")]))
    );
    assert!(config.notices().is_empty(), "{:?}", config.notices());
    let config = setup.load(&[&format!(r#"{KEY}={{"c":"r"}}"#)]).unwrap();
    assert_eq!(
        tags(&config),
        Some(map(&[("a", "g"), ("b", "p"), ("c", "r")]))
    );
    assert!(config.notices().is_empty(), "{:?}", config.notices());
}

#[test]
fn request_tags_higher_layer_repairs_a_lower_invalid_value() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers":{"databricks":{"request_tags":"zebra-text"}}}"#,
    );
    // The invalid effective value alone: dropped with one notice.
    assert_invalid(&setup.load(&[]).unwrap(), "zebra-text");
    // A valid object above it replaces it.
    let config = setup.load(&[&format!(r#"{KEY}={{"run":"r1"}}"#)]).unwrap();
    assert_eq!(tags(&config), Some(map(&[("run", "r1")])));
    assert!(config.notices().is_empty(), "{:?}", config.notices());

    // A valid lower object does not survive an invalid higher value.
    setup.write(
        &setup.global(),
        r#"{"providers":{"databricks":{"request_tags":{"run":"r0"}}}}"#,
    );
    assert_invalid(&setup.load(&[&format!("{KEY}=7")]).unwrap(), "7");
    // A higher entry of the wrong type spoils the whole merged object.
    assert_invalid(
        &setup.load(&[&format!(r#"{KEY}={{"bad":7}}"#)]).unwrap(),
        "bad",
    );
}

#[test]
fn request_tags_invalid_values_notice_once() {
    let setup = Setup::new();
    let cases: &[(&str, &str)] = &[
        // Malformed JSON reaches the key as a bare string.
        (r#"{"run":"r1""#, r#""r1""#),
        ("not-json-at-all", "not-json-at-all"),
        // Blank and whitespace values.
        ("", "@@"),
        ("   ", "@@"),
        // An object encoded as a string.
        (r#""{\"run\":\"zebra\"}""#, "zebra"),
        // Other JSON types.
        ("42", "42"),
        ("true", "true"),
        ("null", "null"),
        (r#"["alpha","beta"]"#, "alpha"),
        // Nested and mixed objects.
        (r#"{"run":{"deep":"gamma"}}"#, "gamma"),
        (r#"{"run":"delta","count":3}"#, "delta"),
        (r#"{"run":"epsilon","flag":false}"#, "epsilon"),
        (r#"{"run":"zeta","none":null}"#, "zeta"),
        (r#"{"run":"eta","list":["x"]}"#, "eta"),
    ];
    for (text, leaked) in cases {
        let config = setup
            .load(&[&format!("{KEY}={text}")])
            .unwrap_or_else(|e| panic!("{text:?} must not be fatal: {e}"));
        assert_invalid(&config, leaked);
    }
}

#[test]
fn request_tags_repository_cannot_set() {
    let setup = Setup::new();
    setup.write(
        &setup.repository(),
        r#"{"providers":{"databricks":{"request_tags":{"run":"repo"}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(tags(&config), None);
    assert_eq!(only_notice(&config).0, ErrorCode::ConfigKeyIgnored);

    // A valid run override beside it: the repository adds nothing.
    let config = setup.load(&[&format!(r#"{KEY}={{"run":"r1"}}"#)]).unwrap();
    assert_eq!(tags(&config), Some(map(&[("run", "r1")])));
    assert_eq!(only_notice(&config).0, ErrorCode::ConfigKeyIgnored);

    // A repository value of the wrong type is ignored the same way, not
    // judged: the person's own layers decide validity.
    setup.write(
        &setup.repository(),
        r#"{"providers":{"databricks":{"request_tags":7}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(tags(&config), None);
    assert_eq!(only_notice(&config).0, ErrorCode::ConfigKeyIgnored);
}

#[test]
fn request_tags_bad_file_json_remains_fatal() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"providers":{"databricks":"#);
    let error = setup.load(&[]).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ConfigInvalid);

    // The file is well formed: an invalid tag value there is a notice, not
    // an error, while an unrelated wrong type in the same file stays fatal.
    setup.write(
        &setup.global(),
        r#"{"providers":{"databricks":{"request_tags":5}},"compaction":{"tokens":"many"}}"#,
    );
    let error = setup.load(&[]).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ConfigInvalid);
    assert!(error.to_string().contains("compaction.tokens"), "{error}");
}
