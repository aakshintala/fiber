//! A Lua extension's tools through the public API (`docs/extensions.md`,
//! "Registering" and "How an extension runs"), on the fake clock.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

mod common;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use common::{Setup, go_module, install_lua};
use config::{Config, ProjectKey, Sources};
use contract::ErrorCode;
use contract::clock::Clock;
use contract::hook::Hooks;
use contract::inbox::Delivery;
use contract::shapes::{ContentPart, DeclaredEffects, Effect};
use contract::tool::{Effects, EffectsError, Output, Tool};
use extensions::{LuaExtension, LuaTool, SessionExtensions};
use fakes::clock::FakeClock;
use fakes::{CancelToken, Recorder};
use serde_json::{Map, Value, json};

/// How long a test waits for a signal or an answer before failing.
const WAIT: Duration = Duration::from_secs(5);

/// An extension named `fiber.test/<short>` with the entry script `init`.
fn extension(dir: &Path, short: &str, init: &str, clock: Arc<FakeClock>) -> Arc<LuaExtension> {
    std::fs::write(dir.join("init.lua"), init).unwrap();
    Arc::new(LuaExtension::new(
        format!("fiber.test/{short}"),
        dir,
        dir.join("home"),
        clock,
    ))
}

/// The extension's tools, read on a thread under `WAIT`.
fn tools(ext: &Arc<LuaExtension>) -> Vec<LuaTool> {
    let (tx, rx) = mpsc::channel();
    let ext = Arc::clone(ext);
    std::thread::spawn(move || tx.send(ext.tools()));
    rx.recv_timeout(WAIT)
        .expect("waited for the tools")
        .expect("the entry script ran")
}

const NOTES: &str = r#"
fiber.tool("note_count", {
  description = "Counts the notes in a file.",
  input_schema = { type = "object", required = { "path" }, properties = { path = { type = "string" }, deep = { type = "boolean" } } },
  effects = { effects = { "reads" }, paths = { "note.txt" }, reversible = true },
  timeout = 2000,
  run = function() return "3 notes" end,
})
fiber.tool("archive", {
  description = "Archives a note.",
  input_schema = { type = "object" },
  effects = function(args) return { effects = { "writes" }, reversible = false } end,
  timeout = 2000,
  run = function() return "done" end,
})
"#;

#[test]
fn tools_are_listed_by_name_with_their_extension_and_full_definition() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ext = extension(dir.path(), "notes", NOTES, FakeClock::new());
    let tools = tools(&ext);
    let names: Vec<String> = tools.iter().map(|tool| tool.definition().name).collect();
    assert_eq!(names, ["archive", "note_count"]);
    for tool in &tools {
        assert_eq!(tool.extension(), "fiber.test/notes");
        assert!(!tool.definition().deferred, "declared in full");
        assert_eq!(tool.definition().hosted, None);
    }
    let note = tools
        .iter()
        .find(|t| t.definition().name == "note_count")
        .unwrap();
    assert_eq!(note.definition().description, "Counts the notes in a file.");
}

#[test]
fn two_loads_of_one_entry_script_give_byte_identical_definitions() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let serialised = |ext: &Arc<LuaExtension>| -> Vec<String> {
        tools(ext)
            .iter()
            .map(|tool| serde_json::to_string(&tool.definition()).unwrap())
            .collect()
    };
    let first = serialised(&extension(dir.path(), "notes", NOTES, FakeClock::new()));
    let second = serialised(&extension(dir.path(), "notes", NOTES, FakeClock::new()));
    assert_eq!(first.len(), 2);
    assert_eq!(first, second);
    assert!(
        first.iter().any(|line| line.contains(
            r#""input_schema":{"properties":{"deep":{"type":"boolean"},"path":{"type":"string"}},"required":["path"],"type":"object"}"#
        )),
        "{first:?}"
    );
}

// Calls (`docs/extensions.md`, "How an extension runs"): a tool call runs in
// the gaps of the extension's ordered stream, under its own timeout from
// when Fiber asked.

/// When the hook cannot stop the VM, the caller waits 1 second more.
const GRACE: Duration = Duration::from_secs(1);

/// The extension's tools by name, each shareable with a calling thread.
fn by_name(ext: &Arc<LuaExtension>) -> BTreeMap<String, Arc<LuaTool>> {
    tools(ext)
        .into_iter()
        .map(|tool| (tool.definition().name, Arc::new(tool)))
        .collect()
}

fn args(value: Value) -> Map<String, Value> {
    let Value::Object(map) = value else {
        panic!("arguments are an object, not {value}");
    };
    map
}

