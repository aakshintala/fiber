//! A Lua extension's tools through the public API (`docs/extensions.md`,
//! "Registering" and "How an extension runs"), on the fake clock.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::path::Path;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::tool::Tool;
use extensions::{LuaExtension, LuaTool};
use fakes::clock::FakeClock;

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
