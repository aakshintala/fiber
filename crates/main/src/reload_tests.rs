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
    commands_synced: Arc<Mutex<r#loop::RunnableCommands>>,
    declared: Arc<Mutex<std::collections::BTreeMap<String, contract::events::ToolInfo>>>,
    delegate: Arc<RecordingDelegate>,
    servers: Vec<FakeServer>,
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
        let commands_synced = Arc::new(Mutex::new(r#loop::RunnableCommands::new()));
        let synced = commands_synced.clone();
        let declared = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
        let declared_now = declared.clone();
        let empty = empty_servers(&rig, &parts.prompt.clock);
        let state = Arc::new(State {
            sources: parts.sources,
            template: parts.prompt,
            current: Arc::new(crate::session_extensions::Current::new(parts.extensions)),
            switching: Arc::new(parts.switching),
            tools: Mutex::new(Vec::new()),
            kept_infos: Mutex::new(Vec::new()),
            prompt_rows: Mutex::new(Vec::new()),
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
            commands_sync: Arc::new(move |rows| {
                *synced.lock().unwrap() = rows.clone();
            }),
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
            prepare_progress: Mutex::new(Vec::new()),
        });
        Self {
            rig,
            state,
            replacements,
            commands_synced,
            declared,
            delegate,
            servers: Vec::new(),
        }
    }

    /// A fixture with live fixture-backed MCP servers, each `(name,
    /// tools)` declared from its own fake directory with every tool
    /// answering `hi`: the global config declares them, started eagerly,
    /// and `rewrite` replaces the global config before the next
    /// `prepare`, as editing the files does.
    fn with_servers(servers: &[(&str, serde_json::Value)]) -> Self {
        let mut fixture = Self::new();
        let fakes: Vec<FakeServer> = servers
            .iter()
            .map(|(name, tools)| {
                let fake = FakeServer::new(&fixture.rig, name, tools);
                for tool in tools.as_array().unwrap() {
                    let tool = tool.get("name").unwrap().as_str().unwrap();
                    fake.answer(tool, r#"{"content":[{"type":"text","text":"hi"}]}"#);
                }
                fake
            })
            .collect();
        let mut entries = serde_json::Map::new();
        for (fake, (name, _)) in fakes.iter().zip(servers.iter()) {
            entries.insert((*name).to_owned(), fake.entry(json!({})));
        }
        fixture.rewrite(serde_json::Value::Object(entries));
        let config = fixture.rig.config(Vec::<String>::new());
        let specs = crate::mcp_servers::specs(&config).expect("the specs read");
        let workspace = fixture.state.sources.workspace.clone();
        let artifacts = fixture.state.artifacts.clone();
        let cache = config::mcp_cache_dir(&fixture.state.sources.home);
        let clock = fixture.state.template.clock.clone();
        let started = fakes::within(
            "servers start",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                mcp::start_observed(
                    specs.specs,
                    &workspace,
                    &artifacts,
                    Arc::new(DummyImages),
                    &cache,
                    &clock,
                    "test",
                    None,
                )
            },
        );
        assert!(started.failed.is_empty(), "{:?}", started.failed);
        fixture.state = Arc::new(assemble(&fixture.state, started));
        fixture.servers = fakes;
        fixture
    }

    /// Runs the reload preparation, dumping the last reached prepare
    /// checkpoint and every fake MCP server's `stderr.log` and
    /// `requests.log` into the failure when the prepare wait fails:
    /// the wait expired, the worker panicked, or the worker thread
    /// failed to start. The checkpoint record is test-only:
    /// `prepare_inner` appends each stage boundary it passes, with no
    /// pause and no branch, because no outside seam reaches those steps:
    /// the fake servers log a request on arrival, before handling or
    /// answering it. The record and the logs are read in the failure
    /// path; when the wait expired instead, `fakes::within` never joins
    /// its work thread, so both may include later entries.
    #[track_caller]
    fn prepare(&self) -> r#loop::ReloadPrepared {
        let state = self.state.clone();
        state.clear_prepare_progress();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fakes::within(
                "reload prepared",
                Deadline::after(fakes::MUST_SUCCEED_WITHIN),
                move || {
                    state
                        .prepare_inner(
                            &r#loop::ReloadRoute {
                                model: "scripted/s.json".into(),
                                credential: None,
                            },
                            super::Preparation::Explicit,
                        )
                        .unwrap()
                },
            )
        }));
        match outcome {
            Ok(prepared) => prepared,
            Err(payload) => panic!(
                "{message}\n{progress}\n{dump}",
                message = panic_text(&payload),
                progress = self.state.prepare_progress_line(),
                dump = dump_fake_servers(self.rig.root.path()),
            ),
        }
    }
}

/// The text of a caught panic payload, or a fixed line when it is not a
/// string: the wait's own message already names what expired, and the
/// server dump below it shows what the fakes saw.
// debt: one local helper in this module; upgrade trigger: a shared public
// panic-payload formatter that can be reused without broadening this
// ticket's fence.
fn panic_text(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "a non-string panic".to_owned()
    }
}

/// Every fake server directory under the rig's root, sorted by name: a
/// directory holding `tools.json` is one `FakeServer` made, whether the
/// fixture tracks it or a test holds it aside.
fn fake_server_dirs(root: &std::path::Path) -> Vec<(String, std::path::PathBuf)> {
    let mut servers = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if dir.join("tools.json").is_file()
                && let Some(name) = dir.file_name()
            {
                servers.push((name.to_string_lossy().into_owned(), dir));
            }
        }
    }
    servers.sort();
    servers
}

/// Every fake server directory under the rig's root, sorted by name.
/// Each entry shows the server's stderr and every line it received as
/// read in the failure path: when the prepare wait expired instead,
/// `fakes::within` never joins its work thread, so the dump may
/// include later traffic.
fn dump_fake_servers(root: &std::path::Path) -> String {
    let servers = fake_server_dirs(root);
    if servers.is_empty() {
        return format!("fake MCP servers: none under {}", root.display());
    }
    let mut dump = String::from(
        "fake MCP servers read in the failure path \
         (when the prepare wait expired, the wait leaves its work thread running, so this may include later traffic):",
    );
    for (name, dir) in &servers {
        dump.push_str(&dump_fake_server(dir, name));
    }
    dump
}

/// One fake server's logs: its stderr, then every request line it read.
/// A `fail-start` server exits before it reads, so its `requests.log`
/// reads absent, which itself says the start never reached the loop.
// debt: one per-server rendering helper; upgrade trigger: another caller or
// evidence that inlining improves clarity.
fn dump_fake_server(dir: &std::path::Path, name: &str) -> String {
    format!(
        "\nfake MCP server {name}:\n{stderr}{requests}",
        stderr = read_server_log(dir, "stderr.log"),
        requests = read_server_log(dir, "requests.log"),
    )
}

/// One log file's contents, naming when it is missing or empty: both
/// cases diagnose, since a started server always writes `pid.txt` first
/// and reads into `requests.log` right after.
fn read_server_log(dir: &std::path::Path, file: &str) -> String {
    match std::fs::read_to_string(dir.join(file)) {
        Err(_) => format!("{file}: <absent>\n"),
        Ok(text) if text.is_empty() => format!("{file}: <empty>\n"),
        Ok(text) => format!("{file}:\n{}\n", text.trim_end()),
    }
}

#[test]
fn a_reload_that_keeps_the_extension_swaps_in_the_new_set() {
    let fixture = Fixture::new();
    let old = fixture.state.current.get();
    let prepared = fixture.prepare();
    (prepared.installed)();
    // The reload re-resolves every session path through the new set: a
    // binding held by the swapped-out set cannot survive the swap, while
    // the service retains the reloaded set's names.
    assert_eq!(
        fixture.delegate.retains.lock().unwrap().clone(),
        vec![vec!["demo".to_owned(), "judge".to_owned()]],
        "retain runs once with the reloaded set's names"
    );
    assert!(
        !Arc::ptr_eq(&old, &fixture.state.current.get()),
        "the swap installed the reloaded set, not the old one"
    );
}

