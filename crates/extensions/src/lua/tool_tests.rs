//! `fiber.tool`'s registration checks (`docs/extensions.md`, "Registering")
//! and the effects table they share with a call.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::mpsc;

use contract::shapes::{DeclaredEffects, Effect};
use fakes::clock::FakeClock;
use serde_json::json;

use super::*;

/// Wall-clock bound on every wait for the extension.
const WAIT: Duration = Duration::from_secs(5);

/// An extension named `fiber.test/t` whose entry script is `init`, in a
/// fresh temporary directory kept beside it.
fn extension(init: &str) -> (fakes::TempDir, Arc<LuaExtension>) {
    let dir = fakes::TempDir::new("fiber-lua-tool");
    std::fs::write(dir.path().join("init.lua"), init).unwrap();
    let ext = LuaExtension::new(
        "fiber.test/t",
        dir.path(),
        "/nonexistent-fiber-home",
        FakeClock::new(),
    );
    (dir, Arc::new(ext))
}

/// Runs `read` on the extension's registrations on a thread, under `WAIT`.
fn registered<T: Send + 'static>(
    ext: &Arc<LuaExtension>,
    read: impl Fn(&super::super::CallbackTimeouts) -> T + Send + 'static,
) -> T {
    let (tx, rx) = mpsc::channel();
    let ext = Arc::clone(ext);
    std::thread::spawn(move || tx.send(ext.registered(read)));
    rx.recv_timeout(WAIT)
        .expect("waited for the entry script")
        .expect("the entry script ran")
}

fn tools(ext: &Arc<LuaExtension>) -> BTreeMap<String, DeclaredTool> {
    registered(ext, |timeouts| timeouts.tools.clone())
}

fn problems(ext: &Arc<LuaExtension>) -> Vec<String> {
    registered(ext, |timeouts| timeouts.hooks.problems.clone())
}

/// A whole spec as Lua source, with `field` set to `value`, or left out
/// when `value` is empty.
fn spec_with(field: &str, value: &str) -> String {
    let mut fields = vec![
        ("description", "\"d\""),
        ("input_schema", "{ type = \"object\" }"),
        ("effects", "{ effects = {}, reversible = true }"),
        ("timeout", "100"),
        ("run", "function() return \"\" end"),
    ];
    for slot in &mut fields {
        if slot.0 == field {
            slot.1 = value;
        }
    }
    let body: Vec<String> = fields
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, value)| format!("{key} = {value}"))
        .collect();
    format!("{{ {} }}", body.join(", "))
}

#[test]
fn a_tool_registers_its_description_schema_effects_and_timeout() {
    let (_dir, ext) = extension(
        r#"
        fiber.tool("note_count", {
          description = "Counts the notes in a file.",
          input_schema = { type = "object", required = { "path" }, properties = { path = { type = "string" } } },
          effects = { effects = { "reads", "network", "reads" }, paths = { "note.txt" }, reversible = true },
          timeout = 2000,
          run = function() return "3 notes" end,
        })
        "#,
    );
    let tools = tools(&ext);
    let tool = tools.get("note_count").expect("note_count registered");
    assert_eq!(
        tool,
        &DeclaredTool {
            name: "note_count".to_owned(),
            description: "Counts the notes in a file.".to_owned(),
            input_schema: json!({
                "type": "object",
                "required": ["path"],
                "properties": { "path": { "type": "string" } }
            }),
            effects: Some(DeclaredEffects {
                effects: vec![Effect::Reads, Effect::Network],
                reversible: true,
                paths: Some(vec!["note.txt".to_owned()]),
            }),
            timeout: Duration::from_millis(2000),
        }
    );
    assert_eq!(
        serde_json::to_string(&tool.input_schema).unwrap(),
        r#"{"properties":{"path":{"type":"string"}},"required":["path"],"type":"object"}"#,
        "keys sorted, arrays kept in order"
    );
    assert!(problems(&ext).is_empty());
}

#[test]
fn an_effects_function_registers_with_no_static_effects() {
    let (_dir, ext) = extension(&format!(
        "fiber.tool(\"x\", {})\n",
        spec_with("effects", "function() return {} end")
    ));
    assert_eq!(tools(&ext).get("x").unwrap().effects, None);
}

