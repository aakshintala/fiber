use super::*;
use crate::configure::{LoginKind, LogoutRow, Shown, WriteScope};
use crate::configure_fake::Fake;
use fakes::Deadline;
use serde_json::json;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::thread;

struct Hub {
    router: Arc<Router>,
    lines: Receiver<Value>,
    remote: Arc<Remote>,
}
impl Hub {
    fn new(local: Arc<Fake>) -> Self {
        let router = Router::new();
        let (client, hub) = UnixStream::pair().unwrap();
        router.attach(client);
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(hub).lines() {
                let Ok(line) = line else {
                    break;
                };
                if tx.send(serde_json::from_str(&line).unwrap()).is_err() {
                    break;
                }
            }
        });
        let remote = Arc::new(Remote::new(local, "/client".into(), Arc::clone(&router)));
        Self {
            router,
            lines,
            remote,
        }
    }
    fn call<T: Send + 'static>(
        &self,
        operation: &str,
        args: Value,
        answer: Value,
        call: impl FnOnce(Arc<Remote>) -> T + Send + 'static,
    ) -> T {
        let deadline = Deadline::start();
        let (tx, rx) = mpsc::channel();
        let remote = Arc::clone(&self.remote);
        thread::spawn(move || {
            tx.send(call(remote)).unwrap_or(());
        });
        let line = deadline.recv_or_fail(&self.lines, "configure request");
        assert_eq!(line["command"], "configure");
        let mut expected = args;
        expected["operation"] = json!(operation);
        assert_eq!(line["args"], expected);
        self.reply(&line, answer);
        deadline.recv_or_fail(&rx, "seam answer")
    }
    fn reply(&self, line: &Value, result: Value) {
        let response=serde_json::from_value(json!({"kind":"command_accepted","ts":1,"schema_version":1,"payload":{"command_id":line["id"],"result":result}})).unwrap();
        assert!(self.router.route(&crate::link::Line::Hub(response)));
    }
    fn reject(&self, line: &Value, code: &str, message: &str) {
        let response=serde_json::from_value(json!({"kind":"command_rejected","ts":1,"schema_version":1,"payload":{"command_id":line["id"],"code":code,"message":message}})).unwrap();
        assert!(self.router.route(&crate::link::Line::Hub(response)));
    }
}
impl Drop for Hub {
    fn drop(&mut self) {
        self.router.detach();
    }
}

fn hooks_overrides() -> Vec<String> {
    vec![
        "first=value with spaces".into(),
        "first=later".into(),
        "second=\"quoted\"".into(),
    ]
}

fn hooks_call(remote: &dyn Configure, operation: &str) -> Result<Vec<String>, ConfigureError> {
    match operation {
        "blocking_hooks" => remote.blocking_hooks(Path::new("/remote/ws"), &hooks_overrides()),
        "install_hooks" => remote
            .install_hooks(fakes::clock::FakeClock::new())
            .map(|()| vec![]),
        _ => panic!("unknown test operation"),
    }
}

#[test]
fn hooks_fake_records_exact_queries_and_canned_results() {
    let fake = Fake::new(vec![]);
    assert_eq!(hooks_call(&fake, "blocking_hooks"), Ok(vec![]));
    *fake.blocking_hooks_answer.lock().unwrap() = Ok(vec!["guard".into()]);
    assert_eq!(
        hooks_call(&fake, "blocking_hooks"),
        Ok(vec!["guard".into()])
    );
    let error = ConfigureError {
        code: ErrorCode::IoFailed,
        message: "original".into(),
    };
    *fake.blocking_hooks_answer.lock().unwrap() = Err(error.clone());
    assert_eq!(hooks_call(&fake, "blocking_hooks"), Err(error.clone()));
    assert_eq!(
        *fake.blocking_hooks_queries.lock().unwrap(),
        vec![(PathBuf::from("/remote/ws"), hooks_overrides()); 3]
    );
    assert_eq!(hooks_call(&fake, "install_hooks"), Ok(vec![]));
    *fake.hooks_install_answer.lock().unwrap() = Err(error.clone());
    assert_eq!(hooks_call(&fake, "install_hooks"), Err(error));
    assert_eq!(*fake.hooks_installs.lock().unwrap(), 2);
}

#[test]
fn hooks_remote_waits_for_a_worker_link_and_uses_only_the_hub() {
    for operation in ["blocking_hooks", "install_hooks"] {
        let local = Arc::new(Fake::new(vec![]));
        let mut hub = Hub::new(local.clone());
        hub.router.detach();
        let deadline = Deadline::start();
        let (tx, rx) = mpsc::channel();
        let remote = hub.remote.clone();
        thread::spawn(move || {
            tx.send(hooks_call(remote.as_ref(), operation))
                .unwrap_or(());
        });
        while hub.router.link_waiters() != 1 {
            assert!(
                !deadline.left().is_zero(),
                "{operation} did not wait for a worker link"
            );
            assert!(
                rx.try_recv().is_err(),
                "{operation} returned instead of waiting"
            );
            thread::yield_now();
        }
        let (client, stream) = UnixStream::pair().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else {
                    break;
                };
                if tx.send(serde_json::from_str(&line).unwrap()).is_err() {
                    break;
                }
            }
        });
        hub.router.attach(client);
        hub.lines = lines;
        let line = deadline.recv_or_fail(&hub.lines, "hooks operation after attach");
        assert_eq!(line["command"], "configure");
        let (args, answer, expected) = match operation {
            "blocking_hooks" => (
                json!({"operation":operation,"workspace":"/remote/ws","overrides":hooks_overrides()}),
                // Canned hub answer in sorted wire order (`docs/invocation.md:1160`).
                json!({"names":["a","z"]}),
                vec!["a".to_owned(), "z".to_owned()],
            ),
            "install_hooks" => (json!({"operation":operation}), json!({}), vec![]),
            _ => unreachable!(),
        };
        assert_eq!(line["args"], args);
        hub.reply(&line, answer);
        assert_eq!(
            deadline.recv_or_fail(&rx, "hooks worker answer"),
            Ok(expected)
        );
        assert!(local.blocking_hooks_queries.lock().unwrap().is_empty());
        assert_eq!(*local.hooks_installs.lock().unwrap(), 0);
        assert!(local.reads.lock().unwrap().is_empty());
        assert!(local.installs.lock().unwrap().is_empty());
    }
}

#[test]
fn hooks_remote_preserves_rejections_and_inflight_connection_loss() {
    for operation in ["blocking_hooks", "install_hooks"] {
        for reject in [true, false] {
            let hub = Hub::new(Arc::new(Fake::new(vec![])));
            let deadline = Deadline::start();
            let (tx, rx) = mpsc::channel();
            let remote = hub.remote.clone();
            thread::spawn(move || {
                tx.send(hooks_call(remote.as_ref(), operation))
                    .unwrap_or(());
            });
            let line = deadline.recv_or_fail(&hub.lines, "inflight hooks request");
            let expected = if reject {
                hub.reject(&line, "fetch_failed", "unchanged failure");
                ConfigureError {
                    code: ErrorCode::FetchFailed,
                    message: "unchanged failure".into(),
                }
            } else {
                hub.router.detach();
                ConfigureError {
                    code: ErrorCode::IoFailed,
                    message: "connection lost".into(),
                }
            };
            assert_eq!(deadline.recv_or_fail(&rx, "hooks rejection"), Err(expected));
        }
    }
}

#[test]
fn hooks_remote_rejects_malformed_answers() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    for answer in [
        json!({}),
        json!(null),
        json!({"names":null}),
        json!({"names":1}),
        json!({"names":["a",null]}),
    ] {
        let error = hub
            .call(
                "blocking_hooks",
                json!({"workspace":"/remote/ws","overrides":hooks_overrides()}),
                answer,
                |remote| hooks_call(remote.as_ref(), "blocking_hooks"),
            )
            .unwrap_err();
        assert_eq!(error, decode::io_error("blocking_hooks"));
    }
    for answer in [json!(null), json!([]), json!({"names":[]})] {
        let error = hub
            .call("install_hooks", json!({}), answer, |remote| {
                hooks_call(remote.as_ref(), "install_hooks")
            })
            .unwrap_err();
        assert_eq!(error, decode::io_error("install_hooks"));
    }
}

