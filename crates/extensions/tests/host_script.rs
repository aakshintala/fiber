//! Scripted host calls cross the Lua host boundary (`docs/testing.md`, "Testing an extension").

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use common::{Setup, install, write};
use config::{Config, ProjectKey, Sources};
use contract::ErrorCode;
use contract::files::PathLock;
use contract::hook::Hooks;
use contract::inbox::Delivery;
use contract::shapes::Process;
use extensions::{
    Error, ExecEntry, ExecReply, HostScript, HttpEntry, LuaExtension, SessionExtensions,
};
use fakes::ProviderServer;
use fakes::clock::FakeClock;
use serde_json::json;

const WAIT: Duration = Duration::from_secs(5);

struct Attended;
#[allow(
    clippy::panic,
    reason = "the test fails if a scripted OAuth call reaches the real browser boundary"
)]
impl extensions::Browser for Attended {
    fn open(&self, _: &str) {
        panic!("scripted open must not reach the browser");
    }
    fn show(&self, _: &str, _: &str) {
        panic!("scripted show must not reach the browser");
    }
    fn attended(&self) -> bool {
        true
    }
}

#[test]
fn oauth_script_reaches_pkce_open_show_and_callback_without_external_work() {
    let setup = Setup::new();
    let dir = setup.root().join("oauth-script");
    write(
        &dir.join("init.lua"),
        r#"
        fiber.command("oauth", { timeout = 1000, run = function()
            local p = host.oauth.pkce()
            host.oauth.open("https://example.test/" .. p.challenge)
            host.oauth.show("https://example.test/device", p.verifier)
            local query = host.oauth.callback({port = 1})
            local ok, err = pcall(host.oauth.callback, {port = 1})
            return json.encode({ verifier = p.verifier, code = query.code, failed = not ok, error = err.code })
        end })
    "#,
    );
    let script = HostScript::new(
        vec![],
        vec![],
        vec![
            json!({"pkce": {"verifier": "v", "challenge": "c"}}),
            json!({"open": {"url": "https://example.test/c"}}),
            json!({"show": {"url": "https://example.test/device", "code": "v"}}),
            json!({"callback": {"reply": {"query": {"code": "yes"}}}}),
            json!({"callback": {"error": {"code": "io_failed", "message": "busy"}}}),
        ],
    );
    let extension = Arc::new(
        LuaExtension::new("oauth-script", dir, setup.home(), FakeClock::new())
            .with_host_script(script.clone())
            .with_browser(Arc::new(Attended)),
    );
    let (sent, received) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        drop(sent.send(extension.command("oauth", "")));
    });
    let returned = received
        .recv_timeout(WAIT)
        .expect("scripted OAuth command returns")
        .unwrap();
    worker.join().unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&returned).unwrap(),
        json!({"verifier": "v", "code": "yes", "failed": true, "error": "io_failed"})
    );
    assert!(script.unmet().is_empty(), "{:?}", script.unmet());
}

struct NoLock;

impl PathLock for NoLock {
    fn hold(&self, _path: &Path, run: &mut dyn FnMut()) {
        run();
    }

    fn hold_all(&self, _paths: &[PathBuf], run: &mut dyn FnMut()) {
        run();
    }
}

#[allow(
    clippy::unwrap_used,
    reason = "a test fixture helper; setup failures fail the test"
)]
fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = destination.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(from, to).unwrap();
        }
    }
}

#[allow(
    clippy::unwrap_used,
    reason = "a test fixture helper; setup failures fail the test"
)]
fn config(setup: &Setup) -> Config {
    Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("host-script-test").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap()
}

