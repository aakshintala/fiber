use super::*;
use crate::configure::{LoginKind, Shown, WriteScope};
use crate::configure_fake::Fake;
use fakes::Deadline;
use serde_json::json;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
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
}
impl Drop for Hub {
    fn drop(&mut self) {
        self.router.detach();
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
fn ownership_and_settings_merge_keep_the_clients_rows_on_the_client() {
    for key in ["keys.quit", "tui.hover", "\"tui\".x", "keys"] {
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
    let local = Arc::new(Fake::new(vec![
        row("keys.quit"),
        row("model"),
        row("tui.hover"),
    ]));
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
    hub.remote.themes(Path::new("/remote/ws"));
    hub.remote.theme(Path::new("/remote/ws"), "default");
    hub.remote.save_keys(&[]).unwrap();
    assert!(hub.lines.try_recv().is_err());
}

#[test]
fn empty_workspace_uses_the_global_parent_and_caches_per_connection() {
    let hub = Hub::new(Arc::new(Fake::new(vec![])));
    let deadline = Deadline::start();
    for first in [true, false] {
        let (tx, rx) = mpsc::channel();
        let remote = Arc::clone(&hub.remote);
        thread::spawn(move || {
            tx.send(remote.settings(Path::new(""))).unwrap();
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
