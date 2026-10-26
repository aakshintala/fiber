//! Every key in `docs/configuration.md`, "Keys", "Per model" and "MCP
//! servers": a value of the right type is read, a value of the wrong type is
//! `config_invalid` naming the file and the key, and a repository may set it
//! only when the table says yes.

mod common;

use common::{Setup, nest};
use config::{CredentialSource, Source};
use contract::ErrorCode;
use serde_json::{Value, json};
use std::time::Duration;

const STR: &str = "a string";
const ROLE: &str = "a model reference, or an object with `model`, the reference";
const BOOL: &str = "true or false";
const COUNT: &str = "a whole number of zero or more";
const NUMBER: &str = "a number";
const LIST: &str = "a list of strings";
const WARM_CAP: &str = "a whole number less than 12";

#[test]
fn tui_slot_winner_is_a_personal_string_key_with_quoted_slot_names() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"tui":{"slots":{"ledger_row:shell":"alpha"}}}"#,
    );
    assert_eq!(
        setup
            .load(&[])
            .unwrap()
            .text("tui.slots.\"ledger_row:shell\"", None),
        Some("alpha".into())
    );
    setup.write(
        &setup.global(),
        r#"{"tui":{"slots":{"ledger_row:shell":12}}}"#,
    );
    let error = setup.load(&[]).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ConfigInvalid);
    assert!(error.to_string().contains("ledger_row:shell"));
}

#[test]
fn hub_client_keys_prefer_the_persons_project_layer() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"hub":{"default":"global"},"hubs":{"x":{"address":"ws://global"}}}"#,
    );
    setup.write(
        &setup.project(),
        r#"{"hub":{"default":"project"},"hubs":{"x":{"address":"unix:/tmp/project"}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        config.get("hub.default", None),
        Some((json!("project"), Source::Project(setup.project())))
    );
    assert_eq!(
        config.get("hubs.x.address", None),
        Some((json!("unix:/tmp/project"), Source::Project(setup.project())))
    );
}