/// Runs `tool` on `arguments` on its own thread.
fn run(tool: &Arc<LuaTool>, arguments: Value) -> mpsc::Receiver<Output> {
    let (tx, rx) = mpsc::channel();
    let tool = Arc::clone(tool);
    std::thread::spawn(move || {
        let output = tool.run(&args(arguments), &CancelToken::new(), &Recorder::default());
        match tx.send(output) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    rx
}

/// The call's result, under `WAIT`.
fn ran(rx: &mpsc::Receiver<Output>) -> Output {
    rx.recv_timeout(WAIT).expect("waited for the tool call")
}

/// `tool`'s effects for `arguments`, on a thread under `WAIT`.
fn effects(tool: &Arc<LuaTool>, arguments: Value) -> Result<Effects, EffectsError> {
    let tool = Arc::clone(tool);
    fakes::within("the effects call", WAIT, move || {
        tool.effects(&args(arguments))
    })
}

/// The result's text parts, joined.
fn text(output: &Output) -> String {
    output
        .content
        .iter()
        .map(|part| {
            let ContentPart::Text { text } = part else {
                panic!("a non-text part {part:?}");
            };
            text.clone()
        })
        .collect()
}

/// The failure's code and message.
fn failed(output: &Output) -> (ErrorCode, String) {
    let failure = output.error.clone().expect("the call failed");
    (failure.code, failure.message)
}

/// An HTTP server holding one request: `accepted` fires once it has read
/// the request's head, and it answers `body` once `release` is sent. It
/// closes unanswered after `WAIT`.
struct Held {
    url: String,
    accepted: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
}

fn hold(body: &'static str) -> Held {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (accepted_tx, accepted) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut sock = listener.accept().unwrap().0;
        let mut seen = Vec::new();
        let mut byte = [0; 1];
        while !seen.ends_with(b"\r\n\r\n") {
            match sock.read(&mut byte) {
                Ok(0) | Err(_) => break,
                Ok(_) => seen.push(byte[0]),
            }
        }
        match accepted_tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
        if release_rx.recv().is_ok() {
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            match sock.write_all(reply.as_bytes()) {
                Ok(()) | Err(_) => {}
            }
        }
    });
    Held {
        url,
        accepted,
        release,
    }
}

const COUNTED: &str = r#"
local calls = 0
fiber.tool("still", {
  description = "d", input_schema = { type = "object" },
  effects = { effects = { "reads" }, paths = { "a.txt" }, reversible = true },
  timeout = 1000,
  run = function() calls = calls + 1 return "ran" end,
})
fiber.tool("per_path", {
  description = "d", input_schema = { type = "object" },
  effects = function(args) calls = calls + 1 return { effects = { "writes" }, paths = { args.path }, reversible = false } end,
  timeout = 1000,
  run = function() return "ran" end,
})
fiber.command("count", { timeout = 1000, run = function() return tostring(calls) end })
"#;

/// Runs the command `name` on a thread under `WAIT`.
fn command(ext: &Arc<LuaExtension>, name: &str) -> String {
    let ext = Arc::clone(ext);
    let name = name.to_owned();
    fakes::within("the command", WAIT, move || ext.command(&name, "")).expect("the command ran")
}

#[test]
fn static_effects_are_returned_without_starting_a_callback() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ext = extension(dir.path(), "count", COUNTED, FakeClock::new());
    let tools = by_name(&ext);
    let still = &tools["still"];
    assert_eq!(
        effects(still, json!({})),
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: Some(vec!["a.txt".to_owned()]),
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
        })
    );
    assert_eq!(command(&ext, "count"), "0");
    assert_eq!(text(&ran(&run(still, json!({})))), "ran");
    assert_eq!(command(&ext, "count"), "1");
}

#[test]
fn an_effects_function_answers_each_call_from_its_arguments() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ext = extension(dir.path(), "count", COUNTED, FakeClock::new());
    let tools = by_name(&ext);
    let per_path = &tools["per_path"];
    for path in ["a.txt", "b.txt"] {
        assert_eq!(
            effects(per_path, json!({ "path": path })),
            Ok(Effects {
                declared: DeclaredEffects {
                    effects: vec![Effect::Writes],
                    reversible: false,
                    paths: Some(vec![path.to_owned()]),
                },
                subject: Some(String::new()),
                prefix: None,
                always_reviewed: false,
            })
        );
    }
    assert_eq!(command(&ext, "count"), "2");
}

#[test]
fn an_effects_function_that_raises_or_returns_a_bad_shape_fails_naming_the_tool() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ext = extension(
        dir.path(),
        "fx",
        r#"
fiber.tool("raises", {
  description = "d", input_schema = { type = "object" }, timeout = 1000,
  effects = function() error("boom") end,
  run = function() return "" end,
})
fiber.tool("odd", {
  description = "d", input_schema = { type = "object" }, timeout = 1000,
  effects = function() return { effects = { "deletes" }, reversible = true } end,
  run = function() return "" end,
})
"#,
        FakeClock::new(),
    );
    let tools = by_name(&ext);
    assert_eq!(
        effects(&tools["raises"], json!({})),
        Err(EffectsError::Tool(
            "the effects function of the tool `raises` failed: `fiber.test/fx`: init.lua:4: boom"
                .to_owned()
        ))
    );
    assert_eq!(
        effects(&tools["odd"], json!({})),
        Err(EffectsError::Tool(
            "the effects function of the tool `odd` of `fiber.test/fx` returned effects that do not read: \"deletes\" is not `reads`, `writes`, `executes` or `network`"
                .to_owned()
        ))
    );
}