#[test]
fn a_reload_that_drops_the_extension_retains_without_it() {
    let fixture = Fixture::new();
    std::fs::remove_dir_all(fixture.rig.home.join("extensions/demo")).unwrap();
    let prepared = fixture.prepare();
    (prepared.installed)();
    // The dropped extension leaves the reloaded set: `retain` with the
    // reduced names is what stops its running delegates elsewhere.
    assert_eq!(
        fixture.delegate.retains.lock().unwrap().clone(),
        vec![vec!["judge".to_owned()]],
        "retain runs once without the dropped extension"
    );
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
                .prepare_inner(
                    &r#loop::ReloadRoute {
                        model: "scripted/s.json".into(),
                        credential: None,
                    },
                    super::Preparation::Explicit,
                )
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
fn a_reload_install_syncs_the_runnable_discovery_baseline() {
    let fixture = Fixture::new();
    let template = fixture
        .rig
        .workspace
        .join(".agents/skills/release/SKILL.md");
    std::fs::create_dir_all(template.parent().unwrap()).unwrap();
    std::fs::write(
        &template,
        "---\nname: release\ndescription: Releases.\ndisable-model-invocation: true\n---\nBody.\n",
    )
    .unwrap();
    let path = fixture.rig.home.join("extensions/demo/init.lua");
    std::fs::write(
        path,
        "fiber.command('fresh',{timeout=5000,run=function() return 'fresh' end})",
    )
    .unwrap();
    assert!(fixture.commands_synced.lock().unwrap().is_empty());
    let prepared = fixture.prepare();
    assert!(fixture.commands_synced.lock().unwrap().is_empty());
    (prepared.installed)();
    // The install hands the skill set the same skill/template baseline
    // the replacement rows contain, without tracking extension commands.
    let replacements = fixture.replacements.lock().unwrap();
    assert_eq!(replacements.len(), 1);
    assert_eq!(
        *fixture.commands_synced.lock().unwrap(),
        std::collections::BTreeMap::from([("release".to_owned(), false)])
    );
    assert_eq!(
        replacements[0]
            .commands
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        ["release", "fresh"]
    );
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
        *state.tools.lock().unwrap() = vec![("db".into(), echo), ("builtin".into(), read.clone())];
        *state.kept_infos.lock().unwrap() = vec![read_info.clone(), echo_info, web_info];
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
    // The offer flow only adopts declared servers, so the reloaded
    // config still declares the repository server whose tools stay.
    write_repo_config(&fixture, &json!({"db": {"command": "db"}}));
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
                .prepare_inner(
                    &r#loop::ReloadRoute {
                        model: "scripted/s.json".into(),
                        credential: None,
                    },
                    super::Preparation::Explicit,
                )
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

/// Approves the declared `kind` item `name`, as an in-session `fiber approve`
/// would: the next preparation reads the approval fresh.
fn approve(fixture: &Fixture, kind: contract::events::OfferedKind, name: &str) {
    let clock: std::sync::Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let items =
        extensions::declared_items(&fixture.rig.workspace, &*clock).expect("declared items");
    let found = items
        .into_iter()
        .find(|item| item.kind == kind && item.name == name)
        .expect("the declared item");
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

/// Declares `servers` as the workspace repository's `mcp.servers`, as a
/// repository checkout would.
fn write_repo_config(fixture: &Fixture, servers: &serde_json::Value) {
    std::fs::create_dir_all(fixture.rig.workspace.join(".fiber")).unwrap();
    std::fs::write(
        fixture.rig.workspace.join(".fiber/config.json"),
        json!({"mcp": {"servers": servers}}).to_string(),
    )
    .unwrap();
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
    approve(
        &fixture,
        contract::events::OfferedKind::Extension,
        "fiber.test/lint",
    );
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
    approve(&fixture, contract::events::OfferedKind::Extension, "x");
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
    approve(
        &fixture,
        contract::events::OfferedKind::Extension,
        "fiber.test/need",
    );
    let old = fixture.state.current.get();
    let state = fixture.state.clone();
    let rejected = fakes::within(
        "required reload",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            state
                .prepare_inner(
                    &r#loop::ReloadRoute {
                        model: "scripted/s.json".into(),
                        credential: None,
                    },
                    super::Preparation::Explicit,
                )
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
    approve(
        &fixture,
        contract::events::OfferedKind::Extension,
        "fiber.test/old",
    );
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
    approve(
        &fixture,
        contract::events::OfferedKind::Extension,
        "fiber.test/new",
    );
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

#[test]
fn the_late_preparer_skips_the_mcp_sync() {
    // A late preparation never syncs MCP servers: a cached,
    // not-yet-started personal server stays unstarted, no cache is
    // written, and the grouping, records, failures and carried prompt
    // sources stay empty.
    let mut fixture = Fixture::with_servers(&[("late1", json!([{"name": "echo"}]))]);
    // Stopping the eager start and relaunching leaves the valid cache
    // behind, so the next start goes lazy: the slot is cached but has
    // no server.
    fixture.relaunch();
    assert!(
        fixture.state.servers.has_slot("late1"),
        "the cached slot stays"
    );
    assert!(
        !fixture.state.servers.running("late1"),
        "the relaunched slot never started"
    );
    // The eager start's `pid.txt` is the spawn signal: removing it
    // means any later spawn shows up again.
    std::fs::remove_file(fixture.servers[0].dir.join("pid.txt")).expect("pid.txt");
    let cache_dir = config::mcp_cache_dir(&fixture.state.sources.home);
    let cache_before: Vec<(String, Vec<u8>)> = {
        let mut files: Vec<_> = std::fs::read_dir(&cache_dir)
            .expect("the cache dir")
            .map(|entry| {
                let entry = entry.expect("a cache entry");
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    std::fs::read(entry.path()).expect("cache bytes"),
                )
            })
            .collect();
        files.sort();
        files
    };
    let servers = Arc::clone(&fixture.state.servers);
    let state =
        Arc::try_unwrap(fixture.state).unwrap_or_else(|_| panic!("only the test holds the state"));
    let (_reload, late) = state.preparers(Vec::new());
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
    assert!(prepared.server_sync.servers.kept.is_empty());
    assert!(prepared.server_sync.servers.restarted.is_empty());
    assert!(prepared.server_sync.servers.started.is_empty());
    assert!(prepared.server_sync.servers.stopped.is_empty());
    assert!(prepared.server_sync.server_records.is_empty());
    assert!(prepared.server_sync.server_failures.is_empty());
    assert!(
        prepared.server_sync.new_prompts.is_none(),
        "no prompt source is carried"
    );
    assert!(
        !fixture.servers[0].dir.join("pid.txt").exists(),
        "the late preparation starts nothing"
    );
    let mut cache_after: Vec<(String, Vec<u8>)> = std::fs::read_dir(&cache_dir)
        .expect("the cache dir")
        .map(|entry| {
            let entry = entry.expect("a cache entry");
            (
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path()).expect("cache bytes"),
            )
        })
        .collect();
    cache_after.sort();
    assert_eq!(
        cache_after, cache_before,
        "the late preparation writes nothing"
    );
    assert!(
        servers.has_slot("late1") && !servers.running("late1"),
        "the cached slot stays exactly as the session holds it"
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
    approve(
        &fixture,
        contract::events::OfferedKind::Extension,
        "fiber.test/other",
    );
    approve(
        &fixture,
        contract::events::OfferedKind::Extension,
        "fiber.test/need",
    );
    let state = fixture.state.clone();
    let rejected = fakes::within(
        "required detail",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            state
                .prepare_inner(
                    &r#loop::ReloadRoute {
                        model: "scripted/s.json".into(),
                        credential: None,
                    },
                    super::Preparation::Explicit,
                )
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
    approve(
        &fixture,
        contract::events::OfferedKind::Extension,
        "fiber.test/old",
    );
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
    approve(
        &fixture,
        contract::events::OfferedKind::Extension,
        "fiber.test/new",
    );
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
    approve(
        &fixture,
        contract::events::OfferedKind::Extension,
        "fiber.test/old",
    );
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
    approve(
        &fixture,
        contract::events::OfferedKind::Extension,
        "fiber.test/old",
    );
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

/// One fixture-backed MCP server: its directory holds `tools.json` and
/// one `call-<tool>.json` per tool, read by `fakes::mcp_fixture`'s
/// `server.sh`.
struct FakeServer {
    dir: std::path::PathBuf,
}

impl FakeServer {
    fn new(rig: &Rig, name: &str, tools: &serde_json::Value) -> Self {
        let dir = rig.root.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let server = Self { dir };
        server.set_tools(tools);
        server
    }

    fn set_tools(&self, tools: &serde_json::Value) {
        std::fs::write(self.dir.join("tools.json"), tools.to_string()).unwrap();
    }

    fn answer(&self, tool: &str, result: &str) {
        std::fs::write(self.dir.join(format!("call-{tool}.json")), result).unwrap();
    }

    fn pid(&self) -> u32 {
        std::fs::read_to_string(self.dir.join("pid.txt"))
            .expect("pid.txt")
            .trim()
            .parse()
            .expect("a pid")
    }

    fn kill(&self) {
        fakes::kill_pid(self.pid(), "KILL").expect("the server dies");
    }

    /// The global-config entry running this fake: through `/bin/sh` so the
    /// fake's stderr lands in its directory's `stderr.log`, which a
    /// prepare timeout dumps beside `requests.log`. The shell execs the
    /// fixture, so the server keeps its own pid and process group; the
    /// paths ride as positional parameters, so no path needs quoting.
    fn entry(&self, extra: serde_json::Value) -> serde_json::Value {
        let mut entry = serde_json::json!({
            "command": "/bin/sh",
            "args": ["-c", "exec \"$0\" \"$1\" 2>\"$1/stderr.log\"", fakes::mcp_fixture().display().to_string(), self.dir.display().to_string()],
        });
        for (key, value) in extra.as_object().unwrap() {
            entry[key] = value.clone();
        }
        entry
    }
}

/// Rebuilds the `State` around a fresh start, keeping the maps and
/// closures the assertions share: the flip tests relaunch after
/// rewriting a cache so the next start goes lazy.
fn assemble(base: &State, started: mcp::Started) -> State {
    State {
        sources: base.sources.clone(),
        template: base.template.clone(),
        current: Arc::clone(&base.current),
        switching: Arc::clone(&base.switching),
        tools: Mutex::new(started.tools),
        kept_infos: Mutex::new(started.infos),
        prompt_rows: Mutex::new(started.prompts.commands()),
        late: Arc::clone(&base.late),
        repository: Arc::clone(&base.repository),
        declare: Arc::clone(&base.declare),
        replace: Arc::clone(&base.replace),
        commands_sync: Arc::clone(&base.commands_sync),
        drive: Arc::clone(&base.drive),
        model: Arc::clone(&base.model),
        delegate: Arc::clone(&base.delegate),
        attended: Arc::clone(&base.attended),
        answerable: base.answerable,
        log: Arc::clone(&base.log),
        diag: Arc::clone(&base.diag),
        names: Mutex::new(Vec::new()),
        shell_read_only: base.shell_read_only.clone(),
        servers: Arc::new(started.servers),
        artifacts: base.artifacts.clone(),
        images: Arc::clone(&base.images),
        prepare_progress: Mutex::new(Vec::new()),
    }
}

impl Fixture {
    fn try_prepare(&self) -> Result<r#loop::ReloadPrepared, contract::inbox::Rejection> {
        let state = self.state.clone();
        fakes::within(
            "reload attempted",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                state.prepare_inner(
                    &r#loop::ReloadRoute {
                        model: "scripted/s.json".into(),
                        credential: None,
                    },
                    super::Preparation::Explicit,
                )
            },
        )
    }

    /// Replaces the global config's `mcp.servers` with `entries`, as
    /// editing the files does; the next `prepare` loads it fresh.
    fn rewrite(&self, entries: serde_json::Value) {
        self.rig
            .write_global(&json!({"model":"scripted/s.json", "mcp": {"servers": entries}}));
    }

    fn tool(&self, name: &str) -> Arc<dyn contract::tool::Tool> {
        self.state
            .tools
            .lock()
            .unwrap()
            .iter()
            .find(|(_, tool)| tool.definition().name == name)
            .unwrap_or_else(|| panic!("tool {name} is registered"))
            .1
            .clone()
    }

    /// Runs `tool` with no arguments: the fake answers from its
    /// `call-<tool>.json`.
    fn run(&self, tool: &Arc<dyn contract::tool::Tool>) -> contract::tool::Output {
        tool.run(
            &Default::default(),
            &fakes::CancelToken::new(),
            &fakes::Recorder::default(),
        )
    }

    fn declared_names(&self) -> Vec<String> {
        self.declared
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    }

    fn cache_bytes(&self, server: &str) -> Vec<u8> {
        std::fs::read(config::mcp_cache_dir(&self.rig.home).join(format!("{server}.json")))
            .unwrap_or_default()
    }

    /// Flips the cached `resources` flag for `server`, as a server that
    /// advertised resources would leave it: the key stays valid, so the
    /// next start goes lazy.
    fn flip_resources(&self, server: &str, resources: bool) {
        let path = config::mcp_cache_dir(&self.rig.home).join(format!("{server}.json"));
        let mut file: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        file["resources"] = resources.into();
        std::fs::write(&path, file.to_string()).unwrap();
    }

    /// Stops every server and starts the current config again, keeping
    /// the shared maps: after a cache flip the start goes lazy.
    fn relaunch(&mut self) {
        self.state.servers.stop();
        let config = self.rig.config(Vec::<String>::new());
        let specs = crate::mcp_servers::specs(&config).expect("the specs read");
        let mut all = specs.specs;
        all.extend(specs.repository.into_iter().map(|repo| repo.spec));
        let workspace = self.state.sources.workspace.clone();
        let artifacts = self.state.artifacts.clone();
        let cache = config::mcp_cache_dir(&self.state.sources.home);
        let clock = self.state.template.clock.clone();
        let started = fakes::within(
            "servers relaunch",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                mcp::start_observed(
                    all,
                    &workspace,
                    &artifacts,
                    Arc::new(DummyImages),
                    &cache,
                    &clock,
                    "test",
                    None,
                )
            },
        );
        assert!(started.failed.is_empty(), "{:?}", started.failed);
        self.state = Arc::new(assemble(&self.state, started));
    }
}

