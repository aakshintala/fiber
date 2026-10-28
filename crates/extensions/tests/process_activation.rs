//! Process extensions in live sessions (`docs/extensions.md`, "Process
//! extensions"): activation with real session context, host services,
//! event forwarding and shutdown.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use config::{Config, ProjectKey, Sources};
use contract::events::{CallStatus, Event, ExtensionReason};
use contract::hook::{AfterToolCall, Hooks};
use extensions::{
    DataDirBases, Origin, RepositorySet, Request, SessionExtensions, StartContext, plan,
};
use fakes::clock::FakeClock;
use fakes::{CancelToken, Deadline, NoLock, ProcessFixture, Recorder, TempDir};
use serde_json::{Map, Value, json};

/// A process provider needs no Lua file and exposes only its registered
/// callbacks (`docs/extensions.md`, "Process extensions"). Inspection and
/// a missing callback do no work; a positive models call proves dispatch.
#[test]
fn process_provider_handles_inspect_without_calls_and_dispatch_models() {
    let env = Env::new("fiber-process-provider");
    let invoked = env.marker("models.json");
    let mut steps = vec![json!({"read": {}})];
    steps.extend(
        register(
            "fiber.provider",
            "provider",
            json!(["remote", {
                "models": {"timeout": 5000, "run": true}
            }]),
        )
        .as_array()
        .unwrap()
        .clone(),
    );
    steps.push(json!({"send": {"id": "ready", "command": "ready"}}));
    steps.push(
        json!({"read_until": {"path": "kind", "equals": "extension_request", "as": "request"}}),
    );
    steps.push(json!({"write_file": {"path": invoked, "value": {"$ref": "request"}}}));
    steps.push(
        json!({"send": {"id": "answer", "command": "reply", "args": {
            "request_id": {"$ref": "request.payload.request_id"}, "result": []
        }}}),
    );
    steps.push(json!({"loop": [{"read": {}}]}));
    env.install_process(
        "provider",
        &json!({"providers": {"remote": []}}),
        &json!(steps),
    );
    let session = env.load(ExtensionReason::New, 0);
    let provider = session
        .start_provider(
            &env.config(),
            FakeClock::new(),
            Arc::new(NoLock),
            "fiber.test/provider",
            "remote",
        )
        .expect("a process registration supplies the provider without a Lua file");
    assert_eq!(provider.functions().unwrap(), vec!["models"]);
    assert!(!provider.registers("credential").unwrap());
    assert!(
        provider
            .signer(extensions::CredentialPair {
                credential: "remote".into(),
                label: "default".into()
            })
            .unwrap()
            .is_none()
    );
    assert!(
        !invoked.exists(),
        "inspection and no-signer admission run no callback"
    );
    let (sent, received) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        sent.send(provider.list_models(&extensions::ModelsArg::default()))
            .unwrap();
    });
    let (models, returned) = Deadline::after(WAIT)
        .recv_or_fail(&received, "process models callback")
        .unwrap();
    worker.join().unwrap();
    assert!(models.is_empty());
    assert_eq!(returned, json!([]));
    let request = read_json_file(&invoked, "models request");
    assert_eq!(request["payload"]["registration"], "provider");
    assert_eq!(request["payload"]["key"], "models");
    session.finish_processes(&CancelToken::new());
}

/// How long a test waits for the fixture's files before failing.
const WAIT: Duration = Duration::from_secs(20);

/// A Fiber home and workspace in a temporary directory.
struct Env {
    root: TempDir,
}

