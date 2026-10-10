//! `docs/configuration.md`, "Layers", "Per model" and "When Fiber reads
//! configuration": which layer wins, how objects merge, `-c`, and what a
//! problem in a file reports.

mod common;

use std::fs;

use common::Setup;
use config::{CredentialSource, ProjectKey, Source};
use contract::ErrorCode;
use serde_json::json;

fn model_layers(setup: &Setup) {
    setup.write(
        &setup.global(),
        r#"{"model": "openrouter/anthropic/claude-sonnet-5"}"#,
    );
    setup.write(
        &setup.repository(),
        r#"{"model": "databricks/databricks-claude-opus-5"}"#,
    );
}

#[test]
fn the_repository_wins_over_the_global_file() {
    let setup = Setup::new();
    model_layers(&setup);
    assert_eq!(
        setup.load(&[]).unwrap().get("model", None),
        Some((
            json!("databricks/databricks-claude-opus-5"),
            Source::Repository(setup.repository())
        ))
    );
}

#[test]
fn the_per_project_file_wins_over_the_repository() {
    let setup = Setup::new();
    model_layers(&setup);
    setup.write(&setup.project(), r#"{"model": "openai/gpt-5.6"}"#);
    assert_eq!(
        setup.load(&[]).unwrap().get("model", None),
        Some((json!("openai/gpt-5.6"), Source::Project(setup.project())))
    );
}

#[test]
fn a_run_flag_wins_over_every_file() {
    let setup = Setup::new();
    model_layers(&setup);
    setup.write(&setup.project(), r#"{"model": "openai/gpt-5.6"}"#);
    let config = setup
        .load(&["model=anthropic/claude-opus-5", "handoff.tokens=200000"])
        .unwrap();
    assert_eq!(
        config.get("model", None),
        Some((json!("anthropic/claude-opus-5"), Source::Run))
    );
    assert_eq!(
        config.get("handoff.tokens", None),
        Some((json!(200000), Source::Run))
    );
}

#[test]
fn a_later_run_flag_wins_over_an_earlier_one() {
    let setup = Setup::new();
    let config = setup.load(&["model=a/b", "model=c/d"]).unwrap();
    assert_eq!(config.get("model", None), Some((json!("c/d"), Source::Run)));
}

#[test]
fn objects_merge_key_by_key_and_lists_replace() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"handoff": {"tokens": 1000}, "tui": {"panel": {"cards": ["session", "jobs"]}}}"#,
    );
    setup.write(&setup.repository(), r#"{"handoff": {"nudge": false}}"#);
    setup.write(
        &setup.project(),
        r#"{"tui": {"panel": {"cards": ["quota"]}}}"#,
    );
    let merged = setup.load(&[]).unwrap().merged(None);
    assert_eq!(
        merged["handoff"],
        json!({"enabled": true, "nudge": false, "tokens": 1000, "window_fraction": 0.7})
    );
    assert_eq!(merged["tui"]["panel"]["cards"], json!(["quota"]));
    assert_eq!(merged["tui"]["hover"], json!(true));
}

#[test]
fn an_object_names_the_highest_layer_that_set_part_of_it() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"handoff": {"tokens": 1000}}"#);
    setup.write(&setup.repository(), r#"{"handoff": {"nudge": false}}"#);
    let (value, source) = setup.load(&[]).unwrap().get("handoff", None).unwrap();
    assert_eq!(source, Source::Repository(setup.repository()));
    assert_eq!(value["tokens"], json!(1000));
}

#[test]
fn a_per_model_key_wins_over_the_same_layers_top_level() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"handoff": {"tokens": 400000}, "cache": {"lifetime": "1h"},
            "models": {"databricks/databricks-claude-opus-5": {
                "handoff": {"window_fraction": 0.5}, "cache": {"lifetime": "5m"}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    let opus = Some("databricks/databricks-claude-opus-5");
    assert_eq!(
        config.get("handoff.window_fraction", opus),
        Some((json!(0.5), Source::Global(setup.global())))
    );
    assert_eq!(config.get("cache.lifetime", opus).unwrap().0, json!("5m"));
    assert_eq!(config.get("handoff.tokens", opus).unwrap().0, json!(400000));
    assert_eq!(
        config
            .get("cache.lifetime", Some("openai/gpt-5.6"))
            .unwrap()
            .0,
        json!("1h")
    );
    assert_eq!(config.get("cache.lifetime", None).unwrap().0, json!("1h"));
    assert_eq!(config.merged(opus)["cache"]["lifetime"], json!("5m"));
    assert_eq!(config.merged(None)["cache"]["lifetime"], json!("1h"));
}

#[test]
fn a_later_layers_top_level_wins_over_an_earlier_layers_per_model_key() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"models": {"a/b": {"handoff": {"tokens": 1}}}}"#,
    );
    setup.write(&setup.repository(), r#"{"handoff": {"tokens": 2}}"#);
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        config.get("handoff.tokens", Some("a/b")),
        Some((json!(2), Source::Repository(setup.repository())))
    );
}