#[test]
fn provider_install_uses_the_worker_wire_shape_and_decodes_the_outcome() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    let answer = hub.call("install_provider", json!({"workspace":"/remote/ws","provider":"openai"}), json!({"installed":["x"],"summary":["s"],"login":[{"provider":"openai"},{"provider":"codex","error":"e"}]}), |r| r.install_provider(Path::new("/remote/ws"), "openai", fakes::clock::FakeClock::new())).unwrap();
    assert_eq!(
        answer,
        crate::ProviderInstall {
            installed: vec!["x".into()],
            summary: vec!["s".into()],
            login: vec![
                crate::LoginNeed {
                    provider: "openai".into(),
                    error: None
                },
                crate::LoginNeed {
                    provider: "codex".into(),
                    error: Some("e".into())
                }
            ]
        }
    );
    let empty = hub
        .call(
            "install_provider",
            json!({"workspace":"/remote/ws","provider":"openai"}),
            json!({"installed":[],"summary":[],"login":[]}),
            |r| {
                r.install_provider(
                    Path::new("/remote/ws"),
                    "openai",
                    fakes::clock::FakeClock::new(),
                )
            },
        )
        .unwrap();
    assert_eq!(empty, crate::ProviderInstall::default());
}

#[test]
fn provider_install_refuses_malformed_outcomes() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    for answer in [
        json!({"installed":[],"summary":[]}),
        json!({"installed":[],"summary":[],"login":["openai"]}),
        json!({"installed":"x","summary":[],"login":[]}),
        json!({"installed":[1],"summary":[],"login":[]}),
        json!({"installed":[],"summary":"s","login":[]}),
        json!({"installed":[],"summary":[1],"login":[]}),
        json!({"installed":[],"summary":[],"login":"openai"}),
        json!({"installed":[],"summary":[],"login":[{}]}),
        json!({"installed":[],"summary":[],"login":[{"provider":1}]}),
        json!({"installed":[],"summary":[],"login":[{"provider":"openai","error":null}]}),
        json!({"installed":[],"summary":[],"login":[{"provider":"openai","error":1}]}),
    ] {
        let error = hub
            .call(
                "install_provider",
                json!({"workspace":"/remote/ws","provider":"openai"}),
                answer,
                |r| {
                    r.install_provider(
                        Path::new("/remote/ws"),
                        "openai",
                        fakes::clock::FakeClock::new(),
                    )
                },
            )
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::IoFailed);
        assert_eq!(
            error.message,
            "The install_provider answer could not be read."
        );
    }
}

#[test]
fn provider_install_preserves_rejection_code_and_message() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    let deadline = Deadline::start();
    let (tx, rx) = mpsc::channel();
    let remote = Arc::clone(&hub.remote);
    thread::spawn(move || {
        tx.send(remote.install_provider(
            Path::new("/remote/ws"),
            "openai",
            fakes::clock::FakeClock::new(),
        ))
        .unwrap_or(());
    });
    let line = deadline.recv_or_fail(&hub.lines, "provider install request");
    assert_eq!(
        line["args"],
        json!({"operation":"install_provider","workspace":"/remote/ws","provider":"openai"})
    );
    hub.reject(&line, "fetch_failed", "source unavailable");
    let error = deadline
        .recv_or_fail(&rx, "provider install rejection")
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::FetchFailed);
    assert_eq!(error.message, "source unavailable");
}

/// The offer's pending state reads the hub machine's home, never the
/// terminal's: the `configure` operation answers from the hub's own
/// Fiber home, where its `import` writes the receipts.
#[test]
fn import_pending_forwards_the_hubs_answer() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    assert!(
        hub.call("import_pending", json!({}), json!({"pending": true}), |r| {
            r.import_pending_hub()
        })
        .unwrap()
    );
    assert!(
        !hub.call(
            "import_pending",
            json!({}),
            json!({"pending": false}),
            |r| r.import_pending_hub()
        )
        .unwrap()
    );
}

/// A refused or unreadable pending answer fails closed: the offer
/// stays down rather than showing on a guess about the hub's home.
#[test]
fn import_pending_without_a_pending_answer_stays_down() {
    for script in [
        Err(("invalid_arguments", "no such operation")),
        Ok(json!({})),
        Ok(json!({"pending": "soon"})),
    ] {
        let hub = Hub::new(Arc::new(Fake::new(vec![])));
        let deadline = Deadline::start();
        let (tx, rx) = mpsc::channel();
        let remote = Arc::clone(&hub.remote);
        thread::spawn(move || {
            tx.send(remote.import_pending()).unwrap_or(());
        });
        let line = deadline.recv_or_fail(&hub.lines, "import pending request");
        assert_eq!(line["command"], "configure");
        assert_eq!(line["args"], json!({"operation": "import_pending"}));
        match script {
            Err((code, message)) => hub.reject(&line, code, message),
            Ok(answer) => hub.reply(&line, answer),
        }
        assert!(!deadline.recv_or_fail(&rx, "seam answer"));
    }
}

#[test]
fn each_configure_operation_uses_the_hub_wire_shape() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    let w = Path::new("/remote/ws");
    let saved = hub.call(
        "set",
        json!({"workspace":w,"layer":"repository","key":"model","text":"\"a/m\""}),
        json!({"file":"/hub/config.json","warnings":["warning"]}),
        move |r| r.set(w, Layer::Repository, "model", "\"a/m\""),
    );
    assert_eq!(
        saved.unwrap(),
        Saved {
            file: "/hub/config.json".into(),
            warnings: vec!["warning".into()]
        }
    );
    let rules=hub.call("rules",json!({"workspace":w}),json!({"global":{"file":"/hub/rules","rows":[]},"project":{"file":"/hub/project/rules","error":"broken"}}),move |r|r.rules(w)).unwrap();
    assert_eq!(rules.1.rows, Err("broken".into()));
    assert_eq!(
        hub.call(
            "revoke",
            json!({"workspace":w,"scope":"project","line":3,"text":"deny"}),
            json!({"revoked":"stale"}),
            move |r| r.revoke(w, RulesScope::Project, 3, "deny")
        )
        .unwrap(),
        Revoked::Stale
    );
    assert_eq!(
        hub.call(
            "login_targets",
            json!({}),
            json!({"targets":[{"name":"a","kind":"browser"}]}),
            |r| r.login_targets()
        )
        .unwrap()[0]
            .kind,
        LoginKind::Browser
    );
    for label in [None, Some("work")] {
        let mut args = json!({"name":"a","key":"secret"});
        if let Some(label) = label {
            args["label"] = json!(label);
        }
        assert_eq!(
            hub.call(
                "store_key",
                args,
                json!({"path":"credentials/a/default","replaced":false}),
                move |r| r.store_key("a", label, contract::Secret::new("secret".into()))
            )
            .unwrap()
            .path,
            "credentials/a/default"
        );
    }
    assert_eq!(
        hub.call(
            "reviewer_models",
            json!({"name":"a"}),
            json!({"models":["m"],"notices":[]}),
            |r| r.reviewer_models("a", fakes::clock::FakeClock::new())
        )
        .unwrap()
        .models,
        ["m"]
    );
    hub.call(
        "set_reviewer",
        json!({"name":"a","id":"m"}),
        json!(null),
        |r| r.set_reviewer("a", "m"),
    )
    .unwrap();
    assert_eq!(
        hub.call(
            "gaps",
            json!({"name":"a"}),
            json!({"gaps":[{"name":"workspace","env":"ACME_HOST"},{"name":"other","env":null}]}),
            |r| r.gaps("a")
        )
        .unwrap(),
        [
            crate::configure::HostGap {
                name: "workspace".to_owned(),
                env: Some("ACME_HOST".to_owned()),
            },
            crate::configure::HostGap {
                name: "other".to_owned(),
                env: None,
            },
        ]
    );
    assert_eq!(
        hub.call(
            "save_host",
            json!({"name":"a","key":"workspace","value":"h.example"}),
            json!({"masked":"/hub/projects/p/config/a.json"}),
            |r| r.save_host("a", "workspace", "h.example")
        )
        .unwrap(),
        crate::configure::HostSaved {
            masked: Some(PathBuf::from("/hub/projects/p/config/a.json")),
        }
    );
    let read = hub.call(
        "reviewer_notes",
        json!({"workspace":w}),
        json!({"global":{"file":"/hub/config.json","text":"global notes","version":"v1"},"project":{"file":"/hub/projects/p/config.json","text":"","version":"absent"}}),
        move |r| r.reviewer_notes(w),
    ).unwrap();
    assert_eq!(read.global.text, "global notes");
    assert_eq!(read.global.version, "v1");
    assert_eq!(read.project.text, "");
    assert_eq!(read.project.version, "absent");
    let write = crate::configure::NotesWrite {
        version: "v1".into(),
        text: "new global".into(),
    };
    let saved = hub
        .call(
            "save_reviewer_notes",
            json!({"workspace":w,"global":{"version":"v1","text":"new global"}}),
            json!({"saved":"written","files":[{"file":"/hub/config.json","saved":true}]}),
            move |r| r.save_reviewer_notes(w, Some(write), None),
        )
        .unwrap();
    assert_eq!(
        saved,
        crate::configure::NotesSaved::Written(vec![crate::configure::FileSaved {
            file: "/hub/config.json".into(),
            outcome: crate::configure::FileOutcome::Saved,
        }])
    );
    let stale = hub
        .call(
            "save_reviewer_notes",
            json!({"workspace":w,"project":{"version":"old","text":"new project"}}),
            json!({"saved":"stale"}),
            move |r| {
                r.save_reviewer_notes(
                    w,
                    None,
                    Some(crate::configure::NotesWrite {
                        version: "old".into(),
                        text: "new project".into(),
                    }),
                )
            },
        )
        .unwrap();
    assert_eq!(stale, crate::configure::NotesSaved::Stale);
    assert!(
        hub.call(
            "tool_switches",
            json!({"workspace":w}),
            json!({"groups":[]}),
            move |r| r.tool_switches(w)
        )
        .unwrap()
        .is_empty()
    );
    hub.call("switch_tool",json!({"workspace":w,"group":{"extension":"a/e"},"tool":"run","scope":"everywhere","on":false}),json!(null),move |r|r.switch_tool(w,&ToolGroup::Extension("a/e".into()),"run",SwitchScope::Everywhere,false)).unwrap();
    assert_eq!(
        hub.call(
            "skills_disabled",
            json!({"workspace":w}),
            json!({"project":["a"],"everywhere":[]}),
            move |r| r.skills_disabled(w)
        )
        .unwrap()
        .project,
        ["a"]
    );
    hub.call(
        "switch_skill",
        json!({"workspace":w,"name":"a","scope":"project","on":true}),
        json!(null),
        move |r| r.switch_skill(w, "a", SwitchScope::Project, true),
    )
    .unwrap();
    assert_eq!(
        hub.call(
            "skill_text",
            json!({"path":"/hub/SKILL.md"}),
            json!({"text":"skill\n"}),
            |r| r.skill_text(Path::new("/hub/SKILL.md"))
        )
        .unwrap(),
        "skill\n"
    );
}