#[test]
fn an_effects_function_past_the_tools_timeout_fails_naming_the_tool() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let clock = FakeClock::new();
    let ext = extension(
        dir.path(),
        "fx",
        r#"
fiber.tool("slow", {
  description = "d", input_schema = { type = "object" }, timeout = 100,
  effects = function() require("go_fx") while true do end end,
  run = function() return "" end,
})
"#,
        clock.clone(),
    );
    let went = go_module(dir.path(), "fx");
    let tools = by_name(&ext);
    let slow = Arc::clone(&tools["slow"]);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(slow.effects(&Map::new())));
    went.recv_timeout(WAIT)
        .expect("waited for the effects function to start");
    clock.advance(Duration::from_millis(100));
    assert_eq!(
        rx.recv_timeout(WAIT).expect("waited for the effects call"),
        Err(EffectsError::Tool(
            "the effects function of the tool `slow` failed: `fiber.test/fx`: `slow.effects` passed its 100 ms timeout and was stopped."
                .to_owned()
        ))
    );
}

const RETURNS: &str = r#"
local function tool(name, run)
  fiber.tool(name, { description = "d", input_schema = { type = "object" },
    effects = { effects = {}, reversible = true }, timeout = 1000, run = run })
end
tool("plain", function() return "3 notes" end)
tool("part", function() return { type = "text", text = "hello" } end)
tool("part_extra", function() return { type = "text", text = "x", extra = 1 } end)
tool("full", function()
  return {
    content = { { type = "text", text = "a" }, { type = "text", text = "b" } },
    details = { count = 3 },
    error = { code = "nonzero_exit", message = "m" },
    control = { handoff = "n" },
  }
end)
tool("nothing", function() return nil end)
tool("number", function() return 7 end)
tool("colour", function() return { content = "x", colour = "red" } end)
tool("image", function() return { content = { { type = "image", path = "a.png" } } } end)
tool("raises", function() error("boom") end)
tool("args", function(args) return args.path .. "!" end)
"#;

#[test]
fn each_return_maps_to_its_result() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ext = extension(dir.path(), "a", RETURNS, FakeClock::new());
    let tools = by_name(&ext);
    let call = |name: &str| ran(&run(&tools[name], json!({})));
    let plain = call("plain");
    assert_eq!((text(&plain), plain.error), ("3 notes".to_owned(), None));
    let part = call("part");
    assert_eq!((text(&part), part.error), ("hello".to_owned(), None));
    assert_eq!(
        ran(&run(&tools["args"], json!({ "path": "note.txt" }))).content,
        vec![ContentPart::Text {
            text: "note.txt!".to_owned()
        }]
    );
    let full = call("full");
    assert_eq!(text(&full), "ab");
    assert_eq!(full.details, Some(json!({ "count": 3 })));
    assert_eq!(failed(&full), (ErrorCode::NonzeroExit, "m".to_owned()));
    assert_eq!(
        full.control.and_then(|control| control.handoff),
        Some("n".to_owned())
    );
    let bad = |why: &str| (ErrorCode::ToolError, format!("the tool `{why}"));
    for (name, why) in [
        (
            "part_extra",
            "part_extra` of `fiber.test/a` returned a text part holding `extra`",
        ),
        ("nothing", "nothing` of `fiber.test/a` returned nothing"),
        ("number", "number` of `fiber.test/a` returned a number"),
        (
            "colour",
            "colour` of `fiber.test/a` returned `colour`, which is not `content`, `details`, `error` or `control`",
        ),
        (
            "image",
            "image` of `fiber.test/a` returned a `image` part; a tool returns only text parts",
        ),
    ] {
        let output = call(name);
        assert!(output.content.is_empty(), "{name}");
        assert_eq!(failed(&output), bad(why), "{name}");
    }
    assert_eq!(
        failed(&call("raises")),
        (
            ErrorCode::ToolError,
            "`fiber.test/a`: init.lua:21: boom".to_owned()
        )
    );
    // A raised error leaves the VM usable.
    assert_eq!(text(&call("plain")), "3 notes");
}

#[test]
fn a_run_spinning_past_its_timeout_fails_timeout_and_the_next_call_runs() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let clock = FakeClock::new();
    let ext = extension(
        dir.path(),
        "slow",
        r#"
local calls = 0
fiber.tool("spin", {
  description = "d", input_schema = { type = "object" },
  effects = { effects = {}, reversible = true }, timeout = 100,
  run = function()
    calls = calls + 1
    if calls == 1 then require("go_spin") while true do end end
    return "again"
  end,
})
"#,
        clock.clone(),
    );
    let went = go_module(dir.path(), "spin");
    let tools = by_name(&ext);
    let first = run(&tools["spin"], json!({}));
    went.recv_timeout(WAIT).expect("waited for spin to start");
    clock.advance(Duration::from_millis(100));
    assert_eq!(
        failed(&ran(&first)),
        (
            ErrorCode::Timeout,
            "`fiber.test/slow`: `spin` passed its 100 ms timeout and was stopped.".to_owned()
        )
    );
    assert_eq!(text(&ran(&run(&tools["spin"], json!({})))), "again");
}