/// (path, a good value, a wrong value, what the error says it must be,
/// whether a repository may set it)
///
/// One row per key type, plus one row per distinct `OneOf` set
/// (`cache.lifetime`, `thinking`, `tui.logo_glyph`), the `Credential` arms
/// (env, file with an extra field, empty command, command with a timeout),
/// `reviewer.context` (the `PersonFiles` row) and one `models.*` row (the per-model view).
/// `RepoOnly` (`repository_extensions`) is pinned by `tests/declared.rs` and
/// `GlobalOnly` (`diagnostics.level`) by the diagnostics tests below, so
/// they get no row; `cache.warm_cap` keeps its own test below.
fn rows() -> Vec<(&'static [&'static str], Value, Value, &'static str, bool)> {
    vec![
        (&["model"], json!("openai/gpt-5.6"), json!(5), STR, true),
        (&["hub", "default"], json!("box"), json!(5), STR, false),
        (
            &["hubs", "x", "address"],
            json!("ws://box"),
            json!(5),
            STR,
            false,
        ),
        (
            &["roles", "fast"],
            json!("fiber:openai/gpt-5.6:xhigh"),
            json!(true),
            ROLE,
            true,
        ),
        (
            &["skills", "disabled"],
            json!(["review", "grill"]),
            json!("review"),
            LIST,
            false,
        ),
        (
            &["hub", "idle_exit_ms"],
            json!(60000),
            json!("30m"),
            COUNT,
            false,
        ),
        (
            &["hub", "port"],
            json!(65535),
            json!(65536),
            "a whole number less than 65536",
            false,
        ),
        (
            &["hub", "allowed_origins"],
            json!(["https://a.example"]),
            json!("https://a.example"),
            LIST,
            false,
        ),
        (
            &["reviewer", "context"],
            json!("Our org is acme."),
            json!(5),
            STR,
            false,
        ),
        (
            &["handoff", "enabled"],
            json!(false),
            json!("no"),
            BOOL,
            true,
        ),
        (
            &["handoff", "window_fraction"],
            json!(0.5),
            json!("half"),
            NUMBER,
            true,
        ),
        (
            &["cache", "lifetime"],
            json!("5m"),
            json!("2h"),
            "one of \"5m\", \"1h\"",
            true,
        ),
        (
            &["thinking"],
            json!("high"),
            json!("turbo"),
            "one of \"off\", \"minimal\", \"low\", \"medium\", \"high\", \"xhigh\", \"max\"",
            true,
        ),
        (
            &["models", "a/b", "handoff", "enabled"],
            json!(false),
            json!(1),
            BOOL,
            true,
        ),
        (
            &["mcp", "servers", "gh", "env"],
            json!({"A": "b"}),
            json!({"A": 1}),
            "an object of strings",
            true,
        ),
        (
            &["mcp", "servers", "gh", "tools", "search", "hints"],
            json!({"readOnlyHint": true}),
            json!({"readOnlyHint": "yes"}),
            "an object of true or false values",
            false,
        ),
        (
            &["model_lists", "refresh_after"],
            json!("7d"),
            json!("1w"),
            "a duration such as \"7d\"",
            false,
        ),
        (
            &["providers", "acme", "reviewer_model"],
            json!("fast-1"),
            json!(5),
            STR,
            false,
        ),
        (
            &["providers", "openrouter", "credentials", "work"],
            json!({"env": "OPENROUTER_API_KEY"}),
            json!({"env": 1}),
            "one of {\"env\": name}",
            false,
        ),
        (
            &["providers", "openrouter", "credentials", "work"],
            json!({"file": "/k"}),
            json!({"env": "A", "file": "/k"}),
            "one of {\"env\": name}",
            false,
        ),
        (
            &["providers", "openrouter", "credentials", "work"],
            json!({"command": ["op", "read"]}),
            json!({"command": []}),
            "one of {\"env\": name}",
            false,
        ),
        (
            &["providers", "openrouter", "credentials", "work"],
            json!({"command": ["op"], "timeout": "2m"}),
            json!({"command": ["op"], "timeout": "0s"}),
            "one of {\"env\": name}",
            false,
        ),
        (
            &["tui", "logo_glyph"],
            json!("≈"),
            json!("*"),
            "one of \"⌇\", \"≈\"",
            false,
        ),
        (
            &["keys", "send"],
            json!(["ctrl+s"]),
            json!(5),
            "a string or a list of strings",
            false,
        ),
    ]
}

#[test]
fn every_key_reads_a_value_of_its_type() {
    for (path, good, _, _, _) in rows() {
        let setup = Setup::new();
        setup.write(&setup.global(), &nest(path, good.clone()).to_string());
        let config = setup.load(&[]).unwrap();
        assert!(
            config.notices().is_empty(),
            "{path:?}: {:?}",
            config.notices()
        );
        assert_eq!(
            config.get(&path.join("."), None),
            Some((good, Source::Global(setup.global()))),
            "{path:?}"
        );
    }
}

#[test]
fn a_value_of_the_wrong_type_is_config_invalid_naming_the_file_and_key() {
    for (path, _, bad, expected, _) in rows() {
        let setup = Setup::new();
        setup.write(&setup.project(), &nest(path, bad).to_string());
        let e = setup.load(&[]).unwrap_err();
        assert_eq!(e.code(), ErrorCode::ConfigInvalid, "{path:?}");
        let message = e.to_string();
        let start = format!(
            "{}: `{}` must be {expected}",
            setup.project().display(),
            path.join(".")
        );
        assert!(
            message.starts_with(&start) && message.ends_with('.'),
            "{message}"
        );
    }
}

