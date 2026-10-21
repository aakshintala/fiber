use super::*;
use crate::test_support::Rig;
use fakes::Deadline;
use serde_json::json;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

struct NoDrive;
impl contract::extension::Drive for NoDrive {
    fn drive(
        &self,
        _: &str,
        _: &str,
        _: serde_json::Map<String, serde_json::Value>,
        _: contract::inbox::Ack,
    ) {
        panic!("unexpected driver command")
    }
}

impl contract::model::HostModel for NoDrive {
    fn call(
        &self,
        _: &str,
        _: contract::model::HostModelRequest,
        _: contract::model::Bound,
    ) -> Result<contract::model::HostModelReply, Failure> {
        panic!("unexpected model call")
    }
}

struct Named(&'static str);
impl contract::tool::Tool for Named {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: self.0.into(),
            description: "reload test tool".into(),
            input_schema: json!({"type":"object"}),
            deferred: false,
            hosted: None,
        }
    }
    fn effects(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<contract::tool::Effects, contract::tool::EffectsError> {
        panic!("unexpected effects lookup")
    }
    fn run(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
        _: &dyn contract::tool::Cancel,
        _: &dyn contract::emit::Emit,
    ) -> contract::tool::Output {
        panic!("unexpected tool call")
    }
}

/// Records the loaded sets `installed` hands it: `retain` is the only
/// call a reload reaches on the delegate service.
struct RecordingDelegate {
    retains: Mutex<Vec<Vec<String>>>,
}

impl contract::extension::HostDelegate for RecordingDelegate {
    fn start(
        &self,
        _: &str,
        _: &serde_json::Map<String, serde_json::Value>,
        _: &mut dyn FnMut(&contract::JobId),
    ) -> Result<contract::JobId, contract::shapes::Failure> {
        panic!("unexpected delegate start")
    }

    fn retain(&self, loaded: &[String]) -> usize {
        self.retains.lock().unwrap().push(loaded.to_vec());
        0
    }

    fn cancel(&self, _: &[contract::JobId]) -> usize {
        panic!("unexpected delegate cancel")
    }
}

/// An image child that never runs: the rig starts no servers, so no
/// image is ever processed.
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

/// Starts zero servers: the reload tests exercise the preparer, which
/// needs the handle but never restarts through it.
fn empty_servers(rig: &Rig, clock: &Arc<dyn contract::clock::Clock>) -> mcp::Started {
    mcp::start(
        Vec::new(),
        &rig.workspace,
        &rig.workspace.join("artifacts"),
        Arc::new(DummyImages),
        &config::mcp_cache_dir(&rig.home),
        clock,
        "test",
    )
}

struct Fixture {
    rig: Rig,
    state: Arc<State>,
    replacements: Arc<Mutex<Vec<doors::Replacement>>>,
    declared: Arc<Mutex<std::collections::BTreeMap<String, contract::events::ToolInfo>>>,
    delegate: Arc<RecordingDelegate>,
}