#[test]
fn fixture_entry_and_callback_calls_use_the_script_and_never_open_a_socket_or_process() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    let exec_program = setup
        .workspace()
        .join("fiber-scripted-exec-must-not-run")
        .display()
        .to_string();
    assert!(!Path::new(&exec_program).exists());
    let source = setup.root().join("fixture");
    copy_tree(&fakes::lua_fixture(), &source);
    let init_path = source.join("init.lua");
    let mut init = fs::read_to_string(&init_path).unwrap();
    init.push_str(&format!(
        r#"
local entry_reply = host.http({{
  method = "GET", url = "{base}/entry", headers = {{["X-Case"] = "entry"}}
}})
fiber.provider("host-script", {{ models = {{
  timeout = 1000,
  run = function()
    local response = host.http({{
      method = "GET", url = "{base}/models", headers = {{["X-Case"] = "models"}}
    }})
    local run = host.exec("{exec_program}", {{"one"}}, {{cwd = "."}})
    return {{{{
      id = entry_reply.status .. ":" .. entry_reply.body .. "|" ..
        response.status .. ":" .. response.body .. "|" .. run.exit_code .. ":" ..
        run.stdout .. ":" .. run.stderr,
      protocol = "openai-responses", base_url = "{base}/v1", context_window = 1000
    }}}}
  end
}} }})
"#,
        base = server.url(),
        exec_program = exec_program
    ));
    write(&init_path, &init);
    install(&setup.home(), &source, "0.1.0").unwrap();

    let script = HostScript::new(
        vec![
            HttpEntry {
                request: json!({
                    "method": "GET",
                    "url": format!("{}/entry", server.url()),
                    "headers": {"X-CaSe": "entry"},
                    "body": null
                }),
                reply: Ok((201, b"entry".to_vec())),
            },
            HttpEntry {
                request: json!({
                    "method": "GET",
                    "url": format!("{}/models", server.url()),
                    "headers": {"X-CASE": "models"},
                    "body": null
                }),
                reply: Ok((200, b"models".to_vec())),
            },
        ],
        vec![ExecEntry {
            request: json!({
                "program": exec_program,
                "args": ["one"],
                "cwd": setup.workspace().join(".").display().to_string()
            }),
            reply: Ok(ExecReply {
                code: 7,
                stdout: "out".to_owned(),
                stderr: "err".to_owned(),
            }),
        }],
        Vec::new(),
    );
    let config = config(&setup);
    let home = setup.home();
    let host_script = Arc::clone(&script);
    let (loaded_tx, loaded_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let locks: Arc<dyn PathLock> = Arc::new(NoLock);
        let loaded =
            SessionExtensions::load(&home, &config, FakeClock::new(), locks, Some(host_script));
        let _sent = loaded_tx.send(loaded);
    });
    let extensions = loaded_rx
        .recv_timeout(WAIT)
        .expect("waited for the scripted extension to load");
    let (inbox_tx, inbox_rx) = mpsc::channel();
    extensions.deliver_to(inbox_tx);
    let provider = extensions
        .lua_providers()
        .iter()
        .find(|(_, provider)| provider.name() == "host-script")
        .map(|(_, provider)| Arc::clone(provider))
        .expect("loaded the host-script provider");
    let (models_tx, models_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _sent = models_tx.send(provider.models());
    });
    let models = models_rx
        .recv_timeout(WAIT)
        .expect("waited for scripted host calls")
        .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "201:entry|200:models|7:out:err");

    let delivery = inbox_rx
        .recv_timeout(WAIT)
        .expect("waited for the scripted exec delivery");
    let Delivery::ExtensionExec(event) = delivery else {
        panic!("host.exec did not deliver extension_exec: {delivery:?}");
    };
    assert_eq!(event.extension, "fiber.test/lua-fixture");
    assert_eq!(event.program, exec_program);
    assert_eq!(event.args, ["one"]);
    assert_eq!(event.cwd, setup.workspace().join(".").display().to_string());
    assert_eq!(
        event.process,
        Process {
            exit_code: Some(7),
            signal: None,
            timed_out: false,
        }
    );
    assert!(script.unmet().is_empty(), "{:?}", script.unmet());
    assert!(server.requests().is_empty(), "{:?}", server.requests());
}

#[test]
fn an_unscripted_http_call_fails_in_lua_and_is_reported_as_a_miss() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    let dir = setup.root().join("unscripted");
    write(
        &dir.join("init.lua"),
        &format!(
            "fiber.command(\"fetch\", {{ timeout = 1000, run = function()\n\
             return host.http({{ url = \"{}\" }})\n\
             end }})\n",
            server.url()
        ),
    );
    let script = HostScript::new(Vec::new(), Vec::new(), Vec::new());
    let extension = Arc::new(
        LuaExtension::new("unscripted", dir, setup.home(), FakeClock::new())
            .with_host_script(Arc::clone(&script)),
    );
    let (result_tx, result_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _sent = result_tx.send(extension.command("fetch", ""));
    });
    let error = result_rx
        .recv_timeout(WAIT)
        .expect("waited for unscripted host.http to fail")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ExtensionFailed);
    let Error::Lua { message, .. } = error else {
        panic!("expected the uncaught host failure to be a Lua error");
    };
    assert!(
        message.contains("host.http: the case scripts no reply for this request"),
        "{message}"
    );
    let unmet = script.unmet();
    assert_eq!(unmet.len(), 1);
    assert!(unmet[0].contains("host.http[1]"), "{}", unmet[0]);
    assert!(unmet[0].contains(&server.url()), "{}", unmet[0]);
    assert!(server.requests().is_empty(), "{:?}", server.requests());
}