#[test]
fn logout_rows_returns_the_hubs_decoded_rows() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    let rows = hub
        .call(
            "logout_rows",
            json!({}),
            json!({"rows":[
                {"row":"provider","name":"acme","label":"work","default":true,"deletable":true,"siblings":["beta"]},
                {"row":"secret","name":"acme.api_key","deletable":true},
            ]}),
            |r| r.logout_rows(),
        )
        .unwrap();
    assert_eq!(
        rows,
        [
            LogoutRow::Provider {
                name: "acme".into(),
                label: "work".into(),
                default: true,
                deletable: true,
                origin: None,
                siblings: vec!["beta".into()],
            },
            LogoutRow::Secret {
                name: "acme.api_key".into(),
                deletable: true,
            },
        ]
    );
}

#[test]
fn delete_stored_returns_the_hubs_path() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    assert_eq!(
        hub.call(
            "delete_stored",
            json!({"name":"acme","label":"work"}),
            json!({"path":"credentials/acme/work"}),
            |r| r.delete_stored("acme", Some("work".to_owned())),
        )
        .unwrap(),
        "credentials/acme/work"
    );
}

#[test]
fn ownership_and_settings_merge_keep_the_clients_rows_on_the_client() {
    for key in ["keys.quit", "tui.hover", "\"tui\".x", "\"tui\"", "keys"] {
        assert!(client_owned(key), "{key}");
    }
    for key in ["model", "tuix.y", "models.\"tui/x\".thinking", "", "\"tui"] {
        assert!(!client_owned(key), "{key}");
    }
    let row = |key: &str| SettingRow {
        key: key.into(),
        value: Shown::Unset,
        layer: "default".into(),
        file: None,
        scope: WriteScope::GlobalOnly,
    };
    let mut local = Fake::new(vec![row("keys.quit"), row("model"), row("tui.hover")]);
    local.themes = vec!["client-night".into(), "client-day".into()];
    let local = Arc::new(local);
    let hub = Hub::new(Arc::clone(&local));
    let rows=hub.call("settings",json!({"workspace":"/remote/ws"}),json!({"rows":[{"key":"model","layer":"global","shown":"value","value":"\"hub/m\"","scope":"global_only"},{"key":"tui.hover","layer":"global","shown":"value","value":"false","scope":"global_only"},{"key":"extra","layer":"default","shown":"unset","scope":"global_only"}]}),|r|r.settings(Path::new("/remote/ws"))).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
        ["keys.quit", "model", "tui.hover", "extra"]
    );
    assert_eq!(rows[1].value, Shown::Value("\"hub/m\"".into()));
    assert_eq!(rows[2].value, Shown::Unset);
    assert_eq!(*support::lock(&local.reads), [PathBuf::from("/client")]);
    for key in ["tui.hover", "keys.quit"] {
        hub.remote
            .set(Path::new("/remote/ws"), Layer::Global, key, "true")
            .unwrap();
    }
    assert_eq!(support::lock(&local.writes).len(), 2);
    assert_eq!(hub.remote.themes(Path::new("/remote/ws")), local.themes);
    assert!(matches!(
        hub.remote.theme(Path::new("/remote/ws"), "light"),
        crate::ThemeSetting::Light
    ));
    let edits = vec![KeyEdit {
        id: "quit".into(),
        keys: Some(vec!["ctrl+q".into()]),
    }];
    hub.remote.save_keys(&edits).unwrap();
    assert_eq!(*support::lock(&local.keys_saved), [edits]);
    *support::lock(&local.refuse) = Some("client keys are read-only".into());
    let error = hub.remote.save_keys(&[]).unwrap_err();
    assert_eq!(error.code, ErrorCode::Usage);
    assert_eq!(error.message, "client keys are read-only");
    assert!(hub.lines.try_recv().is_err());
}

#[test]
fn no_workspace_reads_use_the_global_parent_and_cache_per_connection() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    let deadline = Deadline::start();
    for first in [true, false] {
        let (tx, rx) = mpsc::channel();
        let remote = Arc::clone(&hub.remote);
        thread::spawn(move || {
            let outcome = remote
                .global_parent()
                .and_then(|parent| remote.settings(&parent));
            tx.send(outcome).unwrap_or(());
        });
        if first {
            let line = deadline.recv_or_fail(&hub.lines, "global file");
            assert_eq!(line["args"], json!({"operation":"global_file"}));
            hub.reply(&line, json!({"file":"/hub/.fiber/config.json"}));
        }
        let line = deadline.recv_or_fail(&hub.lines, "settings at global parent");
        assert_eq!(
            line["args"],
            json!({"operation":"settings","workspace":"/hub/.fiber"})
        );
        hub.reply(&line, json!({"rows":[]}));
        assert!(deadline.recv_or_fail(&rx, "settings").unwrap().is_empty());
    }
}

#[test]
fn reconnect_invalidates_the_global_file_cache() {
    let mut hub = Hub::new(Arc::new(Fake::new(vec![])));
    assert_eq!(
        hub.call(
            "global_file",
            json!({}),
            json!({"file":"/first/config.json"}),
            |r| r.global_file()
        ),
        PathBuf::from("/first/config.json")
    );
    assert_eq!(
        hub.remote.global_file(),
        PathBuf::from("/first/config.json")
    );
    assert!(hub.lines.try_recv().is_err());
    hub.router.detach();
    let (client, stream) = UnixStream::pair().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else {
                break;
            };
            if tx.send(serde_json::from_str(&line).unwrap()).is_err() {
                break;
            }
        }
    });
    hub.router.attach(client);
    hub.lines = rx;
    assert_eq!(
        hub.call(
            "global_file",
            json!({}),
            json!({"file":"/second/config.json"}),
            |r| r.global_file()
        ),
        PathBuf::from("/second/config.json")
    );
}

#[test]
fn reviewer_discovery_waits_for_the_link_before_sending() {
    let mut hub = Hub::new(Arc::new(Fake::new(vec![])));
    hub.router.detach();
    let d = Deadline::start();
    let (tx, rx) = mpsc::channel();
    let remote = Arc::clone(&hub.remote);
    thread::spawn(move || {
        tx.send(remote.reviewer_models("acme", fakes::clock::FakeClock::new()))
            .unwrap();
    });
    while hub.router.link_waiters() != 1 {
        assert!(!d.left().is_zero(), "reviewer did not wait for the link");
        thread::yield_now();
    }
    assert!(hub.lines.try_recv().is_err());
    let (client, stream) = UnixStream::pair().unwrap();
    let (tx, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else {
                break;
            };
            if tx.send(serde_json::from_str(&line).unwrap()).is_err() {
                break;
            }
        }
    });
    hub.router.attach(client);
    hub.lines = lines;
    let line = d.recv_or_fail(&hub.lines, "reviewer after attach");
    assert_eq!(
        line["args"],
        json!({"operation":"reviewer_models","name":"acme"})
    );
    hub.reply(&line, json!({"models":["m"],"selected":"m","notices":[]}));
    assert_eq!(
        d.recv_or_fail(&rx, "reviewer answer").unwrap().selected,
        Some("m".into())
    );
}