impl Fixture {
    fn new() -> Self {
        let rig = Rig::new("fiber-reload-main");
        rig.write_global(&json!({"model":"scripted/s.json"}));
        std::fs::write(rig.workspace.join("s.json"), r#"{"steps":[{"text":"ok"}]}"#).unwrap();
        crate::test_support::install_extension(
            &rig.home,
            "extensions/demo",
            json!({"name":"demo","version":"v1.0.0","fiber":"0.1.0","api":1}),
            &[],
        );
        std::fs::write(rig.home.join("extensions/demo/init.lua"), "fiber.command('old',{timeout=5000,run=function() return 'old' end}); fiber.hook('after_tool',{timeout=5000,on_failure='blocking',run=function() state.set('alive',true) end})").unwrap();
        let marker = rig.root.path().join("reviewer-reads");
        crate::test_support::install_extension(
            &rig.home,
            "extensions/judge",
            json!({"name":"judge", "version":"v1.0.0", "fiber":"0.1.0", "api":1}),
            &[(
                "judge",
                json!({"name":"judge", "credential":{"command":["/bin/sh", "-c", format!("printf x >> '{}'; printf key", marker.display())]}, "models":[{"id":"m","protocol":"openai-responses","base_url":"http://127.0.0.1:9/v1","context_window":10000},{"id":"n","protocol":"openai-responses","base_url":"http://127.0.0.1:9/v1","context_window":10000}]}),
            )],
        );
        let home = rig.home.clone();
        let workspace = rig.workspace.clone();
        let parts = fakes::within(
            "startup parts",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                crate::parts_in(
                    home,
                    workspace,
                    Vec::new(),
                    None,
                    None.into(),
                    None,
                    fakes::clock::FakeClock::new(),
                    None,
                    None,
                    Arc::default(),
                    log::diag::Process::Session,
                    extensions::StateSeed::new(),
                    crate::session_extensions::RepositoryScope::default(),
                )
                .unwrap()
            },
        );
        assert!(
            parts.extensions.notices().is_empty(),
            "{:?}",
            parts.extensions.notices()
        );
        let log = Arc::new(
            log::Log::create(
                &parts.sessions,
                contract::SessionId("reload-test".into()),
                parts.prompt.clock.clone(),
            )
            .unwrap(),
        );
        let delegate = Arc::new(RecordingDelegate {
            retains: Mutex::new(Vec::new()),
        });
        let replacements = Arc::new(Mutex::new(Vec::new()));
        let seen = replacements.clone();
        let declared = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
        let declared_now = declared.clone();
        let empty = empty_servers(&rig, &parts.prompt.clock);
        let state = Arc::new(State {
            sources: parts.sources,
            template: parts.prompt,
            current: Arc::new(crate::session_extensions::Current::new(parts.extensions)),
            switching: Arc::new(parts.switching),
            tools: Vec::new(),
            kept_infos: Vec::new(),
            server_names: vec!["db".into()],
            prompt_rows: Vec::new(),
            late: Arc::default(),
            repository: Arc::new(Mutex::new(
                crate::session_extensions::RepositoryScope::default(),
            )),
            declare: Arc::new(move |name, info| {
                let mut map = declared_now.lock().unwrap();
                match info {
                    Some(info) => {
                        map.insert(name.into(), info);
                    }
                    None => {
                        map.remove(name);
                    }
                }
            }),
            replace: Arc::new(move |replacement| seen.lock().unwrap().push(replacement)),
            drive: Arc::new(NoDrive),
            model: Arc::new(NoDrive),
            delegate: Arc::clone(&delegate) as Arc<dyn contract::extension::HostDelegate>,
            attended: Arc::new(|| true),
            answerable: true,
            log,
            diag: parts.diag,
            names: Mutex::new(Vec::new()),
            shell_read_only: parts.shell_read_only.clone(),
            servers: Arc::new(empty.servers),
            artifacts: rig.workspace.join("artifacts"),
            images: Arc::new(DummyImages),
        });
        Self {
            rig,
            state,
            replacements,
            declared,
            delegate,
        }
    }

    fn prepare(&self) -> r#loop::ReloadPrepared {
        let state = self.state.clone();
        fakes::within(
            "reload prepared",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                state
                    .prepare(&r#loop::ReloadRoute {
                        model: "scripted/s.json".into(),
                        credential: None,
                    })
                    .unwrap()
            },
        )
    }
}

#[test]
fn invalid_config_keeps_the_set_and_a_repaired_config_can_install() {
    let fixture = Fixture::new();
    let old = fixture.state.current.get();
    std::fs::write(fixture.rig.home.join("config.json"), "{").unwrap();
    let state = fixture.state.clone();
    let rejected = fakes::within(
        "invalid reload",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            state
                .prepare(&r#loop::ReloadRoute {
                    model: "scripted/s.json".into(),
                    credential: None,
                })
                .err()
                .unwrap()
        },
    );
    assert_eq!(rejected.code, contract::ErrorCode::ConfigInvalid);
    assert!(rejected.message.contains("config.json"));
    assert!(Arc::ptr_eq(&old, &fixture.state.current.get()));
    assert!(fixture.replacements.lock().unwrap().is_empty());
    fixture.rig.write_global(&json!({"model":"ignored/model", "reviewer":{"context":"fresh notes"}, "skills":{"disabled":["hidden"]}}));
    let prepared = fixture.prepare();
    assert_eq!(
        prepared.settings.reviewer_notes,
        "## Notes that hold everywhere\n\nfresh notes"
    );
    assert_eq!(prepared.prompt.skills_disabled, ["hidden"]);
    assert_eq!(prepared.servers, ["db"]);
    assert!(Arc::ptr_eq(&old, &fixture.state.current.get()));
    (prepared.installed)();
    assert!(!Arc::ptr_eq(&old, &fixture.state.current.get()));
    assert!(Arc::ptr_eq(
        &fixture.state.current.get(),
        &fixture.state.switching.reloadable().loader.extensions
    ));
    assert_eq!(fixture.replacements.lock().unwrap().len(), 1);
    assert_eq!(
        Arc::strong_count(&old),
        1,
        "only the test keeps the retired set alive"
    );
}

