//! Configuration to [`ServerSpec`], without starting a process: every
//! server becomes a spec, a repository's stays out (the repository's offer
//! names it), a `url` server becomes a remote spec, and an entry with
//! both is never started.

use std::time::Duration;

use serde_json::json;

use super::specs;

/// The stdio half of `spec`: an exhaustive match, so a new transport
/// breaks every site that assumes stdio.
fn stdio(spec: &mcp::ServerSpec) -> &mcp::StdioSpec {
    match &spec.transport {
        mcp::Transport::Stdio(stdio) => stdio,
        mcp::Transport::Http { .. } => panic!("a stdio-only test met a remote spec"),
    }
}

struct Setup {
    rig: crate::test_support::Rig,
}

impl Setup {
    fn new() -> Self {
        let rig = crate::test_support::Rig::new("fiber-mcp-servers");
        std::fs::create_dir_all(rig.workspace.join(".fiber")).unwrap();
        Self { rig }
    }

    fn global(&self, value: &serde_json::Value) {
        self.rig.write_global(value);
    }

    fn repository(&self, value: &serde_json::Value) {
        std::fs::write(
            self.rig.workspace.join(".fiber/config.json"),
            value.to_string(),
        )
        .unwrap();
    }

    fn project(&self, value: &serde_json::Value) {
        let dir = self.rig.home.join("projects").join("test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), value.to_string()).unwrap();
    }

    fn specs(&self) -> super::Specs {
        let config = self.rig.config(Vec::<String>::new());
        specs(&config).expect("the specs read")
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
    assert_eq!(stdio(spec).command, "/bin/bash");
    assert!(stdio(spec).args.is_empty());
    assert!(stdio(spec).env.is_empty());
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
    assert_eq!(stdio(spec).args, vec!["--version".to_owned()]);
    assert_eq!(stdio(spec).env.get("FX").map(String::as_str), Some("1"));
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
    let [gated] = specs.repository.as_slice() else {
        panic!("the server is the repository's");
    };
    assert_eq!(gated.spec.name, "fx");
    assert_eq!(stdio(&gated.spec).command, "/bin/bash");
    // Exactly the repository-supplied values: the person's command and
    // empty `env` are the person's own.
    assert_eq!(gated.fields.command, None);
    assert_eq!(gated.fields.args, None);
    assert_eq!(
        gated.fields.env,
        std::collections::BTreeMap::from([("BASH_ENV".to_owned(), json!("/tmp/evil"))])
    );
}

#[test]
fn a_repositorys_entry_wins_over_the_global_one() {
    // Layers merge lowest first with the repository above the global
    // file, so the repository's value is the effective one: still gated.
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"command": "/bin/bash", "env": {"BASH_ENV": "/tmp/safe"}}}}}));
    setup.repository(&json!({"mcp": {"servers": {"fx": {"env": {"BASH_ENV": "/tmp/evil"}}}}}));
    let specs = setup.specs();
    assert!(specs.specs.is_empty());
    assert!(specs.notices.is_empty());
    let [gated] = specs.repository.as_slice() else {
        panic!("the server is the repository's");
    };
    assert_eq!(gated.spec.name, "fx");
    assert_eq!(
        gated.fields.env,
        std::collections::BTreeMap::from([("BASH_ENV".to_owned(), json!("/tmp/evil"))])
    );
}

#[test]
fn a_repository_declaration_gates_a_persons_command_without_command_provenance() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"command": "/bin/bash"}}}}));
    setup.repository(&json!({"mcp": {"servers": {"fx": {"required": true}}}}));
    let specs = setup.specs();
    assert!(
        specs.specs.is_empty(),
        "the declaration is not person-owned"
    );
    let [gated] = specs.repository.as_slice() else {
        panic!("the repository declaration gates the person's command");
    };
    assert_eq!(gated.spec.name, "fx");
    assert_eq!(stdio(&gated.spec).command, "/bin/bash");
    assert_eq!(gated.fields.command, None, "command provenance is global");
}