#[test]
fn each_bad_spec_leaves_one_problem_and_the_rest_stand() {
    let schema = "`input_schema` must be a JSON object whose `type` is `object`";
    let timeout = "`timeout` must be a whole number of milliseconds above 0";
    let cases = [
        (
            "description",
            "",
            "`description` must be a non-empty string",
        ),
        (
            "description",
            "\"\"",
            "`description` must be a non-empty string",
        ),
        (
            "description",
            "7",
            "`description` must be a non-empty string",
        ),
        ("input_schema", "", schema),
        ("input_schema", "{}", schema),
        ("input_schema", "{ type = \"array\" }", schema),
        ("input_schema", "\"object\"", schema),
        ("effects", "", "missing `effects`"),
        (
            "effects",
            "\"reads\"",
            "`effects` must be a table or a function",
        ),
        (
            "effects",
            "{ effects = { \"reads\", \"deletes\" }, reversible = true }",
            "`effects`: \"deletes\" is not `reads`, `writes`, `executes` or `network`",
        ),
        (
            "effects",
            "{ effects = { \"reads\" } }",
            "`effects`: missing `reversible`",
        ),
        (
            "effects",
            "{ effects = {}, reversible = true, risky = true }",
            "`effects`: `risky` is not `effects`, `paths` or `reversible`",
        ),
        ("timeout", "", "missing `timeout`"),
        ("timeout", "0", timeout),
        ("timeout", "-5", timeout),
        ("timeout", "1.5", timeout),
        ("timeout", "\"100\"", timeout),
        ("run", "", "`run` must be a function"),
        ("run", "\"go\"", "`run` must be a function"),
    ];
    for (field, value, why) in cases {
        let (_dir, ext) = extension(&format!(
            "fiber.tool(\"bad\", {})\n\
             fiber.tool(\"good\", {})\n\
             fiber.command(\"c\", {{ timeout = 100, run = function() end }})\n",
            spec_with(field, value),
            spec_with("", ""),
        ));
        assert_eq!(
            problems(&ext),
            vec![format!("`bad` tool not registered: {why}")],
            "{field} = {value:?}"
        );
        let names: Vec<String> = tools(&ext).into_keys().collect();
        assert_eq!(names, ["good"], "{field} = {value:?}");
        assert!(
            registered(&ext, |t| t.commands.contains_key("c")),
            "{field} = {value:?}: the command stands"
        );
    }
}

/// The smallest accepted timeout, 1 ms, registers.
#[test]
fn a_timeout_of_one_millisecond_registers() {
    let (_dir, ext) = extension(&format!(
        "fiber.tool(\"x\", {})\n",
        spec_with("timeout", "1")
    ));
    assert_eq!(
        tools(&ext).get("x").unwrap().timeout,
        Duration::from_millis(1)
    );
}

#[test]
fn a_spec_that_is_not_a_table_is_refused() {
    let (_dir, ext) = extension("fiber.tool(\"x\", \"run\")\n");
    assert_eq!(
        problems(&ext),
        [
            "`x` tool not registered: it takes a table of `description`, `input_schema`, `effects`, `timeout` and `run`"
        ]
    );
    assert!(tools(&ext).is_empty());
}

#[test]
fn a_name_is_one_to_128_letters_digits_underscores_and_hyphens() {
    let long = "a".repeat(128);
    let too_long = "a".repeat(129);
    let accepted = [long.as_str(), "a", "Note_count-9"];
    let refused = [too_long.as_str(), "", "a b", "a/b", "a:b", "a.b"];
    let mut init = String::new();
    for name in accepted.iter().chain(&refused) {
        init.push_str(&format!("fiber.tool(\"{name}\", {})\n", spec_with("", "")));
    }
    init.push_str(&format!("fiber.tool(42, {})\n", spec_with("", "")));
    let (_dir, ext) = extension(&init);
    let names: Vec<String> = tools(&ext).into_keys().collect();
    let mut want: Vec<String> = accepted.iter().map(|s| (*s).to_owned()).collect();
    want.sort();
    assert_eq!(names, want);
    let mut expected: Vec<String> = refused
        .iter()
        .map(|name| {
            format!(
                "`{name}` tool not registered: a name is 1 to 128 of `A-Z`, `a-z`, `0-9`, `_` and `-`"
            )
        })
        .collect();
    expected.push("`42` tool not registered: the name must be a string".to_owned());
    assert_eq!(problems(&ext), expected);
}

#[test]
fn a_name_registered_twice_keeps_the_second() {
    let (_dir, ext) = extension(&format!(
        "fiber.tool(\"x\", {})\nfiber.tool(\"x\", {})\n",
        spec_with("description", "\"first\""),
        spec_with("description", "\"second\""),
    ));
    assert_eq!(tools(&ext).get("x").unwrap().description, "second");
}

