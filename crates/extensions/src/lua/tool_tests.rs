//! `fiber.tool`'s registration checks (`docs/extensions.md`, "Registering")
//! and the effects table they share with a call.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

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
