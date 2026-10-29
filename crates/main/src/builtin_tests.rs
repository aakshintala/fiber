//! The driver shell `builtin` returns runs a command.

use std::sync::{Arc, Weak};
use std::time::Duration;

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::Event;
use contract::tool::Cancel;
use fakes::Deadline;
use fakes::NoSkills;

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

/// The delegates the `builtin` calls declare tools for: no model
/// resolves, so `delegate_spawn` is declared but never runs one.
pub(super) fn delegates() -> crate::delegates::Delegates {
    let root = fakes::TempDir::new("fiber-builtin-delegate");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    crate::delegates::Delegates::new(
        root.path().join("fiber-stub"),
        root.path().join("home"),
        contract::SessionId("s_test".into()),
        root.path().to_path_buf(),
        root.path().join("sessions"),
        jobs,
        clock,
        jobs::Resolve {
            lookup: Arc::new(|_| Err(String::new())),
            catalogue: Arc::from(""),
            credential: Arc::new(|_| Ok(None)),
        },
    )
}

pub(super) fn skills() -> Arc<dyn contract::skills::Skills> {
    Arc::new(NoSkills)
}
/// How long a test waits for one builtin tool call, in real time.
///
/// Each wrapped call starts a child (bash for the driver shell; the image
/// child for `read`), and a fresh exec can stall for seconds on macOS
/// (`docs/testing.md`, "Waits and timeouts"). One call per test = 10 s,
/// within half of nextest's 120 s kill. A passing run never waits on it;
/// it only bounds a hang.
const CALL_WITHIN: Duration = Duration::from_secs(10);