#[test]
fn a_run_parked_on_a_host_call_past_its_timeout_fails_timeout_and_the_next_call_runs() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let clock = FakeClock::new();
    let held = hold("late");
    let ext = extension(
        dir.path(),
        "slow",
        &format!(
            r#"
local calls = 0
fiber.tool("wait", {{
  description = "d", input_schema = {{ type = "object" }},
  effects = {{ effects = {{}}, reversible = true }}, timeout = 100,
  run = function()
    calls = calls + 1
    if calls == 1 then return host.http({{ url = "{}" }}).body end
    return "again"
  end,
}})
"#,
            held.url
        ),
        clock.clone(),
    );
    let tools = by_name(&ext);
    let deadline = clock.now().checked_add(Duration::from_millis(100)).unwrap();
    let first = run(&tools["wait"], json!({}));
    held.accepted
        .recv_timeout(WAIT)
        .expect("waited for the request to reach the server");
    // The caller of a parked call waits to the deadline plus the grace,
    // so the callback is parked under its deadline.
    assert!(
        clock.await_parked(deadline.checked_add(GRACE).unwrap(), WAIT),
        "waited for the caller to see the call parked"
    );
    clock.advance(Duration::from_millis(100));
    assert_eq!(
        failed(&ran(&first)),
        (
            ErrorCode::Timeout,
            "`fiber.test/slow`: `wait` passed its 100 ms timeout and was stopped.".to_owned()
        )
    );
    assert_eq!(text(&ran(&run(&tools["wait"], json!({})))), "again");
}

#[test]
fn two_calls_parked_on_host_calls_both_complete() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let (one, two) = (hold("one"), hold("two"));
    let ext = extension(
        dir.path(),
        "pair",
        r#"
fiber.tool("fetch", {
  description = "d", input_schema = { type = "object" },
  effects = { effects = { "network" }, reversible = true }, timeout = 5000,
  run = function(args) return host.http({ url = args.url }).body end,
})
"#,
        FakeClock::new(),
    );
    let tools = by_name(&ext);
    let first = run(&tools["fetch"], json!({ "url": one.url }));
    one.accepted
        .recv_timeout(WAIT)
        .expect("waited for the first request");
    let second = run(&tools["fetch"], json!({ "url": two.url }));
    two.accepted
        .recv_timeout(WAIT)
        .expect("waited for the second request while the first is parked");
    two.release.send(()).unwrap();
    assert_eq!(text(&ran(&second)), "two");
    one.release.send(()).unwrap();
    assert_eq!(text(&ran(&first)), "one");
}

#[test]
fn exec_in_run_is_not_logged_and_exec_in_an_effects_function_is() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ext = extension(
        dir.path(),
        "exec",
        r#"
fiber.tool("runs", {
  description = "d", input_schema = { type = "object" },
  effects = function() host.exec("sh", { "-c", "exit 3" }) return { effects = { "executes" }, reversible = false } end,
  timeout = 5000,
  run = function() return tostring(host.exec("sh", { "-c", "exit 0" }).exit_code) end,
})
"#,
        FakeClock::new(),
    );
    let (tx, inbox) = mpsc::channel();
    ext.deliver_to(tx);
    let tools = by_name(&ext);
    let runs = &tools["runs"];
    assert_eq!(text(&ran(&run(runs, json!({})))), "0");
    // The run's `extension_exec` would be sent before its reply resumes
    // the callback, so it would be here by now.
    assert!(matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)));
    assert!(effects(runs, json!({})).is_ok());
    match inbox.recv_timeout(WAIT) {
        Ok(Delivery::ExtensionExec(exec)) => {
            assert_eq!(exec.program, "sh");
            assert_eq!(exec.args, ["-c", "exit 3"]);
            assert_eq!(exec.process.exit_code, Some(3));
        }
        other => panic!("expected the effects function's extension_exec, got {other:?}"),
    }
    assert!(matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)));
}

#[test]
fn a_refresh_from_a_tool_is_a_string_naming_the_tool() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ext = extension(
        dir.path(),
        "refresh",
        r#"
fiber.tool("refresh", {
  description = "d", input_schema = { type = "object" },
  effects = { effects = {}, reversible = true }, timeout = 5000,
  run = function()
    local ok, err = pcall(host.oauth.refresh, function() return { token = "t", expires_at = 1 } end)
    return tostring(ok) .. "\n" .. type(err) .. "\n" .. tostring(err)
  end,
})
"#,
        FakeClock::new(),
    );
    let tools = by_name(&ext);
    let said = text(&ran(&run(&tools["refresh"], json!({}))));
    let mut lines = said.lines();
    assert_eq!(lines.next(), Some("false"));
    assert_eq!(lines.next(), Some("string"));
    let message = lines.next().unwrap_or_default();
    assert!(
        message.contains("a tool has no provider credential"),
        "{message}"
    );
}