impl Env {
    fn new(name: &str) -> Self {
        let root = TempDir::new(name);
        std::fs::create_dir(root.path().join("home")).unwrap();
        std::fs::create_dir(root.path().join("workspace")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    /// A workspace file the fixture writes, proving what it received.
    fn marker(&self, name: &str) -> PathBuf {
        self.workspace().join(name)
    }

    /// Installs `fiber.test/<short>` with `extra` merged into its manifest;
    /// the process runs the fixture example in `peer` mode with `config`.
    fn install_process(&self, short: &str, extra: &Value, config: &Value) {
        let (program, argv) = ProcessFixture::command("peer", Some(config));
        let args = argv.into_iter().map(Value::String).collect::<Vec<_>>();
        let mut process = serde_json::Map::new();
        process.insert("program".to_owned(), json!(program.display().to_string()));
        process.insert("args".to_owned(), Value::Array(args));
        process.insert("exit_timeout_ms".to_owned(), json!(1000));
        let mut manifest = json!({
            "name": format!("fiber.test/{short}"),
            "version": "v1",
            "fiber": "0.1.0",
            "api": 1,
            "process": process,
        });
        for (key, value) in extra.as_object().unwrap() {
            if key == "process"
                && let (Some(into), Some(from)) = (
                    manifest
                        .get_mut("process")
                        .and_then(|process| process.as_object_mut()),
                    value.as_object(),
                )
            {
                for (inner, replacement) in from {
                    into.insert(inner.clone(), replacement.clone());
                }
                continue;
            }
            manifest
                .as_object_mut()
                .unwrap()
                .insert(key.clone(), value.clone());
        }
        let source = self.root.path().join("src").join(short);
        write(&source.join("extension.json"), &manifest.to_string());
        plan(
            &self.home(),
            &Request::Path(source),
            "0.1.0",
            &Origin::github(),
            &*FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    fn config(&self) -> Config {
        self.config_with(&[] as &[&str])
    }

    fn config_with(&self, overrides: &[&str]) -> Config {
        Config::load(Sources {
            home: self.home(),
            workspace: self.workspace(),
            project: ProjectKey::new("p").unwrap(),
            overrides: overrides
                .iter()
                .map(|override_| (*override_).to_owned())
                .collect(),
        })
        .unwrap()
    }

    /// Loads the installed extensions with `reason` as the session's latest
    /// start reason and `cursor` as the pre-start event cursor.
    fn load(&self, reason: ExtensionReason, cursor: u64) -> Arc<SessionExtensions> {
        self.load_with(&self.config(), reason, cursor)
    }

    fn load_with(
        &self,
        config: &Config,
        reason: ExtensionReason,
        cursor: u64,
    ) -> Arc<SessionExtensions> {
        self.load_with_clock(config, reason, cursor, FakeClock::new())
    }

    /// Loads on the caller's clock, so a test can move time while the load
    /// waits for a silent child.
    fn load_with_clock(
        &self,
        config: &Config,
        reason: ExtensionReason,
        cursor: u64,
        clock: Arc<FakeClock>,
    ) -> Arc<SessionExtensions> {
        let home = self.home();
        let start = StartContext {
            session_id: contract::SessionId("s_proc".into()),
            reason,
            workspace: self.workspace(),
            data_dirs: DataDirBases {
                home: home.clone(),
                project: ProjectKey::new("p").unwrap(),
            },
            drive: None,
            event_cursor: cursor,
        };
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
        Arc::new(SessionExtensions::load_session(
            &home,
            config,
            clock,
            locks,
            None,
            contract::hook::StateSeed::new(),
            RepositorySet::default(),
            start,
            std::collections::BTreeSet::new(),
        ))
    }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// The first JSON object the fixture writes, waited for under a deadline:
/// the peer writes a marker once it has what the test waits for.
fn read_json_file(path: &Path, what: &str) -> Value {
    let path = path.to_path_buf();
    let watched = path.clone();
    fakes::wait_until(
        &format!("the peer to write {what}"),
        Deadline::after(WAIT),
        move || {
            std::fs::read_to_string(&watched)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .is_some()
        },
    );
    let text = std::fs::read_to_string(&path).expect("the peer writes its marker");
    serde_json::from_str(&text).expect("the marker is one JSON object")
}

/// A valid `fiber.tool` spec with `run`, as JSON: static effects, so no
/// effects function dispatches.
fn tool_spec() -> Value {
    json!({
        "description": "Probes the session.",
        "input_schema": {"type": "object"},
        "effects": {"effects": [], "reversible": true},
        "timeout": 5000,
        "run": true,
    })
}

/// A valid `after_tool` hook spec with `run`, as JSON.
fn hook_spec() -> Value {
    json!({
        "phase": "transform",
        "timeout": 5000,
        "on_failure": "blocking",
        "run": true,
    })
}

/// A valid watcher spec with `run`, as JSON.
fn watcher_spec() -> Value {
    json!({"timeout": 5000, "run": true})
}

/// Sends one registration call, then waits for its answer.
fn register(action: &str, id: &str, args: Value) -> Value {
    json!([
        {"send": {"id": id, "command": "call", "args": {"call": action, "args": args}}},
        {"read_until": {"path": "payload.command_id", "equals": id}},
    ])
}

/// The peer plan registering a tool, an `after_tool` hook and a `notice`
/// watcher, then answering tool and hook requests: the request's
/// registering command id selects the answer.
fn registry_plan(hello: &Path, tool_answer: &Path, hook_answer: &Path) -> Value {
    let tool_branch = json!([
        {"write_file": {"path": tool_answer.to_str().unwrap(), "value": {"$ref": "req"}}},
        {"send": {"id": "a-tool", "command": "reply", "args": {"request_id": {"$ref": "req.payload.request_id"}, "result": "probe-ok"}}},
    ]);
    let hook_branch = json!([
        {"write_file": {"path": hook_answer.to_str().unwrap(), "value": {"$ref": "req"}}},
        {"send": {"id": "a-hook", "command": "reply", "args": {"request_id": {"$ref": "req.payload.request_id"}, "result": {"content": "hooked-content"}}}},
    ]);
    let dispatch = json!([
        {"if": {"path": "req.payload.registration", "equals": "t1", "then": tool_branch, "else": [
            {"if": {"path": "req.payload.registration", "equals": "h1", "then": hook_branch}},
        ]}},
    ]);
    let serve = json!([
        {"read": {"as": "req"}},
        {"if": {"path": "req.kind", "equals": "extension_request", "then": dispatch}},
    ]);
    let mut plan = vec![
        json!({"read": {"as": "hello"}}),
        json!({"write_file": {"path": hello.to_str().unwrap(), "value": {"$ref": "hello"}}}),
    ];
    for action in register("fiber.tool", "t1", json!(["probe", tool_spec()]))
        .as_array()
        .unwrap()
    {
        plan.push(action.clone());
    }
    for action in register("fiber.hook", "h1", json!(["after_tool", hook_spec()]))
        .as_array()
        .unwrap()
    {
        plan.push(action.clone());
    }
    for action in register("fiber.watch", "w1", json!([["notice"], watcher_spec()]))
        .as_array()
        .unwrap()
    {
        plan.push(action.clone());
    }
    plan.push(json!({"send": {"id": "ready1", "command": "ready"}}));
    plan.push(json!({"read_until": {"path": "kind", "equals": "command_accepted"}}));
    plan.push(json!({"loop": serve}));
    Value::Array(plan)
}

/// A session with the registered fixture tool and hook: the peer plan, the
/// loaded set and the hello the fixture captured.
fn registered_session(
    env: &Env,
    short: &str,
    reason: ExtensionReason,
) -> (Arc<SessionExtensions>, Value) {
    let hello = env.marker(&format!("{short}-hello.json"));
    let tool_answer = env.marker(&format!("{short}-tool.json"));
    let hook_answer = env.marker(&format!("{short}-hook.json"));
    let plan = registry_plan(&hello, &tool_answer, &hook_answer);
    env.install_process(short, &json!({}), &plan);
    let session = env.load(reason, 0);
    let captured = read_json_file(&hello, "the hello line");
    (session, captured)
}

/// Loading starts the process package: the hello carries the session's
/// identity, reason, workspace, data directories and folded state, the
/// tool and hook register as a Lua extension's do, and both run.
#[test]
fn process_session_load_registers_tool_and_hook_with_complete_hello() {
    let env = Env::new("fiber-process-session-load");
    let (session, captured) = registered_session(&env, "proc", ExtensionReason::New);
    assert!(
        session.notices().is_empty(),
        "the load raises no notice: {:?}",
        session.notices()
    );
    // The hello is the first complete outbound line, with the session's
    // identity, why it started, its workspace, the extension's data
    // directories and its folded state.
    assert_eq!(captured["kind"], json!("extension_hello"));
    let payload = &captured["payload"];
    assert_eq!(payload["session_id"], json!("s_proc"));
    assert_eq!(payload["reason"], json!("new"));
    assert_eq!(
        payload["workspace"],
        json!(env.workspace().display().to_string())
    );
    assert_eq!(
        payload["data_dirs"]["machine"],
        json!(
            config::extension_data_dir(&env.home(), "fiber.test-proc")
                .display()
                .to_string()
        )
    );
    assert_eq!(
        payload["data_dirs"]["project"],
        json!(
            config::project_data_dir(
                &env.home(),
                &ProjectKey::new("p").unwrap(),
                "fiber.test-proc"
            )
            .display()
            .to_string()
        )
    );
    assert_eq!(payload["state"], json!({}));
    // The tool and hook register behind the same runtime adapters Lua uses.
    let tools = session.tools();
    assert!(
        tools
            .iter()
            .any(|(owner, tool)| owner == "fiber.test/proc" && tool.definition().name == "probe"),
        "the fixture tool registers: {:?}",
        tools
            .iter()
            .map(|(owner, tool)| (owner, tool.definition().name))
            .collect::<Vec<_>>()
    );
    assert!(
        session
            .registrations()
            .iter()
            .any(|hook| matches!(hook, contract::events::HookInfo::Hook { extension, point, .. } if extension == "fiber.test/proc" && point == "after_tool")),
        "the after_tool hook registers"
    );
    // The registered tool runs through the session: the fixture answers.
    let (_, tool) = tools
        .iter()
        .find(|(_, tool)| tool.definition().name == "probe")
        .unwrap();
    let cancel = CancelToken::new();
    let emit = Recorder::default();
    let output = tool.run(&Map::new(), &cancel, &emit);
    assert!(output.error.is_none(), "the tool runs: {:?}", output.error);
    let text: Vec<&str> = output
        .content
        .iter()
        .filter_map(|part| match part {
            contract::shapes::ContentPart::Text { text } => Some(text.as_str()),
            contract::shapes::ContentPart::Image { .. }
            | contract::shapes::ContentPart::Pdf(_)
            | contract::shapes::ContentPart::Unknown => None,
        })
        .collect();
    assert_eq!(text, vec!["probe-ok"]);
    // The hook visibly changes the outcome, as a Lua hook's does.
    let arguments = Map::new();
    let answer = session.after_tool(&AfterToolCall {
        tool: "probe",
        arguments: &arguments,
        status: CallStatus::Completed,
        content: "original",
        details: None,
        process: None,
        spilled: None,
        delivery: None,
        not_regular: false,
    });
    match answer.outcome {
        contract::hook::AfterToolOutcome::Changed { content, .. } => {
            assert_eq!(content.as_deref(), Some("hooked-content"));
        }
        outcome @ contract::hook::AfterToolOutcome::Unchanged
        | outcome @ contract::hook::AfterToolOutcome::Failed(_)
        | outcome @ contract::hook::AfterToolOutcome::Withheld { .. } => {
            panic!("the hook changes the outcome, not {outcome:?}")
        }
    }
    assert_eq!(answer.changed_by, vec!["fiber.test/proc".to_owned()]);
    session.finish_processes(&cancel);
}

/// A fake session model: answers every request with fixed text.
struct FakeModel;

impl contract::model::HostModel for FakeModel {
    fn call(
        &self,
        _extension: &str,
        _request: contract::model::HostModelRequest,
        _bound: contract::model::Bound,
    ) -> Result<contract::model::HostModelReply, contract::shapes::Failure> {
        Ok(contract::model::HostModelReply {
            text: "model-says-hi".to_owned(),
            tokens: contract::shapes::Tokens {
                input: 1,
                cache_read: 0,
                cache_write: Default::default(),
                output: 1,
            },
            cost: None,
        })
    }
}

/// A fake delegate starter: binds and returns a fixed job id.
struct FakeDelegate;

impl contract::extension::HostDelegate for FakeDelegate {
    fn start(
        &self,
        _extension: &str,
        _arguments: &Map<String, Value>,
        bind: &mut dyn FnMut(&contract::JobId),
    ) -> Result<contract::JobId, contract::shapes::Failure> {
        let id = contract::JobId("j_delegate".into());
        bind(&id);
        Ok(id)
    }

    fn retain(&self, _loaded: &[String]) -> usize {
        0
    }

    fn cancel(&self, _jobs: &[contract::JobId]) -> usize {
        0
    }
}

/// One command recorded by the fake driver.
type DriveCall = (String, String, Map<String, Value>);

/// A fake driver: records the commands it receives and answers nothing.
struct FakeDrive {
    received: std::sync::Mutex<Vec<DriveCall>>,
}

impl FakeDrive {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            received: std::sync::Mutex::new(Vec::new()),
        })
    }
}

impl contract::extension::Drive for FakeDrive {
    fn drive(
        &self,
        extension: &str,
        command: &str,
        args: Map<String, Value>,
        answer: contract::inbox::Ack,
    ) {
        self.received
            .lock()
            .unwrap()
            .push((extension.to_owned(), command.to_owned(), args));
        (answer.0)(Ok(None));
    }
}

/// Calls one host service, then writes its answer to the marker file.
fn probe(id: &str, command: &str, args: Value, marker: &str) -> Vec<Value> {
    vec![
        json!({"send": {"id": id, "command": "call", "args": {"call": command, "args": args}}}),
        json!({"read_until": {"path": "payload.command_id", "equals": id, "append_to": "acks"}}),
        json!({"write_file": {"path": marker, "value": {"$ref": "last"}}}),
    ]
}

/// The peer plan probing the installed session services: each host call's
/// answer is written to its marker file, then the tool answers.
fn services_plan(markers: &Path) -> Value {
    let marker = |name: &str| markers.join(name).to_str().unwrap().to_owned();
    let answer = json!({"send": {"id": "a-tool", "command": "reply", "args": {"request_id": {"$ref": "req.payload.request_id"}, "result": "probed"}}});
    let mut probe_body = Vec::new();
    let calls: Vec<(&str, &str, Value, String)> = vec![
        (
            "l1",
            "host.log",
            json!(["service-probe"]),
            marker("log-ack.json"),
        ),
        (
            "c1",
            "host.config.get",
            json!(["probe_key"]),
            marker("config-ack.json"),
        ),
        (
            "d1",
            "host.data_dir",
            json!(["machine"]),
            marker("data-dir-ack.json"),
        ),
        (
            "m1",
            "host.model",
            json!([{"model": "fake/model", "messages": [{"role": "user", "content": "hi"}], "max_tokens": 8}]),
            marker("model-ack.json"),
        ),
        (
            "g1",
            "host.delegate",
            json!([{}, {"timeout": 1000, "run": true}]),
            marker("delegate-ack.json"),
        ),
        (
            "t2",
            "host.tool",
            json!(["probe", {}]),
            marker("tool-ack.json"),
        ),
    ];
    for (id, command, args, file) in calls {
        probe_body.extend(probe(id, command, args, &file));
    }
    // A driver command rides a forward line, not a call: the command
    // names the driver's command directly.
    probe_body.push(json!({"send": {"id": "v1", "command": "steer", "args": {"content": [{"type": "text", "text": "hi"}]}}}));
    probe_body.push(
        json!({"read_until": {"path": "payload.command_id", "equals": "v1", "append_to": "acks"}}),
    );
    probe_body
        .push(json!({"write_file": {"path": marker("drive-ack.json"), "value": {"$ref": "last"}}}));
    probe_body.push(answer);
    let serve = json!([
        {"read": {"as": "req"}},
        {"if": {"path": "req.kind", "equals": "extension_request", "then": probe_body}},
    ]);
    let mut plan = vec![json!({"read": {"as": "hello"}})];
    for action in register("fiber.tool", "t1", json!(["probe", tool_spec()]))
        .as_array()
        .unwrap()
    {
        plan.push(action.clone());
    }
    plan.push(json!({"send": {"id": "ready1", "command": "ready"}}));
    plan.push(json!({"read_until": {"path": "kind", "equals": "command_accepted"}}));
    plan.push(json!({"loop": serve}));
    Value::Array(plan)
}

/// Installed session services answer the process host calls, and calls
/// without a session fail with the existing sentences: the model, the
/// delegate and the driver reply through fakes, while `host.tool` with no
/// runner fails `invalid_arguments`.
#[test]
fn process_host_calls_reach_session_services_and_unset_ones_fail_closed() {
    let env = Env::new("fiber-process-host-services");
    let markers = env.workspace().join("markers");
    std::fs::create_dir_all(&markers).unwrap();
    let plan = services_plan(&markers);
    env.install_process("svc", &json!({}), &plan);
    let config =
        env.config_with(
            &["extensions.\"fiber.test/svc\".settings.probe_key=\"probe-value\""] as &[&str],
        );
    let session = env.load_with(&config, ExtensionReason::New, 0);
    assert!(
        session.notices().is_empty(),
        "the load raises no notice: {:?}",
        session.notices()
    );
    let emit = Arc::new(Recorder::default());
    session.emit_to(emit.clone());
    session.model_to(Arc::new(FakeModel));
    session.delegate_to(Arc::new(FakeDelegate));
    let drive = FakeDrive::new();
    session.drive_to(drive.clone());
    // The tool dispatch runs the fixture's probe, which exercises every
    // service above before answering.
    let tool = session
        .tools()
        .into_iter()
        .find(|(_, tool)| tool.definition().name == "probe")
        .map(|(_, tool)| tool)
        .unwrap();
    let cancel = CancelToken::new();
    let output = tool.run(&Map::new(), &cancel, &*emit);
    assert!(output.error.is_none(), "the probe runs: {:?}", output.error);
    // host.log emits through the installed emitter.
    let seen = emit.clone();
    fakes::wait_until("the status line", Deadline::after(WAIT), move || {
        seen.events().iter().any(
            |event| matches!(event, Event::ExtensionLog(log) if log.message == "service-probe"),
        )
    });
    let ack = |name: &str| read_json_file(&markers.join(name), name);
    // host.config.get reads the session's configuration.
    assert_eq!(
        ack("config-ack.json")["payload"]["result"]["value"],
        json!("probe-value")
    );
    // host.data_dir returns the extension's machine directory.
    assert_eq!(
        ack("data-dir-ack.json")["payload"]["result"]["value"],
        json!(
            config::extension_data_dir(&env.home(), "fiber.test-svc")
                .display()
                .to_string()
        )
    );
    // host.model answers through the installed session model.
    assert_eq!(
        ack("model-ack.json")["payload"]["result"]["value"]["text"],
        json!("model-says-hi")
    );
    // host.delegate answers through the installed starter.
    assert_eq!(
        ack("delegate-ack.json")["payload"]["result"]["value"],
        json!("j_delegate")
    );
    // The driver command reaches the installed driver.
    assert_eq!(ack("drive-ack.json")["kind"], json!("command_accepted"));
    let received = drive.received.lock().unwrap();
    assert!(
        received
            .iter()
            .any(|(extension, command, _)| extension == "fiber.test/svc" && command == "steer"),
        "the driver receives the command: {received:?}"
    );
    // host.tool with no runner fails closed with the existing sentence.
    assert_eq!(ack("tool-ack.json")["kind"], json!("command_rejected"));
    assert_eq!(
        ack("tool-ack.json")["payload"]["code"],
        json!("invalid_arguments")
    );
    let cancel = CancelToken::new();
    session.finish_processes(&cancel);
}

/// The peer plan receiving session events: after `ready`, every line the
/// test forwards is written to its marker, and requests are answered.
fn events_plan(marker: &Path) -> Value {
    let answer = json!({"send": {"id": "a-watch", "command": "reply", "args": {"request_id": {"$ref": "line.payload.request_id"}, "result": null}}});
    let record =
        json!({"append_file": {"path": marker.to_str().unwrap(), "value": {"$ref": "line"}}});
    let on_request = json!([record.clone(), answer]);
    let serve = json!([
        {"read": {"as": "line"}},
        {"if": {"path": "line.kind", "equals": "extension_request", "then": on_request}},
    ]);
    let mut plan = vec![json!({"read": {"as": "hello"}})];
    for action in register("fiber.watch", "w1", json!([["notice"], watcher_spec()]))
        .as_array()
        .unwrap()
    {
        plan.push(action.clone());
    }
    plan.push(json!({"send": {"id": "ready1", "command": "ready"}}));
    plan.push(json!({"read_until": {"path": "kind", "equals": "command_accepted"}}));
    plan.push(json!({"loop": serve}));
    Value::Array(plan)
}

fn notice_envelope(session_id: &str, seq: u64, message: &str) -> contract::Envelope {
    contract::Envelope {
        kind: "notice".to_owned(),
        session_id: contract::SessionId(session_id.to_owned()),
        ts: 1_759_150_000_000 + seq,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: Some(contract::Seq(seq)),
        payload: json!({"code": "test_notice", "message": message})
            .as_object()
            .unwrap()
            .clone(),
    }
}

/// The session forwards every live event envelope to the process owner
/// and replays stored durable lines after hello, while ephemeral events
/// are live-only: the ephemeral envelope arrives once and never replays,
/// the two durable lines after the cursor arrive in order, and
/// redelivering either is a no-op because the cursor already covers it.
/// Each delivery carries the original envelope, with its schema version
/// and its durable `seq` — or no `seq` for the ephemeral line — rather
/// than the filtered watcher argument Lua watchers receive.
#[test]
fn process_event_stream_forwards_live_and_replays_durable_lines() {
    let env = Env::new("fiber-process-event-stream");
    let marker = env.marker("stream.jsonl");
    env.install_process("stream", &json!({}), &events_plan(&marker));
    let session = env.load(ExtensionReason::New, 7);
    // The watcher registered before ready: live events reach it.
    let first = contract::Envelope {
        kind: "notice".to_owned(),
        session_id: contract::SessionId("s_proc".into()),
        ts: 1_759_150_000_000,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"code": "test_notice", "message": "live-ephemeral"})
            .as_object()
            .unwrap()
            .clone(),
    };
    let Ok(Some(first_event)) = Event::from_envelope(&first) else {
        panic!("the envelope parses");
    };
    session.deliver_event(&first_event, &first);
    // Two durable lines after the startup cursor replay in order.
    let replayed = [
        notice_envelope("s_proc", 8, "first"),
        notice_envelope("s_proc", 9, "second"),
    ];
    for line in &replayed {
        session.deliver_line(line);
    }
    // Redelivering either line is a no-op: the owner's cursor already
    // covers both, so no durable line ever forwards twice.
    for line in &replayed {
        session.deliver_line(line);
    }
    let watched = marker.clone();
    fakes::wait_until(
        "the three forwarded lines",
        Deadline::after(WAIT),
        move || {
            std::fs::read_to_string(&watched)
                .map(|text| text.lines().count() >= 3)
                .unwrap_or(false)
        },
    );
    // Finishing closes the child, so nothing more can arrive after this:
    // the marker must hold exactly the live event and the replayed pair.
    let cancel = CancelToken::new();
    session.finish_processes(&cancel);
    let text = std::fs::read_to_string(&marker).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let received: Vec<Value> = lines
        .iter()
        .map(|line| line["payload"]["args"][0].clone())
        .collect();
    assert_eq!(
        received,
        vec![
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(&replayed[0]).unwrap(),
            serde_json::to_value(&replayed[1]).unwrap(),
        ],
        "the live envelope first, then the replayed pair, each exactly once: {received:?}"
    );
}