#[test]
fn reload_replaces_command_list_and_dropped_preparation_does_not_install() {
    let fixture = Fixture::new();
    let old = fixture.state.current.get();
    let path = fixture.rig.home.join("extensions/demo/init.lua");
    std::fs::write(
        path,
        "fiber.command('fresh',{timeout=5000,run=function() return 'fresh' end})",
    )
    .unwrap();
    let prepared = fixture.prepare();
    drop(prepared);
    assert!(Arc::ptr_eq(&old, &fixture.state.current.get()));
    assert!(fixture.replacements.lock().unwrap().is_empty());
    let prepared = fixture.prepare();
    assert!(prepared.notices.is_empty(), "{:?}", prepared.notices);
    (prepared.installed)();
    let replacements = fixture.replacements.lock().unwrap();
    assert_eq!(
        replacements[0]
            .commands
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        ["fresh"]
    );
    assert!(replacements[0].hooks.is_some());
    assert!(fixture.declared.lock().unwrap().is_empty());
}

#[test]
fn reload_preserves_tool_precedence_late_tools_and_only_current_declarations() {
    use contract::events::ToolSource;
    let mut fixture = Fixture::new();
    let read = Arc::new(Named("read"));
    let echo = Arc::new(Named("mcp__db__echo"));
    let late = Arc::new(Named("late"));
    let read_info = crate::builtin::info(&*read, ToolSource::Builtin).unwrap();
    let echo_info = crate::builtin::info(
        &*echo,
        ToolSource::Mcp {
            server: "db".into(),
            tool: "echo".into(),
        },
    )
    .unwrap();
    let late_info = crate::builtin::info(&*late, ToolSource::Builtin).unwrap();
    let web_info = crate::builtin::info(&Named("web_search"), ToolSource::Builtin).unwrap();
    let removed = crate::builtin::info(&Named("removed"), ToolSource::Builtin).unwrap();
    {
        let state = Arc::get_mut(&mut fixture.state).unwrap();
        // Input order forces the built-in/MCP precedence to be established by prepare.
        state.tools = vec![("db".into(), echo), ("builtin".into(), read.clone())];
        state.kept_infos = vec![read_info.clone(), echo_info, web_info];
        *state.names.lock().unwrap() = vec!["removed".into(), "read".into()];
        let mut held = state.late.lock().unwrap();
        held.tools = vec![("repository".into(), read), ("repository".into(), late)];
        held.infos.insert("late".into(), late_info);
    }
    fixture
        .declared
        .lock()
        .unwrap()
        .extend([("removed".into(), removed), ("read".into(), read_info)]);
    let prepared = fixture.prepare();
    assert_eq!(
        prepared
            .tools
            .iter()
            .map(|(owner, tool)| (owner.as_str(), tool.definition().name))
            .collect::<Vec<_>>(),
        [
            ("builtin", "read".into()),
            ("db", "mcp__db__echo".into()),
            ("repository", "late".into())
        ]
    );
    (prepared.installed)();
    let declared = fixture.declared.lock().unwrap();
    assert_eq!(
        declared.keys().map(String::as_str).collect::<Vec<_>>(),
        ["late", "mcp__db__echo", "read"]
    );
    assert!(matches!(declared["read"].source, ToolSource::Builtin));
}

