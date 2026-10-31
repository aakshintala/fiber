//! Configuration to [`ServerSpec`], without starting a process: every
//! server becomes a spec, a repository's stays out (the repository's offer
//! names it), a `url` server becomes a remote spec, and an entry with
//! both is never started.

use std::time::Duration;

use fakes::Deadline;
use fakes::NoSkills;
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
fn the_store_entry_point_uses_the_specs_injected_private_url_file() {
    use std::os::unix::fs::PermissionsExt;
    let setup = Setup::new();
    setup.global(&json!({"mcp":{"servers":{"docs":{"url":"HTTPS://Docs.Example.com:443/mcp"}}}}));
    let specs = setup.specs();
    let mcp::Transport::Http { url, tokens } = &specs.specs[0].transport else {
        panic!("remote spec");
    };
    let token = mcp::Token {
        access_token: contract::Secret::new("at-1".into()),
        refresh_token: None,
        expires_at: None,
        token_endpoint: "https://auth/token".into(),
        client_id: "fiber".into(),
    };
    mcp::store_token(tokens.as_ref(), url, &token).unwrap();
    let key = "22f34f33b505018d3431bbcf0ddbdf68550d6a4cf6725da53d2999700e4bd493";
    let file = setup.rig.home.join("credentials/mcp").join(key);
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(record["url"], "https://docs.example.com/mcp");
    assert_eq!(record["generation"], 1);
    let held = tokens.try_lock(key).unwrap().unwrap();
    assert!(matches!(
        mcp::store_token(tokens.as_ref(), url, &token),
        Err(mcp::TokenError::Busy)
    ));
    drop(held);
    std::fs::write(&file, b"not json").unwrap();
    mcp::store_token(tokens.as_ref(), url, &token).unwrap();
    let repaired: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(repaired["url"], "https://docs.example.com/mcp");
    assert_eq!(repaired["generation"], 1);
}
#[test]
fn userinfo_is_a_named_config_error_without_exposing_credentials() {
    for url in [
        "https://tokenuser-7f3@h/mcp",
        "https://tokenuser-7f3:pw-7f3@h/mcp",
        "https://@h/mcp",
    ] {
        for required in [false, true] {
            let setup = Setup::new();
            setup.global(&json!({"mcp":{"servers":{"docs":{"url":url, "required":required}}}}));
            let config = setup.rig.config(Vec::<String>::new());
            let message = if required {
                let failure = specs(&config).err().unwrap();
                assert_eq!(failure.code, contract::ErrorCode::ConfigInvalid);
                assert!(failure.message.starts_with("The required MCP server"));
                failure.message
            } else {
                let specs = specs(&config).unwrap();
                assert!(specs.specs.is_empty());
                assert!(specs.repository.is_empty());
                assert_eq!(specs.notices.len(), 1);
                assert_eq!(specs.notices[0].code, contract::ErrorCode::ConfigInvalid);
                specs.notices[0].message.clone()
            };
            for words in ["docs", "user name or password", "`url`"] {
                assert!(message.contains(words));
            }
            for secret in ["tokenuser-7f3", "pw-7f3"] {
                assert!(!message.contains(secret));
            }
        }
    }
    let setup = Setup::new();
    setup.global(&json!({"mcp":{"servers":{"docs":{"url":"https://h/mcp?u@v"}}}}));
    assert_eq!(setup.specs().specs.len(), 1);
}

#[test]
fn no_servers_starts_nothing() {
    let setup = Setup::new();
    let specs = setup.specs();
    assert!(specs.specs.is_empty());
    assert!(specs.notices.is_empty());
}

/// An image child that refuses everything: the tests below start no
/// server, so it never runs.
struct DummyImages;

impl contract::images::Images for DummyImages {
    fn process(
        &self,
        _bytes: &[u8],
        _cancel: &dyn contract::tool::Cancel,
    ) -> Result<contract::provider::ImageRef, contract::images::ImageError> {
        Err(contract::images::ImageError::Unreadable(
            "unused".to_owned(),
        ))
    }
}

