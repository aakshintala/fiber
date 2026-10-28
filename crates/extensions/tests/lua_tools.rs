//! A Lua extension's tools through the public API (`docs/extensions.md`,
//! "Registering" and "How an extension runs"), on the fake clock.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

mod common;

use fakes::MUST_SUCCEED_WITHIN;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use common::{Setup, go_module, install_lua};
use config::{Config, ProjectKey, Sources};
use contract::ErrorCode;
use contract::clock::Clock;
use contract::hook::Hooks;
use contract::inbox::Delivery;
use contract::shapes::{ContentPart, DeclaredEffects, Effect};
use contract::tool::{Bound, Effects, EffectsError, Output, Tool};
use extensions::{LuaExtension, LuaTool, SessionExtensions};
use fakes::Deadline;
use fakes::clock::FakeClock;
use fakes::{CancelToken, NoLock, ProviderServer, Recorder, Response};
use serde_json::{Map, Value, json};

// debt: 4 copies of this helper across the extensions test binaries, move into common/mod.rs once group 9C1 merges
/// The thread parked at `at`: exactly one thread waits there.
#[track_caller]
fn thread_parked_at(clock: &FakeClock, at: Instant) -> std::thread::ThreadId {
    let mark = clock
        .mark_parked(at, fakes::Deadline::after(MUST_SUCCEED_WITHIN))
        .unwrap_or_else(|| panic!("waited for a park at {at:?} within {MUST_SUCCEED_WITHIN:?}"));
    let threads = mark.threads();
    assert_eq!(
        threads.len(),
        1,
        "only one thread parks at {at:?}: {threads:?}"
    );
    match threads.into_iter().next() {
        Some(id) => id,
        None => panic!("one parked thread"),
    }
}

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

/// The extension's tools, read on a thread under `MUST_SUCCEED_WITHIN`.
#[track_caller]
fn tools(ext: &Arc<LuaExtension>) -> Vec<LuaTool> {
    let (tx, rx) = mpsc::channel();
    let ext = Arc::clone(ext);
    std::thread::spawn(move || tx.send(ext.tools()));
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rx)
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

#[test]
fn each_tool_reports_its_declared_keep_as_its_bound() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ext = extension(
        dir.path(),
        "keep",
        r#"
fiber.tool("both", {
  description = "d", input_schema = { type = "object" },
  effects = { effects = {}, reversible = true }, timeout = 1000,
  keep = { head = 2048, tail = 512 },
  run = function() return "" end,
})
fiber.tool("plain", {
  description = "d", input_schema = { type = "object" },
  effects = { effects = {}, reversible = true }, timeout = 1000,
  run = function() return "" end,
})
"#,
        FakeClock::new(),
    );
    let tools = by_name(&ext);
    assert_eq!(
        tools["both"].bound(),
        Bound {
            start: 2048,
            end: 512
        }
    );
    assert_eq!(tools["plain"].bound(), Bound::DEFAULT);
}

// Calls (`docs/extensions.md`, "How an extension runs"): a tool call runs in
// the gaps of the extension's ordered stream, under its own timeout from
// when Fiber asked.

/// When the hook cannot stop the VM, the caller waits 1 second more.
const GRACE: Duration = Duration::from_secs(1);

/// The extension's tools by name, each shareable with a calling thread.
#[track_caller]
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

/// The call's result, under `MUST_SUCCEED_WITHIN`.
#[track_caller]
fn ran(rx: &mpsc::Receiver<Output>, wait: &Deadline) -> Output {
    wait.recv(rx).expect("waited for the tool call")
}