#[test]
fn reviewer_credentials_publish_only_when_installed_and_are_reused_afterwards() {
    let fixture = Fixture::new();
    let marker = fixture.rig.root.path().join("reviewer-reads");
    assert!(!marker.exists());
    fixture.rig.write_global(&json!({"model":"scripted/s.json", "reviewer":{"model":"judge/m"}, "cache_lifetime":"long"}));
    let prepared = fixture.prepare();
    assert_eq!(
        prepared.reviewer.as_ref().unwrap().model.reference,
        "judge/m"
    );
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "x");
    drop(prepared);
    let prepared = fixture.prepare();
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "x");
    (prepared.installed)();
    let prepared = fixture.prepare();
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "x");
    (prepared.installed)();
    let switching = fixture.state.switching.clone();
    let switched = fakes::within(
        "model switch after reload",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            crate::switch::prepare(
                &switching,
                &crate::switch::Door {
                    declare: Arc::new(|_, _| {}),
                    backend: None,
                    hosted_stands: false,
                },
                &contract::commands::ModelArgs {
                    model: "judge/n".into(),
                    thinking: None,
                },
                None,
                None,
            )
            .unwrap()
        },
    );
    assert_eq!(switched.model.reference, "judge/n");
    assert_eq!(
        switched.reviewer.as_ref().unwrap().model.reference,
        "judge/m"
    );
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "x");
}

#[test]
fn spawn_failure_runs_no_preparation_and_names_the_reload_thread() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let build = move |_: &r#loop::ReloadRoute| {
        count.fetch_add(1, Ordering::SeqCst);
        Err(contract::inbox::Rejection {
            code: contract::ErrorCode::ConfigInvalid,
            message: "must not run".into(),
        })
    };
    let prepare = preparer(build, |purpose, _| {
        assert_eq!(purpose, "reload");
        Err(doors::failure(
            contract::ErrorCode::IoFailed,
            "a thread for the reload: injected spawn failure",
        ))
    });
    let error = match prepare(&r#loop::ReloadRoute {
        model: "p/m".into(),
        credential: None,
    }) {
        Err(error) => error,
        Ok(_) => panic!("spawn failure accepted"),
    };
    assert_eq!(error.code, contract::ErrorCode::IoFailed);
    assert_eq!(
        error.message,
        "a thread for the reload: injected spawn failure"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn config_rejection_passes_through_without_installing() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let build = move |route: &r#loop::ReloadRoute| {
        assert_eq!(route.model, "p/current");
        assert_eq!(route.credential.as_deref(), Some("work"));
        count.fetch_add(1, Ordering::SeqCst);
        Err(contract::inbox::Rejection {
            code: contract::ErrorCode::ConfigInvalid,
            message: "config.json: invalid JSON".into(),
        })
    };
    let prepare = preparer(build, |_, work| Ok(work()));
    let error = match prepare(&r#loop::ReloadRoute {
        model: "p/current".into(),
        credential: Some("work".into()),
    }) {
        Err(error) => error,
        Ok(_) => panic!("invalid configuration accepted"),
    };
    assert_eq!(error.code, contract::ErrorCode::ConfigInvalid);
    assert_eq!(error.message, "config.json: invalid JSON");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn install_replaces_the_shared_shell_read_only_list() {
    let fixture = Fixture::new();
    fixture.rig.write_global(
        &json!({"model":"scripted/s.json", "shell":{"read_only":{"jq":{"flags":[]}}}}),
    );
    let prepared = fixture.prepare();
    assert!(
        fixture.state.shell_read_only.lock().unwrap().is_empty(),
        "a prepared reload changes nothing before it installs"
    );
    (prepared.installed)();
    assert_eq!(
        *fixture.state.shell_read_only.lock().unwrap(),
        vec![("jq".to_owned(), Vec::new())]
    );
}

#[test]
fn a_rejected_reload_keeps_the_list_and_a_repaired_config_installs() {
    let fixture = Fixture::new();
    fixture.rig.write_global(
        &json!({"model":"scripted/s.json", "shell":{"read_only":{"yq":{"flags":[]}}}}),
    );
    let prepared = fixture.prepare();
    (prepared.installed)();
    assert_eq!(
        *fixture.state.shell_read_only.lock().unwrap(),
        vec![("yq".to_owned(), Vec::new())]
    );
    std::fs::write(fixture.rig.home.join("config.json"), "{").unwrap();
    let state = fixture.state.clone();
    let rejected = fakes::within(
        "invalid reload",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            state
                .prepare(&r#loop::ReloadRoute {
                    model: "scripted/s.json".into(),
                    credential: None,
                })
                .err()
                .unwrap()
        },
    );
    assert_eq!(rejected.code, contract::ErrorCode::ConfigInvalid);
    assert_eq!(
        *fixture.state.shell_read_only.lock().unwrap(),
        vec![("yq".to_owned(), Vec::new())]
    );
    fixture.rig.write_global(
        &json!({"model":"scripted/s.json", "shell":{"read_only":{"jq":{"flags":[]}}}}),
    );
    let prepared = fixture.prepare();
    (prepared.installed)();
    assert_eq!(
        *fixture.state.shell_read_only.lock().unwrap(),
        vec![("jq".to_owned(), Vec::new())]
    );
}