#[test]
fn a_repository_sets_only_the_keys_the_table_allows() {
    for (path, good, _, _, repo) in rows() {
        let setup = Setup::new();
        setup.write(&setup.repository(), &nest(path, good.clone()).to_string());
        let config = setup.load(&[]).unwrap();
        let got = config.get(&path.join("."), None);
        if repo {
            assert!(config.notices().is_empty(), "{path:?}");
            assert_eq!(
                got,
                Some((good, Source::Repository(setup.repository()))),
                "{path:?}"
            );
        } else {
            assert_ne!(
                got.map(|(v, _)| v),
                Some(good),
                "{path:?} was set by a repository"
            );
            let [notice] = config.notices() else {
                panic!("{path:?}: {:?}", config.notices());
            };
            assert_eq!(notice.code, ErrorCode::ConfigKeyIgnored);
            assert_eq!(
                notice.message,
                format!(
                    "{}: ignored `{}`, which a repository may not set.",
                    setup.repository().display(),
                    path.join(".")
                )
            );
        }
    }
}

#[test]
fn a_repository_that_sets_a_person_only_key_with_the_wrong_type_is_only_ignored() {
    let setup = Setup::new();
    setup.write(
        &setup.repository(),
        r#"{"tui": {"hover": "sometimes"}, "model": "a/b"}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(config.notices().len(), 1);
    assert_eq!(
        config.get("tui.hover", None),
        Some((json!(true), Source::Default))
    );
    assert_eq!(config.get("model", None).unwrap().0, json!("a/b"));
}

#[test]
fn an_object_key_holding_another_type_is_config_invalid() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"handoff": 5}"#);
    let e = setup.load(&[]).unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert_eq!(
        e.to_string(),
        format!("{}: `handoff` must be an object.", setup.global().display())
    );
}

#[test]
fn a_provider_credential_label_reads_as_its_source() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"openrouter": {"credentials": {"work": {"command": ["op", "read", "op://k"]}}}}}"#,
    );
    let (value, _) = setup
        .load(&[])
        .unwrap()
        .get("providers.openrouter.credentials.work", None)
        .unwrap();
    assert_eq!(
        serde_json::from_value::<CredentialSource>(value).unwrap(),
        CredentialSource::Command {
            argv: vec!["op".into(), "read".into(), "op://k".into()],
            timeout: Duration::from_secs(30),
        }
    );
}

#[test]
fn a_credential_source_reads_its_timeout_and_refuses_a_bad_one() {
    for (value, expected) in [
        (
            json!({"command": ["op"]}),
            CredentialSource::Command {
                argv: vec!["op".into()],
                timeout: Duration::from_secs(30),
            },
        ),
        (
            json!({"command": ["op"], "timeout": "2m"}),
            CredentialSource::Command {
                argv: vec!["op".into()],
                timeout: Duration::from_secs(120),
            },
        ),
        (
            json!({"command": ["op"], "timeout": "1s"}),
            CredentialSource::Command {
                argv: vec!["op".into()],
                timeout: Duration::from_secs(1),
            },
        ),
        (json!({"env": "A"}), CredentialSource::Env("A".into())),
        (json!({"file": "/k"}), CredentialSource::File("/k".into())),
    ] {
        assert_eq!(
            serde_json::from_value::<CredentialSource>(value.clone()).unwrap(),
            expected,
            "{value}"
        );
    }
    for (value, hidden) in [
        (json!({"command": ["op"], "timeout": "0s"}), vec!["op"]),
        (json!({"command": ["op"], "timeout": "1w"}), vec!["op"]),
        (json!({"command": ["op"], "timeout": 30}), vec!["op"]),
        (
            json!({"command": ["op"], "timeout": "99999999999999999999s"}),
            vec!["op"],
        ),
        (
            json!({"command": ["op"], "timeout": "213503982334602d"}),
            vec!["op"],
        ),
        (json!({"env": "A", "timeout": "1s"}), vec!["A"]),
        (json!({"file": "/k", "timeout": "1s"}), vec!["/k"]),
        (json!({"command": ["op"], "extra": 1}), vec!["op"]),
        (json!({"env": "A", "file": "/k"}), vec!["A", "/k"]),
        (json!({}), Vec::new()),
    ] {
        let message = serde_json::from_value::<CredentialSource>(value.clone())
            .unwrap_err()
            .to_string();
        for word in hidden {
            assert!(!message.contains(word), "{value}: {message}");
        }
    }
}

