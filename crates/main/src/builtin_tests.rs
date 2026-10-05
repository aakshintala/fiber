//! The driver shell `builtin` returns runs a command.

use std::sync::{Arc, Weak};

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::Event;
use contract::shapes::ContentPart;
use contract::tool::Cancel;

struct Never;

impl Cancel for Never {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

struct Quiet;

impl Emit for Quiet {
    fn emit(&self, _event: &Event) {}
}

#[test]
fn the_driver_shell_runs_echo() {
    let root = fakes::TempDir::new("fiber-driver-shell");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let (_tools, _infos, driver, _forget) = super::builtin(root.path(), &clock).unwrap();
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "command".to_owned(),
        serde_json::Value::String("echo hi".to_owned()),
    );
    let output = driver.run(&arguments, &Never, &Quiet);
    let text = output
        .content
        .iter()
        .find_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } | ContentPart::Unknown => None,
        })
        .expect("echo wrote text");
    assert!(text.contains("hi"), "{text}");
    assert_eq!(output.process.expect("echo ran").exit_code, Some(0));
}

#[test]
fn the_forget_callback_clears_what_the_file_tools_have_seen() {
    let root = fakes::TempDir::new("fiber-forget");
    std::fs::write(root.path().join("a.txt"), "old\n").unwrap();
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let (tools, _infos, _driver, forget) = super::builtin(root.path(), &clock).unwrap();
    let tool = |name: &str| {
        tools
            .iter()
            .find(|(_, tool)| tool.definition().name == name)
            .map(|(_, tool)| Arc::clone(tool))
            .unwrap()
    };
    let arguments = |value: serde_json::Value| value.as_object().unwrap().clone();
    let read = arguments(serde_json::json!({"path": "a.txt"}));
    let write = arguments(serde_json::json!({"path": "a.txt", "content": "new\n"}));
    tool("read").run(&read, &Never, &Quiet);

    forget();

    // What was read is forgotten, so the write is stale.
    let output = tool("write").run(&write, &Never, &Quiet);
    assert_eq!(
        output.error.map(|error| error.code),
        Some(contract::ErrorCode::StaleFile)
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("a.txt")).unwrap(),
        "old\n"
    );
}

#[test]
fn without_the_forget_callback_the_same_write_goes_through() {
    let root = fakes::TempDir::new("fiber-no-forget");
    std::fs::write(root.path().join("a.txt"), "old\n").unwrap();
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let (tools, _infos, _driver, _forget) = super::builtin(root.path(), &clock).unwrap();
    let tool = |name: &str| {
        tools
            .iter()
            .find(|(_, tool)| tool.definition().name == name)
            .map(|(_, tool)| Arc::clone(tool))
            .unwrap()
    };
    let arguments = |value: serde_json::Value| value.as_object().unwrap().clone();
    tool("read").run(
        &arguments(serde_json::json!({"path": "a.txt"})),
        &Never,
        &Quiet,
    );

    let output = tool("write").run(
        &arguments(serde_json::json!({"path": "a.txt", "content": "new\n"})),
        &Never,
        &Quiet,
    );

    assert!(output.error.is_none(), "{:?}", output.error);
    assert_eq!(
        std::fs::read_to_string(root.path().join("a.txt")).unwrap(),
        "new\n"
    );
}

#[test]
fn builtin_registers_the_tools_in_name_order_with_handoff_among_them() {
    let root = fakes::TempDir::new("fiber-names");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let (tools, infos, _driver, _forget) = super::builtin(root.path(), &clock).unwrap();

    let names: Vec<String> = tools
        .iter()
        .map(|(by, tool)| {
            assert_eq!(by, "builtin");
            tool.definition().name
        })
        .collect();
    assert_eq!(names, ["edit", "handoff", "read", "shell", "write"]);
    assert_eq!(
        infos
            .iter()
            .map(|info| info.name.as_str())
            .collect::<Vec<_>>(),
        ["edit", "handoff", "read", "shell", "write"]
    );
}