// Settling names across a session's extensions (`docs/extensions.md`,
// "What a package holds" and "Registering").

struct NoLock;

impl contract::files::PathLock for NoLock {
    fn hold(&self, _path: &Path, run: &mut dyn FnMut()) {
        run();
    }

    fn hold_all(&self, _paths: &[PathBuf], run: &mut dyn FnMut()) {
        run();
    }
}

/// The session's extensions, loaded on a thread under `WAIT`.
fn session(setup: &Setup) -> Arc<SessionExtensions> {
    let config = Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    let home = setup.home();
    let loaded = fakes::within("the extensions to load", WAIT, move || {
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
        SessionExtensions::load(&home, &config, FakeClock::new(), locks, None)
    });
    Arc::new(loaded)
}

/// Each declared tool as `(extension, tool)`.
fn declared(session: &SessionExtensions) -> Vec<(String, String)> {
    session
        .tools()
        .iter()
        .map(|(extension, tool)| (extension.clone(), tool.definition().name))
        .collect()
}

fn loaded(session: &SessionExtensions) -> Vec<String> {
    session.loaded().into_iter().map(|ext| ext.name).collect()
}

/// A tool `name` returning `said`, as a line of Lua.
fn tool_line(name: &str, said: &str) -> String {
    format!(
        r#"fiber.tool("{name}", {{ description = "d", input_schema = {{ type = "object" }}, effects = {{ effects = {{ "reads" }}, reversible = true }}, timeout = 1000, run = function() return "{said}" end }})
"#
    )
}

const HOOK_AND_COMMAND: &str = r#"
fiber.hook("after_tool", { timeout = 1000, on_failure = "non-blocking", run = function() return nil end })
fiber.command("mine", { timeout = 1000, run = function() return "" end })
"#;

#[test]
fn a_declared_replacement_of_read_is_declared_with_its_extension() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "myread",
        &json!({"replaces": ["read"]}),
        &tool_line("read", "mine"),
    );
    let session = session(&setup);
    assert_eq!(
        declared(&session),
        [("fiber.test/myread".to_owned(), "read".to_owned())]
    );
    assert_eq!(session.notices(), []);
}

#[test]
fn an_undeclared_replacement_of_read_unloads_the_extension_and_all_it_registered() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "myread",
        &json!({"replaces": []}),
        &format!("{}{HOOK_AND_COMMAND}", tool_line("read", "mine")),
    );
    let session = session(&setup);
    assert!(declared(&session).is_empty());
    assert!(loaded(&session).is_empty());
    assert!(!session.has_hooks());
    assert!(session.commands().is_empty());
    let notices = session.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(
        notices[0].message,
        "Tool `read` replaces a built-in tool its manifest does not list in `replaces`; `fiber.test/myread` is not loaded."
    );
    assert_eq!(notices[0].extension.as_deref(), Some("fiber.test/myread"));
}

#[test]
fn two_extensions_registering_one_tool_lose_it_and_keep_their_others() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "a",
        &json!({"replaces": []}),
        &format!("{}{}", tool_line("dup", "a"), tool_line("only_a", "a")),
    );
    install_lua(
        &setup,
        "b",
        &json!({"replaces": []}),
        &format!("{}{}", tool_line("dup", "b"), tool_line("only_b", "b")),
    );
    let session = session(&setup);
    assert_eq!(
        declared(&session),
        [
            ("fiber.test/a".to_owned(), "only_a".to_owned()),
            ("fiber.test/b".to_owned(), "only_b".to_owned())
        ]
    );
    assert_eq!(loaded(&session), ["fiber.test/a", "fiber.test/b"]);
    let notices = session.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(
        notices[0].message,
        "Extensions `fiber.test/a` and `fiber.test/b` both register the tool `dup`, so neither gets it."
    );
    assert_eq!(notices[0].extension, None);
}

#[test]
fn an_extension_with_only_tools_stays_loaded_and_delivers() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "only",
        &json!({"replaces": []}),
        r#"
fiber.tool("probe", {
  description = "d", input_schema = { type = "object" }, timeout = 5000,
  effects = function() host.exec("sh", { "-c", "exit 4" }) return { effects = { "reads" }, reversible = true } end,
  run = function() return "" end,
})
"#,
    );
    let session = session(&setup);
    let (tx, inbox) = mpsc::channel();
    session.deliver_to(tx);
    let tools = session.tools();
    let (_, probe) = tools.first().expect("the tool is declared");
    let probe = Arc::clone(probe);
    fakes::within("the effects call", WAIT, move || probe.effects(&Map::new()))
        .expect("the effects function ran");
    match inbox.recv_timeout(WAIT) {
        Ok(Delivery::ExtensionExec(exec)) => assert_eq!(exec.process.exit_code, Some(4)),
        other => panic!("expected the effects function's extension_exec, got {other:?}"),
    }
}