#[test]
fn an_explicit_null_timeout_beside_command_is_refused() {
    let message =
        serde_json::from_value::<CredentialSource>(json!({"command": ["op"], "timeout": null}))
            .expect_err("an explicit null timeout is refused")
            .to_string();
    assert!(message.contains("must be a duration"), "{message}");
    assert!(!message.contains("op"), "{message}");
}

#[test]
fn an_explicit_null_timeout_beside_env_is_refused() {
    let message = serde_json::from_value::<CredentialSource>(json!({"env": "A", "timeout": null}))
        .expect_err("an explicit null timeout beside env is refused")
        .to_string();
    assert!(message.contains("must be a duration"), "{message}");
    assert!(!message.contains('A'), "{message}");
}

#[test]
fn an_explicit_null_timeout_beside_file_is_refused() {
    let message =
        serde_json::from_value::<CredentialSource>(json!({"file": "/k", "timeout": null}))
            .expect_err("an explicit null timeout beside file is refused")
            .to_string();
    assert!(message.contains("must be a duration"), "{message}");
    assert!(!message.contains("/k"), "{message}");
}

#[test]
fn an_omitted_timeout_beside_command_selects_the_thirty_second_default() {
    assert_eq!(
        serde_json::from_value::<CredentialSource>(json!({"command": ["op"]})).unwrap(),
        CredentialSource::Command {
            argv: vec!["op".into()],
            timeout: Duration::from_secs(30),
        }
    );
}

#[test]
fn a_repository_declares_an_mcp_server_but_not_its_hints() {
    let setup = Setup::new();
    setup.write(
        &setup.repository(),
        r#"{"mcp": {"servers": {"gh": {"command": "gh-mcp", "tools": {"search": {"hints": {"readOnlyHint": true}}}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        config.get("mcp.servers.gh", None),
        Some((
            json!({"command": "gh-mcp", "tools": {"search": {}}}),
            Source::Repository(setup.repository())
        ))
    );
    assert_eq!(config.notices().len(), 1);
    assert!(
        config.notices()[0]
            .message
            .contains("`mcp.servers.gh.tools.search.hints`")
    );
}

#[test]
fn a_server_field_left_out_takes_its_default() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"mcp": {"servers": {"gh": {"url": "https://x"}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        config.get("mcp.servers.gh.required", None),
        Some((json!(false), Source::Default))
    );
    assert_eq!(
        config.get("mcp.servers.gh.declare_in_full", None),
        Some((json!(false), Source::Default))
    );
    assert_eq!(
        config.get("mcp.servers.gh.startup_timeout_ms", None),
        Some((json!(5000), Source::Default))
    );
    assert_eq!(
        config.get("extensions.acme.startup_timeout_ms", None),
        Some((json!(5000), Source::Default))
    );
    assert_eq!(
        config.get("extensions.acme.enabled", None),
        Some((json!(true), Source::Default))
    );
    assert_eq!(config.get("mcp.servers.gh.timeout_ms", None), None);
    assert_eq!(config.get("model", None), None);
    assert_eq!(config.get("no.such.key", None), None);
}

#[test]
fn with_no_files_the_configuration_is_the_built_in_defaults() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    assert!(config.notices().is_empty());
    assert_eq!(
        config.merged(None),
        json!({
            "cache": {"lifetime": "1h", "warm_cap": 2, "warm_idle": false, "diagnostics": false},
            "diagnostics": {"level": "info"},
            "handoff": {"enabled": true, "nudge": true, "tokens": 400000, "window_fraction": 0.7},
            "model_lists": {"refresh_after": "24h"},
            "quota": {"notice_at": 80},
            "retry": {"attempts": 3, "initial_delay_ms": 2000, "max_delay_ms": 60000},
            "reviewer": {"block_limits": {"consecutive": 3, "session": 20}},
            "hub": {"allowed_origins": [], "idle_exit_ms": 1800000},
            "session": {"idle_exit_ms": 1800000},
            "skills": {"disabled": []},
            "tui": {
                "attention": {"bell": true, "notification": true, "title": true},
                "hover": true,
                "inline_images": true,
                "logo_glyph": "⌇",
                "panel": {
                    "cards": ["session", "changed_files", "delegates", "jobs", "quota"],
                    "width": 21
                },
                "rail": {"shown": true, "width": 15},
                "reduced_motion": false
            }
        })
    );
    assert_eq!(
        config.get("handoff.tokens", None),
        Some((json!(400000), Source::Default))
    );
}

