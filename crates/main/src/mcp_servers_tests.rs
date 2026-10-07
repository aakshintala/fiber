//! Configuration to [`ServerSpec`], without starting a process: every
//! server becomes a spec, a repository's stays out (the repository's offer
//! names it), and a `url` server is skipped silently.

use std::time::Duration;

use serde_json::json;

use super::specs;

struct Setup {
    _root: fakes::TempDir,
    home: std::path::PathBuf,
    workspace: std::path::PathBuf,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-mcp-servers");
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(workspace.join(".fiber")).unwrap();
        Self {
            _root: root,
            home,
            workspace,
        }
    }

    fn global(&self, value: &serde_json::Value) {
        std::fs::write(self.home.join("config.json"), value.to_string()).unwrap();
    }

    fn repository(&self, value: &serde_json::Value) {
        std::fs::write(self.workspace.join(".fiber/config.json"), value.to_string()).unwrap();
    }

    fn specs(&self) -> super::Specs {
        let project = config::ProjectKey::new("test").unwrap();
        let config = config::Config::load(config::Sources {
            home: self.home.clone(),
            workspace: self.workspace.clone(),
            project,
            overrides: Vec::new(),
        })
        .unwrap();
        specs(&config)
    }
}

#[test]
fn no_servers_starts_nothing() {
    let setup = Setup::new();
    let specs = setup.specs();
    assert!(specs.specs.is_empty());
    assert!(specs.notices.is_empty());
}

#[test]
fn a_persons_server_becomes_a_spec_with_defaults() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"command": "/bin/bash"}}}}));
    let specs = setup.specs();
    assert_eq!(specs.notices.len(), 0);
    assert_eq!(specs.specs.len(), 1);
    let spec = &specs.specs[0];
    assert_eq!(spec.name, "fx");
    assert_eq!(spec.command, "/bin/bash");
    assert!(spec.args.is_empty());
    assert!(spec.env.is_empty());
    assert_eq!(spec.startup_timeout, Duration::from_millis(5000));
    assert_eq!(spec.call_timeout, Duration::from_millis(600_000));
    assert_eq!(spec.enabled, None);
    assert!(spec.disabled.is_empty());
    assert!(spec.hints.is_empty());
    assert!(!spec.required);
}

#[test]
fn required_true_reaches_the_spec() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"command": "/bin/bash", "required": true}}}}));
    let specs = setup.specs();
    assert_eq!(specs.specs.len(), 1);
    assert!(specs.specs[0].required);
}

#[test]
fn timeouts_lists_and_overrides_reach_the_spec() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {
        "command": "/bin/bash",
        "args": ["--version"],
        "env": {"FX": "1"},
        "startup_timeout_ms": 1000,
        "timeout_ms": 2000,
        "tools": {
            "enabled": ["echo"],
            "disabled": ["other"],
            "echo": {"hints": {"readOnlyHint": true}},
        },
    }}}}));
    let specs = setup.specs();
    assert_eq!(specs.specs.len(), 1);
    let spec = &specs.specs[0];
    assert_eq!(spec.args, vec!["--version".to_owned()]);
    assert_eq!(spec.env.get("FX").map(String::as_str), Some("1"));
    assert_eq!(spec.startup_timeout, Duration::from_millis(1000));
    assert_eq!(spec.call_timeout, Duration::from_millis(2000));
    assert_eq!(spec.enabled, Some(vec!["echo".to_owned()]));
    assert_eq!(spec.disabled, vec!["other".to_owned()]);
    assert_eq!(
        spec.hints.get("echo").map(|hints| hints.read_only),
        Some(Some(true))
    );
}

#[test]
fn a_persons_empty_env_does_not_mask_a_repositorys_entry() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"command": "/bin/bash", "env": {}}}}}));
    setup.repository(&json!({"mcp": {"servers": {"fx": {"env": {"BASH_ENV": "/tmp/evil"}}}}}));
    let specs = setup.specs();
    assert!(specs.specs.is_empty());
    assert!(specs.notices.is_empty());
}

#[test]
fn a_repositorys_entry_wins_over_the_global_one() {
    // Layers merge lowest first with the repository above the global
    // file, so the repository's value is the effective one: still skipped.
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"command": "/bin/bash", "env": {"BASH_ENV": "/tmp/safe"}}}}}));
    setup.repository(&json!({"mcp": {"servers": {"fx": {"env": {"BASH_ENV": "/tmp/evil"}}}}}));
    let specs = setup.specs();
    assert!(specs.specs.is_empty());
    assert!(specs.notices.is_empty());
}

#[test]
fn a_repositorys_server_is_skipped() {
    let setup = Setup::new();
    setup.repository(&json!({"mcp": {"servers": {"repo": {"command": "/bin/bash"}}}}));
    let specs = setup.specs();
    assert!(specs.specs.is_empty());
    assert!(specs.notices.is_empty());
}

#[test]
fn a_persons_command_with_a_repositorys_args_is_repository_code() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"command": "/bin/bash"}}}}));
    setup.repository(&json!({"mcp": {"servers": {"fx": {"args": ["evil.sh"]}}}}));
    let specs = setup.specs();
    assert!(specs.specs.is_empty());
    assert!(specs.notices.is_empty());
}

#[test]
fn a_repositorys_hints_never_reach_the_spec() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"command": "/bin/bash"}}}}));
    setup.repository(&json!({"mcp": {"servers": {"fx": {
        "tools": {"echo": {"hints": {"destructiveHint": true}}},
    }}}}));
    let specs = setup.specs();
    assert_eq!(specs.specs.len(), 1);
    assert!(specs.specs[0].hints.is_empty());
}

#[test]
fn a_url_server_is_skipped_silently() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"remote": {"url": "https://example.com/mcp"}}}}));
    let specs = setup.specs();
    assert!(specs.specs.is_empty());
    assert!(specs.notices.is_empty());
}

#[test]
fn a_dotted_name_reads_its_own_keys() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"my.server": {"command": "/bin/bash"}}}}));
    let specs = setup.specs();
    assert_eq!(specs.specs.len(), 1);
    assert_eq!(specs.specs[0].name, "my.server");
}
