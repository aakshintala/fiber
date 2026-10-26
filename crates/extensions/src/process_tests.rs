//! Native process registrations (`super::process`): validation with the
//! Lua registry's exact wording, and references for every function a call
//! registers. Each row pins one Lua case beside its process twin: the same
//! input refuses with the same sentence on both sides.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

use std::time::Duration;

use contract::{CommandId, ErrorCode};
use serde_json::{Value, json};

use super::process::{RegisterOutcome, Registry};
use super::runtime::Runtime;
use super::test_support::Home;

/// A process package directory holding `manifest_extra`, read back as the
/// registry's manifest.
fn package_dir(extra: Value) -> fakes::TempDir {
    let dir = fakes::TempDir::new("fiber-process-registry");
    let mut manifest = json!({
        "name": "fiber.test/proc",
        "version": "v1.2.3",
        "fiber": "0.1.0",
        "api": 1,
    });
    for (key, value) in extra.as_object().unwrap() {
        manifest[key] = value.clone();
    }
    std::fs::write(dir.path().join("extension.json"), manifest.to_string()).unwrap();
    dir
}

/// A registry for a `fiber.test/proc` package holding `manifest_extra`.
fn test_registry(extra: Value) -> (fakes::TempDir, Registry) {
    let dir = package_dir(extra);
    let manifest = config::read_manifest(dir.path()).unwrap();
    let registry = Registry::new(
        "fiber.test/proc".to_owned(),
        Runtime::new(fakes::clock::FakeClock::new()),
        &manifest,
    );
    (dir, registry)
}

/// Registers `call` with `args` as command `id` outside any callback.
fn register(registry: &mut Registry, id: &str, call: &str, args: Value) -> RegisterOutcome {
    let args = args.as_array().unwrap().clone();
    registry.register(CommandId(id.to_owned()), call, &args, false)
}