/// Runs one builtin tool call on its own thread and returns its output.
/// Calling code that blocks is a wait too (`docs/testing.md`, "Waits and
/// timeouts"): on expiry the test fails naming the call.
fn ran(
    tool: Arc<dyn contract::tool::Tool>,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> contract::tool::Output {
    let name = tool.definition().name;
    fakes::within(
        &format!("the {name} call"),
        Deadline::after(CALL_WITHIN),
        move || tool.run(&arguments, &Never, &Quiet),
    )
}

#[test]
fn read_is_wired_to_the_image_child() {
    let root = fakes::TempDir::new("fiber-read-wired");
    std::fs::write(root.path().join("a.png"), b"\x89PNG\r\n\x1a\nrest").unwrap();
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    // A child that takes the image on stdin and prints the one line a stored
    // image has.
    let fiber = fakes::script(
        root.path(),
        "fiber-stub",
        "cat >/dev/null; printf '{\"file\":\"%s.png\",\"mime_type\":\"image/png\",\"width\":1,\"height\":1}\\n' \"$4\"",
    );
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (tools, _infos, _driver, _forget, _images) = super::builtin(
        fiber,
        &root.path().join("home"),
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        None,
        &delegates(),
        skills(),
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
    let output = ran(Arc::clone(read), arguments);
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
    let (tools, _infos, _driver, forget, _images) = super::builtin(
        root.path().join("fiber-stub"),
        &root.path().join("home"),
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        None,
        &delegates(),
        skills(),
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
    let (tools, _infos, _driver, _forget, _images) = super::builtin(
        root.path().join("fiber-stub"),
        &root.path().join("home"),
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        None,
        &delegates(),
        skills(),
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
    let (tools, infos, _driver, _forget, _images) = super::builtin(
        root.path().join("fiber-stub"),
        &root.path().join("home"),
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        None,
        &delegates(),
        skills(),
    )
    .unwrap();
    let names: Vec<String> = tools
        .iter()
        .map(|(who, tool)| {
            assert_eq!(who, "builtin");
            tool.definition().name
        })
        .collect();
    assert_eq!(
        names,
        [
            "ask_user",
            "delegate_message",
            "delegate_models",
            "delegate_spawn",
            "delegate_fork",
            "edit",
            "handoff",
            "name_session",
            "read",
            "session_list",
            "session_message",
            "session_search",
            "shell",
            "skill",
            "web_fetch",
            "write",
            "jobs"
        ]
    );
    let listed: Vec<&str> = infos.iter().map(|info| info.name.as_str()).collect();
    assert_eq!(
        listed,
        [
            "ask_user",
            "delegate_message",
            "delegate_models",
            "delegate_spawn",
            "delegate_fork",
            "edit",
            "handoff",
            "name_session",
            "read",
            "session_list",
            "session_message",
            "session_search",
            "shell",
            "skill",
            "web_fetch",
            "write",
            "jobs"
        ]
    );
}

/// `session_search` reads the session's own project under Fiber home: the
/// key of the workspace's identity, which outside a repository is the
/// workspace itself.
#[test]
fn session_search_declares_reads_on_the_workspace_project_in_fiber_home() {
    let root = fakes::TempDir::new("fiber-builtin-session-search");
    let home = root.path().join("home");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (tools, _infos, _driver, _forget, _images) = super::builtin(
        root.path().join("fiber-stub"),
        &home,
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        None,
        &delegates(),
        skills(),
    )
    .unwrap();
    let (_, search) = tools
        .iter()
        .find(|(_, tool)| tool.definition().name == "session_search")
        .expect("session_search is registered");
    let search = Arc::clone(search);
    let mut arguments = serde_json::Map::new();
    arguments.insert("text".to_owned(), serde_json::Value::from("x"));

    // The identity check runs git, a child process.
    let effects = fakes::within(
        "the session_search effects",
        Deadline::after(CALL_WITHIN),
        move || search.effects(&arguments),
    )
    .unwrap();

    let identity = root.path().canonicalize().unwrap();
    let mut own = home
        .join("projects")
        .join(log::project_key(&identity))
        .into_os_string();
    own.push("/");
    assert_eq!(effects.declared.effects, [contract::shapes::Effect::Reads]);
    assert_eq!(
        effects.declared.paths,
        Some(vec![own.to_string_lossy().into_owned()])
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
    let (tools, _infos, _driver, _forget, _images) = super::builtin(
        root.path().join("fiber-stub"),
        &root.path().join("home"),
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        None,
        &delegates(),
        skills(),
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
    arguments.insert("timeout_ms".to_owned(), serde_json::json!(700_000));
    let output = shell.run(&arguments, &Never, &Quiet);
    let error = output.error.expect("a refusal");
    assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
    assert!(error.message.contains("600000"), "{}", error.message);
    assert!(!marker.exists());
}

/// The model-facing shell schema has one limit argument: `timeout_ms`, with
/// no `deadline_ms`, and a closed shape.
#[test]
fn the_shell_schema_is_closed_and_excludes_deadline_ms() {
    let root = fakes::TempDir::new("fiber-builtin-shell-schema");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (tools, _infos, _driver, _forget, _images) = super::builtin(
        root.path().join("fiber-stub"),
        &root.path().join("home"),
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        None,
        &delegates(),
        skills(),
    )
    .unwrap();
    let shell = tools
        .iter()
        .map(|(_, tool)| tool)
        .find(|tool| tool.definition().name == "shell")
        .expect("a shell tool");
    let schema = shell.definition().input_schema;
    assert!(
        schema["properties"].get("deadline_ms").is_none(),
        "{schema}"
    );
    assert!(schema["properties"].get("timeout_ms").is_some(), "{schema}");
    assert_eq!(schema["additionalProperties"], serde_json::json!(false));
    let required = schema["required"].as_array().expect("required names");
    assert!(required.iter().any(|name| name == "timeout_ms"), "{schema}");
    assert!(
        !required.iter().any(|name| name == "deadline_ms"),
        "{schema}"
    );
}

/// The names and hosted types `builtin` registers for `web_search`.
fn registered_with(web_search: Option<&str>) -> (Vec<(String, Option<String>)>, Vec<String>) {
    let root = fakes::TempDir::new("fiber-hosted-search");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let web_search = super::web_search(web_search, None).unwrap();
    let (tools, infos, _driver, _forget, _images) = super::builtin(
        root.path().join("fiber-stub"),
        &root.path().join("home"),
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        web_search,
        &delegates(),
        skills(),
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

#[test]
fn hosted_is_what_builtin_registers_for_web_search() {
    let root = fakes::TempDir::new("fiber-hosted-alone");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (tools, infos, _driver, _forget, _images) = super::builtin(
        root.path().join("fiber-stub"),
        root.path(),
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        super::web_search(Some("web_search_20250305"), None).unwrap(),
        &delegates(),
        skills(),
    )
    .unwrap();
    let (tool, info) = super::hosted("web_search_20250305").unwrap();
    let registered = tools
        .iter()
        .map(|(by, tool)| (by.clone(), tool.definition()))
        .find(|(_, definition)| definition.name == "web_search")
        .unwrap();
    assert_eq!(registered, ("builtin".to_owned(), tool.definition()));
    let listed = infos.iter().find(|info| info.name == "web_search").unwrap();
    assert_eq!(
        serde_json::to_value(listed).unwrap(),
        serde_json::to_value(&info).unwrap()
    );
}

/// The keywords `docs/dependencies.md` ("Tool arguments are checked
/// against a subset of JSON Schema") lists, plus `description`.
const SUBSET: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "enum",
    "items",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
    "anyOf",
    "$ref",
    "description",
];

/// Every keyword outside the subset in `node` and the schemas below it,
/// each as `path: keyword`.
fn outside_subset(node: &serde_json::Value, path: &str, found: &mut Vec<String>) {
    let Some(map) = node.as_object() else {
        return;
    };
    for (key, value) in map {
        if !SUBSET.contains(&key.as_str()) {
            found.push(format!("{path}: {key}"));
        }
        match key.as_str() {
            "properties" => {
                for (name, property) in value.as_object().into_iter().flatten() {
                    outside_subset(property, &format!("{path}.{name}"), found);
                }
            }
            "items" | "additionalProperties" => {
                outside_subset(value, &format!("{path}[]"), found);
            }
            "anyOf" => {
                for (index, branch) in value.as_array().into_iter().flatten().enumerate() {
                    outside_subset(branch, &format!("{path}|{index}"), found);
                }
            }
            _ => {}
        }
    }
}

#[test]
fn outside_subset_names_a_keyword_at_any_depth() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "a": {"type": "array", "items": {"type": "string", "pattern": "x"}},
            "b": {"anyOf": [{"type": "string"}, {"type": "integer", "format": "f"}]},
            "pattern": {"type": "string"}
        },
        "additionalProperties": false,
        "default": 1
    });
    let mut found = Vec::new();
    outside_subset(&schema, "$", &mut found);
    found.sort();
    assert_eq!(
        found,
        ["$.a[]: pattern", "$.b|1: format", "$: default"],
        "a property named like a keyword is not a keyword"
    );
}

#[test]
fn every_builtin_schema_keeps_to_the_documented_subset() {
    let root = fakes::TempDir::new("fiber-builtin-subset");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (tools, _infos, _driver, _forget, _images) = super::builtin(
        root.path().join("fiber-stub"),
        &root.path().join("home"),
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        super::web_search(Some("web_search_20250305"), None).unwrap(),
        &delegates(),
        skills(),
    )
    .unwrap();
    assert!(tools.len() >= 9, "web_search and jobs register too");
    let mut found = Vec::new();
    for (_, tool) in &tools {
        let definition = tool.definition();
        outside_subset(&definition.input_schema, &definition.name, &mut found);
    }
    assert!(found.is_empty(), "outside the subset: {found:?}");
}

/// The keywords of the strict subset (`provider::strict::fits`, without its
/// size limits).
const STRICT_KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "description",
];

/// Tools whose `docs/tools.md` section says they are sent with
/// `strict: false`.
const NOT_STRICT: &[&str] = &[
    "ask_user",       // "Asking the person", "The call"
    "delegate_fork",  // "Background jobs"
    "delegate_spawn", // "Background jobs"
    "jobs",           // "Background jobs"
    "name_session",   // "Naming the session"
    "session_search", // "Searching past sessions"
    "shell",          // "Shell", "Running a command"
    "web_search",     // "Web fetch and web search", "web_search"
];

/// Whether `schema` fits the vendor's strict subset: scalar nullable unions
/// only at required property positions, otherwise one string `type`, every
/// object listing each property once in `required` and closing with
/// `additionalProperties: false`, and no keyword outside the subset.
fn strict(schema: &serde_json::Value) -> bool {
    schema.get("type").and_then(serde_json::Value::as_str) == Some("object")
        && strict_node(schema, false)
}

fn strict_node(schema: &serde_json::Value, property: bool) -> bool {
    use serde_json::Value;
    let Some(map) = schema.as_object() else {
        return false;
    };
    if map
        .keys()
        .any(|key| !STRICT_KEYWORDS.contains(&key.as_str()))
    {
        return false;
    }
    match map.get("type").and_then(Value::as_str) {
        Some("object") => {
            let Some(properties) = map.get("properties").and_then(Value::as_object) else {
                return false;
            };
            let Some(required) = map.get("required").and_then(Value::as_array) else {
                return false;
            };
            let mut names: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
            names.sort_unstable();
            names.dedup();
            names.len() == required.len()
                && names.len() == properties.len()
                && names.iter().all(|name| properties.contains_key(*name))
                && map.get("additionalProperties") == Some(&Value::Bool(false))
                && properties.values().all(|p| strict_node(p, true))
        }
        Some("array") => map
            .get("items")
            .is_some_and(|items| strict_node(items, false)),
        Some("string" | "number" | "integer" | "boolean" | "null") => {
            !map.contains_key("properties") && !map.contains_key("items")
        }
        _ => {
            let Some(types) = map.get("type").and_then(Value::as_array) else {
                return false;
            };
            let names: Vec<_> = types.iter().filter_map(Value::as_str).collect();
            property
                && types.len() == 2
                && names.contains(&"null")
                && names
                    .iter()
                    .any(|name| matches!(*name, "integer" | "string" | "boolean"))
                && !map.contains_key("enum")
                && !map.contains_key("properties")
                && !map.contains_key("items")
        }
    }
}

#[test]
fn the_strict_walker_rejects_what_the_wire_rejects() {
    use serde_json::json;
    let fits = json!({"type": "object", "properties": {"name": {"type": "string"}},
        "required": ["name"], "additionalProperties": false});
    assert!(strict(&fits));
    let property = |p| json!({"type": "object", "properties": {"name": p}, "required": ["name"], "additionalProperties": false});
    for types in [
        json!(["integer", "null"]),
        json!(["null", "string"]),
        json!(["boolean", "null"]),
    ] {
        let nullable = json!({"type": types});
        assert!(strict(&property(nullable.clone())));
        assert!(strict(&property(property(nullable.clone()))));
        assert!(!strict(&nullable));
        assert!(!strict(&property(
            json!({"type": "array", "items": nullable})
        )));
    }
    for p in [
        json!({"type": []}),
        json!({"type": ["null"]}),
        json!({"type": ["null", "null"]}),
        json!({"type": ["integer", "string"]}),
        json!({"type": ["integer", "null", "string"]}),
        json!({"type": ["object", "null"]}),
        json!({"type": ["array", "null"]}),
        json!({"type": ["number", "null"]}),
        json!({"type": ["integer", "null"], "enum": [1, null]}),
        json!({"type": ["integer", "null"], "properties": {}}),
        json!({"type": ["integer", "null"], "items": {"type": "integer"}}),
    ] {
        assert!(!strict(&property(p.clone())), "{p}");
    }
    let mut optional = property(json!({"type": ["integer", "null"]}));
    optional["required"] = json!([]);
    assert!(!strict(&optional));
    for (label, schema) in [
        (
            "minLength",
            json!({"type": "object", "properties": {"name":
            {"type": "string", "minLength": 1}}, "required": ["name"],
            "additionalProperties": false}),
        ),
        (
            "optional property",
            json!({"type": "object", "properties": {"name":
            {"type": "string"}}, "required": [], "additionalProperties": false}),
        ),
        (
            "open object",
            json!({"type": "object", "properties": {"name":
            {"type": "string"}}, "required": ["name"]}),
        ),
        ("scalar root", json!({"type": "string"})),
        (
            "non-string required entry",
            json!({"type": "object", "properties": {"name": {"type": "string"}},
            "required": ["name", 1], "additionalProperties": false}),
        ),
        (
            "properties on a scalar",
            json!({"type": "object", "properties": {"name":
            {"type": "string", "properties": {}}}, "required": ["name"],
            "additionalProperties": false}),
        ),
    ] {
        assert!(!strict(&schema), "{label}");
    }
}

#[test]
fn every_builtin_fits_the_strict_shape_unless_its_section_says_strict_false() {
    let root = fakes::TempDir::new("fiber-builtin-strict");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let backend = stub_backend();
    let (tools, _infos, _driver, _forget, _images) = super::builtin(
        root.path().join("fiber-stub"),
        &root.path().join("home"),
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        super::web_search(None, Some(&backend)).unwrap(),
        &delegates(),
        skills(),
    )
    .unwrap();
    let mut walked = Vec::new();
    for (_, tool) in &tools {
        let definition = tool.definition();
        assert_eq!(definition.hosted, None, "{}", definition.name);
        assert_eq!(
            strict(&definition.input_schema),
            !NOT_STRICT.contains(&definition.name.as_str()),
            "{}: fits the strict shape exactly when its section does not say `strict: false`",
            definition.name
        );
        walked.push(definition.name);
    }
    walked.sort();
    let listed: Vec<&str> = contract::tool::BUILT_IN_TOOLS
        .iter()
        .filter(|name| **name != "tool_search")
        .copied()
        .collect();
    assert_eq!(walked, listed);
}

/// The names an extension must list in `replaces` are exactly the names
/// `builtin` can register: `web_search` included whether or not the
/// model hosts a search.
#[test]
fn built_in_tools_is_every_name_builtin_registers() {
    let names = |web_search| {
        let (definitions, _) = registered_with(web_search);
        let mut names: Vec<String> = definitions.into_iter().map(|(name, _)| name).collect();
        names.sort();
        names
    };
    let listed: Vec<String> = contract::tool::BUILT_IN_TOOLS
        .iter()
        .filter(|name| **name != "tool_search")
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(names(Some("web_search_20250305")), listed);
    let mut without = names(None);
    without.push("web_search".to_owned());
    without.sort();
    assert_eq!(without, listed);
}

/// `tool_search` is a built-in name in sorted place after `skill`, though
/// `builtin` never registers it: the loop builds it over the deferred tools.
#[test]
fn built_in_tools_holds_tool_search_after_skill() {
    let list = contract::tool::BUILT_IN_TOOLS;
    assert!(list.contains(&"tool_search"), "{list:?}");
    let mut sorted = list.to_vec();
    sorted.sort();
    assert_eq!(sorted, list, "{list:?}");
    let skill = list.iter().position(|name| *name == "skill").unwrap();
    assert_eq!(list[skill + 1], "tool_search", "{list:?}");
}

/// The factory builds the search tool over the catalogue, naming its
/// sources in the description; with none, no such sentence.
#[test]
fn tool_search_factory_builds_the_search_naming_its_sources() {
    let factory = super::tool_search_factory();
    let search = factory(&[], &["fx".to_owned()]);
    let definition = search.definition();
    assert_eq!(definition.name, contract::provider::TOOL_SEARCH);
    assert!(!definition.deferred);
    assert!(
        definition
            .description
            .ends_with("Deferred tools come from: fx."),
        "{}",
        definition.description
    );
    let bare = factory(&[], &[]);
    assert_eq!(bare.definition().name, contract::provider::TOOL_SEARCH);
    assert!(
        !bare
            .definition()
            .description
            .contains("Deferred tools come from"),
        "{}",
        bare.definition().description
    );
}

/// The rows adapter publishes only the search row's declares, and every
/// row's state move.
#[test]
fn tool_rows_forwards_only_the_search_declares_and_every_state() {
    let declared = Arc::new(std::sync::Mutex::new(Vec::new()));
    let moved = Arc::new(std::sync::Mutex::new(Vec::new()));
    let store = Arc::clone(&declared);
    let declare: doors::Declare = Arc::new(move |name: &str, info| {
        store
            .lock()
            .unwrap()
            .push((name.to_owned(), info.is_some()));
    });
    let moves = Arc::clone(&moved);
    #[allow(
        clippy::type_complexity,
        reason = "the setter type is doors' closure type, named at neither end"
    )]
    let state: Arc<dyn Fn(&str, contract::events::ToolState) + Send + Sync> =
        Arc::new(move |name: &str, state| {
            moves.lock().unwrap().push((name.to_owned(), state));
        });
    let rows = super::tool_rows(declare, state);
    let info = || contract::events::ToolInfo {
        name: "tool_search".to_owned(),
        source: contract::events::ToolSource::Builtin,
        state: contract::events::ToolState::Full,
        bytes: 1,
        tokens: None,
    };
    (rows.declare)("mcp__fx__echo", Some(info()));
    assert!(declared.lock().unwrap().is_empty());
    (rows.declare)("tool_search", Some(info()));
    (rows.declare)("tool_search", None);
    assert_eq!(
        *declared.lock().unwrap(),
        [
            ("tool_search".to_owned(), true),
            ("tool_search".to_owned(), false)
        ]
    );
    (rows.state)("mcp__fx__echo", contract::events::ToolState::Loaded);
    (rows.state)("tool_search", contract::events::ToolState::Deferred);
    assert_eq!(
        *moved.lock().unwrap(),
        [
            (
                "mcp__fx__echo".to_owned(),
                contract::events::ToolState::Loaded
            ),
            (
                "tool_search".to_owned(),
                contract::events::ToolState::Deferred
            ),
        ]
    );
}