#[test]
fn set_refuses_a_key_at_the_wrong_layer_naming_the_key() {
    let setup = Setup::new();
    let e = config::set(
        &setup.home(),
        &setup.workspace(),
        &common::key(),
        config::Layer::Global,
        "repository_extensions",
        json!([{"path": "pkg"}]),
    )
    .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Usage);
    assert!(e.to_string().contains("`repository_extensions`"), "{e}");
    assert!(
        e.to_string()
            .contains("only a repository's own file may set it"),
        "{e}"
    );
    let e = config::set(
        &setup.home(),
        &setup.workspace(),
        &common::key(),
        config::Layer::Repository,
        "session.idle_exit_ms",
        json!(60000),
    )
    .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Usage);
    assert!(e.to_string().contains("`session.idle_exit_ms`"), "{e}");
    assert!(e.to_string().contains("a repository may not set it"), "{e}");
}

#[test]
fn model_lists_refresh_after_accepts_durations_and_refuses_the_rest() {
    for good in ["90s", "30m", "24h", "7d"] {
        let setup = Setup::new();
        setup.write(
            &setup.global(),
            &nest(&["model_lists", "refresh_after"], json!(good)).to_string(),
        );
        let config = setup.load(&[]).unwrap();
        assert!(config.notices().is_empty(), "{good}");
        assert_eq!(
            config.get("model_lists.refresh_after", None).unwrap().0,
            json!(good),
            "{good}"
        );
    }
    for bad in [
        json!("0h"),
        json!("24"),
        json!("1w"),
        json!(""),
        json!(24),
        // No byte slice of these is a duration: each must refuse, never panic.
        json!("é"),
        json!("5é"),
        json!("٣h"),
    ] {
        let setup = Setup::new();
        setup.write(
            &setup.global(),
            &nest(&["model_lists", "refresh_after"], bad.clone()).to_string(),
        );
        let err = setup.load(&[]).unwrap_err();
        assert_eq!(err.code(), ErrorCode::ConfigInvalid, "{bad}");
        assert!(
            err.to_string()
                .contains("`model_lists.refresh_after` must be"),
            "{bad}: {err}"
        );
    }
}

#[test]
fn parse_duration_pins_exact_seconds_for_every_unit() {
    for (text, seconds) in [("1s", 1), ("1m", 60), ("1h", 3600), ("1d", 86400)] {
        assert_eq!(
            config::parse_duration(text),
            Some(Duration::from_secs(seconds)),
            "{text}"
        );
    }
}

#[test]
fn refresh_after_defaults_to_a_day_and_reads_ninety_minutes() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    assert_eq!(config::refresh_after(&config), Duration::from_secs(86400));
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        &nest(&["model_lists", "refresh_after"], json!("90m")).to_string(),
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(config::refresh_after(&config), Duration::from_secs(5400));
}

#[test]
fn warm_cap_refuses_twelve_or_more_and_anything_not_a_whole_number() {
    for good in [0, 2, 11] {
        let setup = Setup::new();
        setup.write(
            &setup.global(),
            &nest(&["cache", "warm_cap"], json!(good)).to_string(),
        );
        let config = setup.load(&[]).unwrap();
        assert_eq!(config.get("cache.warm_cap", None).unwrap().0, json!(good));
    }
    for bad in [json!(12), json!(100), json!(2.5), json!(-1), json!("2")] {
        let setup = Setup::new();
        setup.write(
            &setup.global(),
            &nest(&["cache", "warm_cap"], bad.clone()).to_string(),
        );
        let e = setup.load(&[]).unwrap_err();
        assert_eq!(e.code(), ErrorCode::ConfigInvalid, "{bad}");
        assert!(e.to_string().contains(WARM_CAP), "{e}");
    }
}