#[test]
fn a_dropped_shell_read_only_preparation_changes_nothing() {
    let fixture = Fixture::new();
    fixture.rig.write_global(
        &json!({"model":"scripted/s.json", "shell":{"read_only":{"jq":{"flags":[]}}}}),
    );
    let prepared = fixture.prepare();
    drop(prepared);
    assert!(
        fixture.state.shell_read_only.lock().unwrap().is_empty(),
        "a preparation that never installs leaves the list as it was"
    );
}

#[test]
fn a_second_reload_replaces_again_and_an_empty_config_empties() {
    let fixture = Fixture::new();
    fixture.rig.write_global(
        &json!({"model":"scripted/s.json", "shell":{"read_only":{"jq":{"flags":[]}}}}),
    );
    let prepared = fixture.prepare();
    (prepared.installed)();
    fixture.rig.write_global(
        &json!({"model":"scripted/s.json", "shell":{"read_only":{"zq":{"flags":["-e"]}}}}),
    );
    let prepared = fixture.prepare();
    (prepared.installed)();
    assert_eq!(
        *fixture.state.shell_read_only.lock().unwrap(),
        vec![("zq".to_owned(), vec!["-e".to_owned()])]
    );
    fixture
        .rig
        .write_global(&json!({"model":"scripted/s.json"}));
    let prepared = fixture.prepare();
    (prepared.installed)();
    assert!(
        fixture.state.shell_read_only.lock().unwrap().is_empty(),
        "a reload from a config with no entry empties the list"
    );
}

/// The repository package `dir` with this manifest and `init.lua`, listed
/// by git as a repository package is.
fn declare_pkgs(rig: &Rig, pkgs: &[(&str, serde_json::Value, &str, bool)]) {
    fakes::git::git(&rig.workspace, &["init", "--quiet"]);
    let mut declared = Vec::new();
    for (dir, manifest, init, required) in pkgs {
        let pkg = rig.workspace.join(dir);
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("extension.json"), manifest.to_string()).unwrap();
        std::fs::write(pkg.join("init.lua"), init).unwrap();
        declared.push(json!({"path": dir, "required": required}));
    }
    std::fs::create_dir_all(rig.workspace.join(".fiber")).unwrap();
    std::fs::write(
        rig.workspace.join(".fiber/config.json"),
        json!({"repository_extensions": declared}).to_string(),
    )
    .unwrap();
}

fn manifest(name: &str) -> serde_json::Value {
    json!({"name": name, "version": "v1.0.0", "fiber": "0.1.0", "api": 1})
}

/// Approves the declared extension `name` through the public store API.
fn approve(fixture: &Fixture, name: &str) {
    let clock: std::sync::Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let items =
        extensions::declared_items(&fixture.rig.workspace, &*clock).expect("declared items");
    let found = items
        .into_iter()
        .find(|item| item.kind == contract::events::OfferedKind::Extension && item.name == name)
        .expect("the declared extension");
    let mut index = extensions::Index::load(&fixture.rig.home);
    let digest = extensions::hash(&mut index, &found).expect("the content hash");
    let store = extensions::Store::new(
        &fixture.rig.home,
        &fixture.state.sources.project,
        std::sync::Arc::clone(&clock),
    )
    .fetching(extensions::Fetch::new(
        extensions::Origin::github(),
        env!("CARGO_PKG_VERSION"),
        clock,
    ));
    store.approve(&found, &digest).expect("the approval");
}

#[test]
fn reload_lists_an_approved_repository_package() {
    let fixture = Fixture::new();
    declare_pkgs(
        &fixture.rig,
        &[(
            "pkg",
            manifest("fiber.test/lint"),
            "fiber.command('lint',{timeout=5000,run=function() return 'lint' end})",
            false,
        )],
    );
    approve(&fixture, "fiber.test/lint");
    let prepared = fixture.prepare();
    let listed = prepared
        .extensions
        .iter()
        .find(|extension| extension.name == "fiber.test/lint")
        .expect("the approved package is listed");
    assert_eq!(listed.version, "v1.0.0");
}

