//! Every key in `docs/configuration.md`, "Keys", "Per model" and "MCP
//! servers": a value of the right type is read, a value of the wrong type is
//! `config_invalid` naming the file and the key, and a repository may set it
//! only when the table says yes.

mod common;

use common::{Setup, nest};
use config::{CredentialSource, Source};
use contract::ErrorCode;
use serde_json::{Value, json};

const STR: &str = "a string";
const BOOL: &str = "true or false";
const COUNT: &str = "a whole number of zero or more";
const NUMBER: &str = "a number";
const LIST: &str = "a list of strings";

/// (path, a good value, a wrong value, what the error says it must be,
/// whether a repository may set it)
fn rows() -> Vec<(&'static [&'static str], Value, Value, &'static str, bool)> {
    vec![
        (&["model"], json!("openai/gpt-5.6"), json!(5), STR, true),
        (
            &["roles", "fast"],
            json!("fiber:openai/gpt-5.6:xhigh"),
            json!(true),
            STR,
            true,
        ),
        (
            &["permissions", "mode"],
            json!("yolo"),
            json!("readonly"),
            "one of \"auto\", \"yolo\"",
            false,
        ),
        (
            &["permissions", "mode"],
            json!("yolo"),
            json!(1),
            "one of \"auto\", \"yolo\"",
            false,
        ),
        (
            &["reviewer", "model"],
            json!("openai/gpt-5.6-mini"),
            json!([]),
            STR,
            false,
        ),
        (
            &["reviewer", "block_limits", "consecutive"],
            json!(4),
            json!(-1),
            COUNT,
            false,
        ),
        (
            &["reviewer", "block_limits", "session"],
            json!(30),
            json!(2.5),
            COUNT,
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
            &["handoff", "tokens"],
            json!(200000),
            json!("200000"),
            COUNT,
            true,
        ),
        (
            &["handoff", "window_fraction"],
            json!(0.5),
            json!("half"),
            NUMBER,
            true,
        ),
        (&["handoff", "nudge"], json!(false), json!(0), BOOL, true),
        (
            &["cache", "lifetime"],
            json!("5m"),
            json!("2h"),
            "one of \"5m\", \"1h\"",
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
            &["models", "a/b", "handoff", "tokens"],
            json!(1000),
            json!(true),
            COUNT,
            true,
        ),
        (
            &["models", "a/b", "handoff", "window_fraction"],
            json!(0.4),
            json!(null),
            NUMBER,
            true,
        ),
        (
            &["models", "a/b", "handoff", "nudge"],
            json!(true),
            json!("yes"),
            BOOL,
            true,
        ),
        (
            &["models", "a/b", "cache", "lifetime"],
            json!("1h"),
            json!(60),
            "one of \"5m\", \"1h\"",
            true,
        ),
        (&["retry", "attempts"], json!(5), json!(-3), COUNT, true),
        (
            &["retry", "initial_delay_ms"],
            json!(100),
            json!("1s"),
            COUNT,
            true,
        ),
        (
            &["retry", "max_delay_ms"],
            json!(1000),
            json!(1.5),
            COUNT,
            true,
        ),
        (
            &["tools", "read", "max_result_bytes"],
            json!(32768),
            json!("big"),
            COUNT,
            true,
        ),
        (
            &["tools", "read", "deferred"],
            json!(true),
            json!("true"),
            BOOL,
            true,
        ),
        (
            &["web_search", "backend"],
            json!("exa"),
            json!(1),
            STR,
            false,
        ),
        (
            &["shell", "read_only", "jq", "flags"],
            json!(["--json", "-p"]),
            json!(["--json", 1]),
            LIST,
            false,
        ),
        (&["budget", "usd"], json!(5.0), json!("5"), NUMBER, false),
        (
            &["quota", "notice_at"],
            json!(90),
            json!("90"),
            NUMBER,
            true,
        ),
        (
            &["mcp", "servers", "gh", "command"],
            json!("gh-mcp"),
            json!(["gh-mcp"]),
            STR,
            true,
        ),
        (
            &["mcp", "servers", "gh", "args"],
            json!(["--stdio"]),
            json!("--stdio"),
            LIST,
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
            &["mcp", "servers", "gh", "url"],
            json!("https://example.com/mcp"),
            json!(1),
            STR,
            true,
        ),
        (
            &["mcp", "servers", "gh", "required"],
            json!(true),
            json!("yes"),
            BOOL,
            true,
        ),
        (
            &["mcp", "servers", "gh", "startup_timeout_ms"],
            json!(100),
            json!(-1),
            COUNT,
            true,
        ),
        (
            &["mcp", "servers", "gh", "timeout_ms"],
            json!(100),
            json!("1s"),
            COUNT,
            true,
        ),
        (
            &["mcp", "servers", "gh", "eager"],
            json!(true),
            json!(1),
            BOOL,
            true,
        ),
        (
            &["mcp", "servers", "gh", "tools", "enabled"],
            json!(["a"]),
            json!("a"),
            LIST,
            true,
        ),
        (
            &["mcp", "servers", "gh", "tools", "disabled"],
            json!(["b"]),
            json!([true]),
            LIST,
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
            &["extensions", "acme", "version"],
            json!("v1.4.0"),
            json!(1.4),
            STR,
            true,
        ),
        (
            &["extensions", "acme", "startup_timeout_ms"],
            json!(100),
            json!("fast"),
            COUNT,
            true,
        ),
        (
            &["extensions", "acme", "commands", "deploy"],
            json!("acme-deploy"),
            json!(false),
            STR,
            true,
        ),
        (
            &["extensions", "acme", "tools", "enabled"],
            json!(["t"]),
            json!({}),
            LIST,
            true,
        ),
        (
            &["extensions", "acme", "tools", "disabled"],
            json!(["t"]),
            json!(null),
            LIST,
            true,
        ),
        (
            &["extensions", "acme", "hook_timeout_ms"],
            json!(100),
            json!(-5),
            COUNT,
            false,
        ),
        (
            &["hooks", "order", "before_tool"],
            json!(["redact", "acme"]),
            json!("redact"),
            LIST,
            false,
        ),
        (
            &["providers", "openrouter", "credential"],
            json!({"env": "OPENROUTER_API_KEY"}),
            json!({"env": 1}),
            "one of {\"env\": name}",
            false,
        ),
        (
            &["providers", "openrouter", "credential"],
            json!({"file": "/k"}),
            json!({"env": "A", "file": "/k"}),
            "one of {\"env\": name}",
            false,
        ),
        (
            &["providers", "openrouter", "credential"],
            json!({"command": ["op", "read"]}),
            json!({"command": []}),
            "one of {\"env\": name}",
            false,
        ),
        (
            &["tui", "panel", "cards"],
            json!(["quota"]),
            json!("quota"),
            LIST,
            false,
        ),
        (&["tui", "theme"], json!("light"), json!(1), STR, false),
        (
            &["tui", "reduced_motion"],
            json!(true),
            json!("on"),
            BOOL,
            false,
        ),
        (
            &["tui", "screen_reader"],
            json!(true),
            json!(1),
            BOOL,
            false,
        ),
        (
            &["tui", "attention", "notification"],
            json!(false),
            json!(0),
            BOOL,
            false,
        ),
        (
            &["tui", "attention", "bell"],
            json!(false),
            json!(0),
            BOOL,
            false,
        ),
        (
            &["tui", "attention", "title"],
            json!(false),
            json!(0),
            BOOL,
            false,
        ),
        (&["tui", "hover"], json!(false), json!(0), BOOL, false),
        (
            &["tui", "inline_images"],
            json!(false),
            json!(0),
            BOOL,
            false,
        ),
        (
            &["tui", "logo_glyph"],
            json!("≈"),
            json!("*"),
            "one of \"⌇\", \"≈\"",
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
fn a_provider_credential_reads_as_its_source() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"openrouter": {"credential": {"command": ["op", "read", "op://k"]}}}}"#,
    );
    let (value, _) = setup
        .load(&[])
        .unwrap()
        .get("providers.openrouter.credential", None)
        .unwrap();
    assert_eq!(
        serde_json::from_value::<CredentialSource>(value).unwrap(),
        CredentialSource::Command(vec!["op".into(), "read".into(), "op://k".into()])
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
        config.get("mcp.servers.gh.eager", None),
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
            "cache": {"lifetime": "1h"},
            "handoff": {"enabled": true, "nudge": true, "tokens": 400000, "window_fraction": 0.7},
            "permissions": {"mode": "auto"},
            "quota": {"notice_at": 80},
            "retry": {"attempts": 3, "initial_delay_ms": 2000, "max_delay_ms": 60000},
            "reviewer": {"block_limits": {"consecutive": 3, "session": 20}},
            "tui": {
                "attention": {"bell": true, "notification": true, "title": true},
                "hover": true,
                "inline_images": true,
                "logo_glyph": "⌇",
                "panel": {"cards": ["session", "changed_files", "delegates", "jobs", "quota"]},
                "reduced_motion": false
            }
        })
    );
    assert_eq!(
        config.get("handoff.tokens", None),
        Some((json!(400000), Source::Default))
    );
}
