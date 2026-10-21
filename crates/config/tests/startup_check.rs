//! The shared startup classification: refusals with their sentences,
//! the repository partition, name order, debug redaction, and the script
//! read's bounded, never-blocking behavior.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::Setup;
use config::{
    ConfigError, McpRefusalKind, McpServer, McpTransport, ScriptRead, mcp_servers, read_script,
};
use fakes::Deadline;
use serde_json::json;

/// Whether the URL holds a user name or password, as the tests see it.
fn has_userinfo(url: &str) -> bool {
    url.contains('@')
}

/// The servers of a global `mcp.servers` value.
fn checked(global: &serde_json::Value) -> config::McpServers {
    let setup = Setup::new();
    setup.write(&setup.global(), &global.to_string());
    let config = setup.load(&[]).unwrap();
    mcp_servers(&config, &has_userinfo).unwrap()
}

/// The refusal for one global entry.
fn refused(global: &serde_json::Value) -> config::McpRefusal {
    let found = checked(global).refused;
    assert_eq!(found.len(), 1, "{found:?}");
    found.into_iter().next().unwrap()
}

#[test]
fn an_entry_with_both_transports_is_a_named_refusal() {
    let refusal = refused(
        &json!({"mcp": {"servers": {"fx": {"command": "run", "url": "https://fx.example/mcp"}}}}),
    );
    assert_eq!(refusal.name, "fx");
    assert!(!refusal.required);
    assert!(matches!(refusal.kind, McpRefusalKind::Both));
    assert_eq!(
        refusal.message(),
        "The MCP server `fx` declares both `command` and `url`, so Fiber starts neither. Keep the one you mean."
    );
}

#[test]
fn a_required_entry_with_both_transports_fails_the_read() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        &json!({"mcp": {"servers": {"fx": {"command": "run", "url": "https://fx.example/mcp", "required": true}}}}).to_string(),
    );
    let config = setup.load(&[]).unwrap();
    let refusal = mcp_servers(&config, &has_userinfo).unwrap_err();
    assert_eq!(refusal.name, "fx");
    assert!(refusal.required);
    assert!(matches!(refusal.kind, McpRefusalKind::Both));
    assert_eq!(
        refusal.message(),
        "The required MCP server `fx` declares both `command` and `url`, so Fiber cannot start it. Keep the one you mean."
    );
}

#[test]
fn a_url_with_userinfo_is_a_named_refusal_for_every_shape() {
    for url in [
        "https://tokenuser-7f3@h/mcp",
        "https://tokenuser-7f3:pw-7f3@h/mcp",
        "https://@h/mcp",
    ] {
        for required in [false, true] {
            let setup = Setup::new();
            setup.write(
                &setup.global(),
                &json!({"mcp": {"servers": {"docs": {"url": url, "required": required}}}})
                    .to_string(),
            );
            let config = setup.load(&[]).unwrap();
            if required {
                let refusal = mcp_servers(&config, &has_userinfo).unwrap_err();
                assert_eq!(refusal.name, "docs");
                assert!(matches!(refusal.kind, McpRefusalKind::Userinfo));
                assert_eq!(
                    refusal.message(),
                    "The required MCP server `docs` has a user name or password in its `url`, so Fiber cannot start it. Take them out of the URL."
                );
            } else {
                let refusal = refused(&json!({"mcp": {"servers": {"docs": {"url": url}}}}));
                assert_eq!(refusal.name, "docs");
                assert!(matches!(refusal.kind, McpRefusalKind::Userinfo));
                assert_eq!(
                    refusal.message(),
                    "The MCP server `docs` has a user name or password in its `url`, so Fiber does not start it. Take them out of the URL."
                );
            }
        }
    }
}

#[test]
fn a_clean_url_is_a_remote_server() {
    let found =
        checked(&json!({"mcp": {"servers": {"docs": {"url": "https://docs.example/mcp"}}}}));
    assert!(found.refused.is_empty());
    let [server] = found.servers.as_slice() else {
        panic!("one server: {found:?}");
    };
    assert_eq!(server.name, "docs");
    assert!(!server.required);
    assert!(!server.repository);
    let McpTransport::Remote { url } = &server.transport else {
        panic!("a remote server: {server:?}");
    };
    assert_eq!(url, "https://docs.example/mcp");
}