#[test]
fn reload_lists_a_repository_package_disabled_in_configuration() {
    let fixture = Fixture::new();
    declare_pkgs(&fixture.rig, &[("pkg", manifest("x"), "-- entry\n", false)]);
    approve(&fixture, "x");
    // The `enabled` gate is personal: an approved repository copy loads
    // even when configuration disables the name.
    fixture
        .rig
        .write_global(&json!({"model":"scripted/s.json", "extensions":{"x":{"enabled":false}}}));
    let prepared = fixture.prepare();
    assert!(
        prepared
            .extensions
            .iter()
            .any(|extension| extension.name == "x"),
        "the disabled repository package still lists: {:?}",
        prepared.extensions
    );
}

#[test]
fn reload_rejects_a_missing_required_package_and_keeps_current() {
    let fixture = Fixture::new();
    declare_pkgs(
        &fixture.rig,
        &[(
            "pkg",
            manifest("fiber.test/need"),
            "error(\"boom\")\n",
            true,
        )],
    );
    approve(&fixture, "fiber.test/need");
    let old = fixture.state.current.get();
    let state = fixture.state.clone();
    let rejected = fakes::within(
        "required reload",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            state
                .prepare(&r#loop::ReloadRoute {
                    model: "scripted/s.json".into(),
                    credential: None,
                })
                .err()
                .unwrap()
        },
    );
    assert_eq!(rejected.code, contract::ErrorCode::ExtensionRequiredFailed);
    assert!(
        rejected.message.contains("fiber.test/need"),
        "the rejection names the missing extension: {}",
        rejected.message
    );
    assert!(Arc::ptr_eq(&old, &fixture.state.current.get()));
}

#[test]
fn the_late_preparer_drops_written_notices_and_keeps_new_ones() {
    let fixture = Fixture::new();
    declare_pkgs(
        &fixture.rig,
        &[(
            "old",
            manifest("fiber.test/old"),
            "error(\"old boom\")\n",
            false,
        )],
    );
    approve(&fixture, "fiber.test/old");
    // The first preparation stands in for the startup: its notices are
    // what the session already wrote.
    let written = fixture.prepare().notices;
    assert_eq!(written.len(), 1, "{written:?}");
    declare_pkgs(
        &fixture.rig,
        &[
            (
                "old",
                manifest("fiber.test/old"),
                "error(\"old boom\")\n",
                false,
            ),
            (
                "new",
                manifest("fiber.test/new"),
                "error(\"new boom\")\n",
                false,
            ),
        ],
    );
    approve(&fixture, "fiber.test/new");
    let state =
        Arc::try_unwrap(fixture.state).unwrap_or_else(|_| panic!("only the test holds the state"));
    let (_reload, late) = state.preparers(written);
    let prepared = fakes::within(
        "late preparation",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            late(&r#loop::ReloadRoute {
                model: "scripted/s.json".into(),
                credential: None,
            })
            .unwrap()
        },
    );
    assert_eq!(prepared.notices.len(), 1, "{:?}", prepared.notices);
    assert!(
        prepared.notices[0].message.contains("new boom"),
        "the new notice is kept: {:?}",
        prepared.notices
    );
}

/// A startup notice of the kind `mcp_servers_started` writes.
fn startup_notice(text: &str) -> contract::events::Notice {
    contract::events::Notice {
        code: contract::ErrorCode::ExtensionFailed,
        message: text.to_owned(),
        extension: None,
    }
}