/// No servers started: the hub's view of the session's slots.
fn empty_servers(
    rig: &crate::test_support::Rig,
    clock: &std::sync::Arc<dyn contract::clock::Clock>,
) -> std::sync::Arc<mcp::Servers> {
    let started = mcp::start(
        Vec::new(),
        &rig.workspace,
        &rig.root.path().join("artifacts"),
        std::sync::Arc::new(DummyImages),
        &config::mcp_cache_dir(&rig.home),
        clock,
        "test",
    );
    assert!(started.failed.is_empty());
    std::sync::Arc::new(started.servers)
}

fn sources_of(rig: &crate::test_support::Rig) -> config::Sources {
    config::Sources {
        home: rig.home.clone(),
        workspace: rig.workspace.clone(),
        project: config::ProjectKey::new("test").unwrap(),
        overrides: Vec::new(),
    }
}

#[test]
fn reconnecting_a_repository_server_is_invalid_arguments() {
    // The repository arm fires before any start: a mutant matching any
    // other server would let the reconnect through to adoption.
    let setup = Setup::new();
    setup.repository(&json!({"mcp": {"servers": {"db": {"command": "/bin/true"}}}}));
    let config = setup.rig.config(Vec::<String>::new());
    let clock: std::sync::Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let servers = empty_servers(&setup.rig, &clock);
    let error = super::adoption_spec(&config, "db").expect_err("a repository server never adopts");
    assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
    assert!(
        error.message.contains("is a repository server"),
        "{error:?}"
    );
    servers.stop();
}

#[test]
fn a_ready_after_a_failure_clears_the_failure() {
    // The log fold drops a failure its ready supersedes: without the
    // removal the stopped-then-restarted server would read failed.
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"db": {"command": "/bin/true"}}}}));
    let clock: std::sync::Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let sessions = setup.rig.root.path().join("sessions");
    let log = std::sync::Arc::new(
        log::Log::create(
            &sessions,
            contract::SessionId("s_ready".into()),
            clock.clone(),
        )
        .expect("the log opens"),
    );
    let failed = || {
        contract::events::Event::McpServerFailed(contract::events::McpServerFailed {
            server: "db".into(),
            reason: contract::events::ServerFailure::StartFailed,
            will_restart: false,
            error: contract::shapes::Failure {
                code: contract::ErrorCode::McpServerUnavailable,
                message: "The MCP server `db` failed to start: refused.".into(),
                retry_after_ms: None,
                provider: None,
            },
        })
    };
    log.append(&failed(), None, None)
        .expect("the failure appends");
    log.append(
        &contract::events::Event::McpServerReady(contract::events::McpServerReady {
            server: "db".into(),
        }),
        None,
        None,
    )
    .expect("the ready appends");
    let servers = empty_servers(&setup.rig, &clock);
    let waiting = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    let refresh = super::servers_refresh(
        std::collections::HashMap::new(),
        0,
        sources_of(&setup.rig),
        servers.clone(),
        std::sync::Arc::downgrade(&log),
        clock,
        waiting,
    );
    let rows = refresh(Vec::new()).expect("the refresh reads");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "db");
    assert_eq!(
        rows[0].state,
        contract::events::McpServerState::Connecting,
        "{rows:?}"
    );
    servers.stop();
}

#[test]
fn a_settled_failure_reads_needs_login_before_its_line_is_written() {
    // The pre-commit gap: the startup settles every server before the
    // door's commit writes their failure lines, and the door serves
    // queries from before that commit, so a query in the gap reads the
    // settled row, never the `connecting` of a start still in flight.
    // The seed carries the settled failure the commit is about to
    // write (`docs/mcp.md`, "Servers, their state and the per-server
    // commands").
    // No failure line yet: the startup commit has not run.
    let rows = seeded_rows(
        seed_of(contract::events::ServerFailure::NotLoggedIn),
        &[],
        &[],
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "remote");
    assert_eq!(
        rows[0].state,
        contract::events::McpServerState::NeedsLogin,
        "{rows:?}"
    );
}

/// The startup's settled failures carrying one failure for the remote
/// server with `reason`: what the commit writes and the reader's seed
/// carries together.
fn seed_of(
    reason: contract::events::ServerFailure,
) -> std::collections::HashMap<String, contract::events::McpServerFailed> {
    std::collections::HashMap::from([("remote".to_owned(), settled_failure(reason))])
}