/// A search backend answering nothing, for choosing what `builtin`
/// declares.
struct StubBackend;

impl contract::search::SearchBackend for StubBackend {
    fn search(
        &self,
        _query: &str,
        _domains: &contract::search::Domains,
        _cancel: &dyn contract::tool::Cancel,
    ) -> Result<Option<Vec<contract::search::SearchResult>>, contract::shapes::Failure> {
        Ok(Some(Vec::new()))
    }
}

fn stub_backend() -> Arc<dyn contract::search::SearchBackend> {
    Arc::new(StubBackend)
}

#[test]
fn a_hosted_search_wins_over_an_installed_backend() {
    let backend = stub_backend();
    let (tool, _) = super::web_search(Some("web_search_20250305"), Some(&backend))
        .unwrap()
        .expect("a search is declared");
    assert_eq!(
        tool.definition().hosted.as_deref(),
        Some("web_search_20250305")
    );
}

#[test]
fn no_hosted_search_with_a_backend_declares_fibers_own_tool() {
    let backend = stub_backend();
    let (tool, info) = super::web_search(None, Some(&backend))
        .unwrap()
        .expect("a search is declared");
    let definition = tool.definition();
    assert_eq!(definition.name, "web_search");
    assert_eq!(definition.hosted, None);
    assert_eq!(info.name, "web_search");
    assert!(
        matches!(info.source, contract::events::ToolSource::Builtin),
        "{:?}",
        info.source
    );
}