#[test]
fn a_run_flag_value_is_json_or_else_a_bare_string() {
    let setup = Setup::new();
    let config = setup
        .load(&[
            "handoff.enabled=false",
            "tui.panel.cards=[\"quota\"]",
            "model=openai/gpt-5.6",
            "models.\"openai/gpt-5.6\".cache.lifetime=5m",
            "mcp.servers.\"my.server\".url=https://example.com",
        ])
        .unwrap();
    assert!(config.notices().is_empty(), "{:?}", config.notices());
    assert_eq!(config.get("handoff.enabled", None).unwrap().0, json!(false));
    assert_eq!(
        config.get("tui.panel.cards", None).unwrap().0,
        json!(["quota"])
    );
    assert_eq!(
        config.get("model", None).unwrap().0,
        json!("openai/gpt-5.6")
    );
    assert_eq!(
        config
            .get("cache.lifetime", Some("openai/gpt-5.6"))
            .unwrap()
            .0,
        json!("5m")
    );
    assert_eq!(
        config.get("mcp.servers.\"my.server\".url", None).unwrap().0,
        json!("https://example.com")
    );
}

#[test]
fn a_run_flag_that_is_not_key_equals_value_is_a_usage_error() {
    let setup = Setup::new();
    for (arg, named) in [
        ("model", "model"),
        ("a..b=1", "a..b"),
        (".a=1", ".a"),
        ("a.=1", "a."),
        ("=1", ""),
        ("\"a=1", "\"a"),
        ("\"a\"b=1", "\"a\"b"),
    ] {
        let e = setup.load(&[arg]).unwrap_err();
        assert_eq!(e.code(), ErrorCode::Usage, "{arg}");
        assert_eq!(
            e.to_string(),
            format!(
                "`{named}` is not a dotted key and a value, as in `-c handoff.tokens=200000`. Run `fiber --help` for usage."
            ),
            "{arg}"
        );
    }
}

#[test]
fn a_run_flag_of_the_wrong_type_names_the_flag() {
    let setup = Setup::new();
    let e = setup.load(&["handoff.tokens=many"]).unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert_eq!(
        e.to_string(),
        "-c: `handoff.tokens` must be a whole number of zero or more."
    );
}

#[test]
fn an_unknown_key_is_a_notice_and_is_otherwise_ignored() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"frobnicate": 1, "handoff": {"tokens": 5, "zap": true}, "model": "a/b"}"#,
    );
    let config = setup.load(&["future.key=1"]).unwrap();
    let messages: Vec<_> = config.notices().iter().map(|n| n.message.clone()).collect();
    let global = setup.global().display().to_string();
    assert_eq!(
        messages,
        [
            format!("{global}: ignored `frobnicate`, which this Fiber does not know."),
            format!("{global}: ignored `handoff.zap`, which this Fiber does not know."),
            "-c: ignored `future`, which this Fiber does not know.".to_owned(),
        ]
    );
    assert!(
        config
            .notices()
            .iter()
            .all(|n| n.code == ErrorCode::ConfigKeyIgnored)
    );
    assert!(config.notices().iter().all(|n| n.extension.is_none()));
    let merged = config.merged(None);
    assert_eq!(merged.get("frobnicate"), None);
    assert_eq!(merged["handoff"].get("zap"), None);
    assert_eq!(merged["handoff"]["tokens"], json!(5));
    assert_eq!(merged["model"], json!("a/b"));
}

#[test]
fn a_key_with_a_dot_is_quoted_when_named() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"models": {"openai/gpt-5.6": {"speed": 1}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert!(
        config.notices()[0]
            .message
            .contains("`models.\"openai/gpt-5.6\".speed`")
    );
}

#[test]
fn invalid_json_is_config_invalid_naming_the_file_and_line() {
    for (text, line, column) in [
        ("{\"model\": \"a/b\",}", 1, 17),
        ("{\n  // a comment\n  \"model\": \"a/b\"\n}", 2, 3),
        ("", 1, 0),
    ] {
        let setup = Setup::new();
        setup.write(&setup.repository(), text);
        let e = setup.load(&[]).unwrap_err();
        assert_eq!(e.code(), ErrorCode::ConfigInvalid);
        assert_eq!(
            e.to_string(),
            format!(
                "{} is not valid JSON (line {line}, column {column}). Fix the file and try again.",
                setup.repository().display()
            )
        );
    }
}