#[test]
fn hub_rejections_keep_their_code_and_message_and_global_failure_is_empty() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    let d = Deadline::start();
    for global in [false, true] {
        let (tx, rx) = mpsc::channel();
        let remote = Arc::clone(&hub.remote);
        thread::spawn(move || {
            let result = if global {
                Ok(remote.global_file())
            } else {
                remote.login_targets().map(|_| PathBuf::new())
            };
            tx.send(result).unwrap();
        });
        let line = d.recv_or_fail(&hub.lines, "rejected request");
        let response=serde_json::from_value(json!({"kind":"command_rejected","ts":1,"schema_version":1,"payload":{"command_id":line["id"],"code":"stale_request","message":"hub message"}})).unwrap();
        assert!(hub.router.route(&crate::link::Line::Hub(response)));
        let result = d.recv_or_fail(&rx, "rejected answer");
        if global {
            assert_eq!(result.unwrap(), PathBuf::new());
        } else {
            assert_eq!(
                result.unwrap_err(),
                ConfigureError {
                    code: ErrorCode::StaleRequest,
                    message: "hub message".into()
                }
            );
        }
    }
}

#[test]
fn browser_login_is_a_remote_device_login_through_the_seam() {
    struct Show(mpsc::Sender<(String, String)>);
    impl LoginShow for Show {
        fn open(&self, _: &str) {
            panic!("a remote login may not open the browser");
        }
        fn show(&self, url: &str, code: &str) {
            self.0.send((url.into(), code.into())).unwrap();
        }
    }
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    let d = Deadline::start();
    let (tx, shown) = mpsc::channel();
    let login =
        hub.remote
            .browser_login("acme", Arc::new(Show(tx)), fakes::clock::FakeClock::new());
    assert!(hub.lines.try_recv().is_err());
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(login.run()).unwrap();
    });
    let start = d.recv_or_fail(&hub.lines, "device start through Configure");
    assert_eq!(start["command"], "device_login");
    assert_eq!(start["args"], json!({"name":"acme"}));
    hub.reply(&start, json!({"url":"url","code":"code"}));
    assert_eq!(
        d.recv_or_fail(&shown, "device code through Configure"),
        ("url".into(), "code".into())
    );
    let result = d.recv_or_fail(&hub.lines, "device result through Configure");
    assert_eq!(result["command"], "device_login_result");
    hub.reply(&result, json!({"path":"hub credential"}));
    assert_eq!(
        d.recv_or_fail(&rx, "device stored through Configure")
            .unwrap(),
        Stored {
            path: "hub credential".into(),
            replaced: false
        }
    );
}

#[test]
fn installation_preserves_local_seams_and_uses_the_clients_directory() {
    let router = Router::new();
    let local: Arc<dyn Configure> = Arc::new(Fake::new(vec![]));
    let installed = seams(
        &crate::LaunchLocation::Remote { hub: "box".into() },
        Some(Arc::clone(&local)),
        None,
        &router,
    );
    let remote = installed.remote.unwrap();
    assert_eq!(remote.client_dir, std::env::current_dir().unwrap());
    assert!(installed.seam.is_some());
    assert!(Arc::ptr_eq(&remote.local, &local));
    assert!(installed.models.is_some());
    let missing = seams(
        &crate::LaunchLocation::Remote { hub: "box".into() },
        None,
        None,
        &router,
    );
    assert!(missing.remote.is_none());
    assert!(missing.seam.is_none());
    assert!(missing.models.is_none());
    let installed = seams(
        &crate::LaunchLocation::default(),
        Some(Arc::clone(&local)),
        None,
        &router,
    );
    assert!(installed.remote.is_none());
    assert!(Arc::ptr_eq(&installed.seam.unwrap(), &local));
    assert!(installed.models.is_none());
}

#[test]
fn editor_copies_are_private_exact_and_removed_on_drop() {
    use std::os::unix::fs::PermissionsExt;
    let parent = fakes::TempDir::new("remote-copy");
    for text in ["", "{}", "{}\n", "a\r\nb\n\n"] {
        let copy = RemoteCopy::create(
            parent.path(),
            Path::new("/hub/config.json"),
            text,
            "v1".into(),
        )
        .unwrap();
        assert_eq!(std::fs::read(&copy.temp).unwrap(), text.as_bytes());
        assert_eq!(
            std::fs::metadata(&copy.dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(copy.temp.file_name().unwrap(), "config.json");
        assert_eq!(copy.hub, PathBuf::from("/hub/config.json"));
        assert_eq!(copy.version, "v1");
        let dir = copy.dir.clone();
        drop(copy);
        assert!(!dir.exists());
    }
}

#[test]
fn ordinary_copy_cleanup_failure_is_best_effort() {
    use std::os::unix::fs::PermissionsExt;
    let parent = fakes::TempDir::new("remote-copy-failure");
    let copy =
        RemoteCopy::create(parent.path(), Path::new("/hub/rules"), "rules", "v".into()).unwrap();
    let dir = copy.dir.clone();
    let inner = dir.join("inner");
    std::fs::create_dir(&inner).unwrap();
    let file = inner.join("held");
    std::fs::write(&file, "held").unwrap();
    std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert_eq!(
        std::fs::remove_file(&file).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied,
        "cleanup fixture must deny removal; do not run as root"
    );
    drop(copy);
    assert!(dir.exists());
    std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn fetch_and_store_preserve_text_version_and_stale_rejections() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    let d = Deadline::start();
    let (tx, rx) = mpsc::channel();
    let remote = Arc::clone(&hub.remote);
    thread::spawn(move || {
        tx.send(remote.fetch_file(Path::new("/w"), Path::new("/hub/config.json")))
            .unwrap();
    });
    let line = d.recv_or_fail(&hub.lines, "read_config");
    assert_eq!(line["command"], "read_config");
    assert_eq!(
        line["args"],
        json!({"workspace":"/w","path":"/hub/config.json"})
    );
    hub.reply(&line, json!({"text":"{}\n","version":"v1"}));
    assert_eq!(
        d.recv_or_fail(&rx, "fetched copy").unwrap(),
        ("{}\n".into(), "v1".into())
    );
    for stale in [false, true] {
        let (tx, rx) = mpsc::channel();
        let remote = Arc::clone(&hub.remote);
        thread::spawn(move || {
            tx.send(remote.store_file(
                Path::new("/w"),
                Path::new("/hub/config.json"),
                "changed\n",
                "v1",
            ))
            .unwrap();
        });
        let line = d.recv_or_fail(&hub.lines, "write_config");
        assert_eq!(line["command"], "write_config");
        assert_eq!(
            line["args"],
            json!({"workspace":"/w","path":"/hub/config.json","text":"changed\n","version":"v1"})
        );
        if stale {
            let response=serde_json::from_value(json!({"kind":"command_rejected","ts":1,"schema_version":1,"payload":{"command_id":line["id"],"code":"stale_request","message":"file changed"}})).unwrap();
            assert!(hub.router.route(&crate::link::Line::Hub(response)));
            assert_eq!(
                d.recv_or_fail(&rx, "stale write").unwrap_err(),
                ConfigureError {
                    code: ErrorCode::StaleRequest,
                    message: "file changed".into()
                }
            );
        } else {
            hub.reply(&line, json!(null));
            d.recv_or_fail(&rx, "stored copy").unwrap();
        }
    }
}

#[test]
fn fetch_file_global_reads_hub_global_files_but_refuses_project_files() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    let d = Deadline::start();
    // A hub-global file resolves the parent, then reads there: the
    // global file answers once, and the read names the parent.
    let (tx, rx) = mpsc::channel();
    let remote = Arc::clone(&hub.remote);
    thread::spawn(move || {
        tx.send(remote.fetch_file_global(Path::new("/hub/rules")))
            .unwrap();
    });
    let line = d.recv_or_fail(&hub.lines, "global file");
    assert_eq!(line["args"], json!({"operation": "global_file"}));
    hub.reply(&line, json!({"file": "/hub/config.json"}));
    let line = d.recv_or_fail(&hub.lines, "read_config");
    assert_eq!(line["command"], "read_config");
    assert_eq!(
        line["args"],
        json!({"workspace":"/hub","path":"/hub/rules"})
    );
    hub.reply(&line, json!({"text":"global\n","version":"v1"}));
    assert_eq!(
        d.recv_or_fail(&rx, "fetched copy").unwrap(),
        ("global\n".into(), "v1".into())
    );
    // A project-scoped file has no address without a workspace: the
    // read fails naming read_config, and no line is sent.
    for file in ["/hub/projects/p/rules", "/hub/projects/p/config.json"] {
        let error = hub.remote.fetch_file_global(Path::new(file)).unwrap_err();
        assert_eq!(error.code, ErrorCode::IoFailed, "file {file}");
        assert!(
            error.message.contains("read_config"),
            "file {file}: {}",
            error.message
        );
    }
    assert!(hub.lines.try_recv().is_err(), "a project file sent a line");
}

#[test]
fn store_file_global_writes_hub_global_files_but_refuses_project_files() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    let d = Deadline::start();
    // A hub-global file resolves the parent, then writes there.
    let (tx, rx) = mpsc::channel();
    let remote = Arc::clone(&hub.remote);
    thread::spawn(move || {
        tx.send(remote.store_file_global(Path::new("/hub/rules"), "global\n", "v1"))
            .unwrap();
    });
    let line = d.recv_or_fail(&hub.lines, "global file");
    assert_eq!(line["args"], json!({"operation": "global_file"}));
    hub.reply(&line, json!({"file": "/hub/config.json"}));
    let line = d.recv_or_fail(&hub.lines, "write_config");
    assert_eq!(line["command"], "write_config");
    assert_eq!(
        line["args"],
        json!({"workspace":"/hub","path":"/hub/rules","text":"global\n","version":"v1"})
    );
    hub.reply(&line, json!(null));
    d.recv_or_fail(&rx, "stored copy").unwrap();
    // A project-scoped file has no address without a workspace: the
    // write fails naming write_config, and no line is sent.
    for file in ["/hub/projects/p/rules", "/hub/projects/p/config.json"] {
        let error = hub
            .remote
            .store_file_global(Path::new(file), "project\n", "v1")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::IoFailed, "file {file}");
        assert!(
            error.message.contains("write_config"),
            "file {file}: {}",
            error.message
        );
    }
    assert!(hub.lines.try_recv().is_err(), "a project file sent a line");
}

