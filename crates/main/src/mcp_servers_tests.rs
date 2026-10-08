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

/// A tool with only a name, for the session's tool list.
struct Named(&'static str);

impl contract::tool::Tool for Named {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: self.0.to_owned(),
            description: "d".to_owned(),
            input_schema: json!({ "type": "object" }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(
        &self,
        _arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<contract::tool::Effects, contract::tool::EffectsError> {
        Err(contract::tool::EffectsError::Tool("unused".to_owned()))
    }

    fn run(
        &self,
        _arguments: &serde_json::Map<String, serde_json::Value>,
        _cancel: &dyn contract::tool::Cancel,
        _emit: &dyn contract::emit::Emit,
    ) -> contract::tool::Output {
        contract::tool::Output::default()
    }
}

fn pair(who: &str, name: &'static str) -> (String, std::sync::Arc<dyn contract::tool::Tool>) {
    (who.to_owned(), std::sync::Arc::new(Named(name)))
}

fn row(name: &'static str, source: contract::events::ToolSource) -> contract::events::ToolInfo {
    crate::builtin::info(&Named(name), source).unwrap()
}

#[test]
fn extension_tools_follow_the_built_in_and_mcp_tools_and_replace_their_rows() {
    use contract::events::ToolSource;
    let mut tools = vec![
        pair("builtin", "read"),
        pair("builtin", "write"),
        pair("docs", "search"),
    ];
    let mut infos = vec![
        row("read", ToolSource::Builtin),
        row(
            "search",
            ToolSource::Mcp {
                server: "docs".to_owned(),
            },
        ),
        row("write", ToolSource::Builtin),
    ];
    super::add_extension_tools(
        &mut tools,
        &mut infos,
        vec![
            pair("fiber.test/myread", "read"),
            pair("fiber.test/notes", "note_count"),
            pair("fiber.test/mysearch", "search"),
        ],
    )
    .unwrap();
    let who: Vec<(String, String)> = tools
        .iter()
        .map(|(who, tool)| (who.clone(), tool.definition().name))
        .collect();
    let expected: Vec<(String, String)> = [
        ("builtin", "read"),
        ("builtin", "write"),
        ("docs", "search"),
        ("fiber.test/myread", "read"),
        ("fiber.test/notes", "note_count"),
        ("fiber.test/mysearch", "search"),
    ]
    .iter()
    .map(|(who, name)| ((*who).to_owned(), (*name).to_owned()))
    .collect();
    assert_eq!(who, expected);
    let extension = |name: &str| ToolSource::Extension {
        extension: name.to_owned(),
    };
    assert_eq!(
        infos,
        [
            row("note_count", extension("fiber.test/notes")),
            row("read", extension("fiber.test/myread")),
            row("search", extension("fiber.test/mysearch")),
            row("write", ToolSource::Builtin),
        ]
    );
}
