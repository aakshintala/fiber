use super::*;
use crate::test_support::Rig;
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

struct Fixture {
    rig: Rig,
    state: Arc<State>,
    replacements: Arc<Mutex<Vec<doors::Replacement>>>,
    declared: Arc<Mutex<std::collections::BTreeMap<String, contract::events::ToolInfo>>>,
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
        let parts = fakes::within("startup parts", fakes::MUST_SUCCEED_WITHIN, move || {
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
            )
            .unwrap()
        });
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
        let replacements = Arc::new(Mutex::new(Vec::new()));
        let seen = replacements.clone();
        let declared = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
        let declared_now = declared.clone();
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
            attended: Arc::new(|| true),
            answerable: true,
            log,
            diag: parts.diag,
            names: Mutex::new(Vec::new()),
        });
        Self {
            rig,
            state,
            replacements,
            declared,
        }
    }

    fn prepare(&self) -> r#loop::ReloadPrepared {
        let state = self.state.clone();
        fakes::within("reload prepared", fakes::MUST_SUCCEED_WITHIN, move || {
            state
                .prepare(&r#loop::ReloadRoute {
                    model: "scripted/s.json".into(),
                    credential: None,
                })
                .unwrap()
        })
    }
}

#[test]
fn invalid_config_keeps_the_set_and_a_repaired_config_can_install() {
    let fixture = Fixture::new();
    let old = fixture.state.current.get();
    std::fs::write(fixture.rig.home.join("config.json"), "{").unwrap();
    let state = fixture.state.clone();
    let rejected = fakes::within("invalid reload", fakes::MUST_SUCCEED_WITHIN, move || {
        state
            .prepare(&r#loop::ReloadRoute {
                model: "scripted/s.json".into(),
                credential: None,
            })
            .err()
            .unwrap()
    });
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
        fakes::MUST_SUCCEED_WITHIN,
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