/// `tool`'s effects for `arguments`, on a thread under `MUST_SUCCEED_WITHIN`.
#[track_caller]
fn effects(tool: &Arc<LuaTool>, arguments: Value) -> Result<Effects, EffectsError> {
    let tool = Arc::clone(tool);
    fakes::within(
        "the effects call",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || tool.effects(&args(arguments)),
    )
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

/// Runs the command `name` on a thread under `MUST_SUCCEED_WITHIN`.
#[track_caller]
fn command(ext: &Arc<LuaExtension>, name: &str) -> String {
    let ext = Arc::clone(ext);
    let name = name.to_owned();
    fakes::within(
        "the command",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || ext.command(&name, ""),
    )
    .expect("the command ran")
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
                confined: false,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
            hosts: None,
        })
    );
    assert_eq!(command(&ext, "count"), "0");
    assert_eq!(
        text(&ran(
            &run(still, json!({})),
            &Deadline::after(MUST_SUCCEED_WITHIN)
        )),
        "ran"
    );
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
                    confined: false,
                },
                subject: Some(String::new()),
                prefix: None,
                always_reviewed: false,
                hosts: None,
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
    let asked = clock.now();
    let (tx, rx) = mpsc::channel();
    let call = std::thread::spawn(move || {
        let result = slow.effects(&Map::new());
        tx.send(()).unwrap();
        result
    });
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&went)
        .expect("waited for the effects function to start");
    assert!(
        clock.await_parked(
            asked + Duration::from_millis(100) + GRACE,
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "waited for the effects call to park at its grace"
    );
    clock.advance_when_parked(
        Some(asked + Duration::from_millis(100) + GRACE),
        None,
        Duration::from_millis(100),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    assert_eq!(
        {
            Deadline::after(MUST_SUCCEED_WITHIN).recv(&rx).expect("waited for the effects call");
            call.join().expect("effects thread panicked")
        },
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
    let call = |name: &str| {
        ran(
            &run(&tools[name], json!({})),
            &Deadline::after(MUST_SUCCEED_WITHIN),
        )
    };
    let plain = call("plain");
    assert_eq!((text(&plain), plain.error), ("3 notes".to_owned(), None));
    let part = call("part");
    assert_eq!((text(&part), part.error), ("hello".to_owned(), None));
    assert_eq!(
        ran(
            &run(&tools["args"], json!({ "path": "note.txt" })),
            &Deadline::after(MUST_SUCCEED_WITHIN)
        )
        .content,
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
            "colour` of `fiber.test/a` returned `colour`, which is not `content`, `details`, `error`, `control` or `hosts_surfaced`",
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
    let asked = clock.now();
    let first = run(&tools["spin"], json!({}));
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&went)
        .expect("waited for spin to start");
    assert!(
        clock.await_parked(
            asked + Duration::from_millis(100) + GRACE,
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "waited for spin to park at its grace"
    );
    clock.advance_when_parked(
        Some(asked + Duration::from_millis(100) + GRACE),
        None,
        Duration::from_millis(100),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    assert_eq!(
        failed(&ran(&first, &Deadline::after(MUST_SUCCEED_WITHIN))),
        (
            ErrorCode::Timeout,
            "`fiber.test/slow`: `spin` passed its 100 ms timeout and was stopped.".to_owned()
        )
    );
    assert_eq!(
        text(&ran(
            &run(&tools["spin"], json!({})),
            &Deadline::after(MUST_SUCCEED_WITHIN)
        )),
        "again"
    );
}

#[test]
fn a_run_parked_on_a_host_call_past_its_timeout_fails_timeout_and_the_next_call_runs() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let clock = FakeClock::new();
    let held = ProviderServer::start([Response::status(200, "late")]).unwrap();
    held.hold();
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
            held.url()
        ),
        clock.clone(),
    );
    let tools = by_name(&ext);
    let deadline = clock.now().checked_add(Duration::from_millis(100)).unwrap();
    let first = run(&tools["wait"], json!({}));
    assert!(
        held.await_requests(1, Deadline::after(MUST_SUCCEED_WITHIN)),
        "waited for the request to reach the server"
    );
    // The caller of a parked call waits to the deadline plus the grace,
    // so the callback is parked under its deadline.
    assert!(
        clock.await_parked(
            deadline.checked_add(GRACE).unwrap(),
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "waited for the caller to see the call parked"
    );
    let scheduler = thread_parked_at(&clock, deadline);
    clock.advance_when_parked(
        Some(deadline),
        Some(scheduler),
        Duration::from_millis(100),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    assert_eq!(
        failed(&ran(&first, &Deadline::after(MUST_SUCCEED_WITHIN))),
        (
            ErrorCode::Timeout,
            "`fiber.test/slow`: `wait` passed its 100 ms timeout and was stopped.".to_owned()
        )
    );
    assert_eq!(
        text(&ran(
            &run(&tools["wait"], json!({})),
            &Deadline::after(MUST_SUCCEED_WITHIN)
        )),
        "again"
    );
}

#[test]
fn two_calls_parked_on_host_calls_both_complete() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let one = ProviderServer::start([Response::status(200, "one")]).unwrap();
    one.hold();
    let two = ProviderServer::start([Response::status(200, "two")]).unwrap();
    two.hold();
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
    let first = run(&tools["fetch"], json!({ "url": one.url() }));
    assert!(
        one.await_requests(1, Deadline::after(MUST_SUCCEED_WITHIN)),
        "waited for the first request"
    );
    let second = run(&tools["fetch"], json!({ "url": two.url() }));
    assert!(
        two.await_requests(1, Deadline::after(MUST_SUCCEED_WITHIN)),
        "waited for the second request while the first is parked"
    );
    two.release();
    assert_eq!(
        text(&ran(&second, &Deadline::after(MUST_SUCCEED_WITHIN))),
        "two"
    );
    one.release();
    assert_eq!(
        text(&ran(&first, &Deadline::after(MUST_SUCCEED_WITHIN))),
        "one"
    );
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
    assert_eq!(
        text(&ran(
            &run(runs, json!({})),
            &Deadline::after(MUST_SUCCEED_WITHIN)
        )),
        "0"
    );
    // The run's `extension_exec` would be sent before its reply resumes
    // the callback, so it would be here by now.
    assert!(matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)));
    assert!(effects(runs, json!({})).is_ok());
    match Deadline::after(MUST_SUCCEED_WITHIN).recv(&inbox) {
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
    let said = text(&ran(
        &run(&tools["refresh"], json!({})),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    ));
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

/// The session's extensions with these config overrides, loaded on a
/// thread under `MUST_SUCCEED_WITHIN`.
#[track_caller]
fn session_with(setup: &Setup, overrides: &[&str]) -> Arc<SessionExtensions> {
    let config = Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("p").unwrap(),
        overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
    })
    .unwrap();
    let home = setup.home();
    let loaded = fakes::within(
        "the extensions to load",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
            SessionExtensions::load(&home, &config, FakeClock::new(), locks, None)
        },
    );
    Arc::new(loaded)
}