#[test]
fn a_rejected_required_package_quotes_its_own_notice() {
    let fixture = Fixture::new();
    // The other package fails too, and its notice comes first in the
    // set: the rejection must quote the missing package's own notice.
    declare_pkgs(
        &fixture.rig,
        &[
            (
                "other",
                manifest("fiber.test/other"),
                "error(\"other boom\")\n",
                false,
            ),
            (
                "need",
                manifest("fiber.test/need"),
                "error(\"need boom\")\n",
                true,
            ),
        ],
    );
    approve(&fixture, "fiber.test/other");
    approve(&fixture, "fiber.test/need");
    let state = fixture.state.clone();
    let rejected = fakes::within(
        "required detail",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            state
                .prepare(&r#loop::ReloadRoute {
                    model: "scripted/s.json".into(),
                    credential: None,
                })
                .err()
                .unwrap()
        },
    );
    assert_eq!(rejected.code, contract::ErrorCode::ExtensionRequiredFailed);
    assert!(
        rejected.message.contains("need boom"),
        "the rejection quotes the missing package's notice: {}",
        rejected.message
    );
    assert!(
        !rejected.message.contains("other boom"),
        "another package's notice is not quoted: {}",
        rejected.message
    );
}

#[test]
fn a_late_install_does_not_repeat_the_startup_notices() {
    let fixture = Fixture::new();
    declare_pkgs(
        &fixture.rig,
        &[(
            "old",
            manifest("fiber.test/old"),
            "error(\"old boom\")\n",
            false,
        )],
    );
    approve(&fixture, "fiber.test/old");
    // The startup installs the old package: its notice is the one the
    // session already wrote.
    let prepared = fixture.prepare();
    (prepared.installed)();
    let written = crate::session_extensions::startup_written(&fixture.state.current.get(), &[]);
    assert_eq!(written.len(), 1, "{written:?}");
    assert!(written[0].message.contains("old boom"), "{written:?}");
    declare_pkgs(
        &fixture.rig,
        &[
            (
                "old",
                manifest("fiber.test/old"),
                "error(\"old boom\")\n",
                false,
            ),
            (
                "new",
                manifest("fiber.test/new"),
                "error(\"new boom\")\n",
                false,
            ),
        ],
    );
    approve(&fixture, "fiber.test/new");
    let state =
        Arc::try_unwrap(fixture.state).unwrap_or_else(|_| panic!("only the test holds the state"));
    let (_reload, late) = state.preparers(written);
    let prepared = fakes::within(
        "late install",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            late(&r#loop::ReloadRoute {
                model: "scripted/s.json".into(),
                credential: None,
            })
            .unwrap()
        },
    );
    assert_eq!(prepared.notices.len(), 1, "{:?}", prepared.notices);
    assert!(
        prepared.notices[0].message.contains("new boom"),
        "only the new notice is written: {:?}",
        prepared.notices
    );
}

#[test]
fn startup_written_is_the_session_notices_then_the_startup_notices() {
    let fixture = Fixture::new();
    declare_pkgs(
        &fixture.rig,
        &[(
            "old",
            manifest("fiber.test/old"),
            "error(\"old boom\")\n",
            false,
        )],
    );
    approve(&fixture, "fiber.test/old");
    let prepared = fixture.prepare();
    (prepared.installed)();
    let current = fixture.state.current.get();
    let session = current.notices();
    assert_eq!(session.len(), 1, "{session:?}");
    let startup = startup_notice("a startup note");
    assert_eq!(
        crate::session_extensions::startup_written(&current, std::slice::from_ref(&startup)),
        vec![session[0].clone(), startup],
    );
}

#[test]
fn unwritten_keeps_only_the_startup_notices_the_session_did_not_write() {
    let fixture = Fixture::new();
    declare_pkgs(
        &fixture.rig,
        &[(
            "old",
            manifest("fiber.test/old"),
            "error(\"old boom\")\n",
            false,
        )],
    );
    approve(&fixture, "fiber.test/old");
    let prepared = fixture.prepare();
    (prepared.installed)();
    let current = fixture.state.current.get();
    let written = current.notices()[0].clone();
    let first = startup_notice("first new");
    let second = startup_notice("second new");
    let kept = crate::session_extensions::unwritten(
        vec![first.clone(), written, second.clone()],
        &current,
    );
    assert_eq!(kept, vec![first, second]);
}

/// `installed` hands the reloaded set to the delegate service: the
/// recording service's `retain` gets the new set's loaded names.
#[test]
fn installed_retains_the_reloaded_set_on_the_delegate_service() {
    let fixture = Fixture::new();
    let prepared = fixture.prepare();
    (prepared.installed)();
    assert_eq!(
        fixture.delegate.retains.lock().unwrap().clone(),
        vec![vec!["demo".to_owned(), "judge".to_owned()]],
        "retain runs once with the reloaded set's names"
    );
}