#[test]
fn a_well_named_secret_passes_with_names_but_no_values_read() {
    let found = checked(
        &json!({"mcp": {"servers": {"fx": {"command": "run", "args": ["--serve"], "env": {"KEY": "literal", "TOK": {"secret": "mcp.fx.API_KEY"}}}}}}),
    );
    assert!(found.refused.is_empty());
    let [server] = found.servers.as_slice() else {
        panic!("one server: {found:?}");
    };
    let McpTransport::Stdio {
        command,
        args,
        env,
        secrets,
    } = &server.transport
    else {
        panic!("a stdio server: {server:?}");
    };
    assert_eq!(command, "run");
    assert_eq!(args, &["--serve"]);
    assert_eq!(env.get("KEY").map(String::as_str), Some("literal"));
    assert_eq!(
        secrets.get("TOK").map(String::as_str),
        Some("mcp.fx.API_KEY")
    );
}

#[test]
fn a_secret_named_for_another_server_is_a_named_refusal() {
    for secret in ["other.API_KEY", "mcp.other.API_KEY", "API_KEY"] {
        for required in [false, true] {
            let setup = Setup::new();
            setup.write(
                &setup.global(),
                &json!({"mcp": {"servers": {"fx": {"command": "run", "env": {"TOK": {"secret": secret}}, "required": required}}}}).to_string(),
            );
            let config = setup.load(&[]).unwrap();
            let outcome = mcp_servers(&config, &has_userinfo);
            let refusal = if required {
                outcome.unwrap_err()
            } else {
                let mut refused = outcome.unwrap().refused;
                assert_eq!(refused.len(), 1);
                refused.pop().unwrap()
            };
            assert_eq!(refusal.name, "fx");
            let McpRefusalKind::BadSecret { secret: name } = &refusal.kind else {
                panic!("a bad secret: {refusal:?}");
            };
            assert_eq!(name, secret);
            let seen = refusal.message();
            assert!(seen.contains(&format!("`{secret}`")), "{seen}");
            assert!(seen.contains("`mcp.fx.<KEY>`"), "{seen}");
            // The refusal's polarity is the sentence's verb: a required
            // entry stops startup, an optional one is skipped.
            if required {
                assert!(seen.contains("cannot start it"), "{seen}");
            } else {
                assert!(seen.contains("does not start it"), "{seen}");
            }
        }
    }
}

#[test]
fn an_entry_with_neither_transport_is_skipped_silently() {
    let found = checked(&json!({"mcp": {"servers": {"idle": {"required": true}}}}));
    assert!(found.servers.is_empty());
    assert!(found.refused.is_empty());
}

#[test]
fn without_servers_nothing_is_classified() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"diagnostics": {"level": "debug"}}"#);
    let config = setup.load(&[]).unwrap();
    let found = mcp_servers(&config, &has_userinfo).unwrap();
    assert!(found.servers.is_empty());
    assert!(found.refused.is_empty());
}

#[test]
fn a_dotted_name_is_quoted_to_its_own_entry() {
    let found = checked(&json!({"mcp": {"servers": {"a.b": {"command": "run"}}}}));
    assert!(found.refused.is_empty());
    let [server] = found.servers.as_slice() else {
        panic!("one server: {found:?}");
    };
    assert_eq!(server.name, "a.b");
    assert!(!server.repository);
}

#[test]
fn servers_and_refusals_come_out_in_name_order() {
    let found = checked(&json!({"mcp": {"servers": {
        "zed": {"command": "run", "url": "https://zed.example/mcp"},
        "mid": {"command": "run"},
        "arc": {"url": "https://arc.example/mcp"},
    }}}));
    let names: Vec<_> = found
        .servers
        .iter()
        .map(|server| server.name.as_str())
        .collect();
    assert_eq!(names, ["arc", "mid"]);
    let refused: Vec<_> = found
        .refused
        .iter()
        .map(|refusal| refusal.name.as_str())
        .collect();
    assert_eq!(refused, ["zed"]);
}