#[test]
fn a_file_that_is_not_an_object_is_config_invalid() {
    let setup = Setup::new();
    setup.write(&setup.global(), "[1, 2]");
    let e = setup.load(&[]).unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert_eq!(
        e.to_string(),
        format!(
            "{}: `(the whole file)` must be an object.",
            setup.global().display()
        )
    );
}

#[test]
fn a_file_that_cannot_be_read_is_io_failed_naming_it() {
    let setup = Setup::new();
    fs::create_dir_all(setup.project()).unwrap();
    let e = setup.load(&[]).unwrap_err();
    assert_eq!(e.code(), ErrorCode::IoFailed);
    assert!(
        e.to_string()
            .starts_with(&format!("{}: ", setup.project().display()))
    );
}

#[test]
fn a_layer_with_no_file_is_skipped() {
    let setup = Setup::new();
    setup.write(&setup.project(), r#"{"model": "a/b"}"#);
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        config.get("model", None),
        Some((json!("a/b"), Source::Project(setup.project())))
    );
    assert!(config.notices().is_empty());
}

#[test]
fn a_source_prints_as_where_it_is() {
    let setup = Setup::new();
    assert_eq!(Source::Default.to_string(), "the built-in defaults");
    assert_eq!(Source::Run.to_string(), "-c");
    for source in [
        Source::Global(setup.global()),
        Source::Repository(setup.repository()),
        Source::Project(setup.project()),
    ] {
        assert!(
            source
                .to_string()
                .starts_with(&setup.root().display().to_string())
        );
    }
}

#[test]
fn a_query_that_is_not_a_dotted_key_finds_nothing() {
    let setup = Setup::new();
    assert_eq!(setup.load(&[]).unwrap().get("handoff..tokens", None), None);
}