/// `finish_processes` shuts every live owner down: the fixture exits when
/// its input closes, so finishing is clean and the child is reaped.
#[test]
fn process_finish_closes_each_live_owner() {
    let env = Env::new("fiber-process-finish");
    let marker = env.marker("stream.jsonl");
    env.install_process("fin", &json!({}), &events_plan(&marker));
    let session = env.load(ExtensionReason::New, 0);
    let cancel = CancelToken::new();
    let errors = session.finish_processes(&cancel);
    assert!(errors.is_empty(), "finishing is clean: {errors:?}");
    assert!(
        session.notices().is_empty(),
        "finishing raises no notice: {:?}",
        session.notices()
    );
}

/// Loads with a zero startup allowance, so the silent child misses its
/// deadline without parking the loader: the fake clock never wakes a
/// parked startup wait, so the timeout path is exercised with the
/// deadline already passed, and the wedged child is killed at once.
fn load_past_deadline(env: &Env, short: &str, required: bool) -> Arc<SessionExtensions> {
    let extra = if required {
        json!({"process": {"required": true}})
    } else {
        json!({})
    };
    env.install_process(short, &extra, &json!([{"read": {}}]));
    let config = env
        .config_with(&[&format!("extensions.\"fiber.test/{short}\".startup_timeout_ms=0") as &str]);
    env.load_with(&config, ExtensionReason::New, 0)
}