// Cancellation (`docs/tools.md`, "Cancellation"): a cancelled call returns
// no content and no error only once it can change nothing more.

/// Runs `tool` with no arguments on its own thread, cancelled by `cancel`.
fn run_cancellable(tool: &Arc<LuaTool>, cancel: &CancelToken) -> mpsc::Receiver<Output> {
    let (tx, rx) = mpsc::channel();
    let tool = Arc::clone(tool);
    let cancel = cancel.clone();
    std::thread::spawn(move || {
        let output = tool.run(&Map::new(), &cancel, &Recorder::default());
        match tx.send(output) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    rx
}

/// What a cancelled call returns.
fn cancelled() -> Output {
    Output::default()
}

/// Waits `bound` for nothing: a bounded receive on a channel no one sends on.
fn pause(bound: Duration) {
    let (_keep, never) = mpsc::channel::<()>();
    assert!(never.recv_timeout(bound).is_err());
}

#[test]
fn a_queued_call_cancelled_before_it_starts_never_runs() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let clock = FakeClock::new();
    let ext = extension(
        dir.path(),
        "queue",
        r#"
local calls = 0
fiber.tool("spin", {
  description = "d", input_schema = { type = "object" },
  effects = { effects = {}, reversible = true }, timeout = 100,
  run = function() require("go_spin") while true do end end,
})
fiber.tool("count", {
  description = "d", input_schema = { type = "object" },
  effects = { effects = {}, reversible = true }, timeout = 60000,
  run = function() calls = calls + 1 return tostring(calls) end,
})
"#,
        clock.clone(),
    );
    let went = go_module(dir.path(), "spin");
    let tools = by_name(&ext);
    let spinning = run(&tools["spin"], json!({}));
    went.recv_timeout(WAIT).expect("waited for spin to start");
    // The thread spins, so this call stays queued.
    let cancel = CancelToken::new();
    let queued = run_cancellable(&tools["count"], &cancel);
    cancel.cancel();
    assert_eq!(ran(&queued), cancelled());
    clock.advance(Duration::from_millis(100));
    assert_eq!(failed(&ran(&spinning)).0, ErrorCode::Timeout);
    assert_eq!(text(&ran(&run(&tools["count"], json!({})))), "1");
}

#[test]
fn a_call_parked_on_a_host_call_is_cancelled_without_the_clock_moving() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let held = hold("late");
    let ext = extension(
        dir.path(),
        "parked",
        &format!(
            r#"
local calls = 0
fiber.tool("wait", {{
  description = "d", input_schema = {{ type = "object" }},
  effects = {{ effects = {{}}, reversible = true }}, timeout = 60000,
  run = function()
    calls = calls + 1
    if calls == 1 then return host.http({{ url = "{}" }}).body end
    return "again"
  end,
}})
"#,
            held.url
        ),
        FakeClock::new(),
    );
    let tools = by_name(&ext);
    let cancel = CancelToken::new();
    let first = run_cancellable(&tools["wait"], &cancel);
    held.accepted
        .recv_timeout(WAIT)
        .expect("waited for the request to reach the server");
    cancel.cancel();
    assert_eq!(ran(&first), cancelled());
    assert_eq!(text(&ran(&run(&tools["wait"], json!({})))), "again");
}

#[test]
fn a_call_spinning_in_lua_is_cancelled_before_its_timeout() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ext = extension(
        dir.path(),
        "spin",
        r#"
local calls = 0
fiber.tool("spin", {
  description = "d", input_schema = { type = "object" },
  effects = { effects = {}, reversible = true }, timeout = 60000,
  run = function()
    calls = calls + 1
    if calls == 1 then require("go_spin") while true do end end
    return "again"
  end,
})
"#,
        FakeClock::new(),
    );
    let went = go_module(dir.path(), "spin");
    let tools = by_name(&ext);
    let cancel = CancelToken::new();
    let first = run_cancellable(&tools["spin"], &cancel);
    went.recv_timeout(WAIT).expect("waited for spin to start");
    cancel.cancel();
    assert_eq!(ran(&first), cancelled());
    // The interrupt is cleared: the next call runs to its own result.
    assert_eq!(text(&ran(&run(&tools["spin"], json!({})))), "again");
}

/// An extension whose tool `exec` runs `script` through `sh`, with the
/// ready FIFO's path as `$0`, the first time, and returns `again` after.
fn exec_extension(dir: &Path, script: &str, ready: &Path) -> Arc<LuaExtension> {
    extension(
        dir,
        "exec",
        &format!(
            r#"
local calls = 0
fiber.tool("exec", {{
  description = "d", input_schema = {{ type = "object" }},
  effects = {{ effects = {{ "executes" }}, reversible = false }}, timeout = 60000,
  run = function()
    calls = calls + 1
    if calls == 1 then host.exec("sh", {{ "-c", {script:?}, {ready:?} }}) end
    return "again"
  end,
}})
"#,
            ready = ready.display().to_string(),
        ),
        FakeClock::new(),
    )
}