fn run_named(
    tools: &[(String, Arc<dyn contract::tool::Tool>)],
    fixture: &Fixture,
    name: &str,
) -> contract::tool::Output {
    let tool = tools
        .iter()
        .find(|(_, tool)| tool.definition().name == name)
        .unwrap_or_else(|| panic!("tool {name} is declared"))
        .1
        .clone();
    fixture.run(&tool)
}

#[test]
fn reload_with_a_changed_executable_serves_the_new_servers_tools() {
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    let first = fixture.prepare();
    assert_eq!(first.server_sync.servers.kept, ["fx"]);
    let pid = fixture.servers[0].pid();
    // A second fake directory is the new executable.
    let next = FakeServer::new(&fixture.rig, "fx-next", &json!([{"name": "plus"}]));
    next.answer("plus", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    fixture.rewrite(json!({"fx": next.entry(json!({}))}));
    let prepared = fixture.prepare();
    assert!(prepared.server_sync.servers.kept.is_empty());
    assert_eq!(prepared.server_sync.servers.restarted, ["fx"]);
    assert!(prepared.server_sync.servers.started.is_empty());
    assert!(prepared.server_sync.servers.stopped.is_empty());
    assert!(
        run_named(&prepared.tools, &fixture, "mcp__fx__plus")
            .error
            .is_none()
    );
    assert!(
        prepared
            .tools
            .iter()
            .all(|(_, tool)| tool.definition().name != "mcp__fx__echo"),
        "the old list is gone"
    );
    assert_ne!(next.pid(), pid, "the new executable answers");
    assert!(
        prepared
            .server_sync
            .server_records
            .iter()
            .any(|record| matches!(record, contract::tool::ServerRecord::Ready(_))),
        "the restart reads ready"
    );
    (prepared.installed)();
    assert!(
        fixture
            .declared_names()
            .contains(&"mcp__fx__plus".to_owned())
    );
    assert!(
        !fixture
            .declared_names()
            .contains(&"mcp__fx__echo".to_owned())
    );
}

#[test]
fn reload_with_only_a_timeout_change_replaces_with_the_new_spec() {
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    let pid = fixture.servers[0].pid();
    fixture.rewrite(json!({"fx": fixture.servers[0].entry(json!({"startup_timeout_ms": 6000}))}));
    let prepared = fixture.prepare();
    assert!(prepared.server_sync.servers.kept.is_empty());
    assert_eq!(prepared.server_sync.servers.restarted, ["fx"]);
    assert_ne!(
        fixture.servers[0].pid(),
        pid,
        "a timeout change restarts the server"
    );
    assert!(
        run_named(&prepared.tools, &fixture, "mcp__fx__echo")
            .error
            .is_none()
    );
}

#[test]
fn reload_routes_a_replaced_servers_kept_prompt_through_the_replacement() {
    // A same-name replacement serving a kept prompt name answers the
    // preserved `/name` row through the replacement source: the row
    // keeps its metadata, but the fetch routes to the new slot, while
    // the old slot serves nothing again after the commit stops it.
    let mut fixture = Fixture::new();
    let fx = FakeServer::new(&fixture.rig, "fx", &json!([{"name": "echo"}]));
    fx.answer("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    std::fs::write(
        fx.dir.join("prompts.json"),
        json!([{"name": "greet", "description": "Greets."}]).to_string(),
    )
    .unwrap();
    std::fs::write(
        fx.dir.join("prompt-greet.json"),
        r#"{"messages":[{"role":"user","content":{"type":"text","text":"Hello, Old."}}]}"#,
    )
    .unwrap();
    fixture.rewrite(json!({"fx": fx.entry(json!({}))}));
    fixture.relaunch();
    fixture.servers = vec![fx];
    assert!(
        fixture
            .state
            .prompt_rows
            .lock()
            .unwrap()
            .iter()
            .any(|row| row.tag == "fx" && row.name == "greet"),
        "the startup lists the prompt"
    );
    // A second fake directory is the replacement executable, serving
    // the same prompt name with new text.
    let next = FakeServer::new(&fixture.rig, "fx-next", &json!([{"name": "echo"}]));
    next.answer("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    std::fs::write(
        next.dir.join("prompts.json"),
        json!([{"name": "greet", "description": "Greets."}]).to_string(),
    )
    .unwrap();
    std::fs::write(
        next.dir.join("prompt-greet.json"),
        r#"{"messages":[{"role":"user","content":{"type":"text","text":"Hello, New."}}]}"#,
    )
    .unwrap();
    let old_pid = fixture.servers[0].pid();
    fixture.rewrite(json!({"fx": next.entry(json!({}))}));
    let prepared = fixture.prepare();
    assert_eq!(prepared.server_sync.servers.restarted, ["fx"]);
    assert_ne!(next.pid(), old_pid, "the new executable answers");
    (prepared.installed)();
    assert!(
        !fakes::pid_alive(old_pid),
        "the old slot serves nothing again after the install stops it"
    );
    let carried = prepared
        .server_sync
        .new_prompts
        .as_ref()
        .expect("the replacement carries its prompts with their fetch");
    assert!(
        carried
            .0
            .iter()
            .any(|row| row.tag == "fx" && row.name == "greet"),
        "the kept prompt name rides the carried rows: {:?}",
        carried.0
    );
    let fetched = (carried.1)("fx", "greet", "", &fakes::CancelToken::new());
    assert!(fetched.error.is_none(), "{:?}", fetched.error);
    assert!(
        fetched.content.iter().any(|part| matches!(
            part,
            contract::shapes::ContentPart::Text { text } if text.contains("New")
        )),
        "the preserved row answers through the replacement source"
    );
}

#[test]
fn reload_with_an_unchanged_spec_keeps_without_reconnecting() {
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    let pid = fixture.servers[0].pid();
    let cache = fixture.cache_bytes("fx");
    let prepared = fixture.prepare();
    assert_eq!(prepared.server_sync.servers.kept, ["fx"]);
    assert!(prepared.server_sync.servers.restarted.is_empty());
    assert!(prepared.server_sync.servers.started.is_empty());
    assert!(prepared.server_sync.servers.stopped.is_empty());
    assert!(prepared.server_sync.server_records.is_empty());
    assert_eq!(fixture.servers[0].pid(), pid, "no reconnect");
    assert_eq!(fixture.cache_bytes("fx"), cache, "no cache write");
    assert!(fixture.run(&fixture.tool("mcp__fx__echo")).error.is_none());
}

#[test]
fn reload_replaces_a_dead_server_beside_the_old_slot() {
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    fixture.prepare();
    let pid = fixture.servers[0].pid();
    fixture.servers[0].kill();
    let prepared = fixture.prepare();
    assert!(prepared.server_sync.servers.kept.is_empty());
    assert_eq!(prepared.server_sync.servers.restarted, ["fx"]);
    assert_ne!(
        fixture.servers[0].pid(),
        pid,
        "the replacement answers from its own server"
    );
    assert!(
        run_named(&prepared.tools, &fixture, "mcp__fx__echo")
            .error
            .is_none()
    );
    assert!(
        prepared
            .server_sync
            .server_records
            .iter()
            .any(|record| matches!(record, contract::tool::ServerRecord::Ready(_))),
        "the replacement reads ready"
    );
    assert!(
        matches!(
            prepared.server_sync.server_records.as_slice(),
            [
                contract::tool::ServerRecord::Failed(failed),
                contract::tool::ServerRecord::Ready(_)
            ] if failed.reason == contract::events::ServerFailure::Died
        ),
        "the detected death rides before the ready"
    );
}

#[test]
fn reload_replaces_a_dead_slot_beside_the_old_one() {
    // A death the session already observed leaves the slot `Dead`
    // once its restart fails: the reload shadow-starts the same spec
    // fresh and files it under `restarted`, never `started`.
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    let pid = fixture.servers[0].pid();
    fixture.servers[0].kill();
    std::fs::write(fixture.servers[0].dir.join("fail-start"), "").unwrap();
    assert!(
        fixture.run(&fixture.tool("mcp__fx__echo")).error.is_some(),
        "the restart fails and the slot dies"
    );
    std::fs::remove_file(fixture.servers[0].dir.join("fail-start")).unwrap();
    let prepared = fixture.prepare();
    assert_eq!(prepared.server_sync.servers.restarted, ["fx"]);
    assert!(
        prepared.server_sync.servers.started.is_empty(),
        "a dead slot never files under started"
    );
    assert_ne!(
        fixture.servers[0].pid(),
        pid,
        "the replacement answers from its own server"
    );
    assert!(
        run_named(&prepared.tools, &fixture, "mcp__fx__echo")
            .error
            .is_none()
    );
}

#[test]
fn reload_starts_an_unstarted_server() {
    let fixture = Fixture::with_servers(&[]);
    let fake = FakeServer::new(&fixture.rig, "fx", &json!([{"name": "echo"}]));
    fake.answer("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    fixture.rewrite(json!({"fx": fake.entry(json!({}))}));
    let prepared = fixture.prepare();
    assert_eq!(prepared.server_sync.servers.started, ["fx"]);
    assert!(prepared.server_sync.servers.kept.is_empty());
    assert!(
        run_named(&prepared.tools, &fixture, "mcp__fx__echo")
            .error
            .is_none()
    );
    assert!(
        prepared
            .server_sync
            .server_records
            .iter()
            .any(|record| matches!(record, contract::tool::ServerRecord::Ready(_))),
    );
}

#[test]
fn reload_declares_a_tool_added_to_an_advertised_empty_list() {
    let fixture = Fixture::with_servers(&[("fx", json!([]))]);
    let quiet = fixture.prepare();
    assert!(
        quiet
            .tools
            .iter()
            .all(|(_, tool)| tool.definition().name != "mcp__fx__echo"),
        "the empty list declares nothing"
    );
    fixture.servers[0].set_tools(&json!([{"name": "echo"}]));
    fixture.servers[0].answer("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    let prepared = fixture.prepare();
    assert_eq!(prepared.server_sync.servers.kept, ["fx"]);
    (prepared.installed)();
    assert!(
        run_named(&prepared.tools, &fixture, "mcp__fx__echo")
            .error
            .is_none()
    );
}

#[test]
fn reload_stops_a_removed_server_and_undeclares_its_tools() {
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    (fixture.prepare().installed)();
    assert!(
        fixture
            .declared_names()
            .contains(&"mcp__fx__echo".to_owned())
    );
    fixture.rewrite(json!({}));
    let prepared = fixture.prepare();
    assert_eq!(prepared.server_sync.servers.stopped, ["fx"]);
    assert!(
        prepared
            .tools
            .iter()
            .all(|(_, tool)| tool.definition().name != "mcp__fx__echo")
    );
    (prepared.installed)();
    assert!(
        !fixture
            .declared_names()
            .contains(&"mcp__fx__echo".to_owned())
    );
    assert!(
        !fixture.state.servers.has_slot("fx"),
        "the old slot is stopped"
    );
}

#[test]
fn reload_starts_a_readded_name_fresh() {
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    fixture.rewrite(json!({}));
    let removed = fixture.prepare();
    assert_eq!(removed.server_sync.servers.stopped, ["fx"]);
    (removed.installed)();
    fixture.rewrite(json!({"fx": fixture.servers[0].entry(json!({}))}));
    let prepared = fixture.prepare();
    assert_eq!(prepared.server_sync.servers.started, ["fx"]);
    assert!(
        run_named(&prepared.tools, &fixture, "mcp__fx__echo")
            .error
            .is_none()
    );
}

#[test]
fn reload_skips_a_repository_server_without_approval() {
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    let repo = FakeServer::new(&fixture.rig, "db", &json!([{"name": "echo"}]));
    std::fs::create_dir_all(fixture.rig.workspace.join(".fiber")).unwrap();
    std::fs::write(
        fixture.rig.workspace.join(".fiber/config.json"),
        json!({"mcp": {"servers": {"db": repo.entry(json!({}))}}}).to_string(),
    )
    .unwrap();
    let prepared = fixture.prepare();
    assert_eq!(prepared.server_sync.servers.kept, ["fx"]);
    assert!(prepared.server_sync.servers.started.is_empty());
    assert!(prepared.server_sync.server_records.is_empty());
    assert!(prepared.server_sync.server_failures.is_empty());
    assert!(
        !fixture.state.servers.has_slot("db"),
        "the unapproved server never starts"
    );
}

#[test]
fn reload_restarts_a_changed_approved_repository_server() {
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    let db = FakeServer::new(&fixture.rig, "db", &json!([{"name": "echo"}]));
    db.answer("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    write_repo_config(&fixture, &json!({"db": db.entry(json!({}))}));
    approve(&fixture, contract::events::OfferedKind::McpServer, "db");
    let first = fixture.prepare();
    assert_eq!(first.server_sync.servers.started, ["db"]);
    assert!(
        run_named(&first.tools, &fixture, "mcp__db__echo")
            .error
            .is_none()
    );
    (first.installed)();
    let pid = db.pid();
    // A new executable under the same name, approved anew: the changed
    // key takes the replace path and the new server answers.
    let next = FakeServer::new(&fixture.rig, "db-next", &json!([{"name": "plus"}]));
    next.answer("plus", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    write_repo_config(&fixture, &json!({"db": next.entry(json!({}))}));
    approve(&fixture, contract::events::OfferedKind::McpServer, "db");
    let prepared = fixture.prepare();
    assert_eq!(prepared.server_sync.servers.kept, ["fx"]);
    assert_eq!(prepared.server_sync.servers.restarted, ["db"]);
    assert!(
        run_named(&prepared.tools, &fixture, "mcp__db__plus")
            .error
            .is_none(),
        "the new executable answers"
    );
    assert!(
        prepared
            .tools
            .iter()
            .all(|(_, tool)| tool.definition().name != "mcp__db__echo"),
        "the old list is gone"
    );
    assert_ne!(next.pid(), pid, "the restart spawns the new executable");
}

#[test]
fn reload_relists_an_unchanged_approved_repository_server_without_reconnecting() {
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    let db = FakeServer::new(&fixture.rig, "db", &json!([{"name": "echo"}]));
    db.answer("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    write_repo_config(&fixture, &json!({"db": db.entry(json!({}))}));
    approve(&fixture, contract::events::OfferedKind::McpServer, "db");
    let first = fixture.prepare();
    assert_eq!(first.server_sync.servers.started, ["db"]);
    (first.installed)();
    let pid = db.pid();
    let prepared = fixture.prepare();
    assert_eq!(prepared.server_sync.servers.kept, ["fx", "db"]);
    assert!(prepared.server_sync.servers.restarted.is_empty());
    assert_eq!(db.pid(), pid, "the re-list reconnects nothing");
    assert!(
        run_named(&prepared.tools, &fixture, "mcp__db__echo")
            .error
            .is_none()
    );
}

#[test]
fn reload_starts_an_approved_in_session_repository_server_with_its_prompts() {
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    let db = FakeServer::new(&fixture.rig, "db", &json!([{"name": "echo"}]));
    db.answer("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    std::fs::write(
        db.dir.join("prompts.json"),
        json!([{"name": "greet", "description": "Greets."}]).to_string(),
    )
    .unwrap();
    std::fs::write(
        db.dir.join("prompt-greet.json"),
        r#"{"messages":[{"role":"user","content":{"type":"text","text":"Hello, Ada."}}]}"#,
    )
    .unwrap();
    write_repo_config(&fixture, &json!({"db": db.entry(json!({}))}));
    // Unapproved, the repository server stays skipped exactly as today.
    let skipped = fixture.prepare();
    assert!(skipped.server_sync.servers.started.is_empty());
    assert!(!fixture.state.servers.has_slot("db"));
    // Approved in-session, it starts with its prompts joining the `/` list.
    approve(&fixture, contract::events::OfferedKind::McpServer, "db");
    let prepared = fixture.prepare();
    assert_eq!(prepared.server_sync.servers.started, ["db"]);
    assert!(
        run_named(&prepared.tools, &fixture, "mcp__db__echo")
            .error
            .is_none()
    );
    let carried = prepared
        .server_sync
        .new_prompts
        .as_ref()
        .expect("the sync carries the new rows with their fetch");
    assert_eq!(carried.0.len(), 1);
    assert_eq!(carried.0[0].name, "greet");
    assert_eq!(carried.0[0].tag, "db");
    let fetch = &carried.1;
    let fetched = fetch("db", "greet", "", &fakes::CancelToken::new());
    assert!(fetched.error.is_none());
    assert!(
        fetched.content.iter().any(|part| matches!(
            part,
            contract::shapes::ContentPart::Text { text } if text.contains("Ada")
        )),
        "the appended prompt answers"
    );
}

#[test]
fn reload_rejects_a_required_shadow_failure_writing_nothing() {
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    let cache = fixture.cache_bytes("fx");
    fixture.rewrite(json!({
        "fx": fixture.servers[0].entry(json!({})),
        "gx": {"command": "/nonexistent/fiber-mcp", "required": true},
    }));
    let rejection = fixture
        .try_prepare()
        .err()
        .expect("a required shadow failure rejects");
    assert_eq!(rejection.code, contract::ErrorCode::McpRequiredServerFailed);
    // Everything untouched: the old tool answers, the cache is
    // byte-identical, the old slot stays live, and no shadow lingers.
    assert!(fixture.run(&fixture.tool("mcp__fx__echo")).error.is_none());
    assert_eq!(fixture.cache_bytes("fx"), cache);
    assert!(fixture.state.servers.running("fx"));
    assert!(!fixture.state.servers.has_slot("gx"));
    assert!(fixture.declared_names().is_empty());
}

#[test]
fn reload_after_a_required_failure_keeps_the_staged_relist_out() {
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}, {"name": "plus"}]))]);
    fixture.prepare();
    let cache = fixture.cache_bytes("fx");
    // The re-list would drop `plus`, but the required failure below
    // rejects the reload before the commit installs anything.
    fixture.servers[0].set_tools(&json!([{"name": "echo"}]));
    fixture.rewrite(json!({
        "fx": fixture.servers[0].entry(json!({})),
        "gx": {"command": "/nonexistent/fiber-mcp", "required": true},
    }));
    let rejection = fixture
        .try_prepare()
        .err()
        .expect("a required shadow failure rejects");
    assert_eq!(rejection.code, contract::ErrorCode::McpRequiredServerFailed);
    assert!(fixture.run(&fixture.tool("mcp__fx__echo")).error.is_none());
    assert!(
        fixture.run(&fixture.tool("mcp__fx__plus")).error.is_none(),
        "the old registration still answers"
    );
    assert_eq!(fixture.cache_bytes("fx"), cache);
    assert!(fixture.state.servers.running("fx"));
    assert!(!fixture.state.servers.has_slot("gx"));
}