/// The session's extensions, loaded on a thread under `MUST_SUCCEED_WITHIN`.
#[track_caller]
fn session(setup: &Setup) -> Arc<SessionExtensions> {
    session_with(setup, &[])
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
fn a_disabled_tool_is_not_declared() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "a",
        &json!({"replaces": []}),
        &format!("{}{}", tool_line("one", "a"), tool_line("two", "a")),
    );
    let session = session_with(
        &setup,
        &[r#"extensions."fiber.test/a".tools.disabled=["one"]"#],
    );
    assert_eq!(
        declared(&session),
        [("fiber.test/a".to_owned(), "two".to_owned())]
    );
    assert_eq!(session.notices(), []);
}

#[test]
fn an_enabled_list_declares_only_those_tools() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "a",
        &json!({"replaces": []}),
        &format!("{}{}", tool_line("one", "a"), tool_line("two", "a")),
    );
    let session = session_with(
        &setup,
        &[r#"extensions."fiber.test/a".tools.enabled=["one", "missing"]"#],
    );
    assert_eq!(
        declared(&session),
        [("fiber.test/a".to_owned(), "one".to_owned())]
    );
    assert_eq!(session.notices(), []);
}

#[test]
fn disabled_wins_over_enabled() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "a",
        &json!({"replaces": []}),
        &format!("{}{}", tool_line("one", "a"), tool_line("two", "a")),
    );
    let session = session_with(
        &setup,
        &[
            r#"extensions."fiber.test/a".tools.enabled=["one", "two"]"#,
            r#"extensions."fiber.test/a".tools.disabled=["two"]"#,
        ],
    );
    assert_eq!(
        declared(&session),
        [("fiber.test/a".to_owned(), "one".to_owned())]
    );
    assert_eq!(session.notices(), []);
}

#[test]
fn an_empty_enabled_list_declares_no_tool() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "a",
        &json!({"replaces": []}),
        &format!("{}{}", tool_line("one", "a"), tool_line("two", "a")),
    );
    let session = session_with(&setup, &[r#"extensions."fiber.test/a".tools.enabled=[]"#]);
    assert!(declared(&session).is_empty());
    assert_eq!(session.notices(), []);
}

#[test]
fn a_disabled_name_no_tool_has_is_ignored() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "a",
        &json!({"replaces": []}),
        &format!("{}{}", tool_line("one", "a"), tool_line("two", "a")),
    );
    let session = session_with(
        &setup,
        &[r#"extensions."fiber.test/a".tools.disabled=["missing"]"#],
    );
    assert_eq!(
        declared(&session),
        [
            ("fiber.test/a".to_owned(), "one".to_owned()),
            ("fiber.test/a".to_owned(), "two".to_owned())
        ]
    );
    assert_eq!(session.notices(), []);
}