/// A required process extension whose child never readies is fatal: the
/// owner is left out and the failure is reported with the required code,
/// while an optional miss and a declined package only skip the owner.
#[test]
fn process_startup_failures_report_required_and_skip_the_rest() {
    // A child that never sends `ready`: the startup deadline passes.
    let env = Env::new("fiber-process-startup-miss");
    let session = load_past_deadline(&env, "miss", true);
    let notices = session.notices();
    assert!(
        notices.iter().any(
            |notice| notice.code == contract::ErrorCode::ExtensionRequiredFailed
                && notice.extension.as_deref() == Some("fiber.test/miss")
        ),
        "the required miss reports: {notices:?}"
    );
    assert!(
        !session
            .loaded_names()
            .contains(&"fiber.test/miss".to_owned()),
        "the missed owner is left out"
    );
    let cancel = CancelToken::new();
    session.finish_processes(&cancel);

    // The same silence from an optional package is not fatal.
    let env = Env::new("fiber-process-startup-optional");
    let session = load_past_deadline(&env, "opt", false);
    assert!(
        session
            .notices()
            .iter()
            .all(|notice| notice.code != contract::ErrorCode::ExtensionRequiredFailed),
        "the optional miss is not fatal: {:?}",
        session.notices()
    );
    assert!(
        !session
            .loaded_names()
            .contains(&"fiber.test/opt".to_owned()),
        "the optional miss is left out"
    );
    let cancel = CancelToken::new();
    session.finish_processes(&cancel);

    // A disabled package never spawns its child at all.
    let env = Env::new("fiber-process-declined");
    env.install_process("off", &json!({}), &json!([{"read": {}}]));
    let config = env.config_with(&["extensions.\"fiber.test/off\".enabled=false"] as &[&str]);
    let session = env.load_with(&config, ExtensionReason::New, 0);
    assert!(
        !session
            .loaded_names()
            .contains(&"fiber.test/off".to_owned()),
        "the declined package never loads"
    );
    let cancel = CancelToken::new();
    session.finish_processes(&cancel);
}