/// The startup's settled failure for the remote server with `reason`:
/// what the commit writes and the reader's seed carries together.
fn settled_failure(reason: contract::events::ServerFailure) -> contract::events::McpServerFailed {
    contract::events::McpServerFailed {
        server: "remote".into(),
        reason,
        will_restart: false,
        error: contract::shapes::Failure {
            code: contract::ErrorCode::McpServerUnavailable,
            message: "The MCP server `remote` failed to start: it needs a login.".into(),
            retry_after_ms: None,
            provider: None,
        },
    }
}

/// The `mcp_servers` rows for the remote server with `seed` settled,
/// `history` already in the log when the reader is built and `later`
/// written after: history stands for the resume's old lines, later for
/// this startup's commit and restarts. The pre-commit gap holds no later
/// lines, and the commit's lines fold over the seed.
fn seeded_rows(
    seed: std::collections::HashMap<String, contract::events::McpServerFailed>,
    history: &[contract::events::Event],
    later: &[contract::events::Event],
) -> Vec<contract::events::McpServerRow> {
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"remote": {"url": "https://example.com/mcp"}}}}));
    let clock: std::sync::Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let sessions = setup.rig.root.path().join("sessions");
    let log = std::sync::Arc::new(
        log::Log::create(
            &sessions,
            contract::SessionId("s_seed".into()),
            clock.clone(),
        )
        .expect("the log opens"),
    );
    for line in history {
        log.append(line, None, None).expect("the line appends");
    }
    // The reader is built here: history is the resume's old lines, and
    // everything appended after is this startup's own.
    let boundary = log.count();
    let servers = empty_servers(&setup.rig, &clock);
    let waiting = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    let refresh = super::servers_refresh(
        seed,
        boundary,
        sources_of(&setup.rig),
        servers.clone(),
        std::sync::Arc::downgrade(&log),
        clock,
        waiting,
    );
    for line in later {
        log.append(line, None, None).expect("the line appends");
    }
    let rows = refresh(Vec::new()).expect("the refresh reads");
    servers.stop();
    rows
}

#[test]
fn a_seeded_start_failure_reads_failed() {
    // The seed keeps the failure's own reason: a start that failed
    // reads `failed`, never the `needs-login` of a failure that wants
    // a login. A mutant mapping every seed to one state fails here.
    let seed = seed_of(contract::events::ServerFailure::StartFailed);
    let rows = seeded_rows(seed, &[], &[]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "remote");
    assert_eq!(
        rows[0].state,
        contract::events::McpServerState::Failed,
        "{rows:?}"
    );
}

#[test]
fn a_ready_line_clears_the_seed() {
    // A restart's ready arrives after the reader is built and folds
    // after the seed, so it supersedes the startup's failure: without
    // the removal the restarted server would read failed. A mutant that
    // keeps seeded entries past a ready fails here.
    let seed = seed_of(contract::events::ServerFailure::NotLoggedIn);
    let ready = contract::events::Event::McpServerReady(contract::events::McpServerReady {
        server: "remote".into(),
    });
    let rows = seeded_rows(seed, &[], &[ready]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "remote");
    assert_eq!(
        rows[0].state,
        contract::events::McpServerState::Connecting,
        "{rows:?}"
    );
}

#[test]
fn a_written_failure_line_agrees_with_the_seed() {
    // The commit writes the same failures the seed carries: folding
    // the lines over the seed leaves the settled row, so readers
    // before and after the commit agree. A mutant that lets the lines
    // drop a seeded entry fails here.
    let seed = seed_of(contract::events::ServerFailure::NotLoggedIn);
    let failed = contract::events::Event::McpServerFailed(settled_failure(
        contract::events::ServerFailure::NotLoggedIn,
    ));
    let rows = seeded_rows(seed, &[], &[failed]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "remote");
    assert_eq!(
        rows[0].state,
        contract::events::McpServerState::NeedsLogin,
        "{rows:?}"
    );
}

#[test]
fn an_old_ready_does_not_clear_the_seed() {
    // A resume inherits the old session's ready, then this startup
    // settles the same server as not-logged-in: the seed supersedes
    // the history, so the row reads `needs-login`, never the
    // `connecting` an old ready folded over the seed would leave. A
    // mutant that folds the whole log over the seed fails here.
    let seed = seed_of(contract::events::ServerFailure::NotLoggedIn);
    let ready = contract::events::Event::McpServerReady(contract::events::McpServerReady {
        server: "remote".into(),
    });
    let rows = seeded_rows(seed, &[ready], &[]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "remote");
    assert_eq!(
        rows[0].state,
        contract::events::McpServerState::NeedsLogin,
        "{rows:?}"
    );
}