#[test]
fn a_repository_declaration_gates_a_persons_command() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        &json!({"mcp": {"servers": {"fx": {"command": "run"}}}}).to_string(),
    );
    setup.write(
        &setup.repository(),
        &json!({"mcp": {"servers": {"fx": {"required": true}}}}).to_string(),
    );
    let config = setup.load(&[]).unwrap();
    let found = mcp_servers(&config, &has_userinfo).unwrap();
    let [server] = found.servers.as_slice() else {
        panic!("one server: {found:?}");
    };
    assert!(server.repository);
}

#[test]
fn a_repository_env_tweak_gates_a_persons_server() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        &json!({"mcp": {"servers": {"fx": {"command": "run"}}}}).to_string(),
    );
    setup.write(
        &setup.repository(),
        &json!({"mcp": {"servers": {"fx": {"env": {"BASH_ENV": "/tmp/evil"}}}}}).to_string(),
    );
    let config = setup.load(&[]).unwrap();
    let found = mcp_servers(&config, &has_userinfo).unwrap();
    let [server] = found.servers.as_slice() else {
        panic!("one server: {found:?}");
    };
    assert!(server.repository);
    let McpTransport::Stdio { env, .. } = &server.transport else {
        panic!("a stdio server: {server:?}");
    };
    assert_eq!(env.get("BASH_ENV").map(String::as_str), Some("/tmp/evil"));
}

#[test]
fn a_repository_command_gates_and_a_persons_url_does_not() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        &json!({"mcp": {"servers": {"docs": {"url": "https://docs.example/mcp"}}}}).to_string(),
    );
    setup.write(
        &setup.repository(),
        &json!({"mcp": {"servers": {"repo": {"command": "run"}}}}).to_string(),
    );
    let config = setup.load(&[]).unwrap();
    let found = mcp_servers(&config, &has_userinfo).unwrap();
    assert!(found.refused.is_empty());
    let by_name = |name: &str| {
        found
            .servers
            .iter()
            .find(|server| server.name == name)
            .unwrap_or_else(|| panic!("a {name} server: {found:?}"))
    };
    assert!(!by_name("docs").repository);
    assert!(by_name("repo").repository);
}

#[test]
fn provenance_gates_when_the_declaration_is_removed_after_load() {
    // The layers keep the repository's share after the file is gone, but
    // the declarations are re-read: with nothing declared, each
    // per-field provenance leg decides its own server.
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        &json!({"mcp": {"servers": {
            "fx-cmd": {"command": "run"},
            "fx-args": {"command": "run"},
            "fx-env": {"command": "run"},
        }}})
        .to_string(),
    );
    setup.write(
        &setup.repository(),
        &json!({"mcp": {"servers": {
            "fx-cmd": {"command": "run"},
            "fx-args": {"args": ["--repo"]},
            "fx-url": {"url": "https://fx.example/mcp"},
            "fx-env": {"env": {"BASH_ENV": "/tmp/evil"}},
        }}})
        .to_string(),
    );
    let config = setup.load(&[]).unwrap();
    fs::remove_file(setup.repository()).unwrap();
    let found = mcp_servers(&config, &has_userinfo).unwrap();
    assert!(found.refused.is_empty(), "{found:?}");
    for name in ["fx-cmd", "fx-args", "fx-url", "fx-env"] {
        let server = found
            .servers
            .iter()
            .find(|server| server.name == name)
            .unwrap_or_else(|| panic!("a {name} server: {found:?}"));
        assert!(
            server.repository,
            "{name} stays gated by its own field: {found:?}"
        );
    }
}