#[test]
fn reload_with_a_failing_replacement_stops_the_old_slot_and_shows_failed() {
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    fixture.rewrite(json!({"fx": {"command": "/nonexistent/fiber-mcp"}}));
    let prepared = fixture.prepare();
    // Non-required, so the reload succeeds with the server failed: the
    // old slot is stopped at install, never restarted.
    assert!(prepared.server_sync.servers.kept.is_empty());
    assert_eq!(prepared.server_sync.servers.restarted, ["fx"]);
    assert_eq!(prepared.server_sync.server_failures.len(), 1);
    assert_eq!(prepared.server_sync.server_failures[0].server, "fx");
    assert!(
        prepared
            .server_sync
            .server_records
            .iter()
            .any(|record| matches!(record, contract::tool::ServerRecord::Failed(_))),
        "the failure rides a failed record"
    );
    assert!(
        prepared
            .tools
            .iter()
            .all(|(_, tool)| tool.definition().name != "mcp__fx__echo")
    );
    (prepared.installed)();
    assert!(
        !fixture.state.servers.has_slot("fx"),
        "the old slot is stopped at install"
    );
    assert!(
        !fixture
            .declared_names()
            .contains(&"mcp__fx__echo".to_owned())
    );
}

#[test]
fn reload_adds_mcp_resources_when_a_protected_slot_offers_them() {
    let mut fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    let repo = FakeServer::new(&fixture.rig, "db", &json!([{"name": "echo"}]));
    repo.answer("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    // The repository server starts as a person server so its cache is
    // written, then moves to the repository layer and goes lazy with a
    // resources flag no live fixture server would set.
    fixture.rewrite(json!({
        "fx": fixture.servers[0].entry(json!({})),
        "db": repo.entry(json!({})),
    }));
    fixture.relaunch();
    fixture.flip_resources("db", true);
    fixture.relaunch();
    assert!(
        fixture
            .state
            .tools
            .lock()
            .unwrap()
            .iter()
            .any(|(_, tool)| tool.definition().name == mcp::RESOURCES_TOOL),
        "the lazy start declares the resources tool"
    );
    write_repo_config(&fixture, &json!({"db": repo.entry(json!({}))}));
    fixture.rewrite(json!({"fx": fixture.servers[0].entry(json!({}))}));
    let prepared = fixture.prepare();
    assert_eq!(prepared.server_sync.servers.started, ["fx"]);
    assert!(
        prepared
            .tools
            .iter()
            .any(|(_, tool)| tool.definition().name == mcp::RESOURCES_TOOL),
        "a protected offering slot keeps the resources tool"
    );
    assert!(fixture.state.servers.has_slot("db"));
    assert!(!fixture.state.servers.running("db"));
    (prepared.installed)();
    assert!(
        fixture
            .declared_names()
            .contains(&mcp::RESOURCES_TOOL.to_owned())
    );
}

#[test]
fn reload_removes_mcp_resources_when_no_post_sync_slot_offers_them() {
    let mut fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    fixture.flip_resources("fx", true);
    fixture.relaunch();
    assert!(
        fixture
            .state
            .tools
            .lock()
            .unwrap()
            .iter()
            .any(|(_, tool)| tool.definition().name == mcp::RESOURCES_TOOL),
        "the lazy start declares the resources tool"
    );
    // The live server advertises no resources, so the sync drops the
    // stale tool and its declaration.
    let prepared = fixture.prepare();
    assert_eq!(prepared.server_sync.servers.started, ["fx"]);
    assert!(
        prepared
            .tools
            .iter()
            .all(|(_, tool)| tool.definition().name != mcp::RESOURCES_TOOL),
    );
    (prepared.installed)();
    assert!(
        !fixture
            .declared_names()
            .contains(&mcp::RESOURCES_TOOL.to_owned())
    );
}

#[test]
fn reload_aligns_the_resources_marker_with_the_installed_set() {
    // The session starts with no offering slot, so the marker is unset.
    let mut fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    assert!(!fixture.state.servers.resources_declared());
    let repo = FakeServer::new(&fixture.rig, "db", &json!([{"name": "echo"}]));
    repo.answer("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    // The repository server starts as a person server so its cache is
    // written, then moves to the repository layer and goes lazy with a
    // resources flag no live fixture server would set.
    fixture.rewrite(json!({
        "fx": fixture.servers[0].entry(json!({})),
        "db": repo.entry(json!({})),
    }));
    fixture.relaunch();
    fixture.flip_resources("db", true);
    fixture.relaunch();
    write_repo_config(&fixture, &json!({"db": repo.entry(json!({}))}));
    fixture.rewrite(json!({"fx": fixture.servers[0].entry(json!({}))}));
    let prepared = fixture.prepare();
    assert!(
        prepared
            .tools
            .iter()
            .any(|(_, tool)| tool.definition().name == mcp::RESOURCES_TOOL),
        "a protected offering slot keeps the resources tool"
    );
    (prepared.installed)();
    assert!(
        fixture.state.servers.resources_declared(),
        "install aligns the marker with the offered set"
    );
    // A repository server approved in-session starts offering resources
    // too: the loaded start carries its own scoped copy.
    let config = fixture.rig.config(Vec::<String>::new());
    let specs = crate::mcp_servers::specs(&config).expect("the specs read");
    let db = specs
        .repository
        .into_iter()
        .find(|repo| repo.spec.name == "db")
        .expect("db stays declared")
        .spec;
    let workspace = fixture.state.sources.workspace.clone();
    let artifacts = fixture.state.artifacts.clone();
    let cache = config::mcp_cache_dir(&fixture.state.sources.home);
    let clock = fixture.state.template.clock.clone();
    let scoped = fakes::within(
        "scoped resources start",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            mcp::start_observed(
                vec![db],
                &workspace,
                &artifacts,
                std::sync::Arc::new(DummyImages),
                &cache,
                &clock,
                "test",
                None,
            )
        },
    );
    assert!(
        scoped
            .tools
            .iter()
            .any(|(_, tool)| tool.definition().name == mcp::RESOURCES_TOOL),
        "the adopted start carries its scoped copy"
    );
    // The post-offer rule strips the scoped copy exactly when the marker
    // is set, so the aggregate the reload installed still declares: on the
    // old code the marker stays unset and the scoped copy replaces it.
    let mut tools = scoped.tools;
    let mut infos = scoped.infos;
    if fixture.state.servers.resources_declared() {
        tools.retain(|(_, tool)| tool.definition().name != mcp::RESOURCES_TOOL);
        infos.retain(|info| info.name != mcp::RESOURCES_TOOL);
    }
    scoped.servers.stop();
    assert!(
        tools
            .iter()
            .all(|(_, tool)| tool.definition().name != mcp::RESOURCES_TOOL),
        "the approval strips its scoped copy"
    );
    assert!(
        fixture
            .declared_names()
            .contains(&mcp::RESOURCES_TOOL.to_owned()),
        "the full resource tool set still declares"
    );
    // The offering goes away with its declaration: the next install
    // clears the marker again, so a later approval offers its own copy
    // instead of suppressing it.
    write_repo_config(&fixture, &json!({}));
    let prepared = fixture.prepare();
    assert!(
        prepared
            .tools
            .iter()
            .all(|(_, tool)| tool.definition().name != mcp::RESOURCES_TOOL),
    );
    (prepared.installed)();
    assert!(!fixture.state.servers.resources_declared());
    assert!(
        !fixture
            .declared_names()
            .contains(&mcp::RESOURCES_TOOL.to_owned())
    );
}

#[test]
fn reload_appends_a_started_servers_prompts_while_keeping_every_startup_row() {
    // A started server's prompts join the `/` list while every startup
    // row stays: the sync carries the re-listed prompt sources, and
    // install appends only rows no earlier row holds.
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    assert!(
        fixture.state.prompt_rows.lock().unwrap().is_empty(),
        "the startup lists no prompts"
    );
    let dir = fixture.rig.root.path().join("new");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("tools.json"),
        json!([{"name": "echo"}]).to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("call-echo.json"),
        r#"{"content":[{"type":"text","text":"hi"}]}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("prompts.json"),
        json!([{"name": "greet", "description": "Greets."}]).to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("prompt-greet.json"),
        r#"{"messages":[{"role":"user","content":{"type":"text","text":"Hello, Ada."}}]}"#,
    )
    .unwrap();
    let entry = json!({
        "command": fakes::mcp_fixture().display().to_string(),
        "args": [dir.display().to_string()],
    });
    fixture.rewrite(json!({
        "fx": fixture.servers[0].entry(json!({})),
        "new": entry,
    }));
    let prepared = fixture.prepare();
    assert_eq!(prepared.server_sync.servers.kept, ["fx"]);
    assert_eq!(prepared.server_sync.servers.started, ["new"]);
    assert!(
        fixture.state.prompt_rows.lock().unwrap().is_empty(),
        "prepare keeps every startup row"
    );
    let carried = prepared
        .server_sync
        .new_prompts
        .as_ref()
        .expect("the sync carries the new rows with their fetch");
    assert_eq!(carried.0.len(), 1);
    assert_eq!(carried.0[0].name, "greet");
    assert_eq!(carried.0[0].tag, "new");
    // The appended row fetches through the carried sources, as a
    // startup row fetches through the startup ones.
    let fetch = &carried.1;
    let fetched = fetch("new", "greet", "", &fakes::CancelToken::new());
    assert!(fetched.error.is_none());
    assert!(
        fetched.content.iter().any(|part| matches!(
            part,
            contract::shapes::ContentPart::Text { text } if text.contains("Ada")
        )),
        "the appended prompt answers"
    );
}