/// A process extension whose child cannot spawn stays out of the loaded
/// set with the spawn failure's notice: an optional failure is not fatal,
/// while a required one reports the required code.
#[test]
fn process_spawn_failure_stays_out_with_its_notice() {
    let missing = || json!({"process": {"program": "fiber-definitely-missing-program"}});
    let env = Env::new("fiber-process-spawn-optional");
    env.install_process("broken", &missing(), &json!([]));
    let session = env.load(ExtensionReason::New, 0);
    assert!(
        !session
            .loaded_names()
            .contains(&"fiber.test/broken".to_owned()),
        "the unspawnable extension is left out"
    );
    assert!(
        session
            .notices()
            .iter()
            .any(
                |notice| notice.extension.as_deref() == Some("fiber.test/broken")
                    && notice.code != contract::ErrorCode::ExtensionRequiredFailed
            ),
        "the optional spawn failure is noticed without the required code: {:?}",
        session.notices()
    );
    session.finish_processes(&CancelToken::new());

    let env = Env::new("fiber-process-spawn-required");
    env.install_process(
        "broken",
        &json!({"process": {
            "program": "fiber-definitely-missing-program",
            "required": true,
        }}),
        &json!([]),
    );
    let session = env.load(ExtensionReason::New, 0);
    assert!(
        session.notices().iter().any(|notice| notice.code
            == contract::ErrorCode::ExtensionRequiredFailed
            && notice.extension.as_deref() == Some("fiber.test/broken")),
        "the required spawn failure reports: {:?}",
        session.notices()
    );
    assert!(
        !session
            .loaded_names()
            .contains(&"fiber.test/broken".to_owned()),
        "the required spawn failure is left out"
    );
    session.finish_processes(&CancelToken::new());
}