#[test]
fn a_logged_failure_without_a_seed_reads_failed() {
    // A mid-session death lands in the log with no seed entry: the fold
    // still records its row, so the server reads `failed`, never the
    // `connecting` of a start with nothing settled. A mutant that drops
    // the failure arm of the fold fails here.
    let failed = contract::events::Event::McpServerFailed(settled_failure(
        contract::events::ServerFailure::StartFailed,
    ));
    let rows = seeded_rows(std::collections::HashMap::new(), &[], &[failed]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "remote");
    assert_eq!(
        rows[0].state,
        contract::events::McpServerState::Failed,
        "{rows:?}"
    );
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
fn a_persons_command_with_repository_args_stays_gated_after_declaration_removed() {
    // With the declaration gone, args are the only effective program
    // field supplied by the repository.
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"command": "/bin/bash"}}}}));
    setup.repository(&json!({"mcp": {"servers": {"fx": {"args": ["run.sh"]}}}}));
    let config = setup.rig.config(Vec::<String>::new());
    setup.repository(&json!({}));
    let specs = specs(&config).expect("the specs read");
    assert!(specs.specs.is_empty());
    let [gated] = specs.repository.as_slice() else {
        panic!("the server is the repository's through args provenance alone");
    };
    assert_eq!(gated.spec.name, "fx");
    assert_eq!(stdio(&gated.spec).command, "/bin/bash");
    assert_eq!(stdio(&gated.spec).args, vec!["run.sh".to_owned()]);
    assert_eq!(gated.fields.command, None);
    assert_eq!(gated.fields.args, Some(json!(["run.sh"])));
    assert!(gated.fields.env.is_empty());
    assert_eq!(gated.fields.url, None);
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
fn a_persons_command_overridden_by_repository_stays_gated_after_declaration_removed() {
    // The declaration is removed between `Config::load` and `specs`:
    // only the effective command's repository provenance keeps the
    // person's server behind approval.
    let setup = Setup::new();
    setup.global(&json!({"mcp": {"servers": {"fx": {"command": "/bin/sh"}}}}));
    setup.repository(&json!({"mcp": {"servers": {"fx": {"command": "/bin/bash"}}}}));
    let config = setup.rig.config(Vec::<String>::new());
    setup.repository(&json!({}));
    let specs = specs(&config).expect("the specs read");
    assert!(specs.specs.is_empty());
    let [gated] = specs.repository.as_slice() else {
        panic!("the server is the repository's through command provenance alone");
    };
    assert_eq!(gated.spec.name, "fx");
    assert_eq!(stdio(&gated.spec).command, "/bin/bash");
    assert_eq!(gated.fields.command, Some(json!("/bin/bash")));
    assert_eq!(gated.fields.args, None);
    assert!(gated.fields.env.is_empty());
    assert_eq!(gated.fields.url, None);
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
    let mcp::Transport::Http { url, .. } = &spec.transport else {
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
    let mcp::Transport::Http { url, .. } = &gated.spec.transport else {
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
    let mcp::Transport::Http { url, .. } = &gated.spec.transport else {
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
    let mcp::Transport::Http { url, .. } = &specs.specs[0].transport else {
        panic!("a remote transport");
    };
    assert_eq!(url, "https://example.com/mcp");
}

#[test]
fn declares_resources_finds_the_tool_by_name() {
    assert!(!super::declares_resources(&[pair("builtin", "read")]));
    assert!(super::declares_resources(&[
        pair("builtin", "read"),
        pair("builtin", "mcp_resources"),
    ]));
    assert!(super::declares_resources(&[
        pair("docs", "search"),
        pair("fiber.test/x", "mcp_resources"),
    ]));
}

/// The delegates the `builtin` calls declare tools for: no model
/// resolves, so `delegate_spawn` is declared but never runs one. A local
/// copy of `builtin_tests`' helper, which is `pub(super)` there.
fn delegates() -> crate::delegates::Delegates {
    let root = fakes::TempDir::new("fiber-mcp-resources-delegate");
    let clock: std::sync::Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        std::sync::Arc::clone(&clock),
        std::sync::Arc::new(fakes::Recorder::default()),
    );
    crate::delegates::Delegates::new(
        root.path().join("fiber-stub"),
        root.path().join("home"),
        contract::SessionId("s_test".into()),
        root.path().to_path_buf(),
        root.path().join("sessions"),
        jobs,
        clock,
        jobs::Resolve {
            lookup: std::sync::Arc::new(|_| Err(String::new())),
            catalogue: std::sync::Arc::from(""),
            credential: std::sync::Arc::new(|_| Ok(None)),
        },
    )
}

fn skills() -> std::sync::Arc<dyn contract::skills::Skills> {
    std::sync::Arc::new(NoSkills)
}

/// How long the wiring waits for a session build and a post-offer load,
/// in real time: neither spawns, so a passing run never waits on it.
const WIRING_WITHIN: std::time::Duration = std::time::Duration::from_secs(30);

/// A rig whose repository declares `db` and nothing else: `db` is
/// unapproved, so `session_tools` leaves it for the post-offer load.
fn offer_rig() -> crate::test_support::Rig {
    let rig = crate::test_support::Rig::new("fiber-mcp-resources-wiring");
    std::fs::create_dir_all(rig.workspace.join("srv")).unwrap();
    std::fs::write(rig.workspace.join("srv/run.sh"), "echo\n").unwrap();
    std::fs::create_dir_all(rig.workspace.join(".fiber")).unwrap();
    std::fs::write(
        rig.workspace.join(".fiber/config.json"),
        serde_json::json!({"mcp": {"servers": {"db": {"command": "./srv/run.sh", "args": ["srv/run.sh"]}}}}).to_string(),
    )
    .unwrap();
    rig
}

/// Starts the test's configured specs as a pending MCP owner.
fn pending(
    rig: &crate::test_support::Rig,
    specs: super::Specs,
    clock: &std::sync::Arc<dyn contract::clock::Clock>,
    artifacts: &std::path::Path,
    project: &config::ProjectKey,
) -> super::PendingMcp {
    super::start_session_tools(
        &rig.home,
        &rig.workspace,
        artifacts,
        clock,
        project,
        Ok((specs.specs, specs.repository)),
        Vec::new(),
        std::sync::Arc::new(DummyImages),
        None,
    )
    .expect("the pending MCP start")
}

/// The session's tools and servers, built inside a deadline: `db` waits
/// for the post-offer load, and `extension_tools` ride after the rest.
#[allow(clippy::type_complexity, reason = "one session's tools and servers")]
fn wired(
    rig: &crate::test_support::Rig,
    extension_tools: Vec<(String, std::sync::Arc<dyn contract::tool::Tool>)>,
) -> (
    Vec<(String, std::sync::Arc<dyn contract::tool::Tool>)>,
    super::SessionServers,
) {
    let config = rig.config(Vec::<String>::new());
    let specs = super::specs(&config).expect("the specs read");
    assert!(specs.specs.is_empty(), "no person server");
    assert_eq!(specs.repository.len(), 1);
    let clock: std::sync::Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let artifacts = rig.root.path().join("artifacts");
    let jobs = jobs::Registry::new(
        artifacts.clone(),
        std::sync::Arc::clone(&clock),
        std::sync::Arc::new(fakes::Recorder::default()),
    );
    let locks = std::sync::Arc::new(tools::PathLocks::new());
    let project = config::ProjectKey::new("test").unwrap();
    let fiber = rig.root.path().join("fiber-stub");
    let home = rig.home.clone();
    let workspace = rig.workspace.clone();
    let delegates = delegates();
    let skills = skills();
    let pending = pending(rig, specs, &clock, &artifacts, &project);
    let (tools, _infos, _driver, servers) = fakes::within(
        "the session tools",
        Deadline::after(WIRING_WITHIN),
        move || {
            super::session_tools(
                fiber,
                &home,
                &workspace,
                &artifacts,
                &clock,
                &jobs,
                &locks,
                &project,
                pending,
                None,
                &delegates,
                skills,
                extension_tools,
                None,
                Default::default(),
                std::sync::Arc::new(std::sync::Mutex::new(tools::SandboxSettings {
                    enabled: false,
                    allow_write: Vec::new(),
                    deny_read: Vec::new(),
                    fiber_home: home.clone(),
                })),
                None,
            )
            .expect("the session builds")
        },
    );
    (tools, servers)
}

/// Approves `db`'s current content and writes its cache offering
/// resources under its declaration key, so the load declares lazily and
/// spawns nothing. Returns the offer key the load reports.
fn approve_db(
    rig: &crate::test_support::Rig,
    clock: &std::sync::Arc<dyn contract::clock::Clock>,
) -> r#loop::OfferKey {
    let item = extensions::declared_items(&rig.workspace, clock.as_ref())
        .unwrap()
        .into_iter()
        .find(|item| item.kind == contract::events::OfferedKind::McpServer && item.name == "db")
        .unwrap();
    let project = config::ProjectKey::new("test").unwrap();
    let mut index = extensions::Index::load(&rig.home);
    let hash = extensions::hash(&mut index, &item).unwrap();
    extensions::Store::new(&rig.home, &project, std::sync::Arc::clone(clock))
        .approve(&item, &hash)
        .unwrap();
    // The declaration key of `./srv/run.sh` with `srv/run.sh` and no env
    // (the control key of the post-offer tests, recomputed with:
    // `python3 -c "import hashlib, json; d={'command': './srv/run.sh',
    // 'args': ['srv/run.sh'], 'env': {}, 'secrets': {}}; b=json.dumps(d, separators=
    // (',', ':'), sort_keys=True).encode(); print(hashlib.sha256(b).
    // hexdigest())"`).
    const KEY: &str = "c8006cb33c5dd250d5260106e564fdb95eb4836614213fee7585f71cb434553c";
    let dir = rig.home.join("cache").join("mcp");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("db.json"),
        serde_json::json!({"version": 1, "key": KEY, "tools": [{"name": "echo"}], "prompts": [], "resources": true}).to_string(),
    )
    .unwrap();
    r#loop::OfferKey {
        kind: contract::events::OfferedKind::McpServer,
        name: "db".to_owned(),
        hash,
    }
}