#[test]
fn search_maps_ranked_rows_errors_and_stale_without_taking_credits() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    let d = Deadline::start();
    let (tx, rx) = mpsc::channel();
    hub.router.set_feed(tx, crate::link::Credits::new(0));
    for fresh in [true, false] {
        hub.remote
            .search(Path::new("/hub/ws"), "../src", fresh, 19)
            .unwrap();
        let line = d.recv_or_fail(&hub.lines, "remote file search");
        let mut args = json!({"workspace":"/hub/ws","text":"../src"});
        if fresh {
            args["fresh"] = json!(true);
        }
        assert_eq!(line["command"], "search_files");
        assert_eq!(line["args"], args);
        hub.reply(&line, json!({"rows":["../src/","README.md"]}));
        assert!(
            matches!(d.recv_or_fail(&rx,"ranked file rows"),crate::Input::Files {generation:19,result:Ok(rows)} if rows==["../src/","README.md"])
        );
    }
    for (code, message, expected) in [
        ("stale_request", "older search", None),
        ("io_failed", "cannot search", Some("cannot search")),
    ] {
        hub.remote
            .search(Path::new("/hub/ws"), "x", false, 20)
            .unwrap();
        let line = d.recv_or_fail(&hub.lines, "rejected search");
        let response=serde_json::from_value(json!({"kind":"command_rejected","ts":1,"schema_version":1,"payload":{"command_id":line["id"],"code":code,"message":message}})).unwrap();
        assert!(hub.router.route(&crate::link::Line::Hub(response)));
        if let Some(expected) = expected {
            assert!(
                matches!(d.recv_or_fail(&rx,"search rejection"),crate::Input::Files {generation:20,result:Err(message)} if message==expected)
            );
        } else {
            assert!(rx.try_recv().is_err());
        }
    }
    hub.remote
        .search(Path::new("/hub/ws"), "~/", false, 21)
        .unwrap();
    let line = d.recv_or_fail(&hub.lines, "accepted search error");
    hub.reply(&line, json!({"error":"permission denied"}));
    assert!(
        matches!(d.recv_or_fail(&rx,"search walk error"),crate::Input::Files {generation:21,result:Err(message)} if message=="permission denied")
    );
}