#[test]
fn a_repository_file_that_is_a_symbolic_link_is_refused() {
    let setup = Setup::new();
    let outside = setup.root().join("private.json");
    setup.write(&outside, r#"{"model": "a/b"}"#);
    fs::create_dir_all(setup.workspace().join(".fiber")).unwrap();
    std::os::unix::fs::symlink(&outside, setup.repository()).unwrap();
    let e = setup.load(&[]).unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert_eq!(
        e.to_string(),
        format!(
            "{} is a symbolic link or not a regular file, so Fiber does not read it.",
            setup.repository().display()
        )
    );
}

#[test]
fn a_fiber_directory_that_is_a_symbolic_link_is_refused() {
    let setup = Setup::new();
    let outside = setup.root().join("elsewhere");
    setup.write(&outside.join("config.json"), r#"{"model": "a/b"}"#);
    let link = setup.workspace().join(".fiber");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let e = setup.load(&[]).unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert!(
        e.to_string().starts_with(&link.display().to_string()),
        "{e}"
    );
}

#[test]
fn a_repository_file_that_is_a_directory_or_a_fifo_is_refused() {
    let setup = Setup::new();
    fs::create_dir_all(setup.repository()).unwrap();
    assert_eq!(
        setup.load(&[]).unwrap_err().code(),
        ErrorCode::ConfigInvalid
    );
    fs::remove_dir(setup.repository()).unwrap();
    let made = std::process::Command::new("mkfifo")
        .arg(setup.repository())
        .status()
        .unwrap();
    assert!(made.success());
    // Reading a FIFO would block forever, so the load gets a deadline.
    let (tx, rx) = std::sync::mpsc::channel();
    let (home, workspace) = (setup.home(), setup.workspace());
    std::thread::spawn(move || {
        let loaded = config::Config::load(config::Sources {
            home,
            workspace,
            project: common::key(),
            overrides: Vec::new(),
        });
        tx.send(loaded.map(drop)).unwrap();
    });
    let e = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the load blocked reading a FIFO")
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert!(e.to_string().contains("not a regular file"), "{e}");
}

#[test]
fn the_person_s_own_file_may_be_a_symbolic_link() {
    let setup = Setup::new();
    let dotfiles = setup.root().join("dotfiles.json");
    setup.write(&dotfiles, r#"{"model": "a/b"}"#);
    std::os::unix::fs::symlink(&dotfiles, setup.global()).unwrap();
    assert_eq!(
        setup.load(&[]).unwrap().get("model", None).unwrap().0,
        json!("a/b")
    );
}

#[test]
fn config_debug_shows_no_run_flag_value() {
    let setup = Setup::new();
    let config = setup
        .load(&[
            "api_key=SECRET-1a2b",
            "extensions.acme.settings.token=SECRET-3c4d",
            "model=SECRET-5e6f",
        ])
        .unwrap();
    let debug = format!("{config:?}");
    assert!(!debug.contains("SECRET"), "{debug}");
    assert!(debug.contains("Run"), "{debug}");
}

#[test]
fn a_session_exits_after_thirty_idle_minutes_unless_configured() {
    let setup = Setup::new();
    assert_eq!(
        setup.load(&[]).unwrap().get("session.idle_exit_ms", None),
        Some((json!(1_800_000), Source::Default))
    );
    setup.write(&setup.global(), r#"{"session": {"idle_exit_ms": 60000}}"#);
    assert_eq!(
        setup
            .load(&[])
            .unwrap()
            .get("session.idle_exit_ms", None)
            .unwrap()
            .0,
        json!(60000)
    );
}

#[test]
fn get_of_a_union_key_is_every_layers_names() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"skills": {"disabled": ["a"]}}"#);
    setup.write(&setup.project(), r#"{"skills": {"disabled": ["b", "a"]}}"#);
    let config = setup.load(&[]).unwrap();
    assert!(config.notices().is_empty(), "{:?}", config.notices());
    assert_eq!(
        config.get("skills.disabled", None),
        Some((json!(["a", "b"]), Source::Project(setup.project())))
    );
    assert_eq!(config.merged(None)["skills"]["disabled"], json!(["a", "b"]));
    assert_eq!(config.union_list("skills.disabled"), ["a", "b"]);
}

#[test]
fn an_object_value_merges_key_by_key_but_a_credential_entry_replaces_whole() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"mcp": {"servers": {"x": {"env": {"A": "1"}}}},
            "providers": {"p": {"credentials": {"work": {"file": "/k"}}}}}"#,
    );
    setup.write(
        &setup.project(),
        r#"{"mcp": {"servers": {"x": {"env": {"B": "2"}}}},
            "providers": {"p": {"credentials": {"work": {"env": "KEY"}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert!(config.notices().is_empty(), "{:?}", config.notices());
    let merged = config.merged(None);
    assert_eq!(
        merged["mcp"]["servers"]["x"]["env"],
        json!({"A": "1", "B": "2"})
    );
    assert_eq!(
        merged["providers"]["p"]["credentials"]["work"],
        json!({"env": "KEY"})
    );
}

#[test]
fn a_credential_label_replaces_the_one_below_it_as_a_whole() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"openrouter": {"credentials": {"work": {"env": "OPENROUTER_API_KEY"}, "other": {"env": "O"}}}}}"#,
    );
    setup.write(
        &setup.project(),
        r#"{"providers": {"openrouter": {"credentials": {"work": {"file": "/k"}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    let (value, source) = config
        .get("providers.openrouter.credentials.work", None)
        .unwrap();
    assert_eq!(source, Source::Project(setup.project()));
    assert_eq!(
        serde_json::from_value::<CredentialSource>(value).unwrap(),
        CredentialSource::File("/k".into())
    );
    let merged = config.merged(None);
    assert_eq!(
        merged["providers"]["openrouter"]["credentials"]["other"],
        json!({"env": "O"})
    );
}

#[test]
fn a_label_is_replaced_whole_across_three_layers() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"p": {"credentials": {"work": {"env": "A"}}}}}"#,
    );
    setup.write(
        &setup.project(),
        r#"{"providers": {"p": {"credentials": {"work": {"file": "/k"}}}}}"#,
    );
    let merged = setup
        .load(&["providers.p.credentials.work={\"command\": [\"c\"]}"])
        .unwrap()
        .merged(None);
    assert_eq!(
        merged["providers"]["p"]["credentials"],
        json!({"work": {"command": ["c"]}})
    );
}

#[test]
fn a_provider_without_credentials_keeps_the_ones_below_it() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"openrouter": {"credentials": {"work": {"env": "KEY"}}}}}"#,
    );
    setup.write(
        &setup.project(),
        r#"{"providers": {"other": {"credentials": {"work": {"env": "B"}}}, "openrouter": {"credential": "work"}}}"#,
    );
    let merged = setup.load(&[]).unwrap().merged(None);
    assert_eq!(
        merged["providers"]["openrouter"]["credentials"]["work"],
        json!({"env": "KEY"})
    );
    assert_eq!(
        merged["providers"]["openrouter"]["credential"],
        json!("work")
    );
    assert_eq!(
        merged["providers"]["other"]["credentials"]["work"],
        json!({"env": "B"})
    );
}