/// Runs the post-offer load inside a deadline, stops the servers, and
/// returns the approved tool names in `(registered_by, name)` order.
fn loaded(session: super::SessionServers, key: r#loop::OfferKey) -> Vec<(String, String)> {
    let super::SessionServers { later, servers, .. } = session;
    let declare: doors::Declare = std::sync::Arc::new(move |_name: &str, _info| {});
    let load = later.into_load(declare, std::collections::BTreeMap::new());
    let approved = fakes::within(
        "the post-offer load",
        Deadline::after(WIRING_WITHIN),
        move || {
            load(&r#loop::OfferOutcome {
                skipped: vec![],
                reported: vec![key],
                held: vec![],
            })
            .expect("the load runs")
        },
    );
    let names: Vec<(String, String)> = approved
        .tools
        .iter()
        .map(|(registered_by, tool)| (registered_by.clone(), tool.definition().name))
        .collect();
    servers.stop();
    names
}

#[test]
fn reload_server_names_keep_successes_and_omit_failed_starts() {
    let rig = offer_rig();
    let clock: std::sync::Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    approve_db(&rig, &clock);
    rig.write_global(&json!({"mcp":{"servers":{"broken":{
        "command":rig.root.path().join("missing-server"), "required":false
    }}}}));
    let config = rig.config(Vec::<String>::new());
    let specs = super::specs(&config).unwrap();
    let artifacts = rig.root.path().join("artifacts");
    let jobs = jobs::Registry::new(
        artifacts.clone(),
        clock.clone(),
        std::sync::Arc::new(fakes::Recorder::default()),
    );
    let locks = std::sync::Arc::new(tools::PathLocks::new());
    let project = config::ProjectKey::new("test").unwrap();
    let home = rig.home.clone();
    let workspace = rig.workspace.clone();
    let fiber = rig.root.path().join("fiber-stub");
    let delegates = delegates();
    let pending = pending(&rig, specs, &clock, &artifacts, &project);
    let (tools, _, _, session) = fakes::within(
        "session with a failed optional server",
        Deadline::after(WIRING_WITHIN),
        move || {
            super::session_tools(
                fiber,
                &home,
                &workspace,
                &artifacts,
                &clock,
                &jobs,
                &locks,
                &project,
                pending,
                None,
                &delegates,
                skills(),
                Vec::new(),
                None,
                Default::default(),
                std::sync::Arc::new(std::sync::Mutex::new(tools::SandboxSettings {
                    enabled: false,
                    allow_write: Vec::new(),
                    deny_read: Vec::new(),
                    fiber_home: home.clone(),
                })),
                None,
            )
            .unwrap()
        },
    );
    assert!(
        tools.iter().any(|(registered_by, tool)| {
            registered_by == "db" && tool.definition().name == "mcp__db__echo"
        }),
        "the session keeps the successful server's tools",
    );
    assert!(
        tools
            .iter()
            .all(|(registered_by, _)| registered_by != "broken"),
        "the session omits the failed server's tools",
    );
    assert_eq!(
        session
            .failed
            .iter()
            .map(|failure| failure.server.as_str())
            .collect::<Vec<_>>(),
        ["broken"]
    );
    session.servers.stop();
}