#[test]
fn warming_is_off_by_default_with_a_cap_of_two_lifetimes() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        config.get("cache.warm_idle", None),
        Some((json!(false), Source::Default))
    );
    assert_eq!(
        config.get("cache.warm_cap", None),
        Some((json!(2), Source::Default))
    );
}

#[test]
fn diagnostics_level_defaults_to_info_and_the_global_file_sets_debug() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        config.get("diagnostics.level", None),
        Some((json!("info"), Source::Default))
    );
    assert!(!config::diagnostics_debug(&config));
    setup.write(&setup.global(), r#"{"diagnostics": {"level": "debug"}}"#);
    let config = setup.load(&[]).unwrap();
    assert!(config.notices().is_empty(), "{:?}", config.notices());
    assert_eq!(
        config.get("diagnostics.level", None),
        Some((json!("debug"), Source::Global(setup.global())))
    );
    assert!(config::diagnostics_debug(&config));
    setup.write(&setup.global(), r#"{"diagnostics": {"level": "info"}}"#);
    assert!(!config::diagnostics_debug(&setup.load(&[]).unwrap()));
}

#[test]
fn a_repository_s_diagnostics_level_is_ignored_with_a_notice() {
    let setup = Setup::new();
    setup.write(
        &setup.repository(),
        r#"{"diagnostics": {"level": "debug"}}"#,
    );
    let config = setup.load(&[]).unwrap();
    let [notice] = config.notices() else {
        panic!("{:?}", config.notices());
    };
    assert_eq!(notice.code, ErrorCode::ConfigKeyIgnored);
    assert_eq!(
        notice.message,
        format!(
            "{}: ignored `diagnostics.level`, which a repository may not set.",
            setup.repository().display()
        )
    );
    assert!(!config::diagnostics_debug(&config));
}

#[test]
fn a_project_file_or_run_flag_diagnostics_level_is_ignored_as_global_only() {
    let setup = Setup::new();
    setup.write(&setup.project(), r#"{"diagnostics": {"level": "debug"}}"#);
    let config = setup.load(&["diagnostics.level=debug"]).unwrap();
    let messages: Vec<_> = config.notices().iter().map(|n| n.message.clone()).collect();
    assert_eq!(
        messages,
        [
            format!(
                "{}: ignored `diagnostics.level`, which only Fiber home's `config.json` may set.",
                setup.project().display()
            ),
            "-c: ignored `diagnostics.level`, which only Fiber home's `config.json` may set."
                .to_owned(),
        ]
    );
    assert!(
        config
            .notices()
            .iter()
            .all(|n| n.code == ErrorCode::ConfigKeyIgnored)
    );
    assert_eq!(
        config.get("diagnostics.level", None),
        Some((json!("info"), Source::Default))
    );
    assert!(!config::diagnostics_debug(&config));
}

#[test]
fn a_run_flag_reviewer_context_is_ignored_as_person_files() {
    let setup = Setup::new();
    setup.write(
        &setup.project(),
        r#"{"reviewer": {"context": "Never touch infra/prod."}}"#,
    );
    let config = setup
        .load(&["reviewer.context=from the command line"])
        .unwrap();
    let [notice] = config.notices() else {
        panic!("{:?}", config.notices());
    };
    assert_eq!(notice.code, ErrorCode::ConfigKeyIgnored);
    assert_eq!(
        notice.message,
        "-c: ignored `reviewer.context`, which only Fiber home's `config.json` \
         or the project's `config.json` in Fiber home may set."
    );
    // The flag never reaches the render: the project file's value stands.
    assert_eq!(
        config.get("reviewer.context", None),
        Some((
            json!("Never touch infra/prod."),
            Source::Project(setup.project())
        ))
    );
    assert_eq!(
        config.reviewer_context(),
        "## Notes for this project\n\nNever touch infra/prod."
    );
}

#[test]
fn a_global_diagnostics_level_that_is_not_info_or_debug_is_config_invalid() {
    for bad in [json!("verbose"), json!(true)] {
        let setup = Setup::new();
        setup.write(
            &setup.global(),
            &nest(&["diagnostics", "level"], bad.clone()).to_string(),
        );
        let e = setup.load(&[]).unwrap_err();
        assert_eq!(e.code(), ErrorCode::ConfigInvalid, "{bad}");
        assert_eq!(
            e.to_string(),
            format!(
                "{}: `diagnostics.level` must be one of \"info\", \"debug\".",
                setup.global().display()
            ),
            "{bad}"
        );
    }
}

#[test]
fn a_keys_entry_reads_a_string_a_list_or_an_empty_list() {
    for good in [json!("ctrl+s"), json!(["ctrl+s", "alt+s"]), json!([])] {
        let setup = Setup::new();
        setup.write(
            &setup.global(),
            &nest(&["keys", "send"], good.clone()).to_string(),
        );
        let config = setup.load(&[]).unwrap();
        assert!(
            config.notices().is_empty(),
            "{good:?}: {:?}",
            config.notices()
        );
        assert_eq!(
            config.get("keys.send", None),
            Some((good.clone(), Source::Global(setup.global()))),
            "{good:?}"
        );
    }
}

#[test]
fn a_keys_entry_holding_anything_but_strings_is_config_invalid() {
    for bad in [json!([5]), json!(["a", 5])] {
        let setup = Setup::new();
        setup.write(
            &setup.global(),
            &nest(&["keys", "send"], bad.clone()).to_string(),
        );
        let e = setup.load(&[]).unwrap_err();
        assert_eq!(e.code(), ErrorCode::ConfigInvalid, "{bad}");
        assert_eq!(
            e.to_string(),
            format!(
                "{}: `keys.send` must be a string or a list of strings.",
                setup.global().display()
            ),
            "{bad}"
        );
    }
}

#[test]
fn the_project_file_in_fiber_home_merges_its_keys_with_the_global_file() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"keys": {"send": "ctrl+s"}}"#);
    setup.write(&setup.project(), r#"{"keys": {"copy_focused": ["c"]}}"#);
    let config = setup.load(&[]).unwrap();
    assert!(config.notices().is_empty(), "{:?}", config.notices());
    assert_eq!(
        config.get("keys", None),
        Some((
            json!({"send": "ctrl+s", "copy_focused": ["c"]}),
            Source::Project(setup.project())
        ))
    );
}

#[test]
fn provider_reviewer_model_has_no_default_until_login_saves_one() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    assert_eq!(config.get("providers.acme.reviewer_model", None), None);
    assert_eq!(
        config.get("providers.acme.credential", None),
        None,
        "no reviewer default may appear beside the credential key"
    );
}