#[test]
fn a_second_reload_retains_the_first_reloads_prompt_rows_for_kept_servers() {
    // A prompt row added by the first reload stays in the `/` list
    // across a second reload that keeps its server: the baseline is the
    // session's current rows, so a sync carrying no new prompts for a
    // kept server keeps the earlier row.
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    let dir = fixture.rig.root.path().join("new");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("tools.json"),
        json!([{"name": "echo"}]).to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("call-echo.json"),
        r#"{"content":[{"type":"text","text":"hi"}]}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("prompts.json"),
        json!([{"name": "greet", "description": "Greets."}]).to_string(),
    )
    .unwrap();
    let entry = json!({
        "command": fakes::mcp_fixture().display().to_string(),
        "args": [dir.display().to_string()],
    });
    fixture.rewrite(json!({
        "fx": fixture.servers[0].entry(json!({})),
        "new": entry,
    }));
    let first = fixture.prepare();
    assert_eq!(first.server_sync.servers.started, ["new"]);
    (first.installed)();
    let second = fixture.prepare();
    assert_eq!(second.server_sync.servers.kept, ["fx", "new"]);
    assert!(
        second.server_sync.new_prompts.is_none(),
        "nothing new on the second reload"
    );
    (second.installed)();
    let tags: Vec<String> = fixture
        .state
        .prompt_rows
        .lock()
        .unwrap()
        .iter()
        .map(|row| row.tag.clone())
        .collect();
    assert!(
        tags.contains(&"new".to_owned()),
        "the first reload's row stays: {tags:?}"
    );
    let installed = fixture.replacements.lock().unwrap();
    let commands = &installed.last().expect("two installs").commands;
    assert!(
        commands.iter().any(|row| row.tag == "new"),
        "the `/` list keeps the first reload's row"
    );
}