#[test]
fn errors_preserve_rejections_and_name_bad_answers() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    let error = hub
        .call("login_targets", json!({}), json!({"targets":7}), |r| {
            r.login_targets()
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert!(error.message.contains("login_targets"));
    hub.router.detach();
    assert_eq!(
        hub.remote.login_targets().unwrap_err(),
        ConfigureError {
            code: ErrorCode::IoFailed,
            message: "connection lost".into()
        }
    );
}

#[test]
fn catalogue_reads_send_the_refresh_word() {
    use crate::catalogue::{Catalogue, Refresh};
    for (refresh, word) in [
        (Refresh::Cached, "cached"),
        (Refresh::Stale, "stale"),
        (Refresh::Every, "every"),
    ] {
        let hub = Hub::new(Arc::new(Fake::new(vec![])));
        hub.remote.set_workspace(Some(PathBuf::from("/remote/ws")));
        let catalogue = hub
            .call(
                "models",
                json!({"workspace": "/remote/ws", "refresh": word}),
                json!({"models": [], "notices": [], "lists": []}),
                move |r| r.models(refresh),
            )
            .unwrap();
        assert_eq!(catalogue, Catalogue::default());
        assert!(
            hub.lines.try_recv().is_err(),
            "a {word} read writes more than one line"
        );
    }
}

#[test]
fn catalogue_answers_decode_field_for_field() {
    use crate::catalogue::{
        Catalogue, LeftOutGroup, LeftOutModel, ListAge, ModelEntry, Price, PriceTier, Refresh,
    };
    use contract::events::Notice;
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    hub.remote.set_workspace(Some(PathBuf::from("/remote/ws")));
    let catalogue = hub
        .call(
            "models",
            json!({"workspace": "/remote/ws", "refresh": "cached"}),
            json!({
                "models": [
                    {"reference": "acme/m1", "provider": "acme", "id": "m1",
                     "name": "Acme One", "levels": ["low", "high"],
                     "default_level": "low", "configured": "high", "roles": ["main"],
                     "price": {"micros_per_mtok": 3000000, "input": 3000000,
                               "output": 15000000, "tiers": [
                                {"input_tokens_above": 500000, "micros_per_mtok": 9000000,
                                 "input": 9000000, "output": 45000000},
                                {"input_tokens_above": 200000, "micros_per_mtok": 6000000,
                                 "input": 6000000, "output": 30000000}]}},
                    {"reference": "acme/m2", "provider": "acme", "id": "m2",
                     "levels": [], "roles": []}
                ],
                "notices": [{"code": "model_invalid", "message": "acme is stale"}],
                "lists": [{"provider": "acme", "updated_ms": 1, "stale": false},
                          {"provider": "other", "stale": true}],
                "left_out": [
                    {"provider": "acme", "extension": "acme-ext",
                     "file": "config/acme-ext.json", "setting": "api_key", "login": false,
                     "models": [{"id": "m1", "name": "Acme One"}, {"id": "m2"}]},
                    {"provider": "opencode",
                     "extension": "github.com/aakshintala/fiber/providers/opencode",
                     "file": "config/opencode.json", "setting": "workspace", "login": true,
                     "models": [{"id": "m3"}]}
                ]
            }),
            |r| r.models(Refresh::Cached),
        )
        .unwrap();
    assert_eq!(
        catalogue,
        Catalogue {
            installable: Vec::new(),
            models: vec![
                ModelEntry {
                    reference: "acme/m1".to_owned(),
                    provider: "acme".to_owned(),
                    id: "m1".to_owned(),
                    name: Some("Acme One".to_owned()),
                    levels: vec!["low".to_owned(), "high".to_owned()],
                    default_level: Some("low".to_owned()),
                    configured: Some("high".to_owned()),
                    roles: vec!["main".to_owned()],
                    price: Some(Price {
                        micros_per_mtok: 3000000,
                        input: 3000000,
                        output: 15000000,
                        tiers: vec![
                            PriceTier {
                                input_tokens_above: 500000,
                                micros_per_mtok: 9000000,
                                input: 9000000,
                                output: 45000000,
                            },
                            PriceTier {
                                input_tokens_above: 200000,
                                micros_per_mtok: 6000000,
                                input: 6000000,
                                output: 30000000,
                            },
                        ],
                    }),
                },
                ModelEntry {
                    reference: "acme/m2".to_owned(),
                    provider: "acme".to_owned(),
                    id: "m2".to_owned(),
                    name: None,
                    levels: Vec::new(),
                    default_level: None,
                    configured: None,
                    roles: Vec::new(),
                    price: None,
                },
            ],
            notices: vec![Notice {
                code: ErrorCode::ModelInvalid,
                message: "acme is stale".to_owned(),
                extension: None,
            }],
            lists: vec![
                ListAge {
                    provider: "acme".to_owned(),
                    updated_ms: Some(1),
                    stale: false,
                },
                ListAge {
                    provider: "other".to_owned(),
                    updated_ms: None,
                    stale: true,
                },
            ],
            left_out: vec![
                LeftOutGroup {
                    provider: "acme".to_owned(),
                    extension: "acme-ext".to_owned(),
                    file: "config/acme-ext.json".to_owned(),
                    setting: "api_key".to_owned(),
                    login: false,
                    models: vec![
                        LeftOutModel {
                            id: "m1".to_owned(),
                            name: Some("Acme One".to_owned()),
                        },
                        LeftOutModel {
                            id: "m2".to_owned(),
                            name: None,
                        },
                    ],
                },
                LeftOutGroup {
                    provider: "opencode".to_owned(),
                    extension: "github.com/aakshintala/fiber/providers/opencode".to_owned(),
                    file: "config/opencode.json".to_owned(),
                    setting: "workspace".to_owned(),
                    login: true,
                    models: vec![LeftOutModel {
                        id: "m3".to_owned(),
                        name: None,
                    }],
                },
            ],
        }
    );
}

#[test]
fn catalogue_answers_decode_their_edges_and_reject_the_rest() {
    use crate::catalogue::Refresh;
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    hub.remote.set_workspace(Some(PathBuf::from("/remote/ws")));
    let args = json!({"workspace": "/remote/ws", "refresh": "cached"});
    // Empty lists are an empty catalogue, and one model with one level reads.
    let catalogue = hub
        .call(
            "models",
            args.clone(),
            json!({"models": [], "notices": [], "lists": []}),
            |r| r.models(Refresh::Cached),
        )
        .unwrap();
    assert!(catalogue.models.is_empty());
    assert!(catalogue.installable.is_empty());
    assert!(
        catalogue.left_out.is_empty(),
        "a missing left_out key decodes empty"
    );
    let installable = hub
        .call(
            "models",
            args.clone(),
            json!({"models": [], "notices": [], "lists": [], "installable": ["openai"]}),
            |r| r.models(Refresh::Cached),
        )
        .unwrap();
    assert_eq!(installable.installable, ["openai"]);
    let catalogue = hub
        .call(
            "models",
            args.clone(),
            json!({"models": [{"reference": "a/m", "provider": "a", "id": "m",
                               "levels": ["low"], "roles": ["x"]}],
                     "notices": [], "lists": []}),
            |r| r.models(Refresh::Cached),
        )
        .unwrap();
    assert_eq!(catalogue.models.len(), 1);
    assert_eq!(catalogue.models[0].levels, vec!["low".to_owned()]);
    // Each malformed answer is an io_failed naming models, with no partial catalogue.
    for (what, answer) in [
        (
            "installable not an array",
            json!({"models": [], "notices": [], "lists": [], "installable": "openai"}),
        ),
        (
            "installable not strings",
            json!({"models": [], "notices": [], "lists": [], "installable": [1]}),
        ),
        (
            "a model without reference",
            json!({"models": [{"provider": "a", "id": "m", "levels": [], "roles": []}],
                   "notices": [], "lists": []}),
        ),
        (
            "levels not an array",
            json!({"models": [{"reference": "a/m", "provider": "a", "id": "m",
                               "levels": "low", "roles": []}],
                   "notices": [], "lists": []}),
        ),
        (
            "a price with a string input",
            json!({"models": [{"reference": "a/m", "provider": "a", "id": "m",
                               "levels": [], "roles": [],
                               "price": {"micros_per_mtok": 1, "input": "3",
                                         "output": 1, "tiers": []}}],
                   "notices": [], "lists": []}),
        ),
        (
            "a null name",
            json!({"models": [{"reference": "a/m", "provider": "a", "id": "m",
                               "name": null, "levels": [], "roles": []}],
                   "notices": [], "lists": []}),
        ),
        (
            "a tier missing its threshold",
            json!({"models": [{"reference": "a/m", "provider": "a", "id": "m",
                               "levels": [], "roles": [],
                               "price": {"micros_per_mtok": 1, "input": 1,
                                         "output": 1, "tiers": [
                                          {"micros_per_mtok": 1, "input": 1,
                                           "output": 1}]}}],
                   "notices": [], "lists": []}),
        ),
        (
            "a list without stale",
            json!({"models": [], "notices": [],
                   "lists": [{"provider": "a"}]}),
        ),
        (
            "notices with a legacy string element",
            json!({"models": [], "notices": ["stale"], "lists": []}),
        ),
        (
            "a left_out element without its file",
            json!({"models": [], "notices": [], "lists": [],
                   "left_out": [{"provider": "acme", "extension": "acme-ext",
                                 "setting": "api_key", "login": false,
                                 "models": [{"id": "m1"}]}]}),
        ),
        ("a result that is not an object", json!([])),
    ] {
        let error = hub
            .call("models", args.clone(), answer, |r| {
                r.models(Refresh::Cached)
            })
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::IoFailed, "{what}");
        assert!(
            error.message.contains("models"),
            "{what}: {}",
            error.message
        );
    }
}

#[test]
fn decode_models_reads_object_notices_with_code_and_message() {
    use contract::events::Notice;
    let value = json!({
        "models": [],
        "notices": [{"code": "model_unconfigured",
                     "message": "The model `fake/alpha` needs the setting `workspace`"}],
        "lists": [],
    });
    let catalogue = decode_models(&value).expect("object notices decode");
    assert_eq!(
        catalogue.notices,
        [Notice {
            code: ErrorCode::ModelUnconfigured,
            message: "The model `fake/alpha` needs the setting `workspace`".to_owned(),
            extension: None,
        }]
    );
    // No `extension` key: the hub omits it, never null.
    assert_eq!(
        serde_json::to_value(&catalogue.notices).unwrap(),
        json!([{"code": "model_unconfigured",
                "message": "The model `fake/alpha` needs the setting `workspace`"}])
    );
}

#[test]
fn catalogue_rejection_keeps_the_hub_message() {
    use crate::catalogue::Refresh;
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    hub.remote.set_workspace(Some(PathBuf::from("/remote/ws")));
    let deadline = Deadline::start();
    let (tx, rx) = mpsc::channel();
    let remote = Arc::clone(&hub.remote);
    thread::spawn(move || {
        tx.send(remote.catalogue_reader()(Refresh::Cached))
            .unwrap_or(());
    });
    let line = deadline.recv_or_fail(&hub.lines, "models line");
    assert_eq!(
        line["args"],
        json!({"operation": "models", "workspace": "/remote/ws", "refresh": "cached"})
    );
    hub.reject(&line, "config_invalid", "bad config");
    assert_eq!(
        deadline.recv_or_fail(&rx, "rejected catalogue"),
        Err("bad config".to_owned())
    );
}