#[test]
fn a_repositorys_server_is_repository_code() {
    let setup = Setup::new();
    setup.repository(&json!({"mcp": {"servers": {"repo": {"command": "/bin/bash"}}}}));
    let specs = setup.specs();
    assert!(specs.specs.is_empty());
    assert!(specs.notices.is_empty());
    let [gated] = specs.repository.as_slice() else {
        panic!("the server is the repository's");
    };
    assert_eq!(gated.spec.name, "repo");
    assert_eq!(stdio(&gated.spec).command, "/bin/bash");
    assert!(stdio(&gated.spec).run.is_none());
    assert_eq!(gated.fields.command, Some(json!("/bin/bash")));
    assert_eq!(gated.fields.args, None);
    assert!(gated.fields.env.is_empty());
}

#[test]
fn a_persons_command_with_a_repositorys_args_is_repository_code() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"command": "/bin/bash"}}}}));
    setup.repository(&json!({"mcp": {"servers": {"fx": {"args": ["evil.sh"]}}}}));
    let specs = setup.specs();
    assert!(specs.specs.is_empty());
    assert!(specs.notices.is_empty());
    let [gated] = specs.repository.as_slice() else {
        panic!("the server is the repository's");
    };
    assert_eq!(gated.spec.name, "fx");
    assert_eq!(stdio(&gated.spec).args, vec!["evil.sh".to_owned()]);
    assert_eq!(gated.fields.command, None);
    assert_eq!(gated.fields.args, Some(json!(["evil.sh"])));
    assert!(gated.fields.env.is_empty());
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
fn a_declaration_removed_after_load_still_gates_through_command_provenance() {
    // The declaration is removed between `Config::load` and `specs`:
    // the merged `command` still comes from the repository's layer, so
    // the fail-closed provenance fallback keeps the server the
    // repository's even though the declaration read no longer names it.
    let setup = Setup::new();
    setup.repository(&json!({"mcp": {"servers": {"fx": {"command": "/bin/bash"}}}}));
    let config = setup.rig.config(Vec::<String>::new());
    setup.repository(&json!({}));
    let specs = specs(&config).expect("the specs read");
    assert!(specs.specs.is_empty());
    let [gated] = specs.repository.as_slice() else {
        panic!("the server is the repository's through provenance alone");
    };
    assert_eq!(gated.spec.name, "fx");
}

#[test]
fn a_declaration_removed_after_load_still_gates_through_env_provenance() {
    // As above, through one repository-supplied `env` entry: `env`
    // merges key by key, so each entry's provenance is checked
    // individually.
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"command": "/bin/bash"}}}}));
    setup.repository(&json!({"mcp": {"servers": {"fx": {"env": {"BASH_ENV": "/tmp/evil"}}}}}));
    let config = setup.rig.config(Vec::<String>::new());
    setup.repository(&json!({}));
    let specs = specs(&config).expect("the specs read");
    assert!(specs.specs.is_empty());
    let [gated] = specs.repository.as_slice() else {
        panic!("the server is the repository's through provenance alone");
    };
    assert_eq!(gated.spec.name, "fx");
    assert_eq!(
        gated.fields.env,
        std::collections::BTreeMap::from([("BASH_ENV".to_owned(), json!("/tmp/evil"))])
    );
}

#[test]
fn program_fields_debug_prints_no_token_or_env_value() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"command": "/bin/bash"}}}}));
    setup.repository(&json!({"mcp": {"servers": {"fx": {
        "args": ["--token=planted6a3f"],
        "env": {"SECRET": "planted1c8b"},
    }}}}));
    let specs = setup.specs();
    let [gated] = specs.repository.as_slice() else {
        panic!("the server is the repository's");
    };
    let printed = format!("{:?}", gated.fields);
    assert!(printed.contains("SECRET"), "{printed}");
    assert!(!printed.contains("planted6a3f"), "{printed}");
    assert!(!printed.contains("planted1c8b"), "{printed}");
}