#[test]
fn a_call_parked_on_exec_returns_once_its_group_is_empty() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ready = fakes::children::Ready::new(dir.path());
    let ext = exec_extension(dir.path(), r#"echo $$ > "$0"; exec sleep 60"#, ready.path());
    let tools = by_name(&ext);
    let cancel = CancelToken::new();
    let first = run_cancellable(&tools["exec"], &cancel);
    let group = ready.wait(WAIT)[0];
    let watchdog = fakes::Watchdog::group(group);
    cancel.cancel();
    assert_eq!(ran(&first), cancelled());
    assert!(
        !fakes::kill_group(group, "0").unwrap(),
        "the group is empty when the call returns"
    );
    watchdog.stand_down(WAIT);
    assert_eq!(text(&ran(&run(&tools["exec"], json!({})))), "again");
}

#[test]
fn a_cancelled_exec_that_ignores_sigterm_returns_only_after_the_kill() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ready = fakes::children::Ready::new(dir.path());
    let out = dir.path().join("out");
    let clock = FakeClock::new();
    let ext = extension(
        dir.path(),
        "exec",
        &format!(
            r#"
fiber.tool("exec", {{
  description = "d", input_schema = {{ type = "object" }},
  effects = {{ effects = {{ "executes" }}, reversible = false }}, timeout = 500,
  run = function()
    host.exec("sh", {{ "-c", {script:?}, {ready:?}, {out:?} }})
    return "ran"
  end,
}})
"#,
            script =
                r#"trap 'echo $$ >> "$0"' TERM; echo $$ > "$0"; while :; do echo x >> "$1"; done"#,
            ready = ready.path().display().to_string(),
            out = out.display().to_string(),
        ),
        clock.clone(),
    );
    let tools = by_name(&ext);
    let cancel = CancelToken::new();
    let first = run_cancellable(&tools["exec"], &cancel);
    let group = ready.wait(WAIT)[0];
    let watchdog = fakes::Watchdog::group(group);
    cancel.cancel();
    // The trap's line: SIGTERM arrived and the group runs on.
    ready.wait(WAIT);
    assert!(
        first.recv_timeout(Duration::from_millis(200)).is_err(),
        "the call returned while its group still ran"
    );
    // Past the SIGKILL 800 ms after the SIGTERM, and past the call's 500 ms
    // deadline and its grace: a cancelled call waits for its run, not its
    // deadline.
    clock.advance(Duration::from_millis(500) + GRACE);
    assert_eq!(ran(&first), cancelled());
    assert!(
        !fakes::kill_group(group, "0").unwrap(),
        "the group is empty when the call returns"
    );
    let len = || std::fs::metadata(&out).unwrap().len();
    let before = len();
    pause(Duration::from_millis(100));
    assert_eq!(len(), before, "nothing writes after the call returned");
    watchdog.stand_down(WAIT);
}

#[test]
fn a_cancelled_call_the_hook_cannot_stop_ends_with_the_vm_abandoned() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let clock = FakeClock::new();
    let ext = extension(
        dir.path(),
        "stuck",
        r#"
fiber.tool("find", {
  description = "d", input_schema = { type = "object" },
  effects = { effects = {}, reversible = true }, timeout = 50,
  run = function()
    require("go_find")
    string.find(string.rep("a", 100000), "a*a*a*a*b")
    host.exec("sh", { "-c", "exit 0" })
    return "found"
  end,
})
fiber.tool("other", {
  description = "d", input_schema = { type = "object" },
  effects = { effects = {}, reversible = true }, timeout = 50,
  run = function() return "other" end,
})
"#,
        clock.clone(),
    );
    let (tx, inbox) = mpsc::channel();
    ext.deliver_to(tx);
    let went = go_module(dir.path(), "find");
    let tools = by_name(&ext);
    let asked = clock.now();
    let cancel = CancelToken::new();
    let first = run_cancellable(&tools["find"], &cancel);
    went.recv_timeout(WAIT)
        .expect("waited for the callback to pass its clock check");
    cancel.cancel();
    let abandon_at = asked + Duration::from_millis(50) + GRACE;
    assert!(
        clock.await_parked(abandon_at, WAIT),
        "waited for the caller to park at the grace"
    );
    clock.advance(Duration::from_millis(50) + GRACE);
    assert_eq!(ran(&first), cancelled());
    let later = ran(&run(&tools["other"], json!({})));
    assert_eq!(
        failed(&later),
        (
            ErrorCode::ToolError,
            "`fiber.test/stuck` is stopped and takes no more calls.".to_owned()
        )
    );
    assert!(matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)));
}