#[test]
fn provider_reviewer_model_rejects_every_non_string_without_reading_it() {
    for bad in [
        json!(true),
        json!({"model": "m"}),
        json!(["m"]),
        json!(null),
    ] {
        let setup = Setup::new();
        setup.write(
            &setup.global(),
            &nest(&["providers", "acme", "reviewer_model"], bad.clone()).to_string(),
        );
        let e = setup.load(&[]).unwrap_err();
        assert_eq!(e.code(), ErrorCode::ConfigInvalid, "{bad}");
        assert_eq!(
            e.to_string(),
            format!(
                "{}: `providers.acme.reviewer_model` must be a string.",
                setup.global().display()
            ),
            "{bad}"
        );
    }
}

#[test]
fn provider_reviewer_model_with_a_dotted_name_is_one_path_segment() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"acme.corp": {"reviewer_model": "fast-1"}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert!(config.notices().is_empty(), "{:?}", config.notices());
    assert_eq!(
        config.get("providers.\"acme.corp\".reviewer_model", None),
        Some((json!("fast-1"), Source::Global(setup.global())))
    );
    assert_eq!(
        config.get_segments(
            &[
                "providers".into(),
                "acme.corp".into(),
                "reviewer_model".into()
            ],
            None
        ),
        Some((json!("fast-1"), Source::Global(setup.global())))
    );
}