#[test]
fn an_extension_replacement_keeps_the_original_mcp_info_for_reload() {
    let rig = offer_rig();
    let clock: std::sync::Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    approve_db(&rig, &clock);
    let (tools, session) = wired(&rig, vec![pair("fiber.test/replacement", "mcp__db__echo")]);
    let row = session
        .kept_infos
        .iter()
        .find(|info| info.name == "mcp__db__echo")
        .unwrap();
    assert!(
        matches!(&row.source, contract::events::ToolSource::Mcp { server, tool } if server == "db" && tool == "echo")
    );
    assert!(session.kept_infos.iter().any(|info| info.name == "read"));
    assert_eq!(
        session
            .base_tools
            .iter()
            .filter(|(_, tool)| tool.definition().name == "mcp__db__echo")
            .map(|(owner, _)| owner.as_str())
            .collect::<Vec<_>>(),
        ["db"]
    );
    assert!(
        session
            .base_tools
            .iter()
            .any(|(_, tool)| tool.definition().name == "read")
    );
    assert_eq!(
        tools
            .iter()
            .filter(|(_, tool)| tool.definition().name == "mcp__db__echo")
            .map(|(owner, _)| owner.as_str())
            .collect::<Vec<_>>(),
        ["db", "fiber.test/replacement"]
    );
    session.servers.stop();
}