#[test]
fn a_url_server_becomes_a_remote_spec() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"remote": {"url": "https://example.com/mcp"}}}}));
    let specs = setup.specs();
    assert!(specs.notices.is_empty());
    let [spec] = specs.specs.as_slice() else {
        panic!("one remote spec");
    };
    assert_eq!(spec.name, "remote");
    let mcp::Transport::Http { url } = &spec.transport else {
        panic!("a remote transport");
    };
    assert_eq!(url, "https://example.com/mcp");
    assert_eq!(spec.startup_timeout, Duration::from_millis(5000));
    assert_eq!(spec.call_timeout, Duration::from_millis(600_000));
    assert_eq!(spec.enabled, None);
    assert!(spec.disabled.is_empty());
    assert!(spec.hints.is_empty());
    assert!(!spec.required);
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
                tool: "search".to_owned(),
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

#[test]
fn a_declaration_error_keeps_a_persons_server_on_the_repository_side() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"command": "/bin/bash"}}}}));
    std::fs::create_dir_all(setup.rig.workspace.join(".fiber/config")).unwrap();
    std::fs::write(
        setup.rig.workspace.join(".fiber/config/hooks.json"),
        json!({"hooks": true}).to_string(),
    )
    .unwrap();
    let specs = setup.specs();
    assert!(specs.specs.is_empty());
    let [gated] = specs.repository.as_slice() else {
        panic!("a declaration error keeps the server behind approval");
    };
    assert_eq!(gated.spec.name, "fx");
}

#[test]
fn a_persons_server_the_repository_only_retimes_is_repository_code() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"command": "/bin/bash"}}}}));
    setup.repository(&json!({"mcp": {"servers": {"fx": {"timeout_ms": 1}}}}));
    let specs = setup.specs();
    assert!(specs.specs.is_empty());
    let [gated] = specs.repository.as_slice() else {
        panic!("the server is the repository's");
    };
    assert_eq!(gated.spec.name, "fx");
    // No `command`, `args` or `env` comes from the repository.
    assert_eq!(gated.fields.command, None);
    assert_eq!(gated.fields.args, None);
    assert!(gated.fields.env.is_empty());
}

#[test]
fn a_persons_server_the_repository_only_hints_stays_the_persons() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"command": "/bin/bash"}}}}));
    setup.repository(&json!({"mcp": {"servers": {"fx": {
        "tools": {"echo": {"hints": {"readOnlyHint": true}}},
    }}}}));
    let specs = setup.specs();
    assert!(specs.repository.is_empty());
    assert_eq!(specs.specs.len(), 1);
    assert_eq!(specs.specs[0].name, "fx");
}

#[test]
fn repository_fields_hold_exactly_what_the_repository_supplied() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {
        "command": "/bin/bash",
        "args": ["--safe"],
        "env": {"SAFE": "1", "MODE": "slow"},
    }}}}));
    setup.repository(&json!({"mcp": {"servers": {"fx": {
        "args": ["run.sh"],
        "env": {"MODE": "fast"},
    }}}}));
    let specs = setup.specs();
    assert!(specs.specs.is_empty());
    let [gated] = specs.repository.as_slice() else {
        panic!("the server is the repository's");
    };
    // The merged spec carries both layers; the fields carry only the
    // repository's share.
    assert_eq!(stdio(&gated.spec).command, "/bin/bash");
    assert_eq!(stdio(&gated.spec).args, vec!["run.sh".to_owned()]);
    assert_eq!(gated.fields.command, None);
    assert_eq!(gated.fields.args, Some(json!(["run.sh"])));
    assert_eq!(
        gated.fields.env,
        std::collections::BTreeMap::from([("MODE".to_owned(), json!("fast"))])
    );
}