#[test]
fn a_dropped_preparation_after_a_server_change_installs_nothing() {
    // `docs/configuration.md`, "When Fiber reads configuration": a
    // rejected reload changes nothing. Preparation stages the
    // replacement but installs nothing: the old slot still answers,
    // the cache is unchanged, and the shadow is staged but not live.
    // Dropping the preparation stops the shadow without declaring
    // anything, so a failure in the fallible steps after the plan
    // cannot leave the reload half-applied.
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    let old_pid = fixture.servers[0].pid();
    let cache = fixture.cache_bytes("fx");
    let next = FakeServer::new(&fixture.rig, "fx-next", &json!([{"name": "plus"}]));
    next.answer("plus", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    fixture.rewrite(json!({"fx": next.entry(json!({}))}));
    let prepared = fixture.prepare();
    assert_eq!(prepared.server_sync.servers.restarted, ["fx"]);
    assert!(
        fakes::pid_alive(old_pid),
        "the old slot still answers before anything is installed"
    );
    assert!(
        fixture.run(&fixture.tool("mcp__fx__echo")).error.is_none(),
        "the old registration still answers"
    );
    assert_eq!(
        fixture.cache_bytes("fx"),
        cache,
        "no cache write before install"
    );
    let shadow_pid = next.pid();
    assert!(
        fakes::pid_alive(shadow_pid),
        "the shadow started while staging"
    );
    drop(prepared);
    assert!(
        !fakes::pid_alive(shadow_pid),
        "the dropped sync stops its shadow"
    );
    assert!(fakes::pid_alive(old_pid), "the old slot keeps serving");
    assert!(
        fixture.run(&fixture.tool("mcp__fx__echo")).error.is_none(),
        "the old registration still answers"
    );
    assert_eq!(fixture.cache_bytes("fx"), cache);
    assert!(
        fixture.declared_names().is_empty(),
        "nothing declared, so no `reloaded` line could follow"
    );
}

#[test]
fn a_late_approval_after_a_replacing_reload_declares_the_replacement() {
    // An explicit reload replaces the server and installs the new
    // set; a later extension approval prepares from the session-held
    // set, so it keeps the replacement's tools, not the startup ones.
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    let next = FakeServer::new(&fixture.rig, "fx-next", &json!([{"name": "plus"}]));
    next.answer("plus", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    fixture.rewrite(json!({"fx": next.entry(json!({}))}));
    let prepared = fixture.prepare();
    assert_eq!(prepared.server_sync.servers.restarted, ["fx"]);
    (prepared.installed)();
    let state =
        Arc::try_unwrap(fixture.state).unwrap_or_else(|_| panic!("only the test holds the state"));
    let (_reload, late) = state.preparers(Vec::new());
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
    let names: Vec<String> = prepared
        .tools
        .iter()
        .map(|(_, tool)| tool.definition().name)
        .collect();
    assert!(
        names.contains(&"mcp__fx__plus".to_owned()),
        "the late set holds the replacement's tools: {names:?}"
    );
    assert!(
        !names.contains(&"mcp__fx__echo".to_owned()),
        "the startup tools stay gone: {names:?}"
    );
    let tool = prepared
        .tools
        .iter()
        .find(|(_, tool)| tool.definition().name == "mcp__fx__plus")
        .expect("the replacement tool")
        .1
        .clone();
    let answered = tool.run(
        &Default::default(),
        &fakes::CancelToken::new(),
        &fakes::Recorder::default(),
    );
    assert!(answered.error.is_none(), "{:?}", answered.error);
}

#[test]
fn a_failed_start_of_a_not_started_server_removes_its_slot_and_its_resources() {
    // A lazy `NotStarted` slot whose start fails at reload leaves no
    // slot behind, so `mcp_resources` does not count it either.
    let mut fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    let fx_entry = fixture.servers[0].entry(json!({}));
    let gx = FakeServer::new(&fixture.rig, "gx", &json!([{"name": "echo"}]));
    gx.answer("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    // Prime gx's cache, then relaunch without it, so the cache stays
    // on disk with no slot.
    fixture.rewrite(json!({"fx": fx_entry.clone(), "gx": gx.entry(json!({}))}));
    fixture.relaunch();
    fixture.flip_resources("gx", true);
    fixture.rewrite(json!({"fx": fx_entry.clone()}));
    fixture.relaunch();
    assert!(!fixture.state.servers.has_slot("gx"));
    // Re-adding gx goes lazy off the cache: a `NotStarted` slot with
    // no server behind it.
    fixture.rewrite(json!({"fx": fx_entry.clone(), "gx": gx.entry(json!({}))}));
    fixture.relaunch();
    assert!(
        fixture.state.servers.has_slot("gx"),
        "the cached slot stays"
    );
    assert!(
        !fixture.state.servers.running("gx"),
        "the relaunched slot never started"
    );
    // The start fails at reload.
    std::fs::write(gx.dir.join("fail-start"), "").unwrap();
    let prepared = fixture.prepare();
    // The final relaunch went lazy off both caches, so both start
    // fresh here; only gx fails.
    assert_eq!(prepared.server_sync.servers.started, ["fx", "gx"]);
    assert_eq!(prepared.server_sync.server_failures.len(), 1);
    assert_eq!(prepared.server_sync.server_failures[0].server, "gx");
    assert!(
        prepared
            .tools
            .iter()
            .all(|(_, tool)| tool.definition().name != mcp::RESOURCES_TOOL),
        "no slot offers resources, so none is declared"
    );
    (prepared.installed)();
    // The install drops the stale slot, so it never serves again and
    // the hub's resource read stops counting it.
    assert!(
        !fixture.state.servers.has_slot("gx"),
        "the failed slot is gone"
    );
    assert!(
        !fixture
            .declared_names()
            .contains(&mcp::RESOURCES_TOOL.to_owned())
    );
}

#[test]
fn a_prepare_timeout_dump_shows_each_fake_servers_stderr_and_requests_log() {
    // The timeout branch itself needs the race, but its formatting does
    // not: fabricated directories drive the dump deterministically.
    let rig = Rig::new("fiber-reload-dump");
    let fx = rig.root.path().join("fx");
    std::fs::create_dir_all(&fx).unwrap();
    std::fs::write(fx.join("tools.json"), r#"[{"name":"echo"}]"#).unwrap();
    std::fs::write(fx.join("stderr.log"), "boom\n").unwrap();
    std::fs::write(fx.join("requests.log"), "{\"jsonrpc\":\"2.0\"}\n").unwrap();
    let gx = rig.root.path().join("gx");
    std::fs::create_dir_all(&gx).unwrap();
    std::fs::write(gx.join("tools.json"), r#"[{"name":"echo"}]"#).unwrap();
    std::fs::write(gx.join("fail-start"), "").unwrap();
    std::fs::write(gx.join("stderr.log"), "").unwrap();
    // No `tools.json`: not a server, so the dump skips it.
    let plain = rig.root.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("config.json"), "{}").unwrap();
    // Not a directory at all.
    std::fs::write(rig.root.path().join("stray.txt"), "x").unwrap();

    let dump = dump_fake_servers(rig.root.path());
    assert!(
        dump.contains("fake MCP server fx:"),
        "names every server: {dump}"
    );
    assert!(dump.contains("boom"), "shows the stderr: {dump}");
    assert!(dump.contains("jsonrpc"), "shows the requests: {dump}");
    assert!(
        dump.contains("fake MCP server gx:"),
        "names the failed server too: {dump}"
    );
    assert!(
        dump.contains("requests.log: <absent>"),
        "a start that never read has no requests: {dump}"
    );
    assert!(
        dump.contains("stderr.log: <empty>"),
        "an empty stderr reads empty, not absent: {dump}"
    );
    assert!(!dump.contains("plain"), "skips a non-server: {dump}");
    assert!(!dump.contains("stray"), "skips a file: {dump}");
    // `fakes::within` never joins its work thread, so when the wait
    // expired the read may hold lines the prepare wrote later: the
    // headline must say so, without claiming every failure is a timeout.
    assert!(
        dump.contains("may include later traffic"),
        "the dump names its conditional later read: {dump}"
    );
    assert!(
        dump.contains("read in the failure path"),
        "the dump names when it was read: {dump}"
    );
}

#[test]
fn describe_prepare_progress_names_the_last_checkpoint_and_its_next_step() {
    // The empty arm flips when the first checkpoint is renamed: a
    // failure before anything records must still name where the
    // prepare would have gone.
    let line = describe_prepare_progress(&[]);
    assert!(
        line.contains("no checkpoint reached"),
        "silence names itself: {line}"
    );
    assert!(
        line.contains("next expected step is config_loaded"),
        "silence names the first step: {line}"
    );
    // Selection flips when the last entry is ignored: the line must
    // name the last reached checkpoint, not an earlier one, and the
    // unfinished step after it.
    let line = describe_prepare_progress(&["config_loaded", "repository_ready", "model_resolved"]);
    assert!(
        line.contains("last reached checkpoint model_resolved"),
        "names the last reached step: {line}"
    );
    assert!(
        !line.contains("last reached checkpoint config_loaded"),
        "an earlier step is not the last: {line}"
    );
    assert!(
        line.contains("next expected step is inputs_built"),
        "names the unfinished step: {line}"
    );
    assert!(
        line.contains("may include later checkpoints"),
        "the read names its conditional later timing: {line}"
    );
    // The final arm flips when the last checkpoint gains a successor:
    // a completed record says so instead of naming a next step.
    let line = describe_prepare_progress(PREPARE_CHECKPOINTS);
    assert!(
        line.contains("last reached checkpoint prepared"),
        "a full record ends at prepared: {line}"
    );
    assert!(
        line.contains("final checkpoint"),
        "no step follows the last one: {line}"
    );
}

#[test]
fn a_successful_prepare_records_every_checkpoint_in_order() {
    // The hook appends only, with no pause and no branch, so a prepare
    // that succeeds holds the whole order, ending at `prepared`.
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    let prepared = fixture.prepare();
    assert!(
        !prepared.tools.is_empty(),
        "the prepare ran to the end: {:?}",
        fixture.state.prepare_progress()
    );
    assert_eq!(
        fixture.state.prepare_progress().as_slice(),
        PREPARE_CHECKPOINTS,
        "every stage boundary recorded in order"
    );
    let line = fixture.state.prepare_progress_line();
    assert!(
        line.contains("last reached checkpoint prepared"),
        "the failure line would name the end: {line}"
    );
}
// debt: one small helper test for the three panic payload forms; upgrade
// trigger: remove the helper or cover these forms through another focused
// test.
#[test]
fn a_caught_panic_reports_its_text() {
    let str_payload: Box<dyn std::any::Any + Send> = Box::new("bang");
    assert_eq!(panic_text(&str_payload), "bang");
    let string_payload: Box<dyn std::any::Any + Send> = Box::new(String::from("boom"));
    assert_eq!(panic_text(&string_payload), "boom");
    let other_payload: Box<dyn std::any::Any + Send> = Box::new(42);
    assert_eq!(panic_text(&other_payload), "a non-string panic");
}

#[test]
fn a_failed_prepare_reports_the_fake_servers_logs() {
    // An invalid config fails the preparation at once, driving the
    // failure branch deterministically: the panic names the wait and
    // carries the live servers' logs.
    let fixture = Fixture::with_servers(&[("fx", json!([{"name": "echo"}]))]);
    std::fs::write(fixture.rig.home.join("config.json"), "{").unwrap();
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fixture.prepare()));
    let payload = match failure {
        Ok(_) => panic!("an invalid config fails the prepare"),
        Err(payload) => payload,
    };
    let text = panic_text(&payload);
    assert!(
        text.contains("reload prepared"),
        "names the expired wait: {text}"
    );
    assert!(
        text.contains("fake MCP server fx:"),
        "carries the server dump: {text}"
    );
    assert!(
        text.contains("requests.log:"),
        "carries the requests log: {text}"
    );
    assert!(
        text.contains("prepare progress: no checkpoint reached"),
        "an invalid config fails before the first checkpoint: {text}"
    );
    assert!(
        text.contains("next expected step is config_loaded"),
        "the failure names the unfinished step: {text}"
    );
}

#[test]
fn a_prepare_timeout_dump_with_no_servers_says_so() {
    let rig = Rig::new("fiber-reload-dump-empty");
    let dump = dump_fake_servers(rig.root.path().join("home").as_path());
    assert!(
        dump.contains("none"),
        "a missing root holds no servers: {dump}"
    );
}

#[test]
fn a_fake_server_entry_captures_stderr_in_its_directory() {
    let rig = Rig::new("fiber-reload-entry");
    let fake = FakeServer::new(&rig, "fx", &json!([{"name": "echo"}]));
    let entry = fake.entry(json!({}));
    assert_eq!(entry["command"], json!("/bin/sh"));
    let args = entry["args"].as_array().unwrap();
    // The wrapper execs the fixture on the directory and redirects the
    // server's stderr into it: the paths ride as positional parameters,
    // never interpolated, so no path needs quoting.
    assert_eq!(args[0], json!("-c"));
    assert!(
        args[1].as_str().unwrap().starts_with("exec \"$0\" \"$1\""),
        "the wrapper execs its parameters: {args:?}"
    );
    assert!(
        args[1].as_str().unwrap().contains("stderr.log"),
        "the wrapper redirects stderr: {args:?}"
    );
    assert_eq!(
        args[2],
        json!(fakes::mcp_fixture().display().to_string()),
        "the fixture rides as $0: {args:?}"
    );
    assert_eq!(
        args[3],
        json!(fake.dir.display().to_string()),
        "the directory rides as $1: {args:?}"
    );
}