#[test]
fn an_extensions_mcp_resources_survives_the_post_offer_load() {
    let rig = offer_rig();
    let (tools, session) = wired(&rig, vec![pair("fiber.test/res", "mcp_resources")]);
    assert!(
        tools.iter().any(|(registered_by, tool)| {
            registered_by == "fiber.test/res" && tool.definition().name == "mcp_resources"
        }),
        "the session holds the extension's registration",
    );
    let clock: std::sync::Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let key = approve_db(&rig, &clock);
    let names = loaded(session, key);
    assert_eq!(
        names,
        [("db".to_owned(), "mcp__db__echo".to_owned())],
        "nothing replaces the extension's registration",
    );
}

#[test]
fn the_post_offer_load_adds_mcp_resources_when_the_session_has_none() {
    let rig = offer_rig();
    let (tools, session) = wired(&rig, Vec::new());
    assert!(
        tools
            .iter()
            .all(|(_, tool)| tool.definition().name != "mcp_resources"),
        "the session declares no mcp_resources",
    );
    let clock: std::sync::Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let key = approve_db(&rig, &clock);
    let names = loaded(session, key);
    assert_eq!(
        names,
        [
            ("db".to_owned(), "mcp__db__echo".to_owned()),
            ("builtin".to_owned(), "mcp_resources".to_owned()),
        ],
    );
}

#[test]
fn a_secret_env_value_is_a_reference_never_a_literal() {
    let setup = Setup::new();
    setup.global(
        &json!({"mcp": {"servers": {"docs": {"command": "serve", "env": {"TOKEN": {"secret": "mcp.docs.TOKEN"}, "PLAIN": "v"}}}}}),
    );
    let specs = setup.specs();
    assert_eq!(specs.specs.len(), 1);
    let stdio = stdio(&specs.specs[0]);
    assert_eq!(stdio.env.get("PLAIN"), Some(&"v".to_owned()));
    assert!(
        !stdio.env.contains_key("TOKEN"),
        "a reference is not a literal"
    );
    assert_eq!(
        stdio.secrets.get("TOKEN"),
        Some(&"mcp.docs.TOKEN".to_owned())
    );
}