#[test]
fn a_server_with_both_command_and_url_is_never_started_with_a_notice() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {
        "command": "/bin/bash",
        "url": "https://example.com/mcp",
    }}}}));
    let specs = setup.specs();
    assert!(specs.specs.is_empty());
    assert!(specs.repository.is_empty());
    let [notice] = specs.notices.as_slice() else {
        panic!("exactly one notice");
    };
    assert_eq!(notice.code, contract::ErrorCode::ConfigInvalid);
    assert_eq!(
        notice.message,
        "The MCP server `fx` declares both `command` and `url`, so Fiber starts neither. Keep the one you mean."
    );
}

#[test]
fn a_required_server_with_both_command_and_url_fails_the_session() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {
        "command": "/bin/bash",
        "url": "https://example.com/mcp",
        "required": true,
    }}}}));
    let config = setup.rig.config(Vec::<String>::new());
    let error = match specs(&config) {
        Ok(_) => panic!("a required both-keys entry fails"),
        Err(error) => error,
    };
    assert_eq!(error.code, contract::ErrorCode::ConfigInvalid);
    assert_eq!(
        error.message,
        "The required MCP server `fx` declares both `command` and `url`, so Fiber cannot start it. Keep the one you mean."
    );
}

#[test]
fn command_and_url_from_different_layers_are_still_both() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"command": "/bin/bash"}}}}));
    setup.project(&json!({"mcp": {"servers": {"fx": {"url": "https://example.com/mcp"}}}}));
    let specs = setup.specs();
    assert!(specs.specs.is_empty());
    assert!(specs.repository.is_empty());
    let [notice] = specs.notices.as_slice() else {
        panic!("exactly one notice");
    };
    assert_eq!(notice.code, contract::ErrorCode::ConfigInvalid);
    assert!(notice.message.contains("`fx`"), "{}", notice.message);
}

#[test]
fn a_server_with_neither_command_nor_url_is_skipped_silently() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"timeout_ms": 1}}}}));
    let specs = setup.specs();
    assert!(specs.specs.is_empty());
    assert!(specs.repository.is_empty());
    assert!(specs.notices.is_empty());
}

#[test]
fn a_repositorys_url_server_is_repository_code_with_its_url_in_fields() {
    let setup = Setup::new();
    setup.repository(&json!({"mcp": {"servers": {"fx": {"url": "https://example.com/mcp"}}}}));
    let specs = setup.specs();
    assert!(specs.specs.is_empty());
    assert!(specs.notices.is_empty());
    let [gated] = specs.repository.as_slice() else {
        panic!("the server is the repository's");
    };
    let mcp::Transport::Http { url } = &gated.spec.transport else {
        panic!("a remote transport");
    };
    assert_eq!(url, "https://example.com/mcp");
    assert_eq!(gated.fields.url, Some(json!("https://example.com/mcp")));
    // A token can ride in the URL, so only its presence prints.
    let printed = format!("{:?}", gated.fields);
    assert!(!printed.contains("example.com"), "{printed}");
}

#[test]
fn a_persons_url_server_the_repository_only_retimes_is_repository_code() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"url": "https://example.com/mcp"}}}}));
    setup.repository(&json!({"mcp": {"servers": {"fx": {"timeout_ms": 1}}}}));
    let specs = setup.specs();
    assert!(specs.specs.is_empty());
    let [gated] = specs.repository.as_slice() else {
        panic!("the server is the repository's");
    };
    let mcp::Transport::Http { url } = &gated.spec.transport else {
        panic!("a remote transport");
    };
    assert_eq!(url, "https://example.com/mcp");
    assert_eq!(gated.fields.url, None);
}

#[test]
fn a_dotted_name_reads_its_own_url() {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"my.server": {"url": "https://example.com/mcp"}}}}));
    let specs = setup.specs();
    assert_eq!(specs.specs.len(), 1);
    let mcp::Transport::Http { url } = &specs.specs[0].transport else {
        panic!("a remote transport");
    };
    assert_eq!(url, "https://example.com/mcp");
}