#[test]
fn catalogue_reads_use_the_workspace_in_use() {
    use crate::catalogue::Refresh;
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    let deadline = Deadline::start();
    // A relative global file answer is an io_failed naming global_file,
    // with no models line, on the worker path.
    for file in ["config.json", ""] {
        let (tx, rx) = mpsc::channel();
        let remote = Arc::clone(&hub.remote);
        thread::spawn(move || {
            tx.send(remote.models(Refresh::Cached)).unwrap_or(());
        });
        let line = deadline.recv_or_fail(&hub.lines, "global file");
        assert_eq!(line["args"], json!({"operation": "global_file"}));
        hub.reply(&line, json!({"file": file}));
        let error = deadline
            .recv_or_fail(&rx, "relative global file")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::IoFailed, "file {file:?}");
        assert!(
            error.message.contains("global_file"),
            "file {file:?}: {}",
            error.message
        );
        assert!(
            hub.lines.try_recv().is_err(),
            "models sent after a bad global file {file:?}"
        );
        // The loop path: settings with an empty workspace is an error
        // naming workspace, sending nothing, whatever the file answers.
        let error = hub.remote.settings(Path::new("")).unwrap_err();
        assert_eq!(error.code, ErrorCode::IoFailed, "file {file:?}");
        assert!(
            error.message.contains("workspace"),
            "file {file:?}: {}",
            error.message
        );
        assert!(
            hub.lines.try_recv().is_err(),
            "settings sent for an empty workspace with file {file:?}"
        );
    }
    // A relative workspace never reaches the wire either.
    hub.remote.set_workspace(Some(PathBuf::from("rel/dir")));
    let error = hub.remote.models(Refresh::Cached).unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert!(
        error.message.contains("workspace"),
        "relative workspace: {}",
        error.message
    );
    assert!(
        hub.lines.try_recv().is_err(),
        "models sent for a relative workspace"
    );
    // A cold read asks global_file, then reads at its parent directory.
    hub.remote.set_workspace(None);
    let (tx, rx) = mpsc::channel();
    let remote = Arc::clone(&hub.remote);
    thread::spawn(move || {
        tx.send(remote.models(Refresh::Cached)).unwrap_or(());
    });
    let line = deadline.recv_or_fail(&hub.lines, "cold global file");
    assert_eq!(line["args"], json!({"operation": "global_file"}));
    hub.reply(&line, json!({"file": "/hub/.fiber/config.json"}));
    let line = deadline.recv_or_fail(&hub.lines, "cold models line");
    assert_eq!(
        line["args"],
        json!({"operation": "models", "workspace": "/hub/.fiber", "refresh": "cached"})
    );
    hub.reply(&line, json!({"models": [], "notices": [], "lists": []}));
    assert!(
        deadline
            .recv_or_fail(&rx, "cold catalogue")
            .unwrap()
            .models
            .is_empty()
    );
    // Once the loop path has cached the global file, a read sends only models.
    assert_eq!(
        hub.call(
            "global_file",
            json!({}),
            json!({"file": "/hub/.fiber/config.json"}),
            |r| r.global_file()
        ),
        PathBuf::from("/hub/.fiber/config.json")
    );
    let (tx, rx) = mpsc::channel();
    let remote = Arc::clone(&hub.remote);
    thread::spawn(move || {
        tx.send(remote.models(Refresh::Stale)).unwrap_or(());
    });
    let line = deadline.recv_or_fail(&hub.lines, "cached models line");
    assert_eq!(
        line["args"],
        json!({"operation": "models", "workspace": "/hub/.fiber", "refresh": "stale"})
    );
    hub.reply(&line, json!({"models": [], "notices": [], "lists": []}));
    assert!(deadline.recv_or_fail(&rx, "cached catalogue").is_ok());
    assert!(
        hub.lines.try_recv().is_err(),
        "a cached read asks global_file again"
    );
    // A set workspace wins over the cache, per read.
    for workspace in ["/a", "/b"] {
        hub.remote.set_workspace(Some(PathBuf::from(workspace)));
        let (tx, rx) = mpsc::channel();
        let remote = Arc::clone(&hub.remote);
        thread::spawn(move || {
            tx.send(remote.models(Refresh::Cached)).unwrap_or(());
        });
        let line = deadline.recv_or_fail(&hub.lines, "models at a set workspace");
        assert_eq!(
            line["args"],
            json!({"operation": "models", "workspace": workspace, "refresh": "cached"})
        );
        hub.reply(&line, json!({"models": [], "notices": [], "lists": []}));
        assert!(
            deadline
                .recv_or_fail(&rx, "catalogue at a set workspace")
                .is_ok()
        );
    }
    // The workspace is copied at the read's start: storing one while a
    // read waits changes only the next read.
    hub.remote.set_workspace(Some(PathBuf::from("/wait")));
    let (tx, rx) = mpsc::channel();
    let remote = Arc::clone(&hub.remote);
    thread::spawn(move || {
        tx.send(remote.models(Refresh::Cached)).unwrap_or(());
    });
    let line = deadline.recv_or_fail(&hub.lines, "waiting models line");
    assert_eq!(
        line["args"],
        json!({"operation": "models", "workspace": "/wait", "refresh": "cached"})
    );
    hub.remote.set_workspace(Some(PathBuf::from("/later")));
    hub.reply(&line, json!({"models": [], "notices": [], "lists": []}));
    assert!(deadline.recv_or_fail(&rx, "waiting catalogue").is_ok());
    let (tx, rx) = mpsc::channel();
    let remote = Arc::clone(&hub.remote);
    thread::spawn(move || {
        tx.send(remote.models(Refresh::Cached)).unwrap_or(());
    });
    let line = deadline.recv_or_fail(&hub.lines, "later models line");
    assert_eq!(
        line["args"],
        json!({"operation": "models", "workspace": "/later", "refresh": "cached"})
    );
    hub.reply(&line, json!({"models": [], "notices": [], "lists": []}));
    assert!(deadline.recv_or_fail(&rx, "later catalogue").is_ok());
}

#[test]
fn an_empty_stored_workspace_reads_at_the_global_file() {
    use crate::catalogue::Refresh;
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    // No call site stores an empty path, but a stored one still reads
    // as a cold read does: no empty path crosses into the hub.
    hub.remote.set_workspace(Some(PathBuf::new()));
    let deadline = Deadline::start();
    let (tx, rx) = mpsc::channel();
    let remote = Arc::clone(&hub.remote);
    thread::spawn(move || {
        tx.send(remote.models(Refresh::Cached)).unwrap_or(());
    });
    let line = deadline.recv_or_fail(&hub.lines, "global file");
    assert_eq!(line["args"], json!({"operation": "global_file"}));
    hub.reply(&line, json!({"file": "/hub/.fiber/config.json"}));
    let line = deadline.recv_or_fail(&hub.lines, "models at global parent");
    assert_eq!(
        line["args"],
        json!({"operation": "models", "workspace": "/hub/.fiber", "refresh": "cached"})
    );
    hub.reply(&line, json!({"models": [], "notices": [], "lists": []}));
    assert!(
        deadline
            .recv_or_fail(&rx, "catalogue")
            .unwrap()
            .models
            .is_empty()
    );
}

#[test]
fn catalogue_reads_wait_for_the_link_and_end_on_reconnect() {
    use crate::catalogue::Refresh;
    let mut hub = Hub::new(Arc::new(Fake::new(vec![])));
    hub.router.detach();
    hub.remote.set_workspace(Some(PathBuf::from("/remote/ws")));
    let deadline = Deadline::start();
    let (tx, rx) = mpsc::channel();
    let remote = Arc::clone(&hub.remote);
    thread::spawn(move || {
        tx.send(remote.models(Refresh::Cached)).unwrap_or(());
    });
    // Nothing is written while detached: the worker parks waiting for
    // the link, so no line goes out.
    while hub.router.link_waiters() != 1 {
        assert!(
            !deadline.left().is_zero(),
            "a catalogue read did not wait for the link"
        );
        thread::yield_now();
    }
    assert!(
        hub.lines.try_recv().is_err(),
        "a catalogue read wrote while detached"
    );
    // After attach the hub reads the models line and the read answers.
    let (client, stream) = UnixStream::pair().unwrap();
    let (ltx, lrx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else {
                break;
            };
            if ltx.send(serde_json::from_str(&line).unwrap()).is_err() {
                break;
            }
        }
    });
    hub.router.attach(client);
    hub.lines = lrx;
    let line = deadline.recv_or_fail(&hub.lines, "models after attach");
    assert_eq!(
        line["args"],
        json!({"operation": "models", "workspace": "/remote/ws", "refresh": "cached"})
    );
    hub.reply(&line, json!({"models": [], "notices": [], "lists": []}));
    assert!(
        deadline
            .recv_or_fail(&rx, "catalogue after attach")
            .unwrap()
            .models
            .is_empty()
    );
    // A reconnect while a read waits ends it with connection lost.
    let (tx, rx) = mpsc::channel();
    let remote = Arc::clone(&hub.remote);
    thread::spawn(move || {
        tx.send(remote.models(Refresh::Cached)).unwrap_or(());
    });
    let line = deadline.recv_or_fail(&hub.lines, "models before the drop");
    assert_eq!(line["command"], "configure");
    hub.router.detach();
    let error = deadline
        .recv_or_fail(&rx, "catalogue after the drop")
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(error.message, "connection lost");
}

#[test]
fn remote_seams_replace_the_client_reader_and_drop_it_without_one() {
    use crate::catalogue::{Catalogue, ReadModels};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let router = Router::new();
    let local: Arc<dyn Configure> = Arc::new(Fake::new(vec![]));
    let calls = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&calls);
    let client: ReadModels = Arc::new(move |_| {
        counting.fetch_add(1, Ordering::SeqCst);
        Ok(Catalogue::default())
    });
    // With a seam the reader is the remote's, whatever the client reads.
    let installed = seams(
        &crate::LaunchLocation::Remote { hub: "box".into() },
        Some(local),
        Some(Arc::clone(&client)),
        &router,
    );
    let reader = installed.models.as_ref().unwrap();
    assert!(!Arc::ptr_eq(reader, &client));
    drop(installed);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // Without a seam the client reader is dropped unread.
    let missing = seams(
        &crate::LaunchLocation::Remote { hub: "box".into() },
        None,
        Some(client),
        &router,
    );
    assert!(missing.models.is_none());
    drop(missing);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // A local launch keeps the client's reader.
    let local: Arc<dyn Configure> = Arc::new(Fake::new(vec![]));
    let client: ReadModels = Arc::new(|_| Ok(Catalogue::default()));
    let kept = seams(
        &crate::LaunchLocation::default(),
        Some(local),
        Some(Arc::clone(&client)),
        &router,
    );
    assert!(Arc::ptr_eq(kept.models.as_ref().unwrap(), &client));
    // A remote seam with no client reader still reads from the hub.
    let local: Arc<dyn Configure> = Arc::new(Fake::new(vec![]));
    let hubbed = seams(
        &crate::LaunchLocation::Remote { hub: "box".into() },
        Some(local),
        None,
        &router,
    );
    assert!(hubbed.models.is_some());
}