/// Installs `fiber.test/<short>` with `init`, `providers` and `manifest`
/// merged over the default manifest, and plans it into `home`.
fn install_case(
    home: &Home,
    short: &str,
    init: Option<&str>,
    providers: &[(&str, &str)],
    manifest_extra: &Value,
) {
    let src_root = fakes::TempDir::new("fiber-process-case");
    let src = src_root.path().join(short);
    std::fs::create_dir_all(&src).unwrap();
    let mut manifest = json!({"name": format!("fiber.test/{short}"), "version": "v1.2.3", "fiber": "0.1.0", "api": 1});
    for (key, value) in manifest_extra.as_object().unwrap() {
        manifest[key] = value.clone();
    }
    if !providers.is_empty() {
        std::fs::create_dir_all(src.join("providers")).unwrap();
        for (name, lua) in providers {
            std::fs::write(src.join("providers").join(format!("{name}.lua")), lua).unwrap();
        }
        // A `providers` key in the manifest stands as written, so a file
        // the manifest does not declare stays undeclared.
        if manifest.get("providers").is_none() {
            let declared: serde_json::Map<String, Value> = providers
                .iter()
                .map(|(name, _)| ((*name).to_owned(), json!([])))
                .collect();
            manifest["providers"] = Value::Object(declared);
        }
    }
    std::fs::write(src.join("extension.json"), manifest.to_string()).unwrap();
    if let Some(init) = init {
        std::fs::write(src.join("init.lua"), init).unwrap();
    }
    crate::plan(
        &home.home(),
        &crate::Request::Path(src),
        "0.1.0",
        &crate::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
}

/// The session notices `init` leaves for `fiber.test/case`, sorted: the
/// Lua side of a parity row.
fn lua_problems(init: &str, manifest_extra: Value, providers: &[(&str, &str)]) -> Vec<String> {
    let home = Home::new();
    install_case(&home, "case", Some(init), providers, &manifest_extra);
    let session = home.load(&[]);
    let mut problems: Vec<String> = session
        .notices()
        .iter()
        .filter(|notice| notice.extension.as_deref() == Some("fiber.test/case"))
        .map(|notice| notice.message.clone())
        .collect();
    problems.sort();
    problems
}

/// The problems one process registration leaves, with its outcome: the
/// process side of a parity row. A refused registration is rejected with
/// the problem's sentence and recorded for the notice; nothing else is.
fn process_problems(
    registry: &mut Registry,
    id: &str,
    call: &str,
    args: Value,
) -> (Vec<String>, RegisterOutcome) {
    let outcome = register(registry, id, call, args);
    let mut problems = registry.problems().to_vec();
    problems.extend(registry.watch_problems().to_vec().iter().cloned());
    problems.sort();
    (problems, outcome)
}

/// One parity row: `lua` beside `call` with `args` refuses with the same
/// sentences. A mismatch means the process registry drifted from the Lua
/// wording it mirrors. A refused call carries the first problem; the rest
/// ride the notice with it.
fn check_parity(
    lua: &str,
    manifest_extra: Value,
    providers: &[(&str, &str)],
    call: &str,
    args: Value,
) {
    let expected = lua_problems(lua, manifest_extra.clone(), providers);
    assert!(!expected.is_empty(), "the Lua case must refuse: {lua}");
    let (_dir, mut registry) = test_registry(manifest_extra);
    let (problems, outcome) = process_problems(&mut registry, "c1", call, args);
    assert_eq!(problems, expected);
    let RegisterOutcome::Rejected { message, .. } = outcome else {
        panic!("the process call must reject, not {outcome:?}");
    };
    assert_eq!(message, expected[0]);
}

/// A valid tool spec with `run`, as JSON: `true` marks the function, as on
/// the wire.
fn tool_spec() -> Value {
    json!({
        "description": "Does the thing.",
        "input_schema": {"type": "object"},
        "effects": {"effects": [], "reversible": true},
        "timeout": 5000,
        "run": true,
    })
}

#[test]
fn tool_name_must_be_a_string() {
    check_parity(
        "fiber.tool(42, { description = \"d\", input_schema = { type = \"object\" }, effects = { effects = {}, reversible = true }, timeout = 5, run = function() end })",
        json!({}),
        &[],
        "fiber.tool",
        json!([42, tool_spec()]),
    );
}

#[test]
fn tool_name_must_be_plain() {
    check_parity(
        "fiber.tool(\"a b\", { description = \"d\", input_schema = { type = \"object\" }, effects = { effects = {}, reversible = true }, timeout = 5, run = function() end })",
        json!({}),
        &[],
        "fiber.tool",
        json!(["a b", tool_spec()]),
    );
}

#[test]
fn tool_spec_must_be_a_table() {
    check_parity(
        "fiber.tool(\"t\", 42)",
        json!({}),
        &[],
        "fiber.tool",
        json!(["t", 42]),
    );
}

#[test]
fn tool_description_must_not_be_empty() {
    let mut spec = tool_spec();
    spec["description"] = json!("");
    check_parity(
        "fiber.tool(\"t\", { description = \"\", input_schema = { type = \"object\" }, effects = { effects = {}, reversible = true }, timeout = 5, run = function() end })",
        json!({}),
        &[],
        "fiber.tool",
        json!(["t", spec]),
    );
}

#[test]
fn tool_schema_must_declare_object() {
    let mut spec = tool_spec();
    spec["input_schema"] = json!({"type": "array"});
    check_parity(
        "fiber.tool(\"t\", { description = \"d\", input_schema = { type = \"array\" }, effects = { effects = {}, reversible = true }, timeout = 5, run = function() end })",
        json!({}),
        &[],
        "fiber.tool",
        json!(["t", spec]),
    );
}

#[test]
fn tool_effects_must_be_present() {
    let mut spec = tool_spec();
    spec.as_object_mut().unwrap().remove("effects");
    check_parity(
        "fiber.tool(\"t\", { description = \"d\", input_schema = { type = \"object\" }, timeout = 5, run = function() end })",
        json!({}),
        &[],
        "fiber.tool",
        json!(["t", spec]),
    );
}

#[test]
fn tool_effects_must_be_a_table_or_function() {
    let mut spec = tool_spec();
    spec["effects"] = json!(42);
    check_parity(
        "fiber.tool(\"t\", { description = \"d\", input_schema = { type = \"object\" }, effects = 42, timeout = 5, run = function() end })",
        json!({}),
        &[],
        "fiber.tool",
        json!(["t", spec]),
    );
}

#[test]
fn tool_effects_table_must_read() {
    let mut spec = tool_spec();
    spec["effects"] = json!({"effects": ["nap"], "reversible": true});
    check_parity(
        "fiber.tool(\"t\", { description = \"d\", input_schema = { type = \"object\" }, effects = { effects = { \"nap\" }, reversible = true }, timeout = 5, run = function() end })",
        json!({}),
        &[],
        "fiber.tool",
        json!(["t", spec]),
    );
}

/// A valid provider function set with `run`, as JSON.
fn provider_spec() -> Value {
    json!({
        "models": {"timeout": 1000, "run": true},
        "quota": {"timeout": 1000, "run": true},
        "credential": {"timeout": 1000, "run": true},
        "sign": {"timeout": 1000, "run": true},
        "cost": {"timeout": 1000, "run": true},
    })
}

#[test]
fn provider_name_and_spec_must_be_present() {
    check_provider_violation(
        "return fiber.provider(\"p\", 42)",
        "fiber.provider",
        json!(["p", 42]),
        "fiber.provider: takes a name and a table of functions",
    );
}

/// A non-string name never reaches Lua's check: the provider file check
/// fires first. The wire names no file, so the check stands alone.
#[test]
fn provider_non_string_name_is_rejected() {
    let (_dir, mut registry) = test_registry(json!({"providers": {"p": []}}));
    let (_, outcome) = process_problems(&mut registry, "c1", "fiber.provider", json!([42, {}]));
    let RegisterOutcome::Rejected { message, .. } = outcome else {
        panic!("a nameless provider refuses, not {outcome:?}");
    };
    assert_eq!(
        message,
        "fiber.provider: takes a name and a table of functions"
    );
}

/// One provider-file violation row: loading `providers/p.lua` holding
/// `provider_lua` fails with a message containing `sentence`; the process
/// twin rejects with the bare sentence. Provider files load lazily, so the
/// Lua side forces the load through `provider_functions` under a deadline
/// instead of reading session notices. Positions and file context do not
/// cross the wire.
fn check_provider_violation(provider_lua: &str, call: &str, args: Value, sentence: &str) {
    let dir = fakes::TempDir::new("fiber-process-provider-case");
    std::fs::write(dir.path().join("init.lua"), "return nil").unwrap();
    std::fs::create_dir_all(dir.path().join("providers")).unwrap();
    std::fs::write(dir.path().join("providers/p.lua"), provider_lua).unwrap();
    let extension = super::LuaExtension::new(
        "ext",
        dir.path(),
        dir.path(),
        fakes::clock::FakeClock::new(),
    );
    let result = super::test_support::bounded(move || extension.provider_functions("p"));
    let message = match result {
        Ok(_) => panic!("the provider file must fail: {provider_lua}"),
        Err(crate::Error::Lua { message, .. }) => message,
        Err(other) => panic!("the provider file must fail in Lua, not {other:?}"),
    };
    assert!(
        message.contains(sentence),
        "the provider load must fail with {sentence}: {message}"
    );
    let (_dir, mut registry) = test_registry(json!({"providers": {"p": []}}));
    let (problems, outcome) = process_problems(&mut registry, "c1", call, args);
    assert!(
        problems.iter().any(|problem| problem == sentence),
        "the process problems must record {sentence}: {problems:?}"
    );
    let RegisterOutcome::Rejected { message, .. } = outcome else {
        panic!("the process call must reject, not {outcome:?}");
    };
    assert_eq!(message, sentence);
}

#[test]
fn provider_unknown_key_is_rejected() {
    check_provider_violation(
        "return fiber.provider(\"p\", { models = { timeout = 1000, run = function() end }, nap = {} })",
        "fiber.provider",
        json!([
            "p",
            json!({"models": {"timeout": 1000, "run": true}, "nap": {}})
        ]),
        "fiber.provider: `nap` is not models, quota, credential, sign or cost",
    );
}

#[test]
fn provider_timeout_must_be_a_whole_millisecond_above_zero() {
    check_provider_violation(
        "return fiber.provider(\"p\", { models = { run = function() end } })",
        "fiber.provider",
        json!(["p", json!({"models": {"run": true}})]),
        "fiber.provider: `models`: `timeout` must be a whole number of milliseconds above 0",
    );
}

#[test]
fn provider_run_must_be_a_function() {
    check_provider_violation(
        "return fiber.provider(\"p\", { models = { timeout = 1000, run = \"x\" } })",
        "fiber.provider",
        json!(["p", json!({"models": {"timeout": 1000, "run": "x"}})]),
        "fiber.provider: `models`: `run` must be a function",
    );
}

#[test]
fn provider_registers_once() {
    let provider_lua = "fiber.provider(\"p\", { models = { timeout = 1000, run = function() end } })\nfiber.provider(\"p\", { models = { timeout = 1000, run = function() end } })";
    let dir = fakes::TempDir::new("fiber-process-provider-once");
    std::fs::write(dir.path().join("init.lua"), "return nil").unwrap();
    std::fs::create_dir_all(dir.path().join("providers")).unwrap();
    std::fs::write(dir.path().join("providers/p.lua"), provider_lua).unwrap();
    let extension = super::LuaExtension::new(
        "ext",
        dir.path(),
        dir.path(),
        fakes::clock::FakeClock::new(),
    );
    let result = super::test_support::bounded(move || extension.provider_functions("p"));
    let Err(crate::Error::Lua { message, .. }) = result else {
        panic!("the duplicate provider file must fail, not {result:?}");
    };
    // Lua names the provider file where the process twin names the
    // provider: a process has no file to name.
    assert!(
        message.contains("fiber.provider: `providers/p.lua` registers its provider exactly once"),
        "the provider load must fail the duplicate: {message}"
    );
    let (_dir, mut registry) = test_registry(json!({"providers": {"p": []}}));
    let outcome = register(
        &mut registry,
        "c1",
        "fiber.provider",
        json!(["p", provider_spec()]),
    );
    assert!(matches!(outcome, RegisterOutcome::Accepted));
    let entry = registry.providers().get("p").unwrap();
    // All five `fiber.provider` functions take a timeout
    // (`docs/extensions.md`, "Registering"), so one fully declared
    // provider holds five.
    assert_eq!(entry.timeouts.len(), 5);
    for key in ["models", "quota", "credential", "sign", "cost"] {
        let reference = entry.runs.get(key).unwrap();
        assert_eq!(reference.key, key);
        assert_eq!(reference.generation, 1);
    }
    let duplicate = register(
        &mut registry,
        "c2",
        "fiber.provider",
        json!(["p", provider_spec()]),
    );
    let RegisterOutcome::Rejected { message, .. } = duplicate else {
        panic!("a duplicate provider refuses, not {duplicate:?}");
    };
    assert_eq!(
        message,
        "fiber.provider: `p` registers its provider exactly once"
    );
}

/// A valid search backend spec with `run`, as JSON.
fn search_spec() -> Value {
    json!({"timeout": 5000, "run": true})
}

#[test]
fn search_backend_name_must_be_a_string() {
    check_parity(
        "fiber.search_backend(42, { timeout = 5, run = function() end })",
        json!({}),
        &[],
        "fiber.search_backend",
        json!([42, search_spec()]),
    );
}

#[test]
fn search_backend_spec_must_be_a_table() {
    check_parity(
        "fiber.search_backend(\"b\", 42)",
        json!({}),
        &[],
        "fiber.search_backend",
        json!(["b", 42]),
    );
}

#[test]
fn search_backend_run_must_be_a_function() {
    let mut spec = search_spec();
    spec.as_object_mut().unwrap().remove("run");
    check_parity(
        "fiber.search_backend(\"b\", { timeout = 5 })",
        json!({}),
        &[],
        "fiber.search_backend",
        json!(["b", spec]),
    );
}

#[test]
fn search_backend_timeout_must_be_a_whole_millisecond_above_zero() {
    let mut spec = search_spec();
    spec["timeout"] = json!(0);
    check_parity(
        "fiber.search_backend(\"b\", { timeout = 0, run = function() end })",
        json!({}),
        &[],
        "fiber.search_backend",
        json!(["b", spec]),
    );
}

#[test]
fn search_backend_registers_under_its_wire_command() {
    let (_dir, mut registry) = test_registry(json!({}));
    let outcome = register(
        &mut registry,
        "c3",
        "fiber.search_backend",
        json!(["b", search_spec()]),
    );
    assert!(matches!(outcome, RegisterOutcome::Accepted));
    let (timeout, run) = registry.search().get("b").unwrap();
    assert_eq!(*timeout, Duration::from_millis(5000));
    assert_eq!(run.command, CommandId("c3".to_owned()));
    assert_eq!(run.key, "run");
    let duplicate = register(
        &mut registry,
        "c4",
        "fiber.search_backend",
        json!(["b", search_spec()]),
    );
    let RegisterOutcome::Rejected { message, .. } = duplicate else {
        panic!("a duplicate backend refuses, not {duplicate:?}");
    };
    assert_eq!(message, "`b` search backend is already registered");
}

/// A valid harness spec with `run`s, as JSON.
fn harness_spec() -> Value {
    json!({
        "timeout": 5000,
        "command": true,
        "line": true,
        "models": true,
        "takes_messages": true,
    })
}

#[test]
fn harness_name_must_be_plain() {
    let (_dir, mut registry) = test_registry(json!({}));
    let (_, outcome) = process_problems(
        &mut registry,
        "c1",
        "fiber.harness",
        json!(["a b", harness_spec()]),
    );
    let RegisterOutcome::Rejected { message, .. } = outcome else {
        panic!("a non-plain harness refuses, not {outcome:?}");
    };
    assert_eq!(
        message,
        "`a b` harness not registered: names are plain: `a b` is empty or holds whitespace, `/` or `:`"
    );
}

#[test]
fn harness_registers_functions_and_data() {
    let (_dir, mut registry) = test_registry(json!({}));
    let outcome = register(
        &mut registry,
        "c9",
        "fiber.harness",
        json!(["h", harness_spec()]),
    );
    assert!(matches!(outcome, RegisterOutcome::Accepted), "{outcome:?}");
    let entry = registry.harness().get("h").unwrap();
    assert_eq!(entry.timeout, Duration::from_millis(5000));
    assert_eq!(entry.data.get("takes_messages"), Some(&Value::Bool(true)));
    for key in ["command", "line", "models"] {
        let reference = entry.runs.get(key).unwrap();
        assert_eq!(reference.command, CommandId("c9".to_owned()));
        assert_eq!(reference.key, key);
        assert_eq!(reference.generation, 1);
    }
    assert!(!entry.runs.contains_key("quota"));
    let without_quota = register(
        &mut registry,
        "c10",
        "fiber.harness",
        json!(["q", {"timeout": 5000, "command": true, "line": true, "models": true, "quota": true}]),
    );
    assert!(matches!(without_quota, RegisterOutcome::Accepted));
    assert!(
        registry
            .harness()
            .get("q")
            .unwrap()
            .runs
            .contains_key("quota")
    );
}

/// A call made while answering another registers nothing: its sentence
/// names the call, as in Lua.
#[test]
fn registrations_in_a_callback_are_rejected() {
    let (_dir, mut registry) = test_registry(json!({}));
    for (call, sentence) in [
        (
            "fiber.tool",
            "fiber.tool: registers only while `init.lua` runs",
        ),
        (
            "fiber.hook",
            "fiber.hook: registers only while `init.lua` runs",
        ),
        (
            "fiber.watch",
            "fiber.watch: registers only while `init.lua` runs",
        ),
        (
            "fiber.command",
            "fiber.command: registers only while `init.lua` runs",
        ),
        (
            "fiber.search_backend",
            "fiber.search_backend: registers only while `init.lua` runs",
        ),
        (
            "fiber.provider",
            "fiber.provider: registers only while its provider file loads",
        ),
        (
            "fiber.harness",
            "fiber.harness: registers only while `init.lua` runs",
        ),
    ] {
        let outcome = registry.register(CommandId("c1".to_owned()), call, &[], true);
        let RegisterOutcome::Rejected { message, .. } = outcome else {
            panic!("a callback registration of {call} refuses, not {outcome:?}");
        };
        assert_eq!(message, sentence);
    }
    assert!(registry.tools().is_empty());
    assert!(registry.commands().is_empty());
}

/// A call the registry does not know is not a registration.
#[test]
fn unknown_calls_are_rejected() {
    let (_dir, mut registry) = test_registry(json!({}));
    let outcome = register(&mut registry, "c1", "fiber.nope", json!([]));
    let RegisterOutcome::Rejected { code, message } = outcome else {
        panic!("an unknown call refuses, not {outcome:?}");
    };
    assert_eq!(code, ErrorCode::InvalidArguments);
    assert_eq!(message, "unknown call `fiber.nope`");
}

/// `null` reads as Lua's `nil`, and a table has no short rendering: the
/// wire decodes `null` to absent, and quotes what it cannot spell.
#[test]
fn shown_names_nothing_and_tables() {
    let (_dir, mut registry) = test_registry(json!({}));
    let (_, outcome) = process_problems(
        &mut registry,
        "c1",
        "fiber.tool",
        json!([Value::Null, tool_spec()]),
    );
    let RegisterOutcome::Rejected { message, .. } = outcome else {
        panic!("a nameless tool refuses, not {outcome:?}");
    };
    assert_eq!(
        message,
        "`nil` tool not registered: the name must be a string"
    );
    let (_dir, mut registry) = test_registry(json!({}));
    let (_, outcome) = process_problems(
        &mut registry,
        "c1",
        "fiber.tool",
        json!([{"odd": 1}, tool_spec()]),
    );
    let RegisterOutcome::Rejected { message, .. } = outcome else {
        panic!("a table-named tool refuses, not {outcome:?}");
    };
    assert_eq!(
        message,
        "`table` tool not registered: the name must be a string"
    );
}

/// A package that declares `commands` registers only them: any other
/// family, and any command outside the manifest, fails the package.
#[test]
fn lazy_packages_register_only_declared_commands() {
    let manifest = json!({"commands": {"go": {"description": "Go."}}});
    let (_dir, mut registry) = test_registry(manifest);
    let tool = register(&mut registry, "c1", "fiber.tool", json!(["t", tool_spec()]));
    let RegisterOutcome::FailedPackage { message, .. } = tool else {
        panic!("a lazy tool fails the package, not {tool:?}");
    };
    assert_eq!(
        message,
        "`fiber.test/proc` declares `commands`, so a `fiber.tool` registration stops the load"
    );
    let manifest = json!({"commands": {"go": {"description": "Go."}}});
    let (_dir, mut registry) = test_registry(manifest);
    let other = register(
        &mut registry,
        "c1",
        "fiber.command",
        json!(["stop", command_spec()]),
    );
    let RegisterOutcome::FailedPackage { message, .. } = other else {
        panic!("an undeclared lazy command fails the package, not {other:?}");
    };
    assert_eq!(
        message,
        "Command `stop` is not declared under `commands`; `fiber.test/proc` registers only the commands its manifest declares"
    );
    let manifest = json!({"commands": {"go": {"description": "Go."}}});
    let (_dir, mut registry) = test_registry(manifest);
    let allowed = register(
        &mut registry,
        "c1",
        "fiber.command",
        json!(["go", command_spec()]),
    );
    assert!(matches!(allowed, RegisterOutcome::Accepted), "{allowed:?}");
}

/// A provider or replacement the manifest does not declare fails the
/// package with `extension_failed`, as in Lua.
#[test]
fn undeclared_providers_fail_the_package() {
    let (_dir, mut registry) = test_registry(json!({}));
    let outcome = register(
        &mut registry,
        "c1",
        "fiber.provider",
        json!(["p", provider_spec()]),
    );
    let RegisterOutcome::FailedPackage { code, message } = outcome else {
        panic!("an undeclared provider fails the package, not {outcome:?}");
    };
    assert_eq!(code, ErrorCode::ExtensionFailed);
    assert_eq!(
        message,
        "The extension `fiber.test/proc` registers the provider `p`, which its manifest does not declare under `providers`."
    );
}

/// A refusal reserves nothing: fixing the call and retrying registers.
#[test]
fn retry_after_a_refusal_registers() {
    let (_dir, mut registry) = test_registry(json!({"providers": {"p": []}}));
    let bad = register(
        &mut registry,
        "c1",
        "fiber.provider",
        json!(["p", {"models": {"run": true}}]),
    );
    assert!(matches!(bad, RegisterOutcome::Rejected { .. }));
    let good = register(
        &mut registry,
        "c2",
        "fiber.provider",
        json!(["p", provider_spec()]),
    );
    assert!(matches!(good, RegisterOutcome::Accepted), "{good:?}");
}

/// An empty object reads as an empty object, not as Lua's empty table
/// turned array: the wire carries JSON, and `{}` is an object there. Lua
/// quotes `[]` for its empty table; the rule is the same, the value is
/// not.
#[test]
fn tool_effects_empty_object_keeps_its_shape() {
    let mut spec = tool_spec();
    spec["effects"] = json!({"effects": [{}], "reversible": true});
    let (_dir, mut registry) = test_registry(json!({}));
    let (_, outcome) = process_problems(&mut registry, "c1", "fiber.tool", json!(["t", spec]));
    let RegisterOutcome::Rejected { message, .. } = outcome else {
        panic!("an unreadable effects table refuses, not {outcome:?}");
    };
    assert_eq!(
        message,
        "`t` tool not registered: `effects`: {} is not `reads`, `writes`, `executes` or `network`"
    );
}

#[test]
fn tool_timeout_must_be_a_whole_millisecond_above_zero() {
    for timeout in [json!(0), json!(-5), json!(1.5), json!("fast")] {
        let mut spec = tool_spec();
        spec["timeout"] = timeout.clone();
        let lua_timeout = match &timeout {
            Value::String(_) => "\"fast\"".to_owned(),
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Array(_)
            | Value::Object(_) => timeout.to_string(),
        };
        check_parity(
            &format!(
                "fiber.tool(\"t\", {{ description = \"d\", input_schema = {{ type = \"object\" }}, effects = {{ effects = {{}}, reversible = true }}, timeout = {lua_timeout}, run = function() end }})"
            ),
            json!({}),
            &[],
            "fiber.tool",
            json!(["t", spec]),
        );
    }
    let mut spec = tool_spec();
    spec.as_object_mut().unwrap().remove("timeout");
    check_parity(
        "fiber.tool(\"t\", { description = \"d\", input_schema = { type = \"object\" }, effects = { effects = {}, reversible = true }, run = function() end })",
        json!({}),
        &[],
        "fiber.tool",
        json!(["t", spec]),
    );
}

#[test]
fn tool_run_must_be_a_function() {
    let mut spec = tool_spec();
    spec["run"] = json!(false);
    check_parity(
        "fiber.tool(\"t\", { description = \"d\", input_schema = { type = \"object\" }, effects = { effects = {}, reversible = true }, timeout = 5, run = false })",
        json!({}),
        &[],
        "fiber.tool",
        json!(["t", spec]),
    );
}

#[test]
fn tool_keep_must_be_a_table_of_head_and_tail() {
    let mut spec = tool_spec();
    spec["keep"] = json!({"head": -1});
    check_parity(
        "fiber.tool(\"t\", { description = \"d\", input_schema = { type = \"object\" }, effects = { effects = {}, reversible = true }, timeout = 5, run = function() end, keep = { head = -1 } })",
        json!({}),
        &[],
        "fiber.tool",
        json!(["t", spec]),
    );
    let mut spec = tool_spec();
    spec["keep"] = json!({"head": 0, "tail": 0});
    check_parity(
        "fiber.tool(\"t\", { description = \"d\", input_schema = { type = \"object\" }, effects = { effects = {}, reversible = true }, timeout = 5, run = function() end, keep = { head = 0, tail = 0 } })",
        json!({}),
        &[],
        "fiber.tool",
        json!(["t", spec]),
    );
    let mut spec = tool_spec();
    spec["keep"] = json!({"start": 10});
    check_parity(
        "fiber.tool(\"t\", { description = \"d\", input_schema = { type = \"object\" }, effects = { effects = {}, reversible = true }, timeout = 5, run = function() end, keep = { start = 10 } })",
        json!({}),
        &[],
        "fiber.tool",
        json!(["t", spec]),
    );
}

#[test]
fn tool_registers_under_its_wire_command() {
    let (_dir, mut registry) = test_registry(json!({}));
    let outcome = register(&mut registry, "c7", "fiber.tool", json!(["t", tool_spec()]));
    let RegisterOutcome::Accepted = outcome else {
        panic!("a valid tool registers, not {outcome:?}");
    };
    let entry = registry.tools().get("t").unwrap();
    assert_eq!(entry.run.command, CommandId("c7".to_owned()));
    assert_eq!(entry.run.key, "run");
    assert_eq!(entry.run.generation, 1);
    assert!(registry.problems().is_empty());
    let duplicate = register(&mut registry, "c8", "fiber.tool", json!(["t", tool_spec()]));
    let RegisterOutcome::Rejected { code, message } = duplicate else {
        panic!("a duplicate tool refuses, not {duplicate:?}");
    };
    assert_eq!(code, ErrorCode::ExtensionFailed);
    assert_eq!(message, "`t` tool is already registered");
    assert_eq!(
        registry.tools().get("t").unwrap().run.command,
        CommandId("c7".to_owned()),
        "the first registration keeps its reference"
    );
}

/// A valid hook spec with `run`, as JSON.
fn hook_spec() -> Value {
    json!({
        "phase": "transform",
        "timeout": 5000,
        "on_failure": "blocking",
        "run": true,
    })
}

#[test]
fn hook_point_must_be_known() {
    check_parity(
        "fiber.hook(\"dawn\", { phase = \"transform\", timeout = 5, on_failure = \"blocking\", run = function() end })",
        json!({}),
        &[],
        "fiber.hook",
        json!(["dawn", hook_spec()]),
    );
}

#[test]
fn hook_spec_must_be_a_table() {
    check_parity(
        "fiber.hook(\"turn_end\", 42)",
        json!({}),
        &[],
        "fiber.hook",
        json!(["turn_end", 42]),
    );
}

#[test]
fn hook_timeout_must_be_a_whole_millisecond_above_zero() {
    let mut spec = hook_spec();
    spec.as_object_mut().unwrap().remove("timeout");
    check_parity(
        "fiber.hook(\"turn_end\", { phase = \"transform\", on_failure = \"blocking\", run = function() end })",
        json!({}),
        &[],
        "fiber.hook",
        json!(["turn_end", spec]),
    );
}

#[test]
fn hook_on_failure_must_be_blocking_or_non_blocking() {
    let mut spec = hook_spec();
    spec["on_failure"] = json!("sometimes");
    check_parity(
        "fiber.hook(\"turn_end\", { phase = \"transform\", timeout = 5, on_failure = \"sometimes\", run = function() end })",
        json!({}),
        &[],
        "fiber.hook",
        json!(["turn_end", spec]),
    );
}

#[test]
fn hook_phase_must_suit_its_point() {
    let mut spec = hook_spec();
    spec["phase"] = json!("check");
    check_parity(
        "fiber.hook(\"turn_end\", { phase = \"check\", timeout = 5, on_failure = \"blocking\", run = function() end })",
        json!({}),
        &[],
        "fiber.hook",
        json!(["turn_end", spec]),
    );
}

#[test]
fn hook_run_must_be_a_function() {
    let mut spec = hook_spec();
    spec.as_object_mut().unwrap().remove("run");
    check_parity(
        "fiber.hook(\"turn_end\", { phase = \"transform\", timeout = 5, on_failure = \"blocking\" })",
        json!({}),
        &[],
        "fiber.hook",
        json!(["turn_end", spec]),
    );
}

#[test]
fn hook_registers_each_point_in_order() {
    let points = [
        "session_start",
        "before_message",
        "turn_start",
        "before_tool",
        "before_model_call",
        "after_tool",
        "turn_end",
        "before_handoff",
    ];
    let home = Home::new();
    let init = points
        .iter()
        .map(|point| {
            format!(
                "fiber.hook(\"{point}\", {{ phase = \"transform\", timeout = 5, on_failure = \"blocking\", run = function() end }})"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    install_case(&home, "case", Some(&init), &[], &json!({}));
    let session = home.load(&[]);
    assert!(
        session
            .notices()
            .iter()
            .find(|notice| notice.extension.as_deref() == Some("fiber.test/case"))
            .is_none(),
        "every hook point registers in Lua: {:?}",
        session.notices()
    );
    let (_dir, mut registry) = test_registry(json!({}));
    for (id, point) in (1..).map(|n: u32| format!("c{n}")).zip(points) {
        let outcome = register(
            &mut registry,
            &id,
            "fiber.hook",
            json!([point, hook_spec()]),
        );
        assert!(matches!(outcome, RegisterOutcome::Accepted), "{point}");
    }
    let runs = registry.hook_runs();
    // One `fiber.hook` call registers one hook (`docs/extensions.md`,
    // "Registering"), so one call per point holds one run per point.
    assert_eq!(runs.len(), points.len());
    for (index, point) in points.iter().enumerate() {
        let reference = runs.get(&(point.to_string(), 0)).unwrap();
        assert_eq!(reference.command, CommandId(format!("c{}", index + 1)));
        assert_eq!(reference.key, "run");
        assert_eq!(reference.generation, 1);
    }
}

/// A valid watcher spec with `run`, as JSON.
fn watcher_spec() -> Value {
    json!({"timeout": 5000, "run": true})
}

#[test]
fn watcher_kinds_must_be_a_non_empty_list() {
    check_parity(
        "fiber.watch({}, { timeout = 5, run = function() end })",
        json!({}),
        &[],
        "fiber.watch",
        json!([[], watcher_spec()]),
    );
}

/// An unknown-only watcher registers nothing but still accepts: Lua keeps
/// the problems and skips the entry (`DeclaredWatchers::read`), so the
/// call stands and the notices ride with it. Rejecting here would answer
/// what Lua accepts.
#[test]
fn watcher_unknown_kinds_register_nothing_but_accept() {
    let expected = lua_problems(
        "fiber.watch({ \"dawn\", \"nap\" }, { timeout = 5, run = function() end })",
        json!({}),
        &[],
    );
    assert_eq!(
        expected,
        [
            "`watcher:0` watcher not registered: `dawn` is not an event kind",
            "`watcher:0` watcher not registered: `nap` is not an event kind",
        ],
        "Lua keeps the problems and registers nothing: {expected:?}"
    );
    let (_dir, mut registry) = test_registry(json!({}));
    let (problems, outcome) = process_problems(
        &mut registry,
        "c1",
        "fiber.watch",
        json!([["dawn", "nap"], watcher_spec()]),
    );
    assert!(
        matches!(outcome, RegisterOutcome::Accepted),
        "an unknown-only watcher still accepts, as in Lua, not {outcome:?}"
    );
    assert_eq!(problems, expected);
    assert!(registry.watchers().watchers.is_empty());
    assert!(registry.watcher_runs().is_empty());
}

/// A refused watcher consumes no index: Lua numbers each attempt by the
/// watchers registered so far, so the next surviving watcher takes the
/// refused one's number.
#[test]
fn watcher_index_counts_only_registered_watchers() {
    let expected = lua_problems(
        "fiber.watch({ \"dawn\" }, { timeout = 5, run = function() end })\nfiber.watch({ \"notice\" }, { timeout = 5, run = function() end })",
        json!({}),
        &[],
    );
    assert_eq!(
        expected,
        ["`watcher:0` watcher not registered: `dawn` is not an event kind"],
        "Lua numbers the refused watcher 0: {expected:?}"
    );
    let (_dir, mut registry) = test_registry(json!({}));
    let (_, accepted) = process_problems(
        &mut registry,
        "c1",
        "fiber.watch",
        json!([["dawn"], watcher_spec()]),
    );
    assert!(
        matches!(accepted, RegisterOutcome::Accepted),
        "an unknown-only watcher accepts and consumes no index: {accepted:?}"
    );
    let (problems, accepted) = process_problems(
        &mut registry,
        "c2",
        "fiber.watch",
        json!([["notice"], watcher_spec()]),
    );
    assert!(
        matches!(accepted, RegisterOutcome::Accepted),
        "{accepted:?}"
    );
    assert_eq!(problems, expected);
    // One `fiber.watch` call registers one watcher (`docs/extensions.md`,
    // "Registering"): the accepted call holds one run, and the
    // accepted-but-empty call holds none.
    assert_eq!(registry.watcher_runs().len(), 1);
    let run = registry.watcher_runs().get(&0).unwrap();
    assert_eq!(run.command, CommandId("c2".to_owned()));
    assert_eq!(registry.watchers().watchers.len(), 1);
}

#[test]
fn watcher_registers_surviving_kinds() {
    let expected = lua_problems(
        "fiber.watch({ \"notice\", \"dawn\" }, { timeout = 5, run = function() end })",
        json!({}),
        &[],
    );
    assert_eq!(
        expected,
        ["`watcher:0` watcher not registered: `dawn` is not an event kind"],
        "Lua keeps the known kind and reports the unknown one: {expected:?}"
    );
    let (_dir, mut registry) = test_registry(json!({}));
    let (problems, outcome) = process_problems(
        &mut registry,
        "c1",
        "fiber.watch",
        json!([["notice", "dawn"], watcher_spec()]),
    );
    let RegisterOutcome::Accepted = outcome else {
        panic!("a partially unknown watcher still registers, not {outcome:?}");
    };
    assert_eq!(problems, expected);
    let watchers = registry.watchers();
    // One `fiber.watch` call registers one watcher (`docs/extensions.md`,
    // "Registering"), with only the surviving kinds kept.
    assert_eq!(watchers.watchers.len(), 1);
    assert_eq!(watchers.watchers[0].kinds, vec!["notice".to_owned()]);
    assert_eq!(registry.watcher_runs().len(), 1);
}

/// A valid command spec with `run`, as JSON.
fn command_spec() -> Value {
    json!({"timeout": 5000, "run": true, "description": "Runs it."})
}

#[test]
fn command_name_must_be_plain() {
    check_parity(
        "fiber.command(\"a b\", { timeout = 5, run = function() end, description = \"Runs it.\" })",
        json!({}),
        &[],
        "fiber.command",
        json!(["a b", command_spec()]),
    );
}

#[test]
fn command_spec_must_be_a_table() {
    check_parity(
        "fiber.command(\"cmd\", 42)",
        json!({}),
        &[],
        "fiber.command",
        json!(["cmd", 42]),
    );
}

#[test]
fn command_timeout_must_be_a_whole_millisecond_above_zero() {
    let mut spec = command_spec();
    spec.as_object_mut().unwrap().remove("timeout");
    check_parity(
        "fiber.command(\"cmd\", { run = function() end, description = \"Runs it.\" })",
        json!({}),
        &[],
        "fiber.command",
        json!(["cmd", spec]),
    );
}

#[test]
fn command_run_must_be_a_function() {
    let mut spec = command_spec();
    spec["run"] = json!("yes");
    check_parity(
        "fiber.command(\"cmd\", { timeout = 5, run = \"yes\", description = \"Runs it.\" })",
        json!({}),
        &[],
        "fiber.command",
        json!(["cmd", spec]),
    );
}

#[test]
fn command_description_must_be_single_line() {
    let mut spec = command_spec();
    spec["description"] = json!("one\ntwo");
    check_parity(
        "fiber.command(\"cmd\", { timeout = 5, run = function() end, description = \"one\\ntwo\" })",
        json!({}),
        &[],
        "fiber.command",
        json!(["cmd", spec]),
    );
}

#[test]
fn command_registers_and_re_registers() {
    let (_dir, mut registry) = test_registry(json!({}));
    let outcome = register(
        &mut registry,
        "c1",
        "fiber.command",
        json!(["cmd", command_spec()]),
    );
    assert!(matches!(outcome, RegisterOutcome::Accepted));
    let second = register(
        &mut registry,
        "c2",
        "fiber.command",
        json!(["cmd", command_spec()]),
    );
    assert!(matches!(second, RegisterOutcome::Accepted));
    let entry = registry.commands().get("cmd").unwrap();
    assert_eq!(entry.run.command, CommandId("c2".to_owned()));
    assert!(registry.problems().is_empty());
}

/// A registry for `fiber.test/proc` on a shared runtime: a restart is a
/// second registry for the same owner, so its registrations mint under
/// the next generation.
fn shared_registry(runtime: &Runtime, extra: Value) -> (fakes::TempDir, Registry) {
    let dir = package_dir(extra);
    let manifest = config::read_manifest(dir.path()).unwrap();
    let registry = Registry::new("fiber.test/proc".to_owned(), runtime.clone(), &manifest);
    (dir, registry)
}

/// A restart re-registers the owner under a new generation: the old
/// provider reference no longer resolves, and it never selects the
/// replacement callback.
#[test]
fn provider_references_resolve_only_within_their_generation() {
    use std::sync::Arc as Shared;
    let runtime = Runtime::new(fakes::clock::FakeClock::new());
    let (_dir1, mut first) = shared_registry(&runtime, json!({"providers": {"p": []}}));
    let outcome = register(
        &mut first,
        "c1",
        "fiber.provider",
        json!(["p", provider_spec()]),
    );
    assert!(matches!(outcome, RegisterOutcome::Accepted), "{outcome:?}");
    let old = first
        .providers()
        .get("p")
        .unwrap()
        .runs
        .get("models")
        .unwrap()
        .clone();
    assert_eq!(old.generation, 1);
    first.set_handler(
        &CommandId("c1".to_owned()),
        "models",
        Shared::new(|_, _| Ok(Some(json!({"generation": 1})))),
    );
    let (_dir2, mut second) = shared_registry(&runtime, json!({"providers": {"p": []}}));
    let outcome = register(
        &mut second,
        "c1",
        "fiber.provider",
        json!(["p", provider_spec()]),
    );
    assert!(matches!(outcome, RegisterOutcome::Accepted), "{outcome:?}");
    let new = second
        .providers()
        .get("p")
        .unwrap()
        .runs
        .get("models")
        .unwrap()
        .clone();
    assert_eq!(new.generation, 2);
    second.set_handler(
        &CommandId("c1".to_owned()),
        "models",
        Shared::new(|_, _| Ok(Some(json!({"generation": 2})))),
    );
    let stale = runtime
        .provider_call(&old, json!({}))
        .unwrap_err()
        .to_string();
    assert!(stale.contains("generation 1"), "{stale}");
    assert_eq!(
        runtime.provider_call(&new, json!({})).unwrap(),
        json!({"generation": 2})
    );
}

/// A positional reference shares the triple: its key may be `"1"`, and it
/// resolves only within the generation that minted it.
#[test]
fn positional_references_share_the_generation_triple() {
    use std::sync::Arc as Shared;
    let runtime = Runtime::new(fakes::clock::FakeClock::new());
    let (_dir, mut registry) = shared_registry(&runtime, json!({}));
    let outcome = register(&mut registry, "c1", "fiber.tool", json!(["t", tool_spec()]));
    assert!(matches!(outcome, RegisterOutcome::Accepted), "{outcome:?}");
    let positional = registry.set_handler(
        &CommandId("c1".to_owned()),
        "1",
        Shared::new(|_, _| Ok(Some(json!({"positional": true})))),
    );
    assert_eq!(positional.command, CommandId("c1".to_owned()));
    assert_eq!(positional.key, "1");
    assert_eq!(positional.generation, 1);
    assert!(runtime.resolve(&positional).is_ok());
    runtime.register_owner("fiber.test/proc", None);
    assert!(runtime.resolve(&positional).is_err());
}