#[test]
fn session_list_declares_reads_on_run_in_fiber_home() {
    let root = fakes::TempDir::new("fiber-session-list");
    let home = root.path().join("home");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (tools, _, _, _, _) = super::builtin(
        root.path().join("fiber-stub"),
        &home,
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        None,
        &delegates(),
        skills(),
    )
    .unwrap();
    let (_, tool) = tools
        .iter()
        .find(|(_, tool)| tool.definition().name == "session_list")
        .expect("session_list is registered");
    let effects = tool.effects(&serde_json::Map::new()).unwrap();
    assert_eq!(effects.declared.effects, [contract::shapes::Effect::Reads]);
    assert_eq!(
        effects.declared.paths,
        Some(vec![format!("{}/run/", home.display())])
    );
}

#[test]
fn session_message_declares_writes_with_no_paths() {
    let root = fakes::TempDir::new("fiber-session-message");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (tools, _, _, _, _) = super::builtin(
        root.path().join("fiber-stub"),
        &root.path().join("home"),
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        None,
        &delegates(),
        skills(),
    )
    .unwrap();
    let (_, tool) = tools
        .iter()
        .find(|(_, tool)| tool.definition().name == "session_message")
        .expect("session_message is registered");
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "id".to_owned(),
        serde_json::Value::from("s_0000000000000001"),
    );
    arguments.insert("text".to_owned(), serde_json::Value::from("hi"));
    let effects = tool.effects(&arguments).unwrap();
    // With no paths, `fast_path` in `crates/loop/src/permission.rs` cannot
    // pass a `writes` call: it reads only what is declared here.
    assert_eq!(effects.declared.effects, [contract::shapes::Effect::Writes]);
    assert!(!effects.declared.reversible);
    assert_eq!(effects.declared.paths, None);
    assert!(!effects.always_reviewed);
}