#[test]
fn debug_prints_names_but_no_command_argument_value_or_url() {
    let found = checked(&json!({"mcp": {"servers": {
        "fx": {"command": "run-sekret-cmd", "args": ["--sekret-flag"], "env": {"KEY": "sekret-literal", "TOK": {"secret": "mcp.fx.API_KEY"}}},
        "docs": {"url": "https://sekret-url.example/mcp"},
    }}}));
    let shown = format!("{:?}", found.servers);
    assert!(shown.contains("fx"), "{shown}");
    assert!(shown.contains("docs"), "{shown}");
    // The transports render redacted: the debug names the shape without
    // any value.
    assert!(shown.contains("Stdio"), "{shown}");
    assert!(shown.contains("Remote"), "{shown}");
    assert!(shown.contains("redacted"), "{shown}");
    for secret in [
        "run-sekret-cmd",
        "--sekret-flag",
        "sekret-literal",
        "sekret-url.example",
    ] {
        assert!(!shown.contains(secret), "{shown}");
    }
    // The servers' own debug names the set without any value.
    let shown = format!("{found:?}");
    assert!(shown.contains("McpServers"), "{shown}");
    assert!(shown.contains("fx"), "{shown}");
    assert!(!shown.contains("run-sekret-cmd"), "{shown}");
}

#[test]
fn debug_names_a_refusal_and_its_kind() {
    let refusal = refused(
        &json!({"mcp": {"servers": {"fx": {"command": "run", "url": "https://fx.example/mcp"}}}}),
    );
    let shown = format!("{refusal:?}");
    assert!(shown.contains("McpRefusal"), "{shown}");
    assert!(shown.contains("fx"), "{shown}");
    assert!(shown.contains("Both"), "{shown}");
}

/// One `read_script` call on a worker thread, so a blocking read would
/// fail the test instead of hanging it.
fn read_on_worker(path: &Path) -> Result<ScriptRead, ConfigError> {
    let path = path.to_path_buf();
    fakes::within(
        "a script read",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || read_script(&path),
    )
}

#[test]
fn read_script_reads_a_regular_file_and_follows_a_link() {
    let setup = Setup::new();
    let file = setup.root().join("run.sh");
    setup.write(&file, "#!/bin/sh\necho hi\n");
    assert!(matches!(
        read_on_worker(&file).unwrap(),
        ScriptRead::Readable
    ));
    let link = setup.root().join("linked.sh");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert!(matches!(
        read_on_worker(&link).unwrap(),
        ScriptRead::Readable
    ));
}

#[test]
fn read_script_reads_a_large_file_without_a_size_limit() {
    let setup = Setup::new();
    let file = setup.root().join("big.sh");
    setup.write(&file, &"echo line\n".repeat(30_000));
    assert!(matches!(
        read_on_worker(&file).unwrap(),
        ScriptRead::Readable
    ));
}

#[test]
fn read_script_never_waits_on_a_fifo_or_its_link() {
    let setup = Setup::new();
    let fifo = setup.root().join("pipe");
    fakes::fifo(&fifo);
    assert!(matches!(
        read_on_worker(&fifo).unwrap(),
        ScriptRead::NotChecked
    ));
    let link = setup.root().join("pipe-link");
    std::os::unix::fs::symlink(&fifo, &link).unwrap();
    assert!(matches!(
        read_on_worker(&link).unwrap(),
        ScriptRead::NotChecked
    ));
}

#[test]
fn read_script_reports_a_missing_file_and_a_directory_as_io_errors() {
    let setup = Setup::new();
    let missing = setup.root().join("absent.sh");
    let ConfigError::Io { source, .. } = read_on_worker(&missing).unwrap_err() else {
        panic!("a missing script is an Io error");
    };
    assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
    let ConfigError::Io { source, .. } = read_on_worker(setup.root()).unwrap_err() else {
        panic!("a directory script is an Io error");
    };
    assert_eq!(source.kind(), std::io::ErrorKind::IsADirectory);
}

#[test]
fn read_script_reports_an_unreadable_file_as_io_errors() {
    let setup = Setup::new();
    let file = setup.root().join("locked.sh");
    setup.write(&file, "echo hi\n");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&file).is_ok() {
        // A privileged runner reads through the mode, so there is no
        // refusal to check here.
        return;
    }
    let ConfigError::Io { source, .. } = read_on_worker(&file).unwrap_err() else {
        panic!("an unreadable script is an Io error");
    };
    assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
}

/// The shared classification answers through one entry point.
#[test]
fn the_servers_come_from_one_shared_check() {
    let servers: Vec<McpServer> =
        checked(&json!({"mcp": {"servers": {"fx": {"command": "run"}}}})).servers;
    assert_eq!(servers.len(), 1);
}