#[test]
fn a_disabled_tool_takes_no_part_in_a_clash() {
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
    let session = session_with(
        &setup,
        &[r#"extensions."fiber.test/a".tools.disabled=["dup"]"#],
    );
    assert_eq!(
        declared(&session),
        [
            ("fiber.test/a".to_owned(), "only_a".to_owned()),
            ("fiber.test/b".to_owned(), "dup".to_owned()),
            ("fiber.test/b".to_owned(), "only_b".to_owned())
        ]
    );
    assert_eq!(session.notices(), []);
}

#[test]
fn reloading_with_a_disabled_list_declares_one_fewer_tool() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "a",
        &json!({"replaces": []}),
        &format!("{}{}", tool_line("one", "a"), tool_line("two", "a")),
    );
    let first = session_with(&setup, &[]);
    assert_eq!(declared(&first).len(), 2);
    let second = session_with(
        &setup,
        &[r#"extensions."fiber.test/a".tools.disabled=["one"]"#],
    );
    assert_eq!(
        declared(&second),
        [("fiber.test/a".to_owned(), "two".to_owned())]
    );
    assert_eq!(second.notices(), []);
}

#[test]
fn an_extension_whose_tools_are_all_disabled_but_with_a_command_stays_loaded() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "a",
        &json!({"replaces": []}),
        &format!("{}{HOOK_AND_COMMAND}", tool_line("one", "a")),
    );
    let session = session_with(
        &setup,
        &[r#"extensions."fiber.test/a".tools.disabled=["one"]"#],
    );
    assert!(declared(&session).is_empty());
    assert_eq!(loaded(&session), ["fiber.test/a"]);
    assert!(session.commands().iter().any(|c| c.name == "mine"));
    assert!(session.has_hooks());
    assert_eq!(session.notices(), []);
}