#[test]
fn a_repository_reviewer_model_is_ignored_and_the_personal_value_stands() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"acme": {"reviewer_model": "mine"}}}"#,
    );
    setup.write(
        &setup.repository(),
        r#"{"providers": {"acme": {"reviewer_model": "theirs"}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    let [notice] = config.notices() else {
        panic!("{:?}", config.notices());
    };
    assert_eq!(notice.code, ErrorCode::ConfigKeyIgnored);
    assert_eq!(
        notice.message,
        format!(
            "{}: ignored `providers.acme.reviewer_model`, which a repository may not set.",
            setup.repository().display()
        )
    );
    assert_eq!(
        config.get("providers.acme.reviewer_model", None),
        Some((json!("mine"), Source::Global(setup.global())))
    );
}

#[test]
fn reviewer_model_layers_prefer_project_then_override_and_providers_stay_independent() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"reviewer": {"model": "all/a"}, "providers": {"acme": {"reviewer_model": "global-a"}, "other": {"reviewer_model": "global-o"}}}"#,
    );
    setup.write(
        &setup.project(),
        r#"{"providers": {"acme": {"reviewer_model": "project-a"}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert!(config.notices().is_empty(), "{:?}", config.notices());
    assert_eq!(
        config.get("providers.acme.reviewer_model", None),
        Some((json!("project-a"), Source::Project(setup.project())))
    );
    assert_eq!(
        config.get("providers.other.reviewer_model", None),
        Some((json!("global-o"), Source::Global(setup.global())))
    );
    assert_eq!(
        config.get("reviewer.model", None),
        Some((json!("all/a"), Source::Global(setup.global()))),
        "the every-provider override is a separate key"
    );
    let config = setup
        .load(&["providers.acme.reviewer_model=flag-a"])
        .unwrap();
    assert_eq!(
        config.get("providers.acme.reviewer_model", None),
        Some((json!("flag-a"), Source::Run))
    );
}

#[test]
fn replace_global_persists_the_exact_local_id_beside_sibling_keys() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"acme": {"credential": "default", "credentials": {"work": {"env": "ACME_KEY"}}, "placeholder": "kept"}}}"#,
    );
    let before: Value =
        serde_json::from_str(&std::fs::read_to_string(setup.global()).unwrap()).unwrap();
    assert_eq!(
        before,
        json!({"providers": {"acme": {"credential": "default", "credentials": {"work": {"env": "ACME_KEY"}}, "placeholder": "kept"}}}),
        "the fixture must hold the siblings the write keeps"
    );
    let replaced = config::replace_global(
        &setup.home(),
        "providers.acme.reviewer_model",
        Some(json!("anthropic/claude-sonnet-5.5")),
    )
    .unwrap();
    assert_eq!(replaced, None);
    let root: Value =
        serde_json::from_str(&std::fs::read_to_string(setup.global()).unwrap()).unwrap();
    assert_eq!(
        root,
        json!({"providers": {"acme": {"credential": "default", "credentials": {"work": {"env": "ACME_KEY"}}, "placeholder": "kept", "reviewer_model": "anthropic/claude-sonnet-5.5"}}}),
        "the local ID lands verbatim, siblings untouched"
    );
    let replaced = config::replace_global(
        &setup.home(),
        "providers.acme.reviewer_model",
        Some(json!("fast-2")),
    )
    .unwrap();
    assert_eq!(replaced, Some(json!("anthropic/claude-sonnet-5.5")));
}

#[test]
fn a_wrongly_typed_reviewer_candidate_writes_nothing() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"reviewer": {"model": "all/a"}}"#);
    let before = std::fs::read(setup.global()).unwrap();
    let e = config::replace_global(
        &setup.home(),
        "providers.acme.reviewer_model",
        Some(json!(5)),
    )
    .unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert!(
        e.to_string().contains("`providers.acme.reviewer_model`"),
        "{e}"
    );
    assert_eq!(
        std::fs::read(setup.global()).unwrap(),
        before,
        "a mistyped candidate must fail before changing the file"
    );
}