#[test]
fn fiber_tool_after_the_entry_script_raises_and_registers_nothing() {
    let (_dir, ext) = extension(&format!(
        "fiber.tool(\"first\", {})\n\
         fiber.command(\"late\", {{ timeout = 1000, run = function()\n\
           local ok, e = pcall(function() fiber.tool(\"late\", {}) end)\n\
           return tostring(ok) .. \" \" .. tostring(e)\n\
         end }})\n",
        spec_with("", ""),
        spec_with("", ""),
    ));
    let before: Vec<String> = tools(&ext).into_keys().collect();
    let (tx, rx) = mpsc::channel();
    let caller = Arc::clone(&ext);
    std::thread::spawn(move || tx.send(caller.command("late", "")));
    let said = rx
        .recv_timeout(WAIT)
        .expect("waited for the command")
        .unwrap();
    assert_eq!(
        said,
        "false init.lua:3: fiber.tool: a tool registers only while `init.lua` runs"
    );
    assert_eq!(before, ["first"]);
    let after: Vec<String> = tools(&ext).into_keys().collect();
    assert_eq!(after, before);
    assert!(problems(&ext).is_empty());
}

#[test]
fn effects_from_reads_each_shape_and_refuses_each_bad_one() {
    assert_eq!(
        effects_from(&json!({"effects": [], "reversible": false})),
        Ok(DeclaredEffects {
            effects: Vec::new(),
            reversible: false,
            paths: None,
        })
    );
    assert_eq!(
        effects_from(&json!({
            "effects": ["writes", "executes", "writes"],
            "reversible": true,
            "paths": []
        })),
        Ok(DeclaredEffects {
            effects: vec![Effect::Writes, Effect::Executes],
            reversible: true,
            paths: Some(Vec::new()),
        })
    );
    let refused = [
        (
            json!(["reads"]),
            "it must be a table of `effects`, `paths` and `reversible`",
        ),
        (json!({"reversible": true}), "missing `effects`"),
        (
            json!({"effects": "reads", "reversible": true}),
            "`effects` must be a list",
        ),
        (
            json!({"effects": [1], "reversible": true}),
            "1 is not `reads`, `writes`, `executes` or `network`",
        ),
        (json!({"effects": []}), "missing `reversible`"),
        (
            json!({"effects": [], "reversible": "yes"}),
            "`reversible` must be true or false",
        ),
        (
            json!({"effects": [], "reversible": true, "paths": [1]}),
            "`paths` must be a list of strings",
        ),
        (
            json!({"effects": [], "reversible": true, "paths": "a"}),
            "`paths` must be a list of strings",
        ),
        (
            json!({"effects": [], "reversible": true, "subject": "a"}),
            "`subject` is not `effects`, `paths` or `reversible`",
        ),
    ];
    for (value, why) in refused {
        assert_eq!(effects_from(&value), Err(why.to_owned()), "{value}");
    }
}

// A tool call runs in the gaps of the extension's ordered stream
// (`docs/extensions.md`, "How an extension runs"): it neither waits behind
// a parked stream item nor holds the stream while it is parked.

/// Waits until `check` holds for the hub's state, or `WAIT` passes.
fn until(ext: &LuaExtension, check: impl Fn(&super::super::Shared) -> bool) -> bool {
    let shared = ext.hub.lock();
    ext.hub.wait_for(shared, WAIT, check)
}

/// Whether a started call is parked on a host call.
fn parked(shared: &super::super::Shared) -> bool {
    shared.calls.values().any(|progress| {
        matches!(
            progress,
            super::super::hub::Progress::Started { parked: true, .. }
        )
    })
}

/// Runs `call` on its own thread and hands back its result.
fn on_thread<T: Send + 'static>(call: impl FnOnce() -> T + Send + 'static) -> mpsc::Receiver<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || match tx.send(call()) {
        Ok(()) | Err(mpsc::SendError(_)) => {}
    });
    rx
}

/// `require("go_<name>")` in `dir` signals that the callback has started.
fn go_module(dir: &std::path::Path, name: &str) -> mpsc::Receiver<()> {
    let path = dir.join(format!("go_{name}.lua"));
    let made = std::process::Command::new("mkfifo").arg(&path).status();
    assert!(made.unwrap().success(), "mkfifo {path:?}");
    on_thread(move || {
        let held = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        drop(held);
    })
}

/// A server that accepts one connection, signals, and holds it unanswered
/// until `WAIT` passes, so a `host.http` call stays parked.
fn hold_server() -> (String, mpsc::Receiver<()>) {
    use std::io::Read;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (accepted_tx, accepted) = mpsc::channel();
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
        let (_keep, wait) = mpsc::channel::<()>();
        match wait.recv_timeout(WAIT) {
            Ok(()) | Err(_) => {}
        }
        drop(sock);
    });
    (url, accepted)
}