/// `delegate_fork` is always judged by the parent's reviewer: no fast path,
/// session grant or standing allow skips it (`docs/delegates.md`,
/// "Permissions"), as `delegate_spawn` is.
#[test]
fn delegate_fork_declares_always_reviewed() {
    let root = fakes::TempDir::new("fiber-builtin-fork-reviewed");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (tools, _, _, _, _) = super::builtin(
        root.path().join("fiber-stub"),
        &root.path().join("home"),
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        None,
        &delegates(),
        skills(),
    )
    .unwrap();
    let (_, tool) = tools
        .iter()
        .find(|(_, tool)| tool.definition().name == "delegate_fork")
        .expect("delegate_fork is registered");
    let mut arguments = serde_json::Map::new();
    arguments.insert("description".to_owned(), serde_json::Value::from("scan"));
    arguments.insert("prompt".to_owned(), serde_json::Value::from("hi"));
    let effects = tool.effects(&arguments).unwrap();
    assert!(effects.always_reviewed);
}

#[test]
fn no_hosted_search_and_no_backend_declares_nothing() {
    assert!(super::web_search(None, None).unwrap().is_none());
}

#[test]
fn builtin_registers_the_web_search_it_is_given() {
    let root = fakes::TempDir::new("fiber-backend-search");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let backend = stub_backend();
    let given = super::web_search(None, Some(&backend)).unwrap();
    let (tools, infos, _driver, _forget, _images) = super::builtin(
        root.path().join("fiber-stub"),
        &root.path().join("home"),
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        given,
        &delegates(),
        skills(),
    )
    .unwrap();
    let registered = tools
        .iter()
        .map(|(by, tool)| (by.clone(), tool.definition()))
        .find(|(_, definition)| definition.name == "web_search")
        .expect("web_search is registered");
    assert_eq!(registered.0, "builtin");
    assert_eq!(registered.1.hosted, None);
    assert!(infos.iter().any(|info| info.name == "web_search"));
}