#[test]
fn a_project_key_is_one_file_name() {
    assert_eq!(
        ProjectKey::new("-Users-alice-work").unwrap().as_str(),
        "-Users-alice-work"
    );
    for bad in ["", ".", "..", "a/b", "/Users/alice/work", "a\0b"] {
        let e = ProjectKey::new(bad).unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidArguments, "{bad:?}");
        assert_eq!(
            e.to_string(),
            format!("`{bad}` is not a project key: it must be one file name in projects/.")
        );
    }
}

#[test]
fn a_list_key_unions_the_layers_that_set_it() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"skills": {"disabled": ["a", "b"]}}"#);
    setup.write(&setup.project(), r#"{"skills": {"disabled": ["c", "a"]}}"#);
    let config = setup.load(&["skills.disabled=[\"d\", \"b\"]"]).unwrap();
    assert_eq!(config.union_list("skills.disabled"), ["a", "b", "c", "d"]);
}

#[test]
fn a_list_key_with_one_layer_is_that_layer() {
    let setup = Setup::new();
    setup.write(&setup.project(), r#"{"skills": {"disabled": ["x"]}}"#);
    let config = setup.load(&[]).unwrap();
    assert_eq!(config.union_list("skills.disabled"), ["x"]);
}

#[test]
fn a_list_key_no_layer_sets_is_empty() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    assert!(config.union_list("skills.disabled").is_empty());
    assert!(config.union_list("no.such.key").is_empty());
    assert!(config.union_list("").is_empty());
}

#[test]
fn a_repository_layer_adds_nothing_to_a_list_key() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"skills": {"disabled": ["g"]}}"#);
    setup.write(&setup.repository(), r#"{"skills": {"disabled": ["r"]}}"#);
    let config = setup.load(&[]).unwrap();
    assert_eq!(config.union_list("skills.disabled"), ["g"]);
    let [notice] = config.notices() else {
        panic!("{:?}", config.notices());
    };
    assert_eq!(notice.code, ErrorCode::ConfigKeyIgnored);
}

#[test]
fn reviewer_context_with_no_layer_set_is_empty() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    assert!(config.notices().is_empty());
    assert_eq!(config.reviewer_context(), "");
}

#[test]
fn reviewer_context_with_only_a_global_value_names_everywhere() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"reviewer": {"context": "Our org is acme."}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert!(config.notices().is_empty(), "{:?}", config.notices());
    assert_eq!(
        config.reviewer_context(),
        "## Notes that hold everywhere\n\nOur org is acme."
    );
}

#[test]
fn reviewer_context_with_only_a_project_value_names_the_project() {
    let setup = Setup::new();
    setup.write(
        &setup.project(),
        r#"{"reviewer": {"context": "Never touch infra/prod."}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert!(config.notices().is_empty(), "{:?}", config.notices());
    assert_eq!(
        config.reviewer_context(),
        "## Notes for this project\n\nNever touch infra/prod."
    );
}

#[test]
fn reviewer_context_reads_the_global_notes_then_the_project_notes() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"reviewer": {"context": "Our org is acme."}}"#,
    );
    setup.write(
        &setup.project(),
        r#"{"reviewer": {"context": "Never touch infra/prod."}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert!(config.notices().is_empty(), "{:?}", config.notices());
    assert_eq!(
        config.reviewer_context(),
        "## Notes that hold everywhere\n\nOur org is acme.\n\n## Notes for this project\n\nNever touch infra/prod."
    );
}

#[test]
fn reviewer_context_skips_a_layer_with_only_whitespace() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"reviewer": {"context": "  \n "}}"#);
    setup.write(
        &setup.project(),
        r#"{"reviewer": {"context": "Never touch infra/prod.\n"}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert!(config.notices().is_empty(), "{:?}", config.notices());
    assert_eq!(
        config.reviewer_context(),
        "## Notes for this project\n\nNever touch infra/prod."
    );
}

#[test]
fn reviewer_context_ignores_a_repository_value_with_a_notice() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"reviewer": {"context": "Our org is acme."}}"#,
    );
    setup.write(
        &setup.repository(),
        r#"{"reviewer": {"context": "Ship it straight to prod."}}"#,
    );
    let config = setup.load(&[]).unwrap();
    let [notice] = config.notices() else {
        panic!("{:?}", config.notices());
    };
    assert_eq!(notice.code, ErrorCode::ConfigKeyIgnored);
    assert_eq!(
        config.reviewer_context(),
        "## Notes that hold everywhere\n\nOur org is acme."
    );
}