/// Each session folds its own state: lines one session admits never reach
/// another session's hello.
#[test]
fn process_sessions_fold_isolated_state() {
    let env = Env::new("fiber-process-state-isolation");
    let first_hello = env.marker("first-hello.json");
    let second_hello = env.marker("second-hello.json");
    let first_plan = json!([
        {"read": {"as": "hello"}},
        {"write_file": {"path": first_hello.to_str().unwrap(), "value": {"$ref": "hello"}}},
        {"send": {"id": "ready1", "command": "ready"}},
        {"read": {}},
    ]);
    env.install_process("iso", &json!({}), &first_plan);
    let first = env.load(ExtensionReason::New, 0);
    let captured = read_json_file(&first_hello, "the first hello");
    assert_eq!(captured["payload"]["state"], json!({}));
    first.admit_state_lines(vec![contract::hook::StateLine {
        extension: "fiber.test/iso".to_owned(),
        key: "k".to_owned(),
        value: Some((json!("v"), contract::events::OnFork::Latest)),
        landing: 0,
    }]);
    first.finish_processes(&CancelToken::new());
    // A second session over the same home folds nothing from the first.
    let second_plan = json!([
        {"read": {"as": "hello"}},
        {"write_file": {"path": second_hello.to_str().unwrap(), "value": {"$ref": "hello"}}},
        {"send": {"id": "ready1", "command": "ready"}},
        {"read": {}},
    ]);
    env.install_process("iso", &json!({}), &second_plan);
    let second = env.load(ExtensionReason::New, 0);
    let captured = read_json_file(&second_hello, "the second hello");
    assert_eq!(captured["payload"]["state"], json!({}));
    second.finish_processes(&CancelToken::new());
}

/// The load records the reason it started with: a fresh session starts new.
#[test]
fn process_session_load_records_its_start_reason() {
    let env = Env::new("fiber-process-start-reason");
    let marker = env.marker("reason-hello.json");
    env.install_process(
        "why",
        &json!({}),
        &json!([
            {"read": {"as": "hello"}},
            {"write_file": {"path": marker.to_str().unwrap(), "value": {"$ref": "hello"}}},
            {"send": {"id": "ready1", "command": "ready"}},
            {"read": {}},
        ]),
    );
    let session = env.load(ExtensionReason::Resume, 0);
    assert_eq!(session.latest_reason(), ExtensionReason::Resume);
    let captured = read_json_file(&marker, "the hello line");
    assert_eq!(captured["payload"]["reason"], json!("resume"));
    let cancel = CancelToken::new();
    session.finish_processes(&cancel);
}