#[test]
fn an_extension_with_only_disabled_tools_declares_nothing_but_stays_listed() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "a",
        &json!({"replaces": []}),
        &tool_line("one", "a"),
    );
    let session = session_with(
        &setup,
        &[r#"extensions."fiber.test/a".tools.disabled=["one"]"#],
    );
    assert!(declared(&session).is_empty());
    // The load record stays: the `has_tools` rule only drops the live VM
    // from delivery, which has no other public signal.
    assert_eq!(loaded(&session), ["fiber.test/a"]);
    assert!(session.commands().is_empty());
    assert!(!session.has_hooks());
    assert_eq!(session.notices(), []);
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
    fakes::within(
        "the effects call",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || probe.effects(&Map::new()),
    )
    .expect("the effects function ran");
    match Deadline::after(MUST_SUCCEED_WITHIN).recv(&inbox) {
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
#[track_caller]
fn pause(bound: Duration) {
    let (_keep, never) = mpsc::channel::<()>();
    assert!(Deadline::after(bound).recv(&never).is_err());
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
    let asked = clock.now();
    let spinning = run(&tools["spin"], json!({}));
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&went)
        .expect("waited for spin to start");
    // The thread spins, so this call stays queued.
    let cancel = CancelToken::new();
    let queued = run_cancellable(&tools["count"], &cancel);
    cancel.cancel();
    assert_eq!(
        ran(&queued, &Deadline::after(MUST_SUCCEED_WITHIN)),
        cancelled()
    );
    assert!(
        clock.await_parked(
            asked + Duration::from_millis(100) + GRACE,
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "waited for spin to park at its grace"
    );
    clock.advance_when_parked(
        Some(asked + Duration::from_millis(100) + GRACE),
        None,
        Duration::from_millis(100),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    assert_eq!(
        failed(&ran(&spinning, &Deadline::after(MUST_SUCCEED_WITHIN))).0,
        ErrorCode::Timeout
    );
    assert_eq!(
        text(&ran(
            &run(&tools["count"], json!({})),
            &Deadline::after(MUST_SUCCEED_WITHIN)
        )),
        "1"
    );
}

#[test]
fn a_call_parked_on_a_host_call_is_cancelled_without_the_clock_moving() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let held = ProviderServer::start([Response::status(200, "late")]).unwrap();
    held.hold();
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
            held.url()
        ),
        FakeClock::new(),
    );
    let tools = by_name(&ext);
    let cancel = CancelToken::new();
    let first = run_cancellable(&tools["wait"], &cancel);
    assert!(
        held.await_requests(1, Deadline::after(MUST_SUCCEED_WITHIN)),
        "waited for the request to reach the server"
    );
    cancel.cancel();
    assert_eq!(
        ran(&first, &Deadline::after(MUST_SUCCEED_WITHIN)),
        cancelled()
    );
    assert_eq!(
        text(&ran(
            &run(&tools["wait"], json!({})),
            &Deadline::after(MUST_SUCCEED_WITHIN)
        )),
        "again"
    );
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
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&went)
        .expect("waited for spin to start");
    cancel.cancel();
    assert_eq!(
        ran(&first, &Deadline::after(MUST_SUCCEED_WITHIN)),
        cancelled()
    );
    // The interrupt is cleared: the next call runs to its own result.
    assert_eq!(
        text(&ran(
            &run(&tools["spin"], json!({})),
            &Deadline::after(MUST_SUCCEED_WITHIN)
        )),
        "again"
    );
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
    let group = ready.wait(fakes::Deadline::after(MUST_SUCCEED_WITHIN))[0];
    let watchdog = fakes::Watchdog::group(group);
    cancel.cancel();
    assert_eq!(
        ran(&first, &Deadline::after(MUST_SUCCEED_WITHIN)),
        cancelled()
    );
    assert!(
        !fakes::kill_group(group, "0").unwrap(),
        "the group is empty when the call returns"
    );
    watchdog.stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
    assert_eq!(
        text(&ran(
            &run(&tools["exec"], json!({})),
            &Deadline::after(MUST_SUCCEED_WITHIN)
        )),
        "again"
    );
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
    let group = ready.wait(fakes::Deadline::after(MUST_SUCCEED_WITHIN))[0];
    let watchdog = fakes::Watchdog::group(group);
    let cancelled_at = clock.now();
    cancel.cancel();
    // The trap's line: SIGTERM arrived and the group runs on.
    ready.wait(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
    assert!(
        Deadline::after(Duration::from_millis(200))
            .recv(&first)
            .is_err(),
        "the call returned while its group still ran"
    );
    // The SIGKILL lands 800 ms after the SIGTERM: the exec parks at its
    // kill bound, and a cancelled call waits for its run, not its deadline.
    let kill_at = cancelled_at + support::group::GRACE;
    let exec_thread = thread_parked_at(&clock, kill_at);
    clock.advance_when_parked(
        Some(kill_at),
        Some(exec_thread),
        support::group::GRACE,
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    assert_eq!(
        ran(&first, &Deadline::after(MUST_SUCCEED_WITHIN)),
        cancelled()
    );
    assert!(
        !fakes::kill_group(group, "0").unwrap(),
        "the group is empty when the call returns"
    );
    let len = || std::fs::metadata(&out).unwrap().len();
    let before = len();
    pause(Duration::from_millis(100));
    assert_eq!(len(), before, "nothing writes after the call returned");
    watchdog.stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
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
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&went)
        .expect("waited for the callback to pass its clock check");
    cancel.cancel();
    let abandon_at = asked + Duration::from_millis(50) + GRACE;
    assert!(
        clock.await_parked(abandon_at, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "waited for the caller to park at the grace"
    );
    clock.advance_when_parked(
        Some(abandon_at),
        None,
        Duration::from_millis(50) + GRACE,
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    assert_eq!(
        ran(&first, &Deadline::after(MUST_SUCCEED_WITHIN)),
        cancelled()
    );
    let later = ran(
        &run(&tools["other"], json!({})),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
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
    let held = ProviderServer::start([Response::status(200, "late")]).unwrap();
    held.hold();
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
            held.url()
        ),
        clock.clone(),
    );
    let went = go_module(dir.path(), "find");
    let tools = by_name(&ext);
    let cancel = CancelToken::new();
    let waiting = run_cancellable(&tools["wait"], &cancel);
    assert!(
        held.await_requests(1, Deadline::after(MUST_SUCCEED_WITHIN)),
        "waited for the request to reach the server"
    );
    let asked = clock.now();
    let stuck = run(&tools["find"], json!({}));
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&went)
        .expect("waited for the stuck callback to pass its clock check");
    // The thread is stuck in a C call, so it never drops the parked call.
    cancel.cancel();
    assert!(
        Deadline::after(Duration::from_millis(200))
            .recv(&waiting)
            .is_err(),
        "the parked call returned before the thread dropped it"
    );
    let abandon_at = asked + Duration::from_millis(50) + GRACE;
    assert!(
        clock.await_parked(abandon_at, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "waited for the stuck call's caller to park at the grace"
    );
    let stuck_caller = thread_parked_at(&clock, abandon_at);
    clock.advance_when_parked(
        Some(abandon_at),
        Some(stuck_caller),
        Duration::from_millis(50) + GRACE,
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    assert_eq!(
        failed(&ran(&stuck, &Deadline::after(MUST_SUCCEED_WITHIN))).0,
        ErrorCode::ToolError
    );
    assert_eq!(
        ran(&waiting, &Deadline::after(MUST_SUCCEED_WITHIN)),
        cancelled()
    );
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
    let group = ready.wait(fakes::Deadline::after(MUST_SUCCEED_WITHIN))[0];
    let watchdog = fakes::Watchdog::group(group);
    let asked = clock.now();
    let abandon_at = asked + Duration::from_millis(50) + GRACE;
    let stuck = run(&tools["find"], json!({}));
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&went)
        .expect("waited for the stuck callback to pass its clock check");
    // The stop's SIGKILL bound anchors at the cancel, so the cancel lands
    // 300 ms into the stuck call's 50 ms timeout plus grace: the abandon
    // below does not cross the 800 ms bound early. The stuck caller is
    // named: the exec run parks on its child, not the clock.
    let stuck_caller = thread_parked_at(&clock, abandon_at);
    clock.advance_when_parked(
        Some(abandon_at),
        Some(stuck_caller),
        Duration::from_millis(300),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    // The thread is stuck in a C call, so it never drops the parked call.
    let cancelled_at = clock.now();
    cancel.cancel();
    // The stop notices the dropped run on its next poll and parks the exec
    // at the kill bound; this read proves that pass ran at 300 ms, so the
    // 750 ms advance below cannot cross the bound early.
    let exec_thread = thread_parked_at(&clock, cancelled_at + support::group::GRACE);
    assert!(
        Deadline::after(Duration::from_millis(200))
            .recv(&first)
            .is_err(),
        "the parked call returned before the abandon"
    );
    let kill_at = cancelled_at + support::group::GRACE;
    assert!(
        clock.await_parked(abandon_at, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "waited for the stuck call's caller to park at the grace"
    );
    clock.advance_when_parked(
        Some(abandon_at),
        Some(stuck_caller),
        Duration::from_millis(750),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    assert_eq!(
        failed(&ran(&stuck, &Deadline::after(MUST_SUCCEED_WITHIN))).0,
        ErrorCode::ToolError
    );
    // Abandoned with the VM: the call is still going while its group runs.
    assert!(
        first.try_recv().is_err(),
        "the call returned at the abandon while its group still ran"
    );
    // The stop reached the run without the Lua thread: SIGTERM arrived,
    // and the group runs on past it.
    ready.wait(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
    assert!(
        Deadline::after(Duration::from_millis(200))
            .recv(&first)
            .is_err(),
        "the call returned while its group still ran"
    );
    // The SIGKILL 800 ms after the SIGTERM: the group is killed and
    // drained before the call returns.
    clock.advance_when_parked(
        Some(kill_at),
        Some(exec_thread),
        Duration::from_millis(50),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    assert_eq!(
        ran(&first, &Deadline::after(MUST_SUCCEED_WITHIN)),
        cancelled()
    );
    assert!(
        !fakes::kill_group(group, "0").unwrap(),
        "the group is empty when the call returns"
    );
    let len = || std::fs::metadata(&out).unwrap().len();
    let before = len();
    pause(Duration::from_millis(100));
    assert_eq!(len(), before, "nothing writes after the call returned");
    watchdog.stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
}

/// `host.tool` through the public API (`docs/extensions.md`, "Running a
/// tool"): a fake `Ask` hands its runner to the hub, and the script sees
/// the inner call's outcome.
///
/// An `Ask` whose inner calls run through `runner`, when it has one.
struct FakeAsk {
    runner: Option<Arc<dyn contract::tool::InnerCalls>>,
}

impl contract::tool::Ask for FakeAsk {
    fn action(&self) -> contract::ActionId {
        contract::ActionId("a_test".to_owned())
    }

    fn answerable(&self) -> bool {
        false
    }

    fn ask(&self, _asking: contract::tool::Asking) -> contract::tool::Answered {
        contract::tool::Answered::NoAnswer
    }

    fn inner_calls(&self) -> Option<Arc<dyn contract::tool::InnerCalls>> {
        self.runner.clone()
    }
}

/// A runner that records each call and completes it with `"file text"`.
#[derive(Default)]
struct ScriptRunner {
    calls: std::sync::Mutex<Vec<(String, String, Value)>>,
}

impl contract::tool::InnerCalls for ScriptRunner {
    fn run(
        &self,
        extension: &str,
        name: &str,
        arguments: Value,
    ) -> Result<Box<contract::events::ToolCallCompleted>, String> {
        self.inner()
            .push((extension.to_owned(), name.to_owned(), arguments));
        Ok(Box::new(contract::events::ToolCallCompleted {
            status: contract::events::CallStatus::Completed,
            reason: None,
            error: None,
            process: None,
            content: vec![ContentPart::Text {
                text: "file text".to_owned(),
            }],
            details: None,
            artifact: None,
            kept: None,
            changes: None,
            control: None,
            changed_by: None,
            provider_item: None,
            hosts_surfaced: Vec::new(),
            loaded: None,
        }))
    }
}

impl ScriptRunner {
    fn inner(&self) -> std::sync::MutexGuard<'_, Vec<(String, String, Value)>> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Runs `tool` through `run_asking` with `ask` on its own thread.
fn run_asking(tool: &Arc<LuaTool>, ask: FakeAsk, arguments: Value) -> mpsc::Receiver<Output> {
    let (tx, rx) = mpsc::channel();
    let tool = Arc::clone(tool);
    std::thread::spawn(move || {
        let output = tool.run_asking(
            &args(arguments),
            &CancelToken::new(),
            &Recorder::default(),
            &ask,
        );
        match tx.send(output) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    rx
}

const FETCH: &str = r#"
fiber.tool("fetch", {
  description = "Fetches a file.",
  input_schema = { type = "object" },
  effects = { effects = { "reads" }, reversible = true },
  timeout = 2000,
  run = function() local o = host.tool("read", { path = "x" }) return o.content[1].text end,
})
"#;

#[test]
fn run_asking_runs_the_script_and_hands_the_runner_to_the_hub() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ext = extension(dir.path(), "fetch", FETCH, FakeClock::new());
    let tools = by_name(&ext);
    let runner = Arc::new(ScriptRunner::default());
    let ask = FakeAsk {
        runner: Some(Arc::clone(&runner) as Arc<dyn contract::tool::InnerCalls>),
    };
    let output = ran(
        &run_asking(&tools["fetch"], ask, json!({})),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    assert_eq!(text(&output), "file text");
    // The runner saw the extension's name, the tool and the arguments.
    assert_eq!(
        runner.inner().clone(),
        vec![(
            "fiber.test/fetch".to_owned(),
            "read".to_owned(),
            json!({"path": "x"})
        )]
    );
}

#[test]
fn run_without_a_runner_fails_naming_the_missing_anchor() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ext = extension(dir.path(), "fetch", FETCH, FakeClock::new());
    let tools = by_name(&ext);
    let (code, message) = failed(&ran(
        &run(&tools["fetch"], json!({})),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    ));
    assert_eq!(code, ErrorCode::ToolError);
    assert!(
        message.contains("called outside a running tool call"),
        "the error names the missing anchor: {message}"
    );
}

#[test]
fn run_asking_without_a_runner_fails_the_same_way() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ext = extension(dir.path(), "fetch", FETCH, FakeClock::new());
    let tools = by_name(&ext);
    let output = ran(
        &run_asking(&tools["fetch"], FakeAsk { runner: None }, json!({})),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    let (code, message) = failed(&output);
    assert_eq!(code, ErrorCode::ToolError);
    assert!(
        message.contains("called outside a running tool call"),
        "the error names the missing anchor: {message}"
    );
}

#[test]
fn run_asking_cancelled_returns_no_content_and_no_error() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ext = extension(dir.path(), "fetch", FETCH, FakeClock::new());
    let tools = by_name(&ext);
    let runner = Arc::new(ScriptRunner::default());
    let ask = FakeAsk {
        runner: Some(Arc::clone(&runner) as Arc<dyn contract::tool::InnerCalls>),
    };
    let cancel = CancelToken::new();
    cancel.cancel();
    let output = tools["fetch"].run_asking(&Map::new(), &cancel, &Recorder::default(), &ask);
    assert!(output.content.is_empty(), "no content: {output:?}");
    assert_eq!(output.error, None, "no error: {output:?}");
    assert!(runner.inner().is_empty(), "the call never ran");
}

// Declared and surfaced hosts (`docs/tools.md`, "What a tool declares"):
// a static table and an effects function both carry `hosts`, and a `run`
// result carries `hosts_surfaced`.

const HOSTS: &str = r#"
fiber.tool("static_hosts", {
  description = "d", input_schema = { type = "object" },
  effects = { effects = { "network" }, hosts = { "https://a.test/x" }, reversible = true },
  timeout = 1000,
  run = function() return "" end,
})
fiber.tool("no_hosts", {
  description = "d", input_schema = { type = "object" },
  effects = { effects = { "network" }, reversible = true },
  timeout = 1000,
  run = function() return "" end,
})
fiber.tool("empty_hosts", {
  description = "d", input_schema = { type = "object" },
  effects = { effects = { "network" }, hosts = {}, reversible = true },
  timeout = 1000,
  run = function() return "" end,
})
fiber.tool("per_arg", {
  description = "d", input_schema = { type = "object" },
  effects = function(args) return { effects = { "network" }, hosts = { args.url }, reversible = true } end,
  timeout = 1000,
  run = function() return "" end,
})
fiber.tool("bad_hosts", {
  description = "d", input_schema = { type = "object" },
  effects = function() return { effects = { "network" }, hosts = { "a.test" }, reversible = true } end,
  timeout = 1000,
  run = function() return "" end,
})
fiber.tool("surfaced", {
  description = "d", input_schema = { type = "object" },
  effects = { effects = { "reads" }, reversible = true },
  timeout = 1000,
  run = function() return { content = "found", hosts_surfaced = { "https://x.test/a" } } end,
})
"#;

#[test]
fn a_static_hosts_list_reaches_effects_as_declared() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ext = extension(dir.path(), "hosts", HOSTS, FakeClock::new());
    let tools = by_name(&ext);
    let output = effects(&tools["static_hosts"], json!({})).expect("static effects read");
    assert_eq!(
        output.hosts,
        Some(vec!["https://a.test/x".to_owned()]),
        "kept as written, not normalised"
    );
    assert_eq!(output.declared.effects, vec![Effect::Network]);
}

#[test]
fn no_hosts_is_unknown_and_an_empty_list_contacts_no_host() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ext = extension(dir.path(), "hosts", HOSTS, FakeClock::new());
    let tools = by_name(&ext);
    assert_eq!(effects(&tools["no_hosts"], json!({})).unwrap().hosts, None);
    assert_eq!(
        effects(&tools["empty_hosts"], json!({})).unwrap().hosts,
        Some(Vec::new())
    );
}

#[test]
fn an_effects_function_returns_hosts_per_argument() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ext = extension(dir.path(), "hosts", HOSTS, FakeClock::new());
    let tools = by_name(&ext);
    for url in ["https://a.test/1", "https://b.test/2"] {
        assert_eq!(
            effects(&tools["per_arg"], json!({ "url": url }))
                .unwrap()
                .hosts,
            Some(vec![url.to_owned()]),
            "{url}"
        );
    }
}

#[test]
fn an_effects_function_returning_a_bad_hosts_list_fails_naming_the_tool() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ext = extension(dir.path(), "hosts", HOSTS, FakeClock::new());
    let tools = by_name(&ext);
    assert_eq!(
        effects(&tools["bad_hosts"], json!({})),
        Err(EffectsError::Tool(
            "the effects function of the tool `bad_hosts` of `fiber.test/hosts` returned effects that do not read: `hosts` must be a list of http or https URLs"
                .to_owned()
        ))
    );
}

#[test]
fn a_run_result_carries_hosts_surfaced() {
    let dir = fakes::TempDir::new("fiber-lua-tools");
    let ext = extension(dir.path(), "hosts", HOSTS, FakeClock::new());
    let tools = by_name(&ext);
    let output = ran(
        &run(&tools["surfaced"], json!({})),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    assert_eq!(text(&output), "found");
    assert_eq!(output.error, None);
    assert_eq!(output.hosts_surfaced, vec!["https://x.test/a".to_owned()]);
}