/// A tool spec as Lua source with `timeout` and `run`.
fn tool_spec(timeout: u64, run: &str) -> String {
    format!(
        "{{ description = \"d\", input_schema = {{ type = \"object\" }}, \
           effects = {{ effects = {{}}, reversible = true }}, timeout = {timeout}, run = {run} }}"
    )
}

const AFTER_TOOL_HOOK: &str = "fiber.hook(\"after_tool\", { timeout = 5000, on_failure = \"non-blocking\",\n\
       run = function() return { content = \"hooked\" } end })\n";

#[test]
fn a_tool_call_completes_while_a_command_is_parked_on_an_ask() {
    let (_dir, ext) = extension(&format!(
        "fiber.command(\"wait\", {{ timeout = 5000, run = function() return tostring(host.ask(\"confirm\", {{ prompt = \"go?\" }}).confirmed) end }})\n\
         fiber.tool(\"quick\", {})\n",
        tool_spec(1000, "function() return \"quick\" end")
    ));
    ext.set_answerable(true);
    let waiting = Arc::clone(&ext);
    let _command = on_thread(move || waiting.command("wait", ""));
    assert!(
        until(&ext, |shared| !shared.asks.is_empty()),
        "waited for the command to ask"
    );
    let quick = Arc::clone(&ext);
    let ran = on_thread(move || quick.tool_run("quick", json!({})));
    assert_eq!(
        ran.recv_timeout(WAIT)
            .expect("the tool call ran while the command waited")
            .unwrap(),
        json!("quick")
    );
    assert_eq!(ext.hub.lock().asks.len(), 1, "the command still waits");
}

#[test]
fn a_hook_runs_to_completion_while_a_tool_call_is_parked() {
    let (url, accepted) = hold_server();
    let (_dir, ext) = extension(&format!(
        "{AFTER_TOOL_HOOK}fiber.tool(\"hold\", {})\n",
        tool_spec(
            5000,
            &format!("function() return host.http({{ url = \"{url}\" }}).body end")
        )
    ));
    let holding = Arc::clone(&ext);
    let _held = on_thread(move || holding.tool_run("hold", json!({})));
    accepted
        .recv_timeout(WAIT)
        .expect("waited for the tool call to reach the server");
    assert!(until(&ext, parked), "waited for the tool call to park");
    let hooked = Arc::clone(&ext);
    let answer = on_thread(move || hooked.hook("after_tool", 0, json!({"content": "x"})));
    assert_eq!(
        answer
            .recv_timeout(WAIT)
            .expect("the hook ran while the tool call was parked")
            .unwrap(),
        json!({"content": "hooked"})
    );
}

/// The VM has one thread: a tool spinning in Lua holds a hook of its
/// extension until the instruction hook stops it at its timeout.
#[test]
fn a_spinning_tool_holds_the_extensions_hook_until_its_timeout() {
    let clock = FakeClock::new();
    let dir = fakes::TempDir::new("fiber-lua-tool");
    std::fs::write(
        dir.path().join("init.lua"),
        format!(
            "{AFTER_TOOL_HOOK}fiber.tool(\"spin\", {})\n",
            tool_spec(100, "function() require(\"go_spin\") while true do end end")
        ),
    )
    .unwrap();
    let went = go_module(dir.path(), "spin");
    let ext = Arc::new(LuaExtension::new(
        "fiber.test/t",
        dir.path(),
        "/nonexistent-fiber-home",
        clock.clone(),
    ));
    let spinning = Arc::clone(&ext);
    let spun = on_thread(move || spinning.tool_run("spin", json!({})));
    went.recv_timeout(WAIT)
        .expect("waited for the tool to spin");
    let hooked = Arc::clone(&ext);
    let answer = on_thread(move || hooked.hook("after_tool", 0, json!({"content": "x"})));
    assert!(
        until(&ext, |shared| shared
            .queue
            .iter()
            .any(|job| matches!(job.target, Target::Hook { .. }))),
        "waited for the hook to queue"
    );
    assert!(
        answer.recv_timeout(Duration::from_millis(200)).is_err(),
        "the hook started while the tool held the thread"
    );
    clock.advance(Duration::from_millis(100));
    let Err(Error::Timeout { callback, .. }) = spun.recv_timeout(WAIT).expect("the tool returned")
    else {
        panic!("the spinning tool did not time out");
    };
    assert_eq!(callback, "spin");
    assert_eq!(
        answer
            .recv_timeout(WAIT)
            .expect("the hook ran once the tool stopped")
            .unwrap(),
        json!({"content": "hooked"})
    );
}