#[test]
fn the_model_shell_carries_no_sandbox_when_off() {
    let root = fakes::TempDir::new("fiber-builtin-sandbox-off");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let (tools, _infos, _driver, _forget, _images) = super::builtin(
        root.path().join("fiber-stub"),
        &root.path().join("home"),
        root.path(),
        &root.path().join("artifacts"),
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        None,
        &delegates(),
        skills(),
    )
    .unwrap();
    let shell = tools
        .iter()
        .find(|(_, tool)| tool.definition().name == "shell")
        .expect("shell is registered");
    let args = {
        let mut map = serde_json::Map::new();
        map.insert(
            "command".into(),
            serde_json::Value::String("echo hi".into()),
        );
        map
    };
    let effects = shell.1.effects(&args).unwrap();
    assert!(!effects.declared.confined);
}

#[test]
fn the_model_shell_marks_confined_when_on_and_available() {
    let root = fakes::TempDir::new("fiber-builtin-sandbox-on");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let jobs = jobs::Registry::new(
        root.path().join("artifacts"),
        Arc::clone(&clock),
        Arc::new(fakes::Recorder::default()),
    );
    let home_base = std::env::var_os("HOME").map_or_else(
        || std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from,
    );
    let fiber_home = home_base.join(format!("fiber-builtin-sandbox-on-{}", std::process::id()));
    let sandbox = std::sync::Arc::new(std::sync::Mutex::new(tools::SandboxSettings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home,
    }));
    let fiber = root.path().join("fiber-stub");
    let artifacts = root.path().join("artifacts");
    let images = super::session_images(fiber.clone(), &artifacts, None);
    let (tools, _infos, _driver, _forget, _images) = super::builtin_timed(
        fiber,
        &root.path().join("home"),
        root.path(),
        &artifacts,
        &clock,
        &jobs,
        &Arc::new(tools::PathLocks::new()),
        images,
        None,
        &delegates(),
        skills(),
        None,
        Default::default(),
        sandbox,
    )
    .unwrap();
    let shell = tools
        .iter()
        .find(|(_, tool)| tool.definition().name == "shell")
        .expect("shell is registered");
    let args = {
        let mut map = serde_json::Map::new();
        map.insert(
            "command".into(),
            serde_json::Value::String("echo hi".into()),
        );
        map
    };
    let effects = shell.1.effects(&args).unwrap();
    #[cfg(target_os = "linux")]
    assert!(effects.declared.confined);
    #[cfg(not(target_os = "linux"))]
    assert!(!effects.declared.confined);
}