#[test]
fn chip_read_takes_the_hub_model_thinking_and_scope() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    let (tx, rx) = mpsc::channel();
    let remote = Arc::clone(&hub.remote);
    thread::spawn(move || {
        tx.send(remote.chip_global()).unwrap_or(());
    });
    let line = Deadline::start().recv_or_fail(&hub.lines, "global file");
    assert_eq!(line["command"], "configure");
    assert_eq!(line["args"], json!({"operation":"global_file"}));
    hub.reply(&line, json!({"file": "/hub/config.json"}));
    let line = Deadline::start().recv_or_fail(&hub.lines, "settings");
    assert_eq!(line["command"], "configure");
    assert_eq!(
        line["args"],
        json!({"operation":"settings","workspace":"/hub"})
    );
    hub.reply(
        &line,
        json!({"rows": [
            {"key":"model","shown":"value","value":"\"hub/m\"",
             "layer":"global","file":"/hub/config.json","scope":"global_only"},
            {"key":"models.hub/m.thinking","shown":"value","value":"\"high\"",
             "layer":"global","file":"/hub/config.json","scope":"global_only"},
            {"key":"scoped_models","shown":"value","value":"[\"hub/m\"]",
             "layer":"global","file":"/hub/config.json","scope":"global_only"},
        ]}),
    );
    assert_eq!(
        Deadline::start().recv_or_fail(&rx, "chip").unwrap(),
        Chip {
            model: Some("hub/m".to_owned()),
            thinking: Some("high".to_owned()),
            scoped: vec!["hub/m".to_owned()],
        }
    );
}

#[test]
fn chip_read_quotes_a_dotted_model_in_the_thinking_key() {
    // A reference holding a dot keeps its quotes in canonical spelling:
    // `acme/m.1` reads `models."acme/m.1".thinking`.
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    let (tx, rx) = mpsc::channel();
    let remote = Arc::clone(&hub.remote);
    thread::spawn(move || {
        tx.send(remote.chip_global()).unwrap_or(());
    });
    let line = Deadline::start().recv_or_fail(&hub.lines, "global file");
    assert_eq!(line["command"], "configure");
    assert_eq!(line["args"], json!({"operation":"global_file"}));
    hub.reply(&line, json!({"file": "/hub/config.json"}));
    let line = Deadline::start().recv_or_fail(&hub.lines, "settings");
    assert_eq!(line["command"], "configure");
    assert_eq!(
        line["args"],
        json!({"operation":"settings","workspace":"/hub"})
    );
    hub.reply(
        &line,
        json!({"rows": [
            {"key":"model","shown":"value","value":"\"acme/m.1\"",             "layer":"global","file":"/hub/config.json","scope":"global_only"},
            {"key":"models.\"acme/m.1\".thinking","shown":"value","value":"\"low\"",
             "layer":"global","file":"/hub/config.json","scope":"global_only"},
            {"key":"scoped_models","shown":"value","value":"[\"acme/m.1\"]",
             "layer":"global","file":"/hub/config.json","scope":"global_only"},
        ]}),
    );
    assert_eq!(
        Deadline::start().recv_or_fail(&rx, "chip").unwrap(),
        Chip {
            model: Some("acme/m.1".to_owned()),
            thinking: Some("low".to_owned()),
            scoped: vec!["acme/m.1".to_owned()],
        }
    );
}

#[test]
fn chip_read_unsets_what_is_not_a_value() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    let (tx, rx) = mpsc::channel();
    let remote = Arc::clone(&hub.remote);
    thread::spawn(move || {
        tx.send(remote.chip_global()).unwrap_or(());
    });
    let line = Deadline::start().recv_or_fail(&hub.lines, "global file");
    hub.reply(&line, json!({"file": "/hub/config.json"}));
    let line = Deadline::start().recv_or_fail(&hub.lines, "settings");
    hub.reply(
        &line,
        json!({"rows": [
            {"key":"model","shown":"value","value":"\"hub/m\"",
             "layer":"global","file":"/hub/config.json","scope":"global_only"},
            {"key":"models.\"hub/m\".thinking","shown":"unset",
             "layer":"global","file":"/hub/config.json","scope":"global_only"},
            {"key":"scoped_models","shown":"value","value":"not json",
             "layer":"global","file":"/hub/config.json","scope":"global_only"},
        ]}),
    );
    assert_eq!(
        Deadline::start().recv_or_fail(&rx, "chip").unwrap(),
        Chip {
            model: Some("hub/m".to_owned()),
            thinking: None,
            scoped: Vec::new(),
        }
    );
}

#[test]
fn chip_read_ignores_non_value_rows_and_other_keys() {
    let local = Arc::new(Fake::new(vec![]));
    let hub = Hub::new(Arc::clone(&local));
    let (tx, rx) = mpsc::channel();
    let remote = Arc::clone(&hub.remote);
    thread::spawn(move || {
        tx.send(remote.chip_global()).unwrap_or(());
    });
    let line = Deadline::start().recv_or_fail(&hub.lines, "global file");
    hub.reply(&line, json!({"file": "/hub/config.json"}));
    let line = Deadline::start().recv_or_fail(&hub.lines, "settings");
    hub.reply(
        &line,
        json!({"rows": [
            {"key":"model","shown":"value","value":"not json",
             "layer":"global","file":"/hub/config.json","scope":"global_only"},
            {"key":"models.\"hub/m\".thinking","shown":"redacted","value":"secret",
             "layer":"global","file":"/hub/config.json","scope":"global_only"},
            {"key":"scoped_models","shown":"union","own":[],"names":[],
             "layer":"global","file":"/hub/config.json","scope":"global_only"},
            {"key":"other","shown":"value","value":"\"x\"",
             "layer":"global","file":"/hub/config.json","scope":"global_only"},
        ]}),
    );
    assert_eq!(
        Deadline::start().recv_or_fail(&rx, "chip").unwrap(),
        Chip {
            model: None,
            thinking: None,
            scoped: Vec::new(),
        }
    );
    // The local seam never answers the chip: no `settings` read lands.
    assert!(local.reads.lock().unwrap().is_empty());
}

#[test]
fn chip_read_rejection_keeps_the_hub_code_and_message() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    let (tx, rx) = mpsc::channel();
    let remote = Arc::clone(&hub.remote);
    thread::spawn(move || {
        tx.send(remote.chip(Path::new("/hub"))).unwrap_or(());
    });
    let line = Deadline::start().recv_or_fail(&hub.lines, "settings");
    hub.reject(&line, "invalid_arguments", "bad settings");
    let error = Deadline::start().recv_or_fail(&rx, "chip").unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArguments);
    assert_eq!(error.message, "bad settings");
}

#[test]
fn chip_read_detached_writes_nothing_and_reports_the_lost_link() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    hub.router.detach();
    let error = hub.remote.chip(Path::new("/hub")).unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(error.message, "connection lost");
    // The loop path fails before writing: the error above is the whole
    // proof, and the wire stays empty without waiting on the clock.
    assert!(
        hub.lines.try_recv().is_err(),
        "a detached read writes nothing"
    );
}

#[test]
fn the_fake_answers_notes_and_records_each_save() {
    use crate::configure::{NotesSaved, NotesWrite, ReviewerNotes};
    let fake = Fake::new(vec![]);
    let w = Path::new("/w");
    let read = fake.reviewer_notes(w).unwrap();
    assert_eq!(read.global.text, "");
    assert_eq!(read.global.version, "absent");
    let write = Some(NotesWrite {
        version: "absent".into(),
        text: "global notes".into(),
    });
    assert_eq!(
        fake.save_reviewer_notes(w, write.clone(), None).unwrap(),
        NotesSaved::Written(vec![])
    );
    // A test sets the answers the views read.
    *fake.notes.lock().unwrap() = Ok(ReviewerNotes {
        global: crate::configure::NotesFile {
            file: "/hub/config.json".into(),
            text: "set notes".into(),
            version: "v1".into(),
        },
        project: crate::configure::NotesFile {
            file: "/hub/project".into(),
            text: "".into(),
            version: "absent".into(),
        },
    });
    *fake.notes_saved.lock().unwrap() = Ok(NotesSaved::Stale);
    assert_eq!(fake.reviewer_notes(w).unwrap().global.text, "set notes");
    assert_eq!(
        fake.save_reviewer_notes(w, None, write.clone()).unwrap(),
        NotesSaved::Stale
    );
    assert_eq!(
        fake.notes_saves(),
        vec![
            (w.to_path_buf(), write.clone(), None),
            (w.to_path_buf(), None, write),
        ]
    );
}