#[test]
fn a_secret_name_is_exactly_the_servers_own() {
    // Each row: the server, the secret name, and whether it loads. A
    // dotted server name stays allowed; anything else is refused.
    for (server, secret, loads) in [
        ("a", "mcp.a.K", true),
        ("a", "mcp.a.b.K", false),
        ("a.b", "mcp.a.b.K", true),
        ("a", "other", false),
        ("a", "mcp.b.K", false),
        ("a", "mcp.a.1K", false),
        ("a", "mcp.a.K.X", false),
        ("a", "mcp.a.", false),
        ("a", "mcp.", false),
        ("a", "mcp.a.K-1", false),
        ("a", "xmc.a.K", false),
    ] {
        let setup = Setup::new();
        let mut servers = serde_json::Map::new();
        servers.insert(
            server.to_owned(),
            json!({"command": "serve", "env": {"K": {"secret": secret}}}),
        );
        setup.global(&json!({"mcp": {"servers": servers}}));
        let specs = setup.specs();
        if loads {
            assert!(specs.specs.len() == 1, "{server} {secret} loads");
            let stdio = stdio(&specs.specs[0]);
            assert_eq!(stdio.secrets.get("K"), Some(&secret.to_owned()));
            assert!(
                specs
                    .notices
                    .iter()
                    .all(|notice| notice.code != contract::ErrorCode::ConfigInvalid),
                "{server} {secret}: {:?}",
                specs.notices
            );
        } else {
            assert!(specs.specs.is_empty(), "{server} {secret} is refused");
            assert!(specs.repository.is_empty());
            assert_eq!(specs.notices.len(), 1, "{server} {secret}");
            assert_eq!(
                specs.notices[0].code,
                contract::ErrorCode::ConfigInvalid,
                "{server} {secret}"
            );
            for words in [server, secret] {
                assert!(
                    specs.notices[0].message.contains(words),
                    "{server} {secret}"
                );
            }
        }
    }
}

#[test]
fn a_bad_secret_name_on_a_required_server_fails_the_read() {
    let setup = Setup::new();
    setup.global(
        &json!({"mcp": {"servers": {"docs": {"command": "serve", "required": true, "env": {"K": {"secret": "mcp.other.K"}}}}}}),
    );
    let config = setup.rig.config(Vec::<String>::new());
    let failure = specs(&config).err().expect("a required bad secret fails");
    assert_eq!(failure.code, contract::ErrorCode::ConfigInvalid);
    for words in ["docs", "mcp.other.K", "mcp.docs.<KEY>"] {
        assert!(failure.message.contains(words), "{}", failure.message);
    }
}

#[test]
fn a_missing_secret_file_is_not_a_construction_error() {
    // The reference is recorded; only the start reads the file.
    let setup = Setup::new();
    setup.global(
        &json!({"mcp": {"servers": {"docs": {"command": "serve", "env": {"TOKEN": {"secret": "mcp.docs.TOKEN"}}}}}}),
    );
    assert!(!setup.rig.home.join("credentials").exists());
    let specs = setup.specs();
    assert_eq!(specs.specs.len(), 1);
    assert_eq!(
        stdio(&specs.specs[0]).secrets.get("TOKEN"),
        Some(&"mcp.docs.TOKEN".to_owned())
    );
}

#[test]
fn the_reader_reads_a_stored_secret_and_reports_a_missing_one() {
    let setup = Setup::new();
    setup.global(
        &json!({"mcp": {"servers": {"docs": {"command": "serve", "env": {"TOKEN": {"secret": "mcp.docs.TOKEN"}}}}}}),
    );
    config::store_secret(
        &setup.rig.home,
        "mcp.docs.TOKEN",
        &contract::Secret::new("tok-1".into()),
    )
    .unwrap();
    let specs = setup.specs();
    let reader = stdio(&specs.specs[0]).reader.clone();
    assert_eq!(reader("mcp.docs.TOKEN").unwrap(), "tok-1");
    assert_eq!(
        reader("mcp.docs.OTHER").unwrap_err(),
        mcp::SecretError::Missing
    );
    assert!(matches!(
        reader("mcp/docs.TOKEN").unwrap_err(),
        mcp::SecretError::Unreadable(_)
    ));
}