#[test]
fn a_cancelled_parked_call_ends_when_a_stuck_call_abandons_the_vm() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let clock = FakeClock::new();
    let held = hold("late");
    let ext = extension(
        dir.path(),
        "stuck",
        &format!(
            r#"
fiber.tool("wait", {{
  description = "d", input_schema = {{ type = "object" }},
  effects = {{ effects = {{}}, reversible = true }}, timeout = 60000,
  run = function() return host.http({{ url = "{}" }}).body end,
}})
fiber.tool("find", {{
  description = "d", input_schema = {{ type = "object" }},
  effects = {{ effects = {{}}, reversible = true }}, timeout = 50,
  run = function()
    require("go_find")
    string.find(string.rep("a", 100000), "a*a*a*a*b")
  end,
}})
"#,
            held.url
        ),
        clock.clone(),
    );
    let went = go_module(dir.path(), "find");
    let tools = by_name(&ext);
    let cancel = CancelToken::new();
    let waiting = run_cancellable(&tools["wait"], &cancel);
    held.accepted
        .recv_timeout(WAIT)
        .expect("waited for the request to reach the server");
    let asked = clock.now();
    let stuck = run(&tools["find"], json!({}));
    went.recv_timeout(WAIT)
        .expect("waited for the stuck callback to pass its clock check");
    // The thread is stuck in a C call, so it never drops the parked call.
    cancel.cancel();
    assert!(
        waiting.recv_timeout(Duration::from_millis(200)).is_err(),
        "the parked call returned before the thread dropped it"
    );
    let abandon_at = asked + Duration::from_millis(50) + GRACE;
    assert!(
        clock.await_parked(abandon_at, WAIT),
        "waited for the stuck call's caller to park at the grace"
    );
    clock.advance(Duration::from_millis(50) + GRACE);
    assert_eq!(failed(&ran(&stuck)).0, ErrorCode::ToolError);
    assert_eq!(ran(&waiting), cancelled());
}

#[test]
fn a_cancelled_exec_abandoned_with_the_vm_returns_only_once_its_group_is_empty() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ready = fakes::children::Ready::new(dir.path());
    let out = dir.path().join("out");
    let clock = FakeClock::new();
    let ext = extension(
        dir.path(),
        "stuckexec",
        &format!(
            r#"
fiber.tool("exec", {{
  description = "d", input_schema = {{ type = "object" }},
  effects = {{ effects = {{ "executes" }}, reversible = false }}, timeout = 60000,
  run = function()
    host.exec("sh", {{ "-c", {script:?}, {ready:?}, {out:?} }})
    return "ran"
  end,
}})
fiber.tool("find", {{
  description = "d", input_schema = {{ type = "object" }},
  effects = {{ effects = {{}}, reversible = true }}, timeout = 50,
  run = function()
    require("go_find")
    string.find(string.rep("a", 100000), "a*a*a*a*b")
  end,
}})
"#,
            script =
                r#"trap 'echo $$ >> "$0"' TERM; echo $$ > "$0"; while :; do echo x >> "$1"; done"#,
            ready = ready.path().display().to_string(),
            out = out.display().to_string(),
        ),
        clock.clone(),
    );
    let went = go_module(dir.path(), "find");
    let tools = by_name(&ext);
    let cancel = CancelToken::new();
    let first = run_cancellable(&tools["exec"], &cancel);
    let group = ready.wait(WAIT)[0];
    let watchdog = fakes::Watchdog::group(group);
    let asked = clock.now();
    let stuck = run(&tools["find"], json!({}));
    went.recv_timeout(WAIT)
        .expect("waited for the stuck callback to pass its clock check");
    // The stop's SIGKILL bound anchors at the cancel, so the cancel lands
    // 300 ms into the stuck call's 50 ms timeout plus grace: the abandon
    // below does not cross the 800 ms bound early.
    clock.advance(Duration::from_millis(300));
    // The thread is stuck in a C call, so it never drops the parked call.
    cancel.cancel();
    assert!(
        first.recv_timeout(Duration::from_millis(200)).is_err(),
        "the parked call returned before the abandon"
    );
    let abandon_at = asked + Duration::from_millis(50) + GRACE;
    assert!(
        clock.await_parked(abandon_at, WAIT),
        "waited for the stuck call's caller to park at the grace"
    );
    clock.advance(Duration::from_millis(750));
    assert_eq!(failed(&ran(&stuck)).0, ErrorCode::ToolError);
    // Abandoned with the VM: the call is still going while its group runs.
    assert!(
        first.try_recv().is_err(),
        "the call returned at the abandon while its group still ran"
    );
    // The stop reached the run without the Lua thread: SIGTERM arrived,
    // and the group runs on past it.
    ready.wait(WAIT);
    assert!(
        first.recv_timeout(Duration::from_millis(200)).is_err(),
        "the call returned while its group still ran"
    );
    // Past the SIGKILL 800 ms after the SIGTERM: the group is killed and
    // drained before the call returns.
    clock.advance(Duration::from_millis(800));
    assert_eq!(ran(&first), cancelled());
    assert!(
        !fakes::kill_group(group, "0").unwrap(),
        "the group is empty when the call returns"
    );
    let len = || std::fs::metadata(&out).unwrap().len();
    let before = len();
    pause(Duration::from_millis(100));
    assert_eq!(len(), before, "nothing writes after the call returned");
    watchdog.stand_down(WAIT);
}
