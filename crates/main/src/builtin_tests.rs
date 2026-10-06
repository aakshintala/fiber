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
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (_tools, _infos, driver, _forget) = super::builtin(
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        None,
    )
    .unwrap();
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
fn read_is_wired_to_the_image_child() {
    let root = fakes::TempDir::new("fiber-read-wired");
    std::fs::write(root.path().join("a.png"), b"\x89PNG\r\n\x1a\nrest").unwrap();
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    // A child that prints the one line a stored image has.
    let fiber = root.path().join("fiber-stub");
    std::fs::write(
        &fiber,
        "#!/bin/sh\nprintf '{\"file\":\"%s.png\",\"mime_type\":\"image/png\",\"width\":1,\"height\":1}\\n' \"$4\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&fiber, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (tools, _infos, _driver, _forget) = super::with_binary(
        fiber,
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        None,
    )
    .unwrap();
    let (_, read) = tools
        .iter()
        .find(|(_, tool)| tool.definition().name == "read")
        .expect("read is registered");
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "path".to_owned(),
        serde_json::Value::String("a.png".to_owned()),
    );
    let output = read.run(&arguments, &Never, &Quiet);
    assert_eq!(output.error, None);
    assert!(
        matches!(
            output.content.get(1),
            Some(contract::shapes::ContentPart::Image { .. })
        ),
        "{:?}",
        output.content
    );
}

#[test]
fn the_forget_callback_clears_what_the_file_tools_have_seen() {
    let root = fakes::TempDir::new("fiber-forget");
    std::fs::write(root.path().join("a.txt"), "old\n").unwrap();
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (tools, _infos, _driver, forget) = super::builtin(
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        None,
    )
    .unwrap();
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
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (tools, _infos, _driver, _forget) = super::builtin(
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        None,
    )
    .unwrap();
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
fn builtin_registers_the_tools_in_name_order_then_jobs() {
    let root = fakes::TempDir::new("fiber-builtin-jobs");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (tools, infos, _driver, _forget) = super::builtin(
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        None,
    )
    .unwrap();
    let names: Vec<String> = tools
        .iter()
        .map(|(who, tool)| {
            assert_eq!(who, "builtin");
            tool.definition().name
        })
        .collect();
    assert_eq!(names, ["edit", "handoff", "read", "shell", "write", "jobs"]);
    let listed: Vec<&str> = infos.iter().map(|info| info.name.as_str()).collect();
    assert_eq!(
        listed,
        ["edit", "handoff", "read", "shell", "write", "jobs"]
    );
}

/// `builtin` serves only `fiber ask`, a non-interactive run, so its
/// model shell allows a monitor 10 minutes at most.
#[test]
fn the_model_shell_is_non_interactive() {
    let root = fakes::TempDir::new("fiber-builtin-monitor");
    let marker = root.path().join("marker");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (tools, _infos, _driver, _forget) = super::builtin(
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        None,
    )
    .unwrap();
    let shell = tools
        .iter()
        .map(|(_, tool)| tool)
        .find(|tool| tool.definition().name == "shell")
        .expect("a shell tool");
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "command".to_owned(),
        serde_json::Value::String(format!("touch {}", marker.display())),
    );
    arguments.insert("monitor".to_owned(), serde_json::Value::Bool(true));
    arguments.insert("deadline_ms".to_owned(), serde_json::json!(700_000));
    let output = shell.run(&arguments, &Never, &Quiet);
    let error = output.error.expect("a refusal");
    assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
    assert!(error.message.contains("600000"), "{}", error.message);
    assert!(!marker.exists());
}

/// The names and hosted types `with_binary` registers for `web_search`.
fn registered_with(web_search: Option<&str>) -> (Vec<(String, Option<String>)>, Vec<String>) {
    let root = fakes::TempDir::new("fiber-hosted-search");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (tools, infos, _driver, _forget) = super::with_binary(
        root.path().join("fiber-stub"),
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        web_search,
    )
    .unwrap();
    let definitions = tools
        .iter()
        .map(|(_, tool)| {
            let definition = tool.definition();
            (definition.name, definition.hosted)
        })
        .collect();
    (
        definitions,
        infos.into_iter().map(|info| info.name).collect(),
    )
}

#[test]
fn a_hosted_search_type_registers_web_search_and_its_info() {
    let (definitions, infos) = registered_with(Some("web_search_20250305"));

    assert!(
        definitions.contains(&(
            "web_search".to_owned(),
            Some("web_search_20250305".to_owned())
        )),
        "{definitions:?}"
    );
    assert!(infos.contains(&"web_search".to_owned()), "{infos:?}");
}

#[test]
fn no_hosted_search_type_registers_no_web_search() {
    let (definitions, infos) = registered_with(None);

    assert!(definitions.iter().all(|(name, _)| name != "web_search"));
    assert!(!infos.contains(&"web_search".to_owned()), "{infos:?}");
}
