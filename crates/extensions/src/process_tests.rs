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

use std::os::unix::process::ExitStatusExt as _;
use std::time::Duration;

use contract::{CommandId, ErrorCode};
use serde_json::{Value, json};

use super::process::{RegisterOutcome, Registry};
use super::runtime::Runtime;
use super::test_support::Home;
use crate::lua::HookPhase;

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
    let null_quota = register(
        &mut registry,
        "c10",
        "fiber.harness",
        json!(["n", {"timeout": 5000, "command": true, "line": true, "models": true, "quota": null}]),
    );
    assert!(
        matches!(null_quota, RegisterOutcome::Accepted),
        "{null_quota:?}"
    );
    assert!(
        !registry
            .harness()
            .get("n")
            .unwrap()
            .runs
            .contains_key("quota")
    );
    let without_quota = register(
        &mut registry,
        "c11",
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
fn a_valid_tool_keeps_its_function_effects_and_one_sided_bound() {
    let (_dir, mut registry) = test_registry(json!({}));
    let mut spec = tool_spec();
    spec["effects"] = json!(true);
    spec["keep"] = json!({"head": 7, "tail": 0});
    let outcome = register(
        &mut registry,
        "c1",
        "fiber.tool",
        json!(["read_file-2", spec]),
    );
    assert!(matches!(outcome, RegisterOutcome::Accepted), "{outcome:?}");
    let entry = registry.tools().get("read_file-2").unwrap();
    assert_eq!(entry.declared.keep.start, 7);
    assert_eq!(entry.declared.keep.end, 0);
    assert_eq!(entry.effects.as_ref().unwrap().key, "effects");
}

#[test]
fn an_omitted_or_null_keep_uses_the_default_bound() {
    for keep in [None, Some(Value::Null)] {
        let (_dir, mut registry) = test_registry(json!({}));
        let mut spec = tool_spec();
        if let Some(keep) = keep {
            spec["keep"] = keep;
        }
        let outcome = register(&mut registry, "c1", "fiber.tool", json!(["lookup", spec]));
        assert!(matches!(outcome, RegisterOutcome::Accepted), "{outcome:?}");
        assert_eq!(
            registry.tools().get("lookup").unwrap().declared.keep,
            contract::tool::Bound::DEFAULT
        );
    }
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
fn hook_rejects_a_phase_outside_the_documented_set() {
    let mut spec = hook_spec();
    spec["phase"] = json!("validate");
    check_parity(
        "fiber.hook(\"before_tool\", { phase = \"validate\", timeout = 5, on_failure = \"blocking\", run = function() end })",
        json!({}),
        &[],
        "fiber.hook",
        json!(["before_tool", spec]),
    );
}

#[test]
fn hook_accepts_non_blocking_and_sanitize_on_turn_end() {
    let (_dir, mut registry) = test_registry(json!({}));
    let outcome = register(
        &mut registry,
        "c1",
        "fiber.hook",
        json!(["turn_end", {
            "phase": "sanitize",
            "timeout": 5000,
            "on_failure": "non-blocking",
            "run": true,
        }]),
    );
    assert!(matches!(outcome, RegisterOutcome::Accepted), "{outcome:?}");
    let hook = &registry.hooks().by_point["turn_end"][0];
    assert_eq!(hook.phase, HookPhase::Sanitize);
    assert!(!hook.blocking);
}

#[test]
fn a_missing_or_null_hook_phase_defaults_to_transform() {
    for phase in [None, Some(Value::Null)] {
        let (_dir, mut registry) = test_registry(json!({}));
        let mut spec = hook_spec();
        if let Some(phase) = phase {
            spec["phase"] = phase;
        } else {
            spec.as_object_mut().unwrap().remove("phase");
        }
        let outcome = register(&mut registry, "c1", "fiber.hook", json!(["turn_end", spec]));
        assert!(matches!(outcome, RegisterOutcome::Accepted), "{outcome:?}");
        assert_eq!(
            registry.hooks().by_point["turn_end"][0].phase,
            HookPhase::Transform
        );
    }
}

#[test]
fn hook_rejection_names_its_entry() {
    let (_dir, mut registry) = test_registry(json!({}));
    let outcome = register(
        &mut registry,
        "c1",
        "fiber.hook",
        json!(["dawn", {
            "entry": "init.lua:17",
            "phase": "transform",
            "timeout": 5000,
            "on_failure": "blocking",
            "run": true,
        }]),
    );
    let RegisterOutcome::Rejected { message, .. } = outcome else {
        panic!("an unknown hook point refuses, not {outcome:?}");
    };
    assert_eq!(
        message,
        "`dawn` hook not registered for entry `init.lua:17`: `dawn` is not a hook point"
    );
}

#[test]
fn missing_hook_failure_names_the_missing_field() {
    for failure in [None, Some(Value::Null)] {
        let (_dir, mut registry) = test_registry(json!({}));
        let mut spec = hook_spec();
        if let Some(failure) = failure {
            spec["on_failure"] = failure;
        } else {
            spec.as_object_mut().unwrap().remove("on_failure");
        }
        let outcome = register(&mut registry, "c1", "fiber.hook", json!(["turn_end", spec]));
        let RegisterOutcome::Rejected { message, .. } = outcome else {
            panic!("a hook without on_failure refuses, not {outcome:?}");
        };
        assert_eq!(
            message,
            "`turn_end` hook not registered: missing `on_failure`"
        );
    }
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
fn command_name_with_a_slash_is_rejected() {
    check_parity(
        "fiber.command(\"group/run\", { timeout = 5, run = function() end, description = \"Runs it.\" })",
        json!({}),
        &[],
        "fiber.command",
        json!(["group/run", command_spec()]),
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
fn command_description_may_be_omitted_or_null() {
    for description in [None, Some(Value::Null)] {
        let (_dir, mut registry) = test_registry(json!({}));
        let mut spec = command_spec();
        if let Some(description) = description {
            spec["description"] = description;
        } else {
            spec.as_object_mut().unwrap().remove("description");
        }
        let outcome = register(&mut registry, "c1", "fiber.command", json!(["cmd", spec]));
        assert!(matches!(outcome, RegisterOutcome::Accepted), "{outcome:?}");
        assert_eq!(registry.commands().get("cmd").unwrap().description, "");
    }
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
/// replacement callback. Provider calls themselves stay on `LuaExtension`
/// directly, so the gate is `resolve`, not dispatch.
#[test]
fn provider_references_resolve_only_within_their_generation() {
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
    assert_eq!(runtime.generation("fiber.test/proc"), Some(1));
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
    assert_eq!(runtime.generation("fiber.test/proc"), Some(2));
    let stale = runtime.resolve(&old).unwrap_err().to_string();
    assert!(stale.contains("generation 1"), "{stale}");
    assert!(runtime.resolve(&new).is_ok());
}

/// Process hooks keep explicit phase separately from the effective
/// `transform` default, never acquire `entry`, and refuse one when
/// supplied. Flipping the flag would list an undeclared phase; keeping the
/// entry would leak configuration identity through self-registration. The
/// phase default and its explicitness follow `docs/extensions.md`, "When
/// several hooks share a point"; `entry` arrives only from configuration
/// declarations (`docs/extensions.md`, "Hooks declared in configuration").
#[test]
fn process_hook_phase_explicitness_and_entry_rejection() {
    let (_dir, mut registry) = test_registry(json!({}));
    let explicit = register(
        &mut registry,
        "c1",
        "fiber.hook",
        json!(["turn_end", {
            "phase": "sanitize", "timeout": 5000,
            "on_failure": "blocking", "run": true,
        }]),
    );
    assert!(
        matches!(explicit, RegisterOutcome::Accepted),
        "{explicit:?}"
    );
    let hook = &registry.hooks().by_point["turn_end"][0];
    assert!(hook.phase_explicit);
    assert_eq!(hook.phase, HookPhase::Sanitize);
    assert_eq!(hook.entry, None);
    let mut omitted = hook_spec();
    omitted.as_object_mut().unwrap().remove("phase");
    let accepted = register(
        &mut registry,
        "c2",
        "fiber.hook",
        json!(["turn_end", omitted]),
    );
    assert!(
        matches!(accepted, RegisterOutcome::Accepted),
        "{accepted:?}"
    );
    let hook = &registry.hooks().by_point["turn_end"][1];
    assert!(!hook.phase_explicit);
    assert_eq!(hook.phase, HookPhase::Transform);
    assert_eq!(hook.entry, None);
    let mut with_entry = hook_spec();
    with_entry
        .as_object_mut()
        .unwrap()
        .insert("entry".to_owned(), Value::String("audit".to_owned()));
    let rejected = register(
        &mut registry,
        "c3",
        "fiber.hook",
        json!(["turn_end", with_entry]),
    );
    let RegisterOutcome::Rejected { message, .. } = rejected else {
        panic!("an entry-carrying process hook is refused");
    };
    assert!(
        message.contains("`entry` is declared configuration only"),
        "{message}"
    );
    assert_eq!(registry.hooks().by_point["turn_end"].len(), 2);
    let watch_rejected = register(
        &mut registry,
        "c4",
        "fiber.watch",
        json!([["turn_completed"], {"timeout": 5000, "run": true, "entry": "ping"}]),
    );
    let RegisterOutcome::Rejected { message, .. } = watch_rejected else {
        panic!("an entry-carrying process watcher is refused");
    };
    assert!(
        message.contains("`entry` is declared configuration only"),
        "{message}"
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

// Process transport and request lifecycle: the owner behind the package
// seam, with the shared checked-in Rust process fixture answering over the
// pipe. Each test supplies bounded JSON protocol actions.

use std::sync::{Arc, Mutex};

use contract::clock::Clock as _;

use super::process::transport::CloseReason;
use super::process::{ProcessExtension, Pumped, context::ProcessContext};

/// The bound a positive wait keeps: fast on success, failing the test
/// instead of hanging it.
const WITHIN: Duration = Duration::from_secs(10);

/// One `host.drive` call the tests observe, answered at once with no
/// result.
struct FakeDrive {
    calls: Mutex<Vec<(String, serde_json::Map<String, Value>)>>,
}

impl contract::extension::Drive for FakeDrive {
    fn drive(
        &self,
        _extension: &str,
        command: &str,
        args: serde_json::Map<String, Value>,
        answer: contract::inbox::Ack,
    ) {
        support::lock(&self.calls).push((command.to_owned(), args));
        answer.0(Ok(None));
    }
}

impl FakeDrive {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
        })
    }
}

/// Fake host/state services behind the seam: records each operation's
/// variant with the callback's attribution, answering from the script.
/// A test scripts the answer, makes the wire call, then reads the record
/// to prove the operation ran with its attribution, or, when the call
/// refuses before dispatch, that nothing ran at all.
struct FakeServices {
    calls: Mutex<Vec<(String, Option<String>)>>,
    answer: Mutex<Result<Option<Value>, (ErrorCode, String)>>,
}

impl FakeServices {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            answer: Mutex::new(Ok(None)),
        })
    }

    /// Scripts every later operation's answer until set again.
    fn answer_with(&self, answer: Result<Option<Value>, (ErrorCode, String)>) {
        *support::lock(&self.answer) = answer;
    }

    /// The recorded `(variant, ran_by)` pairs, in arrival order.
    fn calls(&self) -> Vec<(String, Option<String>)> {
        support::lock(&self.calls).clone()
    }
}

/// The operation's variant, naming which host or state call ran.
fn service_variant(operation: &super::process::context::HostOperation) -> &'static str {
    use super::process::context::HostOperation as Op;
    match operation {
        Op::Http { .. } => "http",
        Op::Exec { .. } => "exec",
        Op::FsRead { .. } => "fs.read",
        Op::FsWrite { .. } => "fs.write",
        Op::FsList { .. } => "fs.list",
        Op::FsStat { .. } => "fs.stat",
        Op::FsMkdir { .. } => "fs.mkdir",
        Op::FsRemove { .. } => "fs.remove",
        Op::FsRename { .. } => "fs.rename",
        Op::ConfigGet { .. } => "config.get",
        Op::ConfigSet { .. } => "config.set",
        Op::Secret { .. } => "secret",
        Op::DataDir { .. } => "data_dir",
        Op::Sha256 { .. } => "sha256",
        Op::HmacSha256 { .. } => "hmac_sha256",
        Op::Status { .. } => "status",
        Op::Widget { .. } => "widget",
        Op::Emit { .. } => "emit",
        Op::Log { .. } => "log",
        Op::Tool { .. } => "tool",
        Op::Model { .. } => "model",
        Op::Delegate { .. } => "delegate",
        Op::OAuthCallback { .. } => "oauth.callback",
        Op::OAuthOpen { .. } => "oauth.open",
        Op::OAuthShow { .. } => "oauth.show",
        Op::OAuthPkcs => "oauth.pkce",
        Op::OAuthPoll { .. } => "oauth.poll",
        Op::StateGet { .. } => "state.get",
        Op::StateKeys => "state.keys",
        Op::StateSet { .. } => "state.set",
        Op::StateUnset { .. } => "state.unset",
    }
}

impl super::process::context::ProcessHostServices for FakeServices {
    fn perform(
        &self,
        _context: &super::process::context::ProcessContext,
        callback: &crate::host::admission::CallbackContext,
        operation: super::process::context::HostOperation,
        _cancel: &dyn contract::tool::Cancel,
    ) -> Result<Option<Value>, (ErrorCode, String)> {
        support::lock(&self.calls).push((
            service_variant(&operation).to_owned(),
            callback.ran_by().map(str::to_owned),
        ));
        support::lock(&self.answer).clone()
    }
}

/// One owner behind the package seam, with its scratch dir and registry:
/// the package holds both, and the session fold reads what the child
/// registers.
struct Harness {
    _dir: fakes::TempDir,
    registry: Arc<Mutex<super::process::Registry>>,
    extension: Arc<ProcessExtension>,
    /// The frozen clock the owner reads: the test moves it to fire
    /// timeouts, since nothing else does.
    clock: Arc<fakes::clock::FakeClock>,
    /// The fake services the owner's host and state calls run through.
    services: Arc<FakeServices>,
}

impl Harness {
    /// Starts the shared Rust process fixture with a JSON protocol plan and
    /// `args` in a scratch workspace, the hello carrying `state`.
    fn start(
        script: &str,
        args: &[&str],
        state: serde_json::Map<String, Value>,
        drive: Option<Arc<dyn contract::extension::Drive>>,
    ) -> Self {
        let recorded = FakeServices::new();
        Self::start_with_services(
            script,
            args,
            state,
            drive,
            {
                let recorded = Arc::clone(&recorded);
                move |_, _, _| -> Arc<dyn super::process::context::ProcessHostServices> { recorded }
            },
            recorded,
        )
    }

    /// Starts as [`Harness::start`], with the services the owner answers
    /// host and state calls through: the fake records, while production
    /// services prove the store behind the seam. The maker receives the
    /// state store, the clock and the runtime the launch wires; the
    /// harness keeps `recorded`, which is the fake for `start` and an
    /// unrecorded stand-in otherwise.
    fn start_with_services(
        script: &str,
        args: &[&str],
        state: serde_json::Map<String, Value>,
        drive: Option<Arc<dyn contract::extension::Drive>>,
        services: impl FnOnce(
            Arc<super::host::state::ExtensionState>,
            Arc<dyn contract::clock::Clock>,
            Runtime,
        ) -> Arc<dyn super::process::context::ProcessHostServices>,
        recorded: Arc<FakeServices>,
    ) -> Self {
        let dir = fakes::TempDir::new("fiber-process-owner");
        std::fs::write(
            dir.path().join("extension.json"),
            json!({
                "name": "fiber.test/proc",
                "version": "v1.2.3",
                "fiber": "0.1.0",
                "api": 1,
            })
            .to_string(),
        )
        .unwrap();
        let manifest = config::read_manifest(dir.path()).unwrap();
        let clock = fakes::clock::FakeClock::new();
        let runtime = Runtime::new(clock.clone());
        let registry = Arc::new(Mutex::new(super::process::Registry::new(
            "fiber.test/proc".to_owned(),
            runtime.clone(),
            &manifest,
        )));
        let context = ProcessContext {
            session_id: contract::SessionId("s_test".into()),
            reason: contract::events::ExtensionReason::New,
            workspace: dir.path().to_path_buf(),
            data_dirs: contract::events::ExtensionDataDirs {
                machine: dir.path().join("machine").display().to_string(),
                project: dir.path().join("project").display().to_string(),
            },
            state,
            extension: "fiber.test/proc".into(),
            home: dir.path().to_path_buf(),
        };
        let mut plan: Value =
            serde_json::from_str(script).expect("the process fixture plan is valid JSON");
        if let Some(object) = plan.as_object_mut() {
            object.insert(
                "args".to_owned(),
                Value::Array(
                    args.iter()
                        .map(|arg| Value::String((*arg).to_owned()))
                        .collect(),
                ),
            );
        } else {
            plan = json!({"actions": plan, "args": args});
        }
        let (program, argv) = fakes::ProcessFixture::command("peer", Some(&plan));
        let launch_clock: Arc<dyn contract::clock::Clock> = clock.clone();
        // One standalone store behind both the launch and the services,
        // as the package seam's, before the session folds it.
        let store = super::host::state::StateRegistry::standalone().store("fiber.test/proc");
        let services = services(store.clone(), launch_clock.clone(), runtime.clone());
        let extension = ProcessExtension::start(super::process::Launch {
            owner: "fiber.test/proc".to_owned(),
            runtime,
            registry: Arc::clone(&registry),
            program,
            args: argv,
            context,
            drive,
            services,
            // The refresh lock resolves pairs from the scratch home.
            oauth: super::process::oauth::FileCredential::new(dir.path().to_path_buf()),
            state: store,
            clock: launch_clock,
            limits: super::process::transport::Limits::default(),
        })
        .expect("a peer child starts");
        Self {
            _dir: dir,
            registry,
            extension: Arc::new(extension),
            clock,
            services: recorded,
        }
    }

    /// The workspace the peer runs in.
    fn workspace(&self) -> &std::path::Path {
        self._dir.path()
    }

    /// Pumps one line, which must arrive and be handled.
    fn pump(&self) {
        let pumped = self.extension.pump_once();
        if pumped != Pumped::Handled {
            let stderr = self.extension.transport().take_stderr();
            panic!(
                "expected a handled peer line, got {pumped:?}; peer stderr: {}",
                String::from_utf8_lossy(&stderr)
            );
        }
    }

    /// Pumps until the pipe closes, then reaps and joins: a watcher stops
    /// and kills the peer once the bound passes with no close, so a
    /// wedged peer fails the test instead of hanging it.
    fn drain(&self) -> CloseReason {
        let watchdog = Arc::clone(&self.extension);
        std::thread::spawn(move || {
            // The held sender keeps the channel live, so the receive
            // ends only when the bound passes, never from a hangup.
            let (_held, never) = std::sync::mpsc::channel::<()>();
            if fakes::Deadline::after(WITHIN).recv(&never).is_err() {
                watchdog.transport().stop();
                match watchdog.transport().signal(rustix::process::Signal::KILL) {
                    Ok(()) | Err(_) => {}
                }
            }
        });
        loop {
            match self.extension.pump_once() {
                Pumped::Handled => {}
                Pumped::Closed(reason) => {
                    match self.extension.transport().wait_exit() {
                        Ok(_) | Err(_) => {}
                    }
                    self.extension.transport().join();
                    return reason;
                }
            }
        }
    }
}

/// Reads `path` as JSON within one wall-clock deadline: the peer writes it
/// once it has what it waits for.
fn read_json_file(path: &std::path::Path, what: &str) -> Value {
    let owned = path.to_path_buf();
    let result = Arc::new(Mutex::new(None));
    let observed = Arc::clone(&result);
    fakes::wait_until(
        &format!("the peer to write {what}"),
        fakes::Deadline::after(WITHIN),
        move || {
            let value = std::fs::read_to_string(&owned)
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok());
            if let Some(value) = value {
                *support::lock(&observed) = Some(value);
                true
            } else {
                false
            }
        },
    );
    support::lock(&result)
        .take()
        .expect("the JSON file appeared before its deadline")
}

/// A valid `fiber.tool` spec with `run`, as JSON: `true` marks the
/// function, as on the wire.
fn wire_tool_spec() -> Value {
    json!({
        "description": "Does the thing.",
        "input_schema": {"type": "object"},
        "effects": {"effects": [], "reversible": true},
        "timeout": 5000,
        "run": true,
    })
}

/// A valid `fiber.hook` spec with `run`, as JSON.
fn wire_hook_spec() -> Value {
    json!({
        "phase": "transform",
        "timeout": 5000,
        "on_failure": "blocking",
        "run": true,
    })
}

/// A valid `fiber.command` spec with `run`, as JSON.
fn wire_command_spec() -> Value {
    json!({"description": "Runs it.", "timeout": 5000, "run": true})
}

/// The hello is the first complete outbound line, carrying the session's
/// identity, why it started, its workspace, the data directories and the
/// folded state.
#[test]
fn hello_is_first_with_the_documented_fields() {
    let out = fakes::TempDir::new("fiber-process-hello");
    let out_path = out.path().join("first.jsonl");
    let script = &json!([
        {"read": {}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "last"}}},
    ])
    .to_string();
    let mut state = serde_json::Map::new();
    state.insert("k".to_owned(), json!("v"));
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap()],
        state,
        Some(FakeDrive::new()),
    );
    assert_eq!(owner.drain(), CloseReason::Eof);
    let line = std::fs::read_to_string(&out_path).expect("the peer writes the first line");
    let envelope: Value = serde_json::from_str(&line).expect("the hello is one JSON line");
    // docs/extensions.md:211, "`extension_hello` is the first line Fiber sends".
    assert_eq!(envelope["kind"], json!("extension_hello"));
    assert_eq!(envelope["session_id"], json!("s_test"));
    assert_eq!(envelope["schema_version"], json!(1));
    assert!(envelope["ts"].is_u64());
    let payload = &envelope["payload"];
    assert_eq!(payload["session_id"], json!("s_test"));
    assert_eq!(payload["reason"], json!("new"));
    assert_eq!(
        payload["workspace"],
        json!(owner.workspace().display().to_string())
    );
    assert_eq!(
        payload["data_dirs"]["machine"],
        json!(owner.workspace().join("machine").display().to_string())
    );
    assert_eq!(
        payload["data_dirs"]["project"],
        json!(owner.workspace().join("project").display().to_string())
    );
    assert_eq!(payload["state"], json!({"k": "v"}));
    assert!(envelope.get("seq").is_none(), "the hello is ephemeral");
}

/// Registrations stand only before `ready`: a later registration refuses
/// with the entry sentence and mutates nothing, and a call naming a dead
/// request reads `stale_request`.
#[test]
fn registrations_stand_only_before_ready() {
    let script = &json!([
        {"read": {}},
        {"send": {"id": "c1", "command": "call", "args": {"call": "fiber.tool", "args": ["t", wire_tool_spec()]}}},
        {"send": {"id": "c2", "command": "call", "args": {"call": "fiber.hook", "args": ["turn_end", wire_hook_spec()]}}},
        {"send": {"id": "c3", "command": "ready"}},
        {"repeat": {"count": 3, "actions": [{"read": {"append_to": "answers"}}]}},
        {"send": {"id": "c4", "command": "call", "args": {"call": "fiber.tool", "args": ["late", wire_tool_spec()]}}},
        {"send": {"id": "c5", "command": "call", "args": {"call": "fiber.tool", "args": ["dead", wire_tool_spec()], "request_id": "r_dead"}}},
        {"repeat": {"count": 2, "actions": [{"read": {"append_to": "answers"}}]}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "answers"}}},
    ])
    .to_string();
    let out = fakes::TempDir::new("fiber-process-register");
    let out_path = out.path().join("answers.json");
    let owner = Harness::start(
        script,
        &[
            out_path.to_str().unwrap(),
            &wire_tool_spec().to_string(),
            &wire_hook_spec().to_string(),
        ],
        serde_json::Map::new(),
        Some(FakeDrive::new()),
    );
    for _ in 0..5 {
        owner.pump();
    }
    assert!(owner.extension.is_ready());
    {
        let registry = support::lock(&owner.registry);
        assert!(registry.tools().contains_key("t"));
        assert!(registry.problems().is_empty());
    }
    assert_eq!(owner.drain(), CloseReason::Eof);
    let answers = read_json_file(&out_path, "the registration answers");
    // docs/invocation.md:368, every command gets exactly one acknowledgement.
    assert_eq!(answers.as_array().unwrap().len(), 5);
    for (id, index) in [("c1", 0), ("c2", 1), ("c3", 2)] {
        let answer = &answers[index];
        assert_eq!(answer["kind"], json!("command_accepted"), "{id}");
        assert_eq!(answer["payload"]["command_id"], json!(id), "{id}");
    }
    let refused = &answers[3];
    assert_eq!(refused["kind"], json!("command_rejected"));
    assert_eq!(refused["payload"]["code"], json!("extension_failed"));
    assert_eq!(
        refused["payload"]["message"],
        json!("fiber.tool: registers only while `init.lua` runs")
    );
    let stale = &answers[4];
    assert_eq!(stale["kind"], json!("command_rejected"));
    assert_eq!(stale["payload"]["code"], json!("stale_request"));
    // The refusal mutated nothing: no tool stands, and no notice grows.
    {
        let registry = support::lock(&owner.registry);
        assert!(!registry.tools().contains_key("late"));
        assert!(!registry.tools().contains_key("dead"));
        assert!(registry.problems().is_empty());
    }
}

/// A failed package stays failed: later calls fail with the first
/// failure, and the registry holds nothing from either call.
#[test]
fn a_failed_package_fails_later_calls_with_the_first_failure() {
    let script = &json!([
        {"read": {}},
        {"send": {"id": "c1", "command": "call", "args": {"call": "fiber.provider", "args": ["p", provider_spec()]}}},
        {"send": {"id": "c2", "command": "call", "args": {"call": "fiber.tool", "args": ["t", wire_tool_spec()]}}},
        {"send": {"id": "c3", "command": "ready"}},
        {"repeat": {"count": 3, "actions": [{"read": {"append_to": "answers"}}]}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "answers"}}},
    ])
    .to_string();
    let out = fakes::TempDir::new("fiber-process-failed");
    let out_path = out.path().join("answers.json");
    let owner = Harness::start(
        script,
        &[
            out_path.to_str().unwrap(),
            &provider_spec().to_string(),
            &wire_tool_spec().to_string(),
        ],
        serde_json::Map::new(),
        Some(FakeDrive::new()),
    );
    for _ in 0..3 {
        owner.pump();
    }
    // Nothing registers: the provider is undeclared, and the failure
    // latches before the tool call.
    {
        let registry = support::lock(&owner.registry);
        assert!(registry.providers().is_empty());
        assert!(registry.tools().is_empty());
        let (code, message) = registry.failure().expect("the package failed");
        assert_eq!(code, contract::ErrorCode::ExtensionFailed);
        assert_eq!(
            message,
            "The extension `fiber.test/proc` registers the provider `p`, which its manifest does not declare under `providers`."
        );
    }
    assert_eq!(owner.drain(), CloseReason::Eof);
    let answers = read_json_file(&out_path, "the failure answers");
    for (id, index) in [("c1", 0), ("c2", 1)] {
        let answer = &answers[index];
        assert_eq!(answer["kind"], json!("command_rejected"), "{id}");
        assert_eq!(answer["payload"]["command_id"], json!(id), "{id}");
        assert_eq!(answer["payload"]["code"], json!("extension_failed"), "{id}");
        assert_eq!(
            answer["payload"]["message"],
            json!(
                "The extension `fiber.test/proc` registers the provider `p`, which its manifest does not declare under `providers`."
            ),
            "{id}"
        );
    }
    // The ready stands: the package is already dead, and the answer says so.
    assert_eq!(answers[2]["kind"], json!("command_accepted"));
}

/// Stream items run one at a time in arrival order while tools run as
/// they arrive: the hook waits, two tools answer reversed, and the
/// command runs only after the hook's reply.
#[test]
fn stream_order_holds_while_tools_answer_reversed() {
    let reply = |name: &str, id: &str, result: Value| {
        json!({"send": {"id": id, "command": "reply", "args": {
            "request_id": {"$ref": format!("{name}.payload.request_id")}, "result": result,
        }}})
    };
    let reversed = |hook: &str, first: &str, second: &str| {
        json!([
            reply(second, "p-tool-0", json!("tool-0")),
            reply(first, "p-tool-1", json!("tool-1")),
            reply(
                hook,
                "p-hook",
                json!({"$ref": format!("{hook}.payload.registration")})
            ),
        ])
    };
    let script = &json!([
        {"read": {}},
        {"send": {"id": "c1", "command": "call", "args": {"call": "fiber.tool", "args": ["t", wire_tool_spec()]}}},
        {"send": {"id": "c2", "command": "call", "args": {"call": "fiber.hook", "args": ["turn_end", wire_hook_spec()]}}},
        {"send": {"id": "c3", "command": "call", "args": {"call": "fiber.command", "args": ["go", wire_command_spec()]}}},
        {"send": {"id": "c4", "command": "ready"}},
        {"repeat": {"count": 4, "actions": [{"read": {}}]}},
        {"read": {"as": "first", "expect": {"kind": "extension_request", "payload": {"registration": "c2"}}, "append_to": "requests"}},
        {"send": {"id": "sync", "command": "tools", "args": {}}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"write_file": {"path": {"$ref": "args.4"}, "value": "acknowledged"}},
        {"read": {"as": "second", "expect": {"kind": "extension_request"}, "append_to": "requests"}},
        {"read": {"as": "third", "expect": {"kind": "extension_request"}, "append_to": "requests"}},
        {"if": {"path": "first.payload.registration", "equals": "c2",
            "then": reversed("first", "second", "third"),
            "else": [{"if": {"path": "second.payload.registration", "equals": "c2",
                "then": reversed("second", "first", "third"),
                "else": reversed("third", "first", "second"),
            }}],
        }},
        // Three reply acknowledgements and the command can interleave.
        // Its reply adds one more acknowledgement, for five lines total.
        {"repeat": {"count": 5, "actions": [
            {"read": {"as": "next", "append_to": "tail"}},
            {"if": {"path": "next.kind", "equals": "extension_request", "then": [
                reply("next", "p-command", json!({"$ref": "next.payload.registration"})),
            ]}},
        ]}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {
            "requests": {"$ref": "requests"}, "tail": {"$ref": "tail"},
        }}},
    ])
    .to_string();
    let out = fakes::TempDir::new("fiber-process-order");
    let out_path = out.path().join("order.json");
    let marker_path = out.path().join("marker-ack");
    let owner = Harness::start(
        script,
        &[
            out_path.to_str().unwrap(),
            &wire_tool_spec().to_string(),
            &wire_hook_spec().to_string(),
            &wire_command_spec().to_string(),
            marker_path.to_str().unwrap(),
        ],
        serde_json::Map::new(),
        Some(FakeDrive::new()),
    );
    for _ in 0..4 {
        owner.pump();
    }
    let (hook, tool, command) = {
        let registry = support::lock(&owner.registry);
        (
            registry.hook_runs()[&("turn_end".to_owned(), 0)].clone(),
            registry.tools()["t"].run.clone(),
            registry.commands()["go"].run.clone(),
        )
    };
    let hook_extension = Arc::clone(&owner.extension);
    let hook_thread = std::thread::spawn(move || {
        let hook = hook;
        let caller = support::cancel::Token::new();
        hook_extension.dispatch_stream(
            &hook,
            json!({"event": "turn_end"}),
            Duration::from_secs(10),
            None,
            &caller,
        )
    });
    // The peer writes the marker only after reading the driver's
    // command_accepted. Wait for that proof before dispatching the command;
    // thread spawn order alone does not order stream admission.
    owner.pump();
    let marker_path = marker_path.clone();
    fakes::wait_until(
        "the peer to read the driver acknowledgement",
        fakes::Deadline::after(WITHIN),
        move || marker_path.is_file(),
    );
    let tool_extension = Arc::clone(&owner.extension);
    let tool_ref = tool.clone();
    let tool_a = std::thread::spawn(move || {
        let tool = tool_ref;
        let caller = support::cancel::Token::new();
        tool_extension.dispatch_tool(
            &tool,
            json!({"input": "a"}),
            Duration::from_secs(10),
            &caller,
        )
    });
    let tool_extension = Arc::clone(&owner.extension);
    let tool_b = std::thread::spawn(move || {
        let tool = tool;
        let caller = support::cancel::Token::new();
        tool_extension.dispatch_tool(
            &tool,
            json!({"input": "b"}),
            Duration::from_secs(10),
            &caller,
        )
    });
    let command_extension = Arc::clone(&owner.extension);
    let command_thread = std::thread::spawn(move || {
        let command = command;
        let caller = support::cancel::Token::new();
        command_extension.dispatch_stream(
            &command,
            json!({"args": []}),
            Duration::from_secs(10),
            None,
            &caller,
        )
    });
    assert_eq!(owner.drain(), CloseReason::Eof);
    let hook_answer = fakes::within(
        "the hook dispatch",
        fakes::Deadline::after(WITHIN),
        move || hook_thread.join().expect("the hook thread"),
    );
    assert_eq!(
        hook_answer.map_err(|error| error.to_string()),
        Ok(Some(json!("c2"))),
        "the hook gets its own reply"
    );
    let mut tool_answers = [
        fakes::within(
            "tool dispatch a",
            fakes::Deadline::after(WITHIN),
            move || tool_a.join().expect("tool thread a"),
        ),
        fakes::within(
            "tool dispatch b",
            fakes::Deadline::after(WITHIN),
            move || tool_b.join().expect("tool thread b"),
        ),
    ];
    tool_answers.sort_by_key(|answer| format!("{answer:?}"));
    assert_eq!(
        tool_answers.map(|answer| answer.map_err(|error| error.to_string())),
        [Ok(Some(json!("tool-0"))), Ok(Some(json!("tool-1")))],
        "both tools complete, replies reversed"
    );
    let command_answer = fakes::within(
        "the command dispatch",
        fakes::Deadline::after(WITHIN),
        move || command_thread.join().expect("the command thread"),
    );
    assert_eq!(
        command_answer.map_err(|error| error.to_string()),
        Ok(Some(json!("c3")))
    );
    // The stream order: the command is read only after the hook's
    // reply, while both tools answer reversed in between. Arrival order
    // varies with the send race; the reply order does not.
    let recorded = read_json_file(&out_path, "the dispatch order");
    let mut log: Vec<Value> = recorded["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|request| json!(["got", request["payload"]["registration"]]))
        .collect();
    log.extend([
        json!(["replied", "tool-0"]),
        json!(["replied", "tool-1"]),
        json!(["replied", "c2"]),
    ]);
    let mut acks = Vec::new();
    for line in recorded["tail"].as_array().unwrap() {
        if line["kind"] == "extension_request" {
            log.push(json!(["got", line["payload"]["registration"]]));
            log.push(json!(["replied", line["payload"]["registration"]]));
        } else {
            acks.push(line.clone());
        }
    }
    let report = json!({"log": log, "acks": acks});
    for (index, ack) in report["acks"]
        .as_array()
        .expect("four acks")
        .iter()
        .enumerate()
    {
        assert_eq!(ack["kind"], json!("command_accepted"), "ack {index}");
    }
    let log = &report["log"];
    let mut got: Vec<Value> = Vec::new();
    let mut replied: Vec<Value> = Vec::new();
    for entry in log.as_array().expect("the log is an array") {
        match (entry[0].as_str(), entry[1].as_str()) {
            (Some("got"), Some(registration)) => got.push(json!(registration)),
            (Some("replied"), Some(result)) => replied.push(json!(result)),
            other => panic!("an unknown log entry: {other:?}"),
        }
    }
    got.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    // docs/extensions.md:209, stream items run one at a time and tool
    // calls run as they arrive; several tools may be outstanding.
    assert_eq!(
        got,
        vec![json!("c1"), json!("c1"), json!("c2"), json!("c3")]
    );
    assert_eq!(
        replied,
        vec![json!("tool-0"), json!("tool-1"), json!("c2"), json!("c3")]
    );
    let position = |kind: &str, what: &str| {
        log.as_array()
            .unwrap()
            .iter()
            .position(|entry| entry[0] == kind && entry[1] == what)
            .unwrap_or(usize::MAX)
    };
    // docs/extensions.md, "Process extensions": stream items are sent one
    // at a time, each after the reply to the last one.
    assert!(
        position("replied", "c2") < position("got", "c3"),
        "the command arrives only after the hook's reply: {log}"
    );
    assert!(
        position("got", "c2") < position("replied", "c2"),
        "the hook answers after it arrives: {log}"
    );
}

/// A timed-out hook fails with its timeout: the late reply is dropped as
/// `stale_request`, and the next hook stays healthy.
#[test]
fn a_timed_out_hook_drops_its_late_reply() {
    let script = &json!([
        {"read": {}},
        {"send": {"id": "c1", "command": "call", "args": {"call": "fiber.hook", "args": ["turn_end", wire_hook_spec()]}}},
        {"send": {"id": "c2", "command": "ready"}},
        {"repeat": {"count": 2, "actions": [{"read": {}}]}},
        {"read": {"as": "first"}},
        {"hold": {"release_on": "line", "release_line": {"test_release": true}}},
        {"send": {"id": "p-late", "command": "reply", "args": {"request_id": {"$ref": "first.payload.request_id"}, "result": "late"}}},
        {"read_until": {"as": "second", "path": "kind", "equals": "extension_request", "append_to": "answers"}},
        {"send": {"id": "p-fresh", "command": "reply", "args": {"request_id": {"$ref": "second.payload.request_id"}, "result": "fresh"}}},
        {"read_until": {"path": "kind", "equals": "command_accepted", "append_to": "answers"}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "answers"}}},
    ])
    .to_string();
    let out = fakes::TempDir::new("fiber-process-timeout");
    let out_path = out.path().join("answers.json");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap(), &wire_hook_spec().to_string()],
        serde_json::Map::new(),
        Some(FakeDrive::new()),
    );
    owner.pump();
    owner.pump();
    let hook = {
        let registry = support::lock(&owner.registry);
        registry.hook_runs()[&("turn_end".to_owned(), 0)].clone()
    };
    // The clock is frozen unless the test moves it: the advancer waits
    // for the dispatch's park at the deadline, then moves time onto it.
    // Landing on the deadline ends the wait, and the timeout fires.
    let parked_at = owner
        .clock
        .now()
        .checked_add(Duration::from_millis(500))
        .expect("the suite starts before its first timeout");
    let advancer = std::thread::spawn({
        let clock = Arc::clone(&owner.clock);
        let bound = fakes::Deadline::after(WITHIN);
        move || {
            clock.advance_when_parked(Some(parked_at), None, Duration::from_millis(500), &bound);
        }
    });
    let hook_extension = Arc::clone(&owner.extension);
    let finished = std::thread::spawn(move || {
        let caller = support::cancel::Token::new();
        let outcome = hook_extension
            .dispatch_stream(
                &hook,
                json!({"event": "turn_end"}),
                Duration::from_millis(500),
                None,
                &caller,
            )
            .map_err(|error| error.to_string());
        (outcome, hook)
    });
    let (timed_out, hook) = fakes::within(
        "the timed-out dispatch",
        fakes::Deadline::after(WITHIN),
        move || finished.join().expect("the dispatch thread"),
    );
    match advancer.join() {
        Ok(()) | Err(_) => {}
    }
    let err = timed_out.unwrap_err();
    assert!(err.contains("passed its 500 ms timeout"), "{err}");
    owner
        .extension
        .transport()
        .send(br#"{"test_release":true}"#.to_vec())
        .expect("release the peer after the timeout");
    let hook_extension = Arc::clone(&owner.extension);
    let healthy = std::thread::spawn(move || {
        let hook = hook;
        let caller = support::cancel::Token::new();
        hook_extension.dispatch_stream(
            &hook,
            json!({"event": "turn_end"}),
            Duration::from_secs(10),
            None,
            &caller,
        )
    });
    assert_eq!(owner.drain(), CloseReason::Eof);
    let healthy = fakes::within(
        "the healthy hook",
        fakes::Deadline::after(WITHIN),
        move || healthy.join().expect("the healthy hook thread"),
    );
    assert_eq!(
        healthy.map_err(|error| error.to_string()),
        Ok(Some(json!("fresh")))
    );
    let answers = read_json_file(&out_path, "the reply answers");
    let answers = answers
        .as_array()
        .expect("the peer records its received lines as an array");
    assert!(
        answers.iter().any(|answer| {
            answer["kind"] == json!("command_rejected")
                && answer["payload"]["code"] == json!("stale_request")
        }),
        "the late reply is rejected as stale: {answers:?}"
    );
    assert!(
        answers
            .iter()
            .any(|answer| answer["kind"] == json!("extension_request")),
        "the next hook is delivered: {answers:?}"
    );
    assert!(
        answers
            .iter()
            .any(|answer| answer["kind"] == json!("command_accepted")),
        "the fresh reply is acknowledged: {answers:?}"
    );
}

/// Cancelling the dispatch ends the pending request: the late reply is
/// dropped as `stale_request`, and the dispatch fails stopped.
#[test]
fn a_cancelled_dispatch_drops_its_late_reply() {
    let script = &json!([
        {"read": {}},
        {"send": {"id": "c1", "command": "call", "args": {"call": "fiber.hook", "args": ["turn_end", wire_hook_spec()]}}},
        {"send": {"id": "c2", "command": "ready"}},
        {"repeat": {"count": 2, "actions": [{"read": {}}]}},
        {"read": {"as": "request"}},
        {"hold": {"release_on": "line", "release_line": {"test_release": true}}},
        {"send": {"id": "p-late", "command": "reply", "args": {"request_id": {"$ref": "request.payload.request_id"}, "result": "late"}}},
        {"read": {}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": [{"$ref": "last"}]}},
    ])
    .to_string();
    let out = fakes::TempDir::new("fiber-process-cancel");
    let out_path = out.path().join("answers.json");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap(), &wire_hook_spec().to_string()],
        serde_json::Map::new(),
        Some(FakeDrive::new()),
    );
    owner.pump();
    owner.pump();
    let hook = {
        let registry = support::lock(&owner.registry);
        registry.hook_runs()[&("turn_end".to_owned(), 0)].clone()
    };
    let caller = support::cancel::Token::new();
    let canceller = caller.clone();
    let hook_extension = Arc::clone(&owner.extension);
    let cancelled = std::thread::spawn(move || {
        let hook = hook;
        let caller = canceller;
        hook_extension.dispatch_stream(
            &hook,
            json!({"event": "turn_end"}),
            Duration::from_secs(10),
            None,
            &caller,
        )
    });
    // The dispatch's park at its deadline proves the request is pending.
    let parked_at = owner
        .clock
        .now()
        .checked_add(Duration::from_secs(10))
        .expect("the suite starts before its first timeout");
    assert!(
        owner
            .clock
            .mark_parked(parked_at, fakes::Deadline::after(WITHIN))
            .is_some(),
        "the dispatch parks while its request is pending"
    );
    caller.cancel();
    let cancelled = fakes::within(
        "the cancelled dispatch",
        fakes::Deadline::after(WITHIN),
        move || cancelled.join().expect("the cancelled thread"),
    );
    assert_eq!(
        cancelled.map_err(|error| error.to_string()),
        Err("`fiber.test/proc` is stopped and takes no more calls.".to_owned())
    );
    owner
        .extension
        .transport()
        .send(br#"{"test_release":true}"#.to_vec())
        .expect("release the peer after cancellation");
    assert_eq!(owner.drain(), CloseReason::Eof);
    let answers = read_json_file(&out_path, "the late reply's answer");
    assert_eq!(answers[0]["kind"], json!("command_rejected"));
    assert_eq!(answers[0]["payload"]["code"], json!("stale_request"));
}

/// Garbage is ignored, unknown and unfit lines are rejected, a bad reply
/// refuses, and an ordinary driver command goes through the `Drive` seam.
#[test]
fn malformed_unknown_and_forwarded_lines() {
    let script = &json!([
        {"read": {}},
        {"send_raw": "not json"},
        {"send": {"id": "u1", "command": "frob"}},
        {"send": {"id": "c9", "command": "call", "args": {"call": 42}}},
        {"send": {"id": "b1", "command": "reply", "args": {"request_id": "r_x", "confirmed": true}}},
        {"send": {"id": "v1", "command": "tools", "args": {}}},
        {"repeat": {"count": 4, "actions": [{"read": {"append_to": "answers"}}]}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "answers"}}},
    ])
    .to_string();
    let out = fakes::TempDir::new("fiber-process-wire");
    let out_path = out.path().join("answers.json");
    let drive = FakeDrive::new();
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap()],
        serde_json::Map::new(),
        Some(Arc::clone(&drive) as Arc<dyn contract::extension::Drive>),
    );
    for _ in 0..5 {
        owner.pump();
    }
    // No registration stands: the pipe holds only supervised shapes and
    // one forwarded command.
    {
        let registry = support::lock(&owner.registry);
        assert!(registry.tools().is_empty());
    }
    assert_eq!(owner.drain(), CloseReason::Eof);
    let answers = read_json_file(&out_path, "the wire answers");
    // docs/invocation.md:368, every command gets exactly one acknowledgement.
    assert_eq!(answers.as_array().unwrap().len(), 4);
    assert_eq!(answers[0]["kind"], json!("command_rejected"));
    assert_eq!(answers[0]["payload"]["code"], json!("unknown_command"));
    assert_eq!(
        answers[0]["payload"]["message"],
        json!("`frob` is not built in this Fiber yet.")
    );
    assert_eq!(answers[1]["kind"], json!("command_rejected"));
    assert_eq!(answers[1]["payload"]["code"], json!("invalid_arguments"));
    assert_eq!(
        answers[1]["payload"]["message"],
        json!("The arguments do not fit this command.")
    );
    assert_eq!(answers[2]["kind"], json!("command_rejected"));
    assert_eq!(answers[2]["payload"]["code"], json!("invalid_arguments"));
    assert_eq!(
        answers[2]["payload"]["message"],
        json!("a `reply` answers with `result` or `error`.")
    );
    assert_eq!(answers[3]["kind"], json!("command_accepted"));
    assert_eq!(
        support::lock(&drive.calls).as_slice(),
        &[("tools".to_owned(), serde_json::Map::new())]
    );
}

/// With no driver the session is closing, and `host.drive` raises it.
#[test]
fn drive_without_a_driver_closes() {
    let script = &json!([
        {"read": {}},
        {"send": {"id": "v1", "command": "tools", "args": {}}},
        {"read": {}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "last"}}},
    ])
    .to_string();
    let out = fakes::TempDir::new("fiber-process-nodrive");
    let out_path = out.path().join("answer.jsonl");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    owner.pump();
    assert_eq!(owner.drain(), CloseReason::Eof);
    let line = std::fs::read_to_string(&out_path).expect("the peer records the answer");
    let answer: Value = serde_json::from_str(&line).expect("one JSON line");
    assert_eq!(answer["kind"], json!("command_rejected"));
    assert_eq!(answer["payload"]["code"], json!("closing"));
    assert_eq!(
        answer["payload"]["message"],
        json!("host.drive: the session is closing")
    );
}

/// Every host and state family reaches the injected services with timer
/// attribution: the peer sends one valid call per family after `ready`,
/// and the fake records each operation's variant. `host.ask` declines
/// with nobody to answer, and `host.oauth.refresh` refuses without a
/// holder; neither reaches the services.
#[test]
fn host_calls_reach_services_with_timer_attribution() {
    let b64 = |bytes: &[u8]| Value::String(crate::host::admission::encode_bytes(bytes));
    let calls = vec![
        ("h1", "host.http", json!([{"url": "https://x.test"}])),
        ("e1", "host.exec", json!(["prog"])),
        ("t1", "host.tool", json!(["t", {"a": 1}])),
        (
            "m1",
            "host.model",
            json!([{"model": "a/b", "messages": [{"role": "user", "content": "hi"}], "max_tokens": 8}]),
        ),
        (
            "d1",
            "host.delegate",
            json!([{"description": "d", "prompt": "p"}, {"timeout": 9, "run": true}]),
        ),
        ("a1", "host.ask", json!(["confirm", {"prompt": "go?"}])),
        ("c1", "host.oauth.callback", json!([8080, "/cb"])),
        ("o1", "host.oauth.open", json!(["https://x.test"])),
        ("s1", "host.oauth.show", json!(["https://x.test", "code-1"])),
        ("p1", "host.oauth.pkce", json!([])),
        ("w1", "host.oauth.poll", json!(["https://x.test"])),
        ("r1", "host.oauth.refresh", json!([true])),
        ("f1", "host.fs.read", json!(["a.txt"])),
        (
            "f2",
            "host.fs.write",
            json!(["a.txt", b64(b"hi"), {"lock": true}]),
        ),
        ("f3", "host.fs.list", json!(["d"])),
        ("f4", "host.fs.stat", json!(["a.txt"])),
        ("f5", "host.fs.mkdir", json!(["d"])),
        ("f6", "host.fs.remove", json!(["d"])),
        ("f7", "host.fs.rename", json!(["a", "b"])),
        ("g1", "host.config.get", json!(["a.b"])),
        ("g2", "host.config.set", json!(["a.b", 7, "machine"])),
        ("n1", "host.secret", json!(["t"])),
        ("q1", "host.data_dir", json!(["machine"])),
        ("y1", "host.sha256", json!([b64(b"x")])),
        ("y2", "host.hmac_sha256", json!([b64(b"k"), b64(b"d")])),
        ("u1", "host.status", json!(["line"])),
        ("u2", "host.widget", json!(["w", ["l"]])),
        ("u3", "host.emit", json!([{"k": 1}])),
        ("l1", "host.log", json!(["line"])),
        ("l2", "host.log", json!([7])),
        ("k1", "state.get", json!(["k"])),
        ("k2", "state.keys", json!([])),
        ("k3", "state.set", json!(["k", 1])),
        ("k4", "state.unset", json!(["k"])),
    ];
    let mut actions = vec![
        json!({"read": {}}),
        json!({"send": {"id": "ready", "command": "ready"}}),
    ];
    actions.extend(calls.iter().map(|(id, call, args)| {
        json!({"send": {"id": id, "command": "call", "args": {"call": call, "args": args}}})
    }));
    actions.extend([
        json!({"repeat": {"count": calls.len() + 1, "actions": [{"read": {"append_to": "answers"}}]}}),
        json!({"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "answers"}}}),
    ]);
    let script = &json!(actions).to_string();
    let out = fakes::TempDir::new("fiber-process-host-calls");
    let out_path = out.path().join("answers.json");
    let owner = Harness::start(
        script,
        &[
            out_path.to_str().unwrap(),
            &serde_json::to_string(&calls).unwrap(),
        ],
        serde_json::Map::new(),
        None,
    );
    for _ in 0..(calls.len() + 1) {
        owner.pump();
    }
    assert_eq!(owner.drain(), CloseReason::Eof);
    let recorded = read_json_file(&out_path, "the host call answers");
    let mut answers: serde_json::Map<String, Value> = recorded
        .as_array()
        .unwrap()
        .iter()
        .map(|answer| {
            (
                answer["payload"]["command_id"].as_str().unwrap().to_owned(),
                answer.clone(),
            )
        })
        .collect();
    assert!(answers.remove("ready").is_some());
    let answers = Value::Object(answers);
    // docs/invocation.md:368, every command gets exactly one acknowledgement.
    assert_eq!(answers.as_object().unwrap().len(), calls.len());
    for (id, _, _) in &calls {
        let answer = &answers[id];
        if *id == "r1" {
            assert_eq!(answer["kind"], json!("command_rejected"), "refresh refuses");
            assert_eq!(answer["payload"]["code"], json!("invalid_arguments"));
            assert_eq!(
                answer["payload"]["message"],
                json!("host.oauth.refresh: a timer has no provider credential to refresh")
            );
            continue;
        }
        assert_eq!(answer["kind"], json!("command_accepted"), "{id} stands");
    }
    assert_eq!(
        answers["a1"]["payload"]["result"]["value"],
        json!({"declined": true}),
        "an ask with no request declines"
    );
    let mut recorded = owner.services.calls();
    recorded.sort();
    let mut expected = vec![
        ("config.get", "g1"),
        ("config.set", "g2"),
        ("data_dir", "q1"),
        ("delegate", "d1"),
        ("emit", "u3"),
        ("exec", "e1"),
        ("fs.list", "f3"),
        ("fs.mkdir", "f5"),
        ("fs.read", "f1"),
        ("fs.remove", "f6"),
        ("fs.rename", "f7"),
        ("fs.stat", "f4"),
        ("fs.write", "f2"),
        ("hmac_sha256", "y2"),
        ("http", "h1"),
        ("log", "l1"),
        ("log", "l2"),
        ("model", "m1"),
        ("oauth.callback", "c1"),
        ("oauth.open", "o1"),
        ("oauth.pkce", "p1"),
        ("oauth.poll", "w1"),
        ("oauth.show", "s1"),
        ("secret", "n1"),
        ("sha256", "y1"),
        ("state.get", "k1"),
        ("state.keys", "k2"),
        ("state.set", "k3"),
        ("state.unset", "k4"),
        ("status", "u1"),
        ("tool", "t1"),
        ("widget", "u2"),
    ];
    expected.sort();
    // docs/extensions.md:31, 354–384: process extensions receive the
    // same documented host calls as Lua extensions, with the same meaning.
    assert_eq!(recorded.len(), expected.len());
    for ((variant, ran_by), (want_variant, _)) in recorded.iter().zip(expected.iter()) {
        assert_eq!(variant, want_variant);
        // Every call ran from a timer, which attributes nothing.
        assert_eq!(ran_by, &None);
    }
}

/// A failing service rejects with its own code and message, proving the
/// service's answer reaches the wire unchanged.
#[test]
fn a_failing_service_rejects_with_its_code() {
    let script = &json!([
        {"read": {}},
        {"send": {"id": "ready", "command": "ready"}},
        {"send": {"id": "c1", "command": "call", "args": {"call": "host.log", "args": ["hi"]}}},
        {"read": {}},
        {"read": {}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "last"}}},
    ])
    .to_string();
    let out = fakes::TempDir::new("fiber-process-failing");
    let out_path = out.path().join("answer.json");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    owner.services.answer_with(Err((
        contract::ErrorCode::IoFailed,
        "the service fell over".to_owned(),
    )));
    owner.pump();
    owner.pump();
    assert_eq!(owner.drain(), CloseReason::Eof);
    let answer = read_json_file(&out_path, "the service answer");
    assert_eq!(answer["kind"], json!("command_rejected"));
    assert_eq!(answer["payload"]["code"], json!("io_failed"));
    assert_eq!(answer["payload"]["message"], json!("the service fell over"));
}

/// A refused host call answers rejected and runs nothing: the services
/// record no operation.
#[test]
fn a_refused_host_call_runs_nothing() {
    let script = &json!([
        {"read": {}},
        {"send": {"id": "ready", "command": "ready"}},
        {"send": {"id": "c1", "command": "call", "args": {"call": "host.exec", "args": [7]}}},
        {"read": {}},
        {"read": {}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "last"}}},
    ])
    .to_string();
    let out = fakes::TempDir::new("fiber-process-refused");
    let out_path = out.path().join("answer.json");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    owner.pump();
    owner.pump();
    assert_eq!(owner.drain(), CloseReason::Eof);
    let answer = read_json_file(&out_path, "the refused answer");
    assert_eq!(answer["kind"], json!("command_rejected"));
    assert_eq!(answer["payload"]["code"], json!("invalid_arguments"));
    assert_eq!(
        answer["payload"]["message"],
        json!("host.exec: `program` must be a string")
    );
    assert!(
        owner.services.calls().is_empty(),
        "a refused call runs no operation"
    );
}

/// The entry script's refusals land before decoding: `host.exec`
/// refuses with the entry sentence, and the services record nothing.
#[test]
fn entry_refusals_land_before_decode() {
    let script = &json!([
        {"read": {}},
        {"send": {"id": "c1", "command": "call", "args": {"call": "host.exec", "args": ["prog"]}}},
        {"read": {}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "last"}}},
    ])
    .to_string();
    let out = fakes::TempDir::new("fiber-process-entry");
    let out_path = out.path().join("answer.json");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    owner.pump();
    assert_eq!(owner.drain(), CloseReason::Eof);
    let answer = read_json_file(&out_path, "the entry answer");
    assert_eq!(answer["kind"], json!("command_rejected"));
    assert_eq!(answer["payload"]["code"], json!("invalid_arguments"));
    assert_eq!(
        answer["payload"]["message"],
        json!("host.exec: not available while init.lua runs")
    );
    assert!(
        owner.services.calls().is_empty(),
        "an entry call runs no operation"
    );
}

/// A host call inside a tool request runs under its attribution: the
/// services record the operation with `tool t`, and the reply settles
/// the dispatch with its value.
#[test]
fn a_tool_request_anchors_its_inner_call() {
    let script = &json!([
        {"read": {}},
        {"send": {"id": "c1", "command": "call", "args": {"call": "fiber.tool", "args": ["t", wire_tool_spec()]}}},
        {"send": {"id": "ready", "command": "ready"}},
        {"repeat": {"count": 2, "actions": [{"read": {}}]}},
        {"read": {"as": "request"}},
        {"send": {"id": "c2", "command": "call", "args": {"call": "host.tool", "args": ["inner", {}], "request_id": {"$ref": "request.payload.request_id"}}}},
        {"read": {"as": "answer"}},
        {"send": {"id": "p-r", "command": "reply", "args": {"request_id": {"$ref": "request.payload.request_id"}, "result": 7}}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "answer"}}},
    ])
    .to_string();
    let out = fakes::TempDir::new("fiber-process-attribution");
    let out_path = out.path().join("answer.json");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap(), &wire_tool_spec().to_string()],
        serde_json::Map::new(),
        None,
    );
    owner.pump();
    owner.pump();
    let func = {
        let registry = support::lock(&owner.registry);
        registry.tools()["t"].run.clone()
    };
    let extension = Arc::clone(&owner.extension);
    let cancel = support::cancel::Token::new();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = extension.dispatch_tool(&func, json!({}), Duration::from_secs(60), &cancel);
        match done_tx.send(outcome) {
            Ok(()) | Err(_) => {}
        }
    });
    owner.pump();
    owner.pump();
    let outcome = fakes::Deadline::after(WITHIN)
        .recv(&done_rx)
        .expect("the dispatch answers within the bound");
    assert_eq!(
        outcome.expect("the dispatch succeeds"),
        Some(json!(7)),
        "the peer's reply settles the dispatch"
    );
    assert_eq!(owner.drain(), CloseReason::Eof);
    assert_eq!(
        owner.services.calls(),
        vec![("tool".to_owned(), Some("tool t".to_owned()))],
        "the inner call runs under the tool's attribution"
    );
    let answer = read_json_file(&out_path, "the inner call answer");
    assert_eq!(answer["kind"], json!("command_accepted"));
}

// OAuth refresh and state retirement behind the package seam: the native
// continuation holds the lock or slot around its nested function request,
// and hook writes commit on success and discard on failure.

/// A browser that never attaches: production services need one, and no
/// test here logs in.
struct ClosedBrowser;

impl crate::oauth::Browser for ClosedBrowser {
    fn open(&self, _url: &str) {}
    fn show(&self, _url: &str, _code: &str) {}
    fn attended(&self) -> bool {
        false
    }
}

/// Writes `value` as the stored pair for `credential` under `label` in
/// `home`, holding no lock after.
fn write_pair(home: &std::path::Path, credential: &str, label: &str, value: &Value) {
    let file = config::CredentialFile::new(home, credential, label).unwrap();
    let held = file.try_lock().unwrap().expect("a free test pair locks");
    held.write(value).unwrap();
}

/// Reads the stored pair back, unlocked.
fn read_pair(home: &std::path::Path, credential: &str, label: &str) -> Option<Value> {
    config::CredentialFile::new(home, credential, label)
        .unwrap()
        .read_unlocked()
        .unwrap()
}

/// The test pair the refresh tests refresh.
fn test_pair() -> crate::lua_provider::CredentialPair {
    crate::lua_provider::CredentialPair {
        credential: "testprov".to_owned(),
        label: "test".to_owned(),
    }
}

/// Mints a provider `credential()` request with `scope` on the owner's
/// table and sends its dispatch line to the child, as the session will:
/// the child answers it by calling `host.oauth.refresh` with its id.
fn dispatch_provider(
    owner: &Harness,
    scope: Option<crate::lua::CredentialFor>,
    deadline: Option<std::time::Instant>,
) -> super::process::requests::Minted {
    let shared = owner.extension.shared();
    let minted = shared.table.mint(
        super::process::requests::RequestKind::Function,
        "fiber.test/proc",
        "provider testprov.credential".to_owned(),
        deadline,
        None,
        scope,
    );
    let payload = serde_json::to_value(&contract::events::ExtensionRequest {
        request_id: minted.id.clone(),
        registration: contract::CommandId("c_prov".into()),
        key: "credential".into(),
        args: vec![json!({"label": "test", "credential": "testprov"})],
    })
    .unwrap();
    let line = super::process::envelope_line(
        shared,
        "extension_request",
        payload.as_object().cloned().unwrap_or_default(),
    );
    owner.extension.transport().send(line).unwrap();
    minted
}

/// The far-future expiry the tests mint: past any test clock.
const FRESH_EXPIRES: i64 = 2_000_000_000;

/// A stored pair long expired on any test clock.
fn expired_pair() -> Value {
    json!({"token": "tok-old", "expires_at": 1})
}

/// Starts a refresh plan with the hello, provider request and refresh call.
fn refresh_plan(actions: Value) -> Value {
    let mut plan = vec![
        json!({"read": {}}),
        json!({"read": {"as": "parent"}}),
        json!({"send": {"id": "c1", "command": "call", "args": {
            "call": "host.oauth.refresh", "args": [true],
            "request_id": {"$ref": "parent.payload.request_id"},
        }}}),
    ];
    plan.extend(actions.as_array().unwrap().iter().cloned());
    json!(plan)
}

/// A refresh the child answers, seeing the protocol positions.
#[test]
fn refresh_runs_its_function_nested_and_stores_what_it_returns() {
    let script = &refresh_plan(json!([
        {"read": {"as": "nested", "expect": {"kind": "extension_request", "payload": {
            "registration": "c1", "key": "1", "args": [expired_pair()],
        }}}},
        {"send": {"id": "p1", "command": "reply", "args": {"request_id": {"$ref": "nested.payload.request_id"}, "result": {"token": "tok-fresh", "expires_at": FRESH_EXPIRES}}}},
        {"read_until": {"path": "payload.command_id", "equals": "c1"}},
        {"if": {"path": "last.kind", "equals": "command_accepted", "else": [{"exit": true}]}},
        {"if": {"path": "last.payload.result.value", "equals": {"token": "tok-fresh", "expires_at": FRESH_EXPIRES}, "else": [{"exit": true}]}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"nested": {"$ref": "nested.payload"}, "answer": {"$ref": "last.payload"}}}},
    ]))
    .to_string();
    let out = fakes::TempDir::new("fiber-process-refresh-nested");
    let out_path = out.path().join("answer.json");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    write_pair(owner.workspace(), "testprov", "test", &expired_pair());
    let deadline = Some(owner.clock.now() + Duration::from_secs(60));
    dispatch_provider(
        &owner,
        Some(crate::lua::CredentialFor::Stored(test_pair())),
        deadline,
    );
    owner.pump();
    owner.pump();
    let seen = read_json_file(&out_path, "the refresh answer");
    assert_eq!(seen["nested"]["registration"], json!("c1"));
    assert_eq!(seen["nested"]["key"], json!("1"));
    // The stored write landed, and the lock is released: a test lock
    // takes it at once.
    assert_eq!(
        read_pair(owner.workspace(), "testprov", "test"),
        Some(json!({"token": "tok-fresh", "expires_at": FRESH_EXPIRES}))
    );
    let file = config::CredentialFile::new(owner.workspace(), "testprov", "test").unwrap();
    assert!(file.try_lock().unwrap().is_some(), "the lock leaked");
    assert_eq!(owner.drain(), CloseReason::Eof);
}

/// A fresh stored pair answers at once, with no nested call: the one
/// line the child reads is the acceptance, not an `extension_request`.
#[test]
fn refresh_returns_a_fresh_pair_without_a_nested_call() {
    let script = &refresh_plan(json!([
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "last.payload.result.value"}}},
    ]))
    .to_string();
    let out = fakes::TempDir::new("fiber-process-refresh-fresh");
    let out_path = out.path().join("answer.json");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    let stored = json!({"token": "tok-stored", "expires_at": FRESH_EXPIRES});
    write_pair(owner.workspace(), "testprov", "test", &stored);
    let deadline = Some(owner.clock.now() + Duration::from_secs(60));
    dispatch_provider(
        &owner,
        Some(crate::lua::CredentialFor::Stored(test_pair())),
        deadline,
    );
    owner.pump();
    let answer = read_json_file(&out_path, "the refresh answer");
    assert_eq!(answer, stored);
    assert_eq!(
        read_pair(owner.workspace(), "testprov", "test"),
        Some(stored)
    );
    assert_eq!(owner.drain(), CloseReason::Eof);
}

/// Two refreshes serialize on the lock: the second sees the first's
/// write and sends no nested call of its own.
#[test]
fn concurrent_refreshes_serialize_on_the_credential_lock() {
    let script = &refresh_plan(json!([
        {"read": {"as": "nested", "expect": {"payload": {"registration": "c1"}}}},
        {"send": {"id": "p1", "command": "reply", "args": {"request_id": {"$ref": "nested.payload.request_id"}, "result": {"token": "tok-fresh", "expires_at": FRESH_EXPIRES}}}},
        {"read_until": {"as": "first", "path": "payload.command_id", "equals": "c1"}},
        {"if": {"path": "first.kind", "equals": "command_accepted", "else": [{"exit": true}]}},
        {"send": {"id": "c2", "command": "call", "args": {"call": "host.oauth.refresh", "args": [true], "request_id": {"$ref": "parent.payload.request_id"}}}},
        {"read_until": {"path": "payload.command_id", "equals": "c2"}},
        {"if": {"path": "last.kind", "equals": "command_accepted", "else": [{"exit": true}]}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"first": {"$ref": "first.payload.result.value"}, "second": {"$ref": "last.payload.result.value"}}}},
    ]))
    .to_string();
    let out = fakes::TempDir::new("fiber-process-refresh-serial");
    let out_path = out.path().join("answer.json");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    write_pair(owner.workspace(), "testprov", "test", &expired_pair());
    let deadline = Some(owner.clock.now() + Duration::from_secs(60));
    dispatch_provider(
        &owner,
        Some(crate::lua::CredentialFor::Stored(test_pair())),
        deadline,
    );
    owner.pump();
    owner.pump();
    owner.pump();
    let seen = read_json_file(&out_path, "the refresh answers");
    let fresh = json!({"token": "tok-fresh", "expires_at": FRESH_EXPIRES});
    assert_eq!(seen["first"], fresh);
    assert_eq!(seen["second"], fresh);
    assert_eq!(
        read_pair(owner.workspace(), "testprov", "test"),
        Some(fresh)
    );
    assert_eq!(owner.drain(), CloseReason::Eof);
}

/// What the function returns is validated before anything is stored: an
/// unusable shape fails the refresh and leaves the stored pair alone.
#[test]
fn refresh_refuses_an_unusable_function_result() {
    let script = &refresh_plan(json!([
        {"read": {"as": "nested", "expect": {"payload": {"registration": "c1"}}}},
        {"send": {"id": "p1", "command": "reply", "args": {"request_id": {"$ref": "nested.payload.request_id"}, "result": {"nope": true}}}},
        {"read_until": {"as": "answer", "path": "payload.command_id", "equals": "c1"}},
        {"if": {"path": "answer.kind", "equals": "command_rejected", "else": [{"exit": true}]}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "answer.payload"}}}
    ]))
    .to_string();
    let out = fakes::TempDir::new("fiber-process-refresh-unusable");
    let out_path = out.path().join("answer.json");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    let stored = expired_pair();
    write_pair(owner.workspace(), "testprov", "test", &stored);
    let deadline = Some(owner.clock.now() + Duration::from_secs(60));
    dispatch_provider(
        &owner,
        Some(crate::lua::CredentialFor::Stored(test_pair())),
        deadline,
    );
    owner.pump();
    owner.pump();
    let seen = read_json_file(&out_path, "the refresh rejection");
    assert_eq!(seen["code"], json!("invalid_arguments"));
    assert_eq!(
        seen["message"],
        json!(
            "host.oauth.refresh: the function must return a table with a `token` string and an `expires_at` whole number of seconds"
        )
    );
    assert_eq!(
        read_pair(owner.workspace(), "testprov", "test"),
        Some(stored)
    );
    let file = config::CredentialFile::new(owner.workspace(), "testprov", "test").unwrap();
    assert!(file.try_lock().unwrap().is_some(), "the lock leaked");
    assert_eq!(owner.drain(), CloseReason::Eof);
}

/// An expired function result is refused before it is stored, by a
/// refresh as by a login.
#[test]
fn refresh_refuses_an_expired_function_result() {
    let script = &refresh_plan(json!([
        {"read": {"as": "nested"}},
        {"send": {"id": "p1", "command": "reply", "args": {"request_id": {"$ref": "nested.payload.request_id"}, "result": {"token": "tok-dead", "expires_at": 1}}}},
        {"read_until": {"path": "payload.command_id", "equals": "c1"}},
        {"if": {"path": "last.kind", "equals": "command_rejected", "else": [{"exit": true}]}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "last.payload"}}},
    ]))
    .to_string();
    let out = fakes::TempDir::new("fiber-process-refresh-expired");
    let out_path = out.path().join("answer.json");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    let stored = expired_pair();
    write_pair(owner.workspace(), "testprov", "test", &stored);
    let deadline = Some(owner.clock.now() + Duration::from_secs(60));
    dispatch_provider(
        &owner,
        Some(crate::lua::CredentialFor::Stored(test_pair())),
        deadline,
    );
    owner.pump();
    owner.pump();
    let seen = read_json_file(&out_path, "the refresh rejection");
    assert_eq!(seen["code"], json!("authentication_failed"));
    assert_eq!(
        seen["message"],
        json!("host.oauth.refresh: the refreshed token has already expired")
    );
    assert_eq!(
        read_pair(owner.workspace(), "testprov", "test"),
        Some(stored)
    );
    assert_eq!(owner.drain(), CloseReason::Eof);
}

/// A function that raises fails the refresh with its own failure, as a
/// raised table passes through the Lua attempt unchanged.
#[test]
fn refresh_passes_the_function_failure_through() {
    let script = &refresh_plan(json!([
        {"read": {"as": "nested"}},
        {"send": {"id": "p1", "command": "reply", "args": {"request_id": {"$ref": "nested.payload.request_id"}, "error": {"code": "extension_failed", "message": "the vendor said no"}}}},
        {"read_until": {"path": "payload.command_id", "equals": "c1"}},
        {"if": {"path": "last.kind", "equals": "command_rejected", "else": [{"exit": true}]}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "last.payload"}}},
    ]))
    .to_string();
    let out = fakes::TempDir::new("fiber-process-refresh-raises");
    let out_path = out.path().join("answer.json");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    let stored = expired_pair();
    write_pair(owner.workspace(), "testprov", "test", &stored);
    let deadline = Some(owner.clock.now() + Duration::from_secs(60));
    dispatch_provider(
        &owner,
        Some(crate::lua::CredentialFor::Stored(test_pair())),
        deadline,
    );
    owner.pump();
    owner.pump();
    let seen = read_json_file(&out_path, "the refresh rejection");
    assert_eq!(seen["code"], json!("extension_failed"));
    assert_eq!(seen["message"], json!("the vendor said no"));
    assert_eq!(
        read_pair(owner.workspace(), "testprov", "test"),
        Some(stored)
    );
    assert_eq!(owner.drain(), CloseReason::Eof);
}

/// A function that never answers fails the refresh unreached: the
/// deadline ends the wait, and the stored pair stands.
#[test]
fn refresh_times_out_when_its_function_never_answers() {
    let script = &refresh_plan(json!([
        {"read": {"expect": {"kind": "extension_request"}}},
        {"write_file": {"path": {"$ref": "args.1"}, "value": {"nested": true}}},
        {"read_until": {"path": "payload.command_id", "equals": "c1"}},
        {"if": {"path": "last.kind", "equals": "command_rejected", "else": [{"exit": true}]}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "last.payload"}}},
    ]))
    .to_string();
    let out = fakes::TempDir::new("fiber-process-refresh-timeout");
    let out_path = out.path().join("answer.json");
    let nested_path = out.path().join("nested.json");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap(), nested_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    let stored = expired_pair();
    write_pair(owner.workspace(), "testprov", "test", &stored);
    let deadline = Some(owner.clock.now() + Duration::from_secs(60));
    dispatch_provider(
        &owner,
        Some(crate::lua::CredentialFor::Stored(test_pair())),
        deadline,
    );
    owner.pump();
    // The nested line reached the child, so the refresh waits on its
    // function: advancing ends that wait, not the lock wait.
    read_json_file(&nested_path, "the nested request");
    #[allow(
        clippy::disallowed_methods,
        reason = "the test wakes past the refresh wait's deadline (ruling 6261259790)"
    )]
    owner.clock.advance(Duration::from_secs(61));
    let seen = read_json_file(&out_path, "the refresh rejection");
    assert_eq!(seen["code"], json!("credential_failed"));
    assert_eq!(
        seen["message"],
        json!("host.oauth.refresh: the refresh function timed out")
    );
    assert_eq!(
        read_pair(owner.workspace(), "testprov", "test"),
        Some(stored)
    );
    assert_eq!(owner.drain(), CloseReason::Eof);
}

/// Cancelling the refresh ends its nested wait: the function never
/// answered, so the refresh fails unreached.
#[test]
fn refresh_cancel_ends_its_nested_wait() {
    let script = &refresh_plan(json!([
        {"read": {"expect": {"kind": "extension_request"}}},
        {"write_file": {"path": {"$ref": "args.1"}, "value": {"nested": true}}},
        {"read": {"expect": {"kind": "command_rejected"}}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "last.payload"}}},
    ]))
    .to_string();
    let out = fakes::TempDir::new("fiber-process-refresh-cancel");
    let out_path = out.path().join("answer.json");
    let nested_path = out.path().join("nested");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap(), nested_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    let stored = expired_pair();
    write_pair(owner.workspace(), "testprov", "test", &stored);
    let deadline = Some(owner.clock.now() + Duration::from_secs(60));
    let parent = dispatch_provider(
        &owner,
        Some(crate::lua::CredentialFor::Stored(test_pair())),
        deadline,
    );
    owner.pump();
    // The nested line reached the child, so the refresh linked its
    // cancel to the nested request: ending the parent ends both.
    read_json_file(&nested_path, "the nested request");
    parent.cancel.cancel();
    let seen = read_json_file(&out_path, "the refresh rejection");
    assert_eq!(seen["code"], json!("credential_failed"));
    assert_eq!(
        seen["message"],
        json!("host.oauth.refresh: the refresh ended before its function answered")
    );
    assert_eq!(
        read_pair(owner.workspace(), "testprov", "test"),
        Some(stored)
    );
    assert_eq!(owner.drain(), CloseReason::Eof);
}

/// A login holds no file: its function sees nil, and what it returns is
/// kept in the slot and answered.
#[test]
fn refresh_in_a_login_fills_the_slot_without_a_file() {
    let script = &refresh_plan(json!([
        {"read": {"as": "nested", "expect": {"payload": {"args": [null]}}}},
        {"send": {"id": "p1", "command": "reply", "args": {"request_id": {"$ref": "nested.payload.request_id"}, "result": {"token": "tok-login", "expires_at": FRESH_EXPIRES}}}},
        {"read_until": {"path": "payload.command_id", "equals": "c1"}},
        {"if": {"path": "last.kind", "equals": "command_accepted", "else": [{"exit": true}]}},
        {"if": {"path": "last.payload.result.value", "equals": {"token": "tok-login", "expires_at": FRESH_EXPIRES}, "else": [{"exit": true}]}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "last.payload.result.value"}}},
    ]))
    .to_string();
    let out = fakes::TempDir::new("fiber-process-refresh-login");
    let out_path = out.path().join("answer.json");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    let slot: crate::oauth::held::LoginSlot = Arc::new(Mutex::new(None));
    let deadline = Some(owner.clock.now() + Duration::from_secs(60));
    dispatch_provider(
        &owner,
        Some(crate::lua::CredentialFor::Login(Arc::clone(&slot))),
        deadline,
    );
    owner.pump();
    owner.pump();
    let seen = read_json_file(&out_path, "the refresh answer");
    let fresh = json!({"token": "tok-login", "expires_at": FRESH_EXPIRES});
    assert_eq!(seen, fresh);
    assert_eq!(*support::lock(&slot), Some(fresh));
    // No file was touched: the home holds no credential.
    assert!(!owner.workspace().join("credentials").exists());
    assert_eq!(owner.drain(), CloseReason::Eof);
}

/// A login that returns an unusable shape fails, and the slot stays
/// empty: nothing is stored from a failed login.
#[test]
fn refresh_in_a_login_refuses_an_unusable_function_result() {
    let script = &refresh_plan(json!([
        {"read": {"as": "nested", "expect": {"payload": {"registration": "c1", "args": [null]}}}},
        {"send": {"id": "p1", "command": "reply", "args": {"request_id": {"$ref": "nested.payload.request_id"}, "result": {"nope": true}}}},
        {"read_until": {"as": "answer", "path": "payload.command_id", "equals": "c1"}},
        {"if": {"path": "answer.kind", "equals": "command_rejected", "else": [{"exit": true}]}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "answer.payload"}}}
    ]))
    .to_string();
    let out = fakes::TempDir::new("fiber-process-refresh-login-bad");
    let out_path = out.path().join("answer.json");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    let slot: crate::oauth::held::LoginSlot = Arc::new(Mutex::new(None));
    let deadline = Some(owner.clock.now() + Duration::from_secs(60));
    dispatch_provider(
        &owner,
        Some(crate::lua::CredentialFor::Login(Arc::clone(&slot))),
        deadline,
    );
    owner.pump();
    owner.pump();
    let seen = read_json_file(&out_path, "the refresh rejection");
    assert_eq!(seen["code"], json!("invalid_arguments"));
    assert_eq!(*support::lock(&slot), None);
    assert_eq!(owner.drain(), CloseReason::Eof);
}

/// Without a credential scope no callback refreshes: the refusal names
/// the callback, as the Lua lock arm does.
#[test]
fn refresh_without_a_scope_names_the_callback() {
    let script = json!([
        {"read": {}},
        {"send": {"id": "e1", "command": "call", "args": {"call": "host.oauth.refresh", "args": [true]}}},
        {"send": {"id": "ready", "command": "ready"}},
        {"read_until": {"as": "e1", "path": "payload.command_id", "equals": "e1"}},
        {"read": {"as": "ready_ack", "expect": {"kind": "command_accepted"}}},
        {"read": {"as": "go", "expect": {"kind": "extension_request"}}},
        {"send": {"id": "r0", "command": "call", "args": {"call": "host.oauth.refresh", "args": [true], "request_id": {"$ref": "go.payload.rids.0"}}}},
        {"read_until": {"as": "r0", "path": "payload.command_id", "equals": "r0"}},
        {"send": {"id": "r1", "command": "call", "args": {"call": "host.oauth.refresh", "args": [true], "request_id": {"$ref": "go.payload.rids.1"}}}},
        {"read_until": {"as": "r1", "path": "payload.command_id", "equals": "r1"}},
        {"send": {"id": "r2", "command": "call", "args": {"call": "host.oauth.refresh", "args": [true], "request_id": {"$ref": "go.payload.rids.2"}}}},
        {"read_until": {"as": "r2", "path": "payload.command_id", "equals": "r2"}},
        {"send": {"id": "r3", "command": "call", "args": {"call": "host.oauth.refresh", "args": [true], "request_id": {"$ref": "go.payload.rids.3"}}}},
        {"read_until": {"as": "r3", "path": "payload.command_id", "equals": "r3"}},
        {"send": {"id": "r4", "command": "call", "args": {"call": "host.oauth.refresh", "args": [true], "request_id": {"$ref": "go.payload.rids.4"}}}},
        {"read_until": {"as": "r4", "path": "payload.command_id", "equals": "r4"}},
        {"send": {"id": "r5", "command": "call", "args": {"call": "host.oauth.refresh", "args": [true], "request_id": {"$ref": "go.payload.rids.5"}}}},
        {"read_until": {"as": "r5", "path": "payload.command_id", "equals": "r5"}},
        {"send": {"id": "r6", "command": "call", "args": {"call": "host.oauth.refresh", "args": [true], "request_id": {"$ref": "go.payload.rids.6"}}}},
        {"read_until": {"as": "r6", "path": "payload.command_id", "equals": "r6"}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {
            "e1": {"$ref": "e1.payload"},
            "r0": {"$ref": "r0.payload"},
            "r1": {"$ref": "r1.payload"},
            "r2": {"$ref": "r2.payload"},
            "r3": {"$ref": "r3.payload"},
            "r4": {"$ref": "r4.payload"},
            "r5": {"$ref": "r5.payload"},
            "r6": {"$ref": "r6.payload"}
        }}}
    ]).to_string();
    let out = fakes::TempDir::new("fiber-process-refresh-refusals");
    let out_path = out.path().join("answer.json");
    let owner = Harness::start(
        &script,
        &[out_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    // The entry refusal and the `ready` acknowledgment arrive first.
    owner.pump();
    owner.pump();
    let ran_bys = [
        "tool t",
        "command go",
        "hook turn_end",
        "watcher 3",
        "delegate",
        "provider testprov.sign",
        "provider testprov.credential",
    ];
    let mut rids = Vec::new();
    for ran_by in ran_bys {
        let minted = owner.extension.shared().table.mint(
            super::process::requests::RequestKind::Function,
            "fiber.test/proc",
            ran_by.to_owned(),
            None,
            None,
            None,
        );
        rids.push(Value::String(minted.id.0.clone()));
    }
    // Hand the script its request ids: fiber never parses this line.
    let line = super::process::envelope_line(
        owner.extension.shared(),
        "extension_request",
        serde_json::Map::from_iter([("rids".to_owned(), Value::Array(rids))]),
    );
    owner.extension.transport().send(line).unwrap();
    for _ in 0..ran_bys.len() {
        owner.pump();
    }
    let seen = read_json_file(&out_path, "the refresh refusals");
    assert_eq!(seen["e1"]["code"], json!("invalid_arguments"));
    assert_eq!(
        seen["e1"]["message"],
        json!("host.oauth.refresh: needs a running callback, not the entry script")
    );
    let expected = [
        "host.oauth.refresh: a tool has no provider credential to refresh",
        "host.oauth.refresh: a command has no provider credential to refresh",
        "host.oauth.refresh: a hook has no provider credential to refresh",
        "host.oauth.refresh: a watcher has no provider credential to refresh",
        "host.oauth.refresh: a delegate run has no provider credential to refresh",
        "host.oauth.refresh: testprov.sign has no credential to refresh; only credential() refreshes",
        "host.oauth.refresh: testprov.credential has no credential to refresh; only credential() refreshes",
    ];
    for (index, message) in expected.iter().enumerate() {
        let id = format!("r{index}");
        assert_eq!(seen[&id]["code"], json!("invalid_arguments"), "{id}");
        assert_eq!(seen[&id]["message"], json!(message), "{id}");
    }
    assert_eq!(owner.drain(), CloseReason::Eof);
}

/// Retiring the generation ends the refresh with no answer: a late
/// answer would leak an old request context into the new generation.
#[test]
fn refresh_retired_mid_wait_answers_nothing() {
    let script = &refresh_plan(json!([
        {"read": {"as": "nested", "expect": {"kind": "extension_request"}}},
        {"write_file": {"path": {"$ref": "args.1"}, "value": {"nested": true}}},
        {"hold": {"release_on": "line", "release_line": {"kind": "release"}}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"saw": "silent"}}}
    ]))
    .to_string();
    let out = fakes::TempDir::new("fiber-process-refresh-retire");
    let out_path = out.path().join("answer.json");
    let nested_path = out.path().join("nested.json");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap(), nested_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    let (at_completion_pause, resume_refresh) =
        owner.extension.shared().pause_next_refresh_completion();
    write_pair(owner.workspace(), "testprov", "test", &expired_pair());
    let deadline = Some(owner.clock.now() + Duration::from_secs(60));
    dispatch_provider(
        &owner,
        Some(crate::lua::CredentialFor::Stored(test_pair())),
        deadline,
    );
    owner.pump();
    // The nested line reached the child, so the refresh waits on its
    // function: retiring ends that wait with no answer. The hold keeps
    // the peer parked until the test releases it, so no clock moves.
    read_json_file(&nested_path, "the nested request");
    owner.extension.shared().table.retire();
    fakes::Deadline::after(WITHIN).recv_or_fail(
        &at_completion_pause,
        "the retired refresh to drop its credential holder",
    );
    // `run_refresh` returned before this pause, so its holder is dropped.
    let file = config::CredentialFile::new(owner.workspace(), "testprov", "test").unwrap();
    assert!(
        file.try_lock().unwrap().is_some(),
        "the completion pause follows credential-lock release"
    );
    resume_refresh
        .send(())
        .expect("resume the refresh after checking its lock");
    owner
        .extension
        .transport()
        .send(br#"{"kind":"release"}"#.to_vec())
        .expect("release the peer after retiring");
    let seen = read_json_file(&out_path, "the retired refresh");
    assert_eq!(seen, json!({"saw": "silent"}));
    assert_eq!(owner.drain(), CloseReason::Eof);
}

/// The lock service behind the continuation: a held lock waits, a
/// cancelled or retired wait ends at once, and a missed deadline times
/// out with the lock sentence.
#[test]
fn file_credential_resolve_follows_the_lock_service() {
    use super::process::oauth::FileCredential;
    let home = fakes::TempDir::new("fiber-process-resolve");
    let pair = test_pair();
    let fake = fakes::clock::FakeClock::new();
    let clock: Arc<dyn contract::clock::Clock> = fake.clone();
    let cancel = support::cancel::Token::new();
    let live = || true;
    // A free lock resolves at once, and dropping the holder releases it.
    write_pair(home.path(), "testprov", "test", &expired_pair());
    let credential = FileCredential::new(home.path().to_path_buf());
    {
        let holder = credential
            .resolve(&pair, clock.as_ref(), &cancel, None, &live)
            .expect("a free lock resolves");
        assert!(matches!(holder, crate::oauth::held::Holder::File(_)));
    }
    let file = config::CredentialFile::new(home.path(), "testprov", "test").unwrap();
    assert!(file.try_lock().unwrap().is_some(), "the lock leaked");
    // A cancelled wait ends cancelled, holding nothing.
    let held = file.try_lock().unwrap().expect("a free test pair locks");
    cancel.cancel();
    let Err((code, message)) = credential.resolve(&pair, clock.as_ref(), &cancel, None, &live)
    else {
        panic!("a cancelled lock wait ends cancelled");
    };
    assert_eq!(code, contract::ErrorCode::Cancelled);
    assert_eq!(message, "host.oauth.refresh: the refresh was cancelled");
    // A retired generation ends the same way.
    let cancel = support::cancel::Token::new();
    let retired = || false;
    let Err((code, message)) = credential.resolve(&pair, clock.as_ref(), &cancel, None, &retired)
    else {
        panic!("a retired lock wait ends cancelled");
    };
    assert_eq!(code, contract::ErrorCode::Cancelled);
    assert_eq!(message, "host.oauth.refresh: the refresh was cancelled");
    drop(held);
}

/// The lock wait ends at the deadline with the lock sentence when the
/// holder never lets go.
#[test]
fn file_credential_resolve_times_out_on_a_held_lock() {
    use super::process::oauth::FileCredential;
    let home = fakes::TempDir::new("fiber-process-resolve-timeout");
    let pair = test_pair();
    write_pair(home.path(), "testprov", "test", &expired_pair());
    let _held = config::CredentialFile::new(home.path(), "testprov", "test")
        .unwrap()
        .try_lock()
        .unwrap()
        .expect("a free test pair locks");
    let fake = fakes::clock::FakeClock::new();
    let clock: Arc<dyn contract::clock::Clock> = fake.clone();
    let credential = FileCredential::new(home.path().to_path_buf());
    let deadline = fake.now() + Duration::from_secs(5);
    let (done, wait) = std::sync::mpsc::channel();
    std::thread::spawn({
        let clock = Arc::clone(&clock);
        move || {
            let cancel = support::cancel::Token::new();
            let live = || true;
            match done.send(credential.resolve(
                &pair,
                clock.as_ref(),
                &cancel,
                Some(deadline),
                &live,
            )) {
                Ok(()) | Err(_) => {}
            }
        }
    });
    #[allow(
        clippy::disallowed_methods,
        reason = "the test wakes past the lock wait's deadline (ruling 6261259790)"
    )]
    fake.advance(Duration::from_secs(6));
    // The lock wait ends at the deadline; the ten-second bound only
    // fails a broken wait instead of hanging the test.
    let answer =
        fakes::Deadline::after(Duration::from_secs(10)).recv_or_fail(&wait, "the lock wait");
    let Err((code, message)) = answer else {
        panic!("a held lock times out, not {answer:?}");
    };
    assert_eq!(code, contract::ErrorCode::Timeout);
    assert_eq!(message, "host.oauth.refresh: the credential lock timed out");
}

/// Production services behind the seam: the state store, the clock and
/// the runtime the launch wires, with no session dispatcher.
fn production_services(
    store: Arc<super::host::state::ExtensionState>,
    clock: Arc<dyn contract::clock::Clock>,
    runtime: super::runtime::Runtime,
) -> Arc<dyn super::process::context::ProcessHostServices> {
    Arc::new(super::process::context::NativeHostServices {
        runtime,
        state: store,
        clock,
        browser: Arc::new(ClosedBrowser),
        model: None,
        delegate: None,
        tool_runner: None,
        secrets: Vec::new(),
    })
}

/// A hook delivery commits its state writes on success: the set joins
/// the hook's transaction, and the reply commits it.
#[test]
fn hook_state_writes_commit_on_success() {
    let script = json!([
        {"read": {}},
        {"send": {"id": "h1", "command": "call", "args": {"call": "fiber.hook", "args": ["turn_end", {"$json": "args.1"}]}}},
        {"send": {"id": "ready", "command": "ready"}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"read": {"as": "delivery", "expect": {"kind": "extension_request"}}},
        {"send": {"id": "s1", "command": "call", "args": {"call": "state.set", "args": ["k", 1], "request_id": {"$ref": "delivery.payload.request_id"}}}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"send": {"id": "p1", "command": "reply", "args": {"request_id": {"$ref": "delivery.payload.request_id"}, "result": "done"}}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"done": true}}}
    ]).to_string();
    let out = fakes::TempDir::new("fiber-process-state-commit");
    let out_path = out.path().join("done.json");
    let owner = Harness::start_with_services(
        &script,
        &[out_path.to_str().unwrap(), &wire_hook_spec().to_string()],
        serde_json::Map::new(),
        None,
        production_services,
        FakeServices::new(),
    );
    owner.pump();
    owner.pump();
    let hook = {
        let registry = support::lock(&owner.registry);
        registry.hook_runs()[&("turn_end".to_owned(), 0)].clone()
    };
    let extension = Arc::clone(&owner.extension);
    let delivered = std::thread::spawn(move || {
        let caller = support::cancel::Token::new();
        extension.dispatch_stream(
            &hook,
            json!({"event": "turn_end"}),
            Duration::from_secs(10),
            Some(7),
            &caller,
        )
    });
    owner.pump();
    owner.pump();
    let outcome = delivered.join().expect("the hook thread ends");
    assert_eq!(outcome.expect("the hook answers"), Some(json!("done")));
    let seen = read_json_file(&out_path, "the hook delivery");
    assert_eq!(seen, json!({"done": true}));
    assert_eq!(owner.extension.shared().state.get("k"), Some(json!(1)));
    assert_eq!(owner.drain(), CloseReason::Eof);
}

/// A hook delivery discards its state writes when it times out: the set
/// joined the hook's transaction, and the missed reply drops it.
#[test]
fn hook_state_writes_discard_on_timeout() {
    let script = json!([
        {"read": {}},
        {"send": {"id": "h1", "command": "call", "args": {"call": "fiber.hook", "args": ["turn_end", {"$json": "args.1"}]}}},
        {"send": {"id": "ready", "command": "ready"}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"read": {"as": "delivery", "expect": {"kind": "extension_request"}}},
        {"send": {"id": "s1", "command": "call", "args": {"call": "state.set", "args": ["k", 1], "request_id": {"$ref": "delivery.payload.request_id"}}}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"exit": true}
    ]).to_string();
    let out = fakes::TempDir::new("fiber-process-state-discard");
    let out_path = out.path().join("done.json");
    let owner = Harness::start_with_services(
        &script,
        &[out_path.to_str().unwrap(), &wire_hook_spec().to_string()],
        serde_json::Map::new(),
        None,
        production_services,
        FakeServices::new(),
    );
    owner.pump();
    owner.pump();
    let hook = {
        let registry = support::lock(&owner.registry);
        registry.hook_runs()[&("turn_end".to_owned(), 0)].clone()
    };
    let extension = Arc::clone(&owner.extension);
    let delivered = std::thread::spawn(move || {
        let caller = support::cancel::Token::new();
        extension.dispatch_stream(
            &hook,
            json!({"event": "turn_end"}),
            Duration::from_secs(10),
            Some(7),
            &caller,
        )
    });
    owner.pump();
    // The set joined transaction 7, and no reply ever comes: the
    // deadline fails the hook, and the failure discards its writes.
    #[allow(
        clippy::disallowed_methods,
        reason = "the test wakes past the hook deadline (ruling 6261259790)"
    )]
    owner.clock.advance(Duration::from_secs(11));
    let outcome = delivered.join().expect("the hook thread ends");
    assert!(outcome.is_err(), "a timed-out hook answers: {outcome:?}");
    assert_eq!(owner.extension.shared().state.get("k"), None);
    assert_eq!(owner.drain(), CloseReason::Eof);
}

/// Commands and watchers never join a transaction: their writes land at
/// once, with no hook call open.
#[test]
fn command_and_watcher_writes_land_without_a_transaction() {
    let script = json!([
        {"read": {}},
        {"send": {"id": "c1", "command": "call", "args": {"call": "fiber.command", "args": ["go", {"$json": "args.1"}]}}},
        {"send": {"id": "w1", "command": "call", "args": {"call": "fiber.watch", "args": [["turn_started"], {"$json": "args.2"}]}}},
        {"send": {"id": "ready", "command": "ready"}},
        {"repeat": {"count": 3, "actions": [{"read": {"expect": {"kind": "command_accepted"}}}]}},
        {"read": {"as": "first", "expect": {"kind": "extension_request"}}},
        {"send": {"id": "s1", "command": "call", "args": {"call": "state.set", "args": ["k", 1], "request_id": {"$ref": "first.payload.request_id"}}}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"send": {"id": "p1", "command": "reply", "args": {"request_id": {"$ref": "first.payload.request_id"}, "result": "done"}}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"read": {"as": "second", "expect": {"kind": "extension_request"}}},
        {"send": {"id": "s2", "command": "call", "args": {"call": "state.set", "args": ["j", 2], "request_id": {"$ref": "second.payload.request_id"}}}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"send": {"id": "p2", "command": "reply", "args": {"request_id": {"$ref": "second.payload.request_id"}, "result": "done"}}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"done": true}}}
    ]).to_string();
    let out = fakes::TempDir::new("fiber-process-state-direct");
    let out_path = out.path().join("done.json");
    let owner = Harness::start_with_services(
        &script,
        &[
            out_path.to_str().unwrap(),
            &wire_command_spec().to_string(),
            &watcher_spec().to_string(),
        ],
        serde_json::Map::new(),
        None,
        production_services,
        FakeServices::new(),
    );
    for _ in 0..3 {
        owner.pump();
    }
    let (command, watcher) = {
        let registry = support::lock(&owner.registry);
        (
            registry.commands()["go"].run.clone(),
            registry
                .watcher_runs()
                .values()
                .next()
                .cloned()
                .expect("a watcher registers"),
        )
    };
    let extension = Arc::clone(&owner.extension);
    let command_thread = std::thread::spawn(move || {
        let caller = support::cancel::Token::new();
        extension.dispatch_stream(&command, json!({}), Duration::from_secs(10), None, &caller)
    });
    owner.pump();
    owner.pump();
    let outcome = command_thread.join().expect("the command thread ends");
    assert_eq!(outcome.expect("the command answers"), Some(json!("done")));
    let extension = Arc::clone(&owner.extension);
    let watcher_thread = std::thread::spawn(move || {
        let caller = support::cancel::Token::new();
        extension.dispatch_stream(&watcher, json!({}), Duration::from_secs(10), None, &caller)
    });
    owner.pump();
    owner.pump();
    let outcome = watcher_thread.join().expect("the watcher thread ends");
    assert_eq!(outcome.expect("the watcher answers"), Some(json!("done")));
    let seen = read_json_file(&out_path, "the deliveries");
    assert_eq!(seen, json!({"done": true}));
    let shared = owner.extension.shared();
    assert_eq!(shared.state.get("k"), Some(json!(1)));
    assert_eq!(shared.state.get("j"), Some(json!(2)));
    assert_eq!(owner.drain(), CloseReason::Eof);
}

/// The state size bound holds on the wire: 65,536 serialized bytes
/// stand, and 65,537 are refused with the bound's sentence.
#[test]
fn state_set_holds_the_size_bound() {
    let script = json!([
        {"send": {"id": "ready", "command": "ready"}},
        {"read": {}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"repeat_string": {"as": "ok", "value": "x", "count": 65534}},
        {"send": {"id": "s1", "command": "call", "args": {"call": "state.set", "args": ["k", {"$ref": "ok"}]}}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"repeat_string": {"as": "big", "value": "x", "count": 65535}},
        {"send": {"id": "s2", "command": "call", "args": {"call": "state.set", "args": ["k", {"$ref": "big"}]}}},
        {"read": {"as": "answer", "expect": {"kind": "command_rejected"}}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"$ref": "answer.payload"}}}
    ]).to_string();
    let out = fakes::TempDir::new("fiber-process-state-bound");
    let out_path = out.path().join("answer.json");
    let owner = Harness::start_with_services(
        &script,
        &[out_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
        production_services,
        FakeServices::new(),
    );
    owner.pump();
    owner.pump();
    owner.pump();
    let seen = read_json_file(&out_path, "the bound rejection");
    assert_eq!(seen["code"], json!("state_too_large"));
    assert_eq!(
        seen["message"],
        json!("state.set: value is 65537 JSON bytes; at most 65536 are allowed")
    );
    let shared = owner.extension.shared();
    let actual = shared.state.get("k").expect("the accepted value is stored");
    let actual = actual.as_str().expect("state.set stored a JSON string");
    let expected = "x".repeat(65534);
    assert_eq!(actual.len(), expected.len(), "state value length");
    let first_difference = actual
        .bytes()
        .zip(expected.bytes())
        .position(|(actual, expected)| actual != expected);
    assert_eq!(
        first_difference, None,
        "first differing offset: {first_difference:?}"
    );
    assert_eq!(owner.drain(), CloseReason::Eof);
}

/// Held credentials travel only the protocol's own positions: the
/// nested arguments, the nested accepted value and the refresh answer.
/// Nothing else the refresh touches names them.
#[test]
fn refresh_keeps_tokens_out_of_everything_but_the_protocol() {
    let script = &refresh_plan(json!([
        {"read": {"as": "nested"}},
        {"send": {"id": "p1", "command": "reply", "args": {"request_id": {"$ref": "nested.payload.request_id"}, "result": {"token": "tok-fresh", "expires_at": FRESH_EXPIRES}}}},
        {"read_until": {"as": "answer", "path": "payload.command_id", "equals": "c1"}},
        {"if": {"path": "answer.kind", "equals": "command_accepted", "else": [{"exit": true}]}},
        {"write_file": {"path": {"$ref": "args.0"}, "value": {"nested": {"$ref": "nested"}, "answer": {"$ref": "answer"}}}},
    ]))
    .to_string();
    let out = fakes::TempDir::new("fiber-process-refresh-redacted");
    let out_path = out.path().join("answer.json");
    let owner = Harness::start(
        script,
        &[out_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    write_pair(owner.workspace(), "testprov", "test", &expired_pair());
    let deadline = Some(owner.clock.now() + Duration::from_secs(60));
    dispatch_provider(
        &owner,
        Some(crate::lua::CredentialFor::Stored(test_pair())),
        deadline,
    );
    owner.pump();
    owner.pump();
    let seen = read_json_file(&out_path, "the refresh exchange");
    // The token rides exactly the three protocol positions: the nested
    // arguments, the nested accepted value and the refresh answer.
    assert_eq!(
        seen["nested"]["payload"]["args"],
        json!([{"token": "tok-old", "expires_at": 1}])
    );
    assert_eq!(
        seen["answer"]["payload"]["result"]["value"],
        json!({"token": "tok-fresh", "expires_at": FRESH_EXPIRES})
    );
    // Nothing else on the line names it: the envelope, the request and
    // reply ids, the registration and the key are bare.
    for path in [
        &seen["nested"]["kind"],
        &seen["nested"]["payload"]["request_id"],
        &seen["nested"]["payload"]["registration"],
        &seen["nested"]["payload"]["key"],
        &seen["answer"]["kind"],
        &seen["answer"]["payload"]["command_id"],
    ] {
        let text = path.as_str().unwrap_or_default();
        assert!(!text.contains("tok-"), "{path}");
    }
    // Nothing Debug-printable around the refresh names it either: the
    // credential source prints only its home, and the held lock only
    // which file it locks, never the pair itself.
    let file = config::CredentialFile::new(owner.workspace(), "testprov", "test").unwrap();
    let held = file.try_lock().unwrap().expect("a free test pair locks");
    let debug = format!(
        "{:?}{:?}",
        super::process::oauth::FileCredential::new(owner.workspace().to_path_buf()),
        crate::oauth::held::Holder::File(held)
    );
    assert!(!debug.contains("tok-"), "{debug}");
    assert_eq!(owner.drain(), CloseReason::Eof);
}

// Startup, death and restart behind the package seam: one synchronized
// transition covers deaths, restart admission and retirement.

/// The lifecycle state the owner stands in.
fn lifecycle_state(owner: &Harness) -> super::process::lifecycle::RunState {
    support::lock(&owner.extension.shared().lifecycle).state()
}

/// How many child deaths the owner counted.
fn lifecycle_deaths(owner: &Harness) -> u32 {
    support::lock(&owner.extension.shared().lifecycle).deaths()
}

/// How many restarts the owner ran.
fn lifecycle_restarts(owner: &Harness) -> u32 {
    support::lock(&owner.extension.shared().lifecycle).restarts()
}

/// A child `ready` between the startup check and its park wakes the wait.
#[test]
fn ready_between_startup_check_and_park_wakes_the_waiter() {
    let script = json!([
        {"read": {}},
        {"send": {"id": "ready", "command": "ready"}},
        {"hold": {"release_on": "never"}}
    ])
    .to_string();
    let owner = Harness::start(&script, &[], serde_json::Map::new(), None);
    let shared = Arc::clone(owner.extension.shared());
    let cancel = support::cancel::Token::new();
    let (checked, checked_rx) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let (answer, answer_rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let mut pause = || {
            checked.send(()).expect("the test awaits the pause point");
            match release_rx.recv() {
                Ok(()) | Err(_) => {}
            }
        };
        let result = super::process::lifecycle::await_ready_with_pause(
            &shared,
            &shared.ready,
            None,
            &cancel,
            &mut pause,
        );
        match answer.send(result) {
            Ok(()) | Err(_) => {}
        }
    });
    fakes::Deadline::after(WITHIN)
        .recv_or_fail(&checked_rx, "startup to reach the pre-park pause point");
    owner.pump();
    release.send(()).expect("the startup waiter is paused");
    let result =
        fakes::Deadline::after(WITHIN).recv_or_fail(&answer_rx, "startup to observe ready");
    assert_eq!(result, Ok(()));
    waiter.join().expect("the startup waiter ends");
    kill_peer(&owner);
    assert_eq!(owner.drain(), CloseReason::Stopped);
}

/// A concurrent restart waiter observes the ready transition if it lands
/// after its state check but before its park.
#[test]
fn ready_between_restart_check_and_park_wakes_the_waiter() {
    let script = json!([
        {"read": {}},
        {"hold": {"release_on": "never"}}
    ])
    .to_string();
    let owner = Harness::start(&script, &[], serde_json::Map::new(), None);
    let shared = Arc::clone(owner.extension.shared());
    super::process::lifecycle::set_restarting_for_test(&shared);
    let cancel = support::cancel::Token::new();
    let (checked, checked_rx) = std::sync::mpsc::channel();
    let (answer, answer_rx) = std::sync::mpsc::channel();
    let mut releases = Vec::new();
    let mut waiters = Vec::new();
    for _ in 0..2 {
        let shared = Arc::clone(&shared);
        let cancel = cancel.clone();
        let checked = checked.clone();
        let (release, release_rx) = std::sync::mpsc::channel();
        releases.push(release);
        let answer = answer.clone();
        waiters.push(std::thread::spawn(move || {
            let mut pause = || {
                checked.send(()).expect("the test awaits the pause point");
                match release_rx.recv() {
                    Ok(()) | Err(_) => {}
                }
            };
            let result = super::process::lifecycle::ensure_running_with_pause(
                &shared, None, &cancel, &mut pause,
            );
            match answer.send(result) {
                Ok(()) | Err(_) => {}
            }
        }));
    }
    drop(checked);
    drop(answer);
    let paused = fakes::Deadline::after(WITHIN);
    for _ in 0..2 {
        paused.recv_or_fail(
            &checked_rx,
            "restart waiter to reach the pre-park pause point",
        );
    }
    shared
        .ready
        .store(true, std::sync::atomic::Ordering::SeqCst);
    super::process::lifecycle::note_ready(&shared);
    for release in releases {
        release.send(()).expect("both restart waiters are paused");
    }
    let answers = fakes::Deadline::after(WITHIN);
    for _ in 0..2 {
        assert_eq!(
            answers.recv_or_fail(&answer_rx, "restart waiter to observe ready"),
            Ok(())
        );
    }
    for waiter in waiters {
        waiter.join().expect("the restart waiter ends");
    }
    kill_peer(&owner);
    assert_eq!(owner.drain(), CloseReason::Stopped);
}

/// Stops the peer's pipes, KILLs its group, then pumps the close and
/// reaps the child.
fn kill_peer(owner: &Harness) {
    let transport = owner.extension.transport();
    transport.stop();
    match transport.signal(rustix::process::Signal::KILL) {
        Ok(()) | Err(_) => {}
    }
}

/// KILLs the peer before stopping its pipes, so end of input cannot let it
/// exit normally when the test needs to prove the signal caused its death.
fn kill_peer_before_stopping(owner: &Harness) {
    let transport = owner.extension.transport();
    match transport.signal(rustix::process::Signal::KILL) {
        Ok(()) | Err(_) => {}
    }
    transport.stop();
}

/// Pumps the one close a kill leaves behind, naming what it proves: a
/// second pump observes the same dead spawn, which counts nothing.
fn pump_closed(owner: &Harness, what: &str) {
    assert!(
        matches!(owner.extension.pump_once(), Pumped::Closed(_)),
        "{what}"
    );
}

/// Ready before the deadline answers Ok: the startup wait owns nothing
/// beyond the observation.
#[test]
fn startup_ready_before_the_deadline_answers_ok() {
    let script = json!([
        {"read": {}},
        {"send": {"id": "ready", "command": "ready"}},
        {"hold": {"release_on": "never"}}
    ])
    .to_string();
    let owner = Harness::start(&script, &[], serde_json::Map::new(), None);
    owner.pump();
    let cancel = support::cancel::Token::new();
    let deadline = Some(owner.clock.now() + Duration::from_secs(60));
    assert_eq!(owner.extension.await_ready(deadline, &cancel), Ok(()));
    assert!(owner.extension.is_ready());
    assert_eq!(
        lifecycle_state(&owner),
        super::process::lifecycle::RunState::Running
    );
    kill_peer(&owner);
    assert_eq!(owner.drain(), CloseReason::Stopped);
}

/// A missed ready deadline kills the wedged child and retires its
/// generation: startup never restarts, it leaves the extension out.
#[test]
fn startup_missed_ready_kills_and_retires() {
    let script = json!([{"read": {}}, {"hold": {"release_on": "never"}}]).to_string();
    let owner = Harness::start(&script, &[], serde_json::Map::new(), None);
    let cancel = support::cancel::Token::new();
    let deadline = Some(owner.clock.now() + Duration::from_secs(60));
    // No pump: the child's ready, if any, stays unread.
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock until await_ready (ruling 6179406086)"
    )]
    owner.clock.advance(Duration::from_secs(61));
    assert_eq!(
        owner.extension.await_ready(deadline, &cancel),
        Err((
            contract::ErrorCode::Timeout,
            "fiber.test/proc: the process did not send ready before the startup deadline"
                .to_owned()
        ))
    );
    assert_eq!(
        lifecycle_state(&owner),
        super::process::lifecycle::RunState::Gone
    );
    assert_eq!(owner.extension.shared().table.generation(), 1);
    assert_eq!(owner.drain(), CloseReason::Stopped);
}

/// Cancelling the startup wait kills the child and retires, as a missed
/// deadline does.
#[test]
fn startup_cancel_kills_and_retires() {
    let script = json!([{"read": {}}, {"hold": {"release_on": "never"}}]).to_string();
    let owner = Harness::start(&script, &[], serde_json::Map::new(), None);
    let cancel = support::cancel::Token::new();
    cancel.cancel();
    let deadline = Some(owner.clock.now() + Duration::from_secs(60));
    assert_eq!(
        owner.extension.await_ready(deadline, &cancel),
        Err((
            contract::ErrorCode::Cancelled,
            "fiber.test/proc: startup was cancelled".to_owned()
        ))
    );
    assert_eq!(
        lifecycle_state(&owner),
        super::process::lifecycle::RunState::Gone
    );
    assert_eq!(owner.drain(), CloseReason::Stopped);
}

/// Registrations without `ready` do not meet the deadline: the registry
/// keeps what arrived, and the wedged child still goes.
#[test]
fn startup_registrations_without_ready_do_not_meet_the_deadline() {
    let script = json!([
        {"read": {}},
        {"send": {"id": "h1", "command": "call", "args": {"call": "fiber.hook", "args": ["turn_end", {"$json": "args.0"}]}}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"hold": {"release_on": "never"}}
    ]).to_string();
    let owner = Harness::start(
        &script,
        &[&wire_hook_spec().to_string()],
        serde_json::Map::new(),
        None,
    );
    owner.pump();
    {
        let registry = support::lock(&owner.registry);
        assert!(
            registry
                .hook_runs()
                .contains_key(&("turn_end".to_owned(), 0))
        );
    }
    let cancel = support::cancel::Token::new();
    let deadline = Some(owner.clock.now() + Duration::from_secs(60));
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock until await_ready (ruling 6179406086)"
    )]
    owner.clock.advance(Duration::from_secs(61));
    assert!(owner.extension.await_ready(deadline, &cancel).is_err());
    assert_eq!(
        lifecycle_state(&owner),
        super::process::lifecycle::RunState::Gone
    );
    assert_eq!(owner.drain(), CloseReason::Stopped);
}

/// Death retires the generation: the in-flight hook ends stopped, late
/// ids read stale, and the death counts once no matter how it is
/// observed.
#[test]
fn death_retires_the_generation_and_stales_pending_calls() {
    let script = json!([
        {"read": {}},
        {"send": {"id": "h1", "command": "call", "args": {"call": "fiber.hook", "args": ["turn_end", {"$json": "args.0"}]}}},
        {"send": {"id": "ready", "command": "ready"}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"read": {"expect": {"kind": "extension_request"}}},
        {"write_file": {"path": {"$ref": "args.1"}, "value": {"delivered": true}}},
        {"hold": {"release_on": "never"}}
    ]).to_string();
    let out = fakes::TempDir::new("fiber-process-death-delivery");
    let marker_path = out.path().join("delivered.json");
    let owner = Harness::start(
        &script,
        &[&wire_hook_spec().to_string(), marker_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    owner.pump();
    owner.pump();
    let hook = {
        let registry = support::lock(&owner.registry);
        registry.hook_runs()[&("turn_end".to_owned(), 0)].clone()
    };
    let extension = Arc::clone(&owner.extension);
    let delivered = std::thread::spawn(move || {
        let caller = support::cancel::Token::new();
        extension.dispatch_stream(
            &hook,
            json!({"event": "turn_end"}),
            Duration::from_secs(60),
            None,
            &caller,
        )
    });
    // The delivery is a direct write, and the child sends nothing after
    // it, so no pump can observe it: the marker proves it landed.
    let seen = read_json_file(&marker_path, "the hook delivery");
    assert_eq!(seen, json!({"delivered": true}));
    kill_peer(&owner);
    assert_eq!(owner.drain(), CloseReason::Stopped);
    let outcome = delivered.join().expect("the hook thread ends");
    assert!(outcome.is_err(), "a dead hook answers: {outcome:?}");
    assert_eq!(lifecycle_deaths(&owner), 1);
    assert_eq!(
        lifecycle_state(&owner),
        super::process::lifecycle::RunState::Dead
    );
    assert_eq!(owner.extension.shared().table.generation(), 1);
}

/// A hook timeout retires only its request: the child lives on, and the
/// death counter stays at zero.
#[test]
fn hook_timeout_leaves_the_child_death_counter_alone() {
    let script = json!([
        {"read": {}},
        {"send": {"id": "h1", "command": "call", "args": {"call": "fiber.hook", "args": ["turn_end", {"$json": "args.0"}]}}},
        {"send": {"id": "ready", "command": "ready"}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"read": {"expect": {"kind": "extension_request"}}},
        {"write_file": {"path": {"$ref": "args.1"}, "value": {"delivered": true}}},
        {"hold": {"release_on": "never"}}
    ]).to_string();
    let out = fakes::TempDir::new("fiber-process-hook-timeout");
    let marker_path = out.path().join("delivered.json");
    let owner = Harness::start(
        &script,
        &[&wire_hook_spec().to_string(), marker_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    owner.pump();
    owner.pump();
    let hook = {
        let registry = support::lock(&owner.registry);
        registry.hook_runs()[&("turn_end".to_owned(), 0)].clone()
    };
    let extension = Arc::clone(&owner.extension);
    let delivered = std::thread::spawn(move || {
        let caller = support::cancel::Token::new();
        extension.dispatch_stream(
            &hook,
            json!({"event": "turn_end"}),
            Duration::from_secs(10),
            None,
            &caller,
        )
    });
    // The in-flight request must land before the clock jumps past it.
    let seen = read_json_file(&marker_path, "the hook delivery");
    assert_eq!(seen, json!({"delivered": true}));
    #[allow(
        clippy::disallowed_methods,
        reason = "the test wakes past the hook deadline (ruling 6261259790)"
    )]
    owner.clock.advance(Duration::from_secs(11));
    let outcome = delivered.join().expect("the hook thread ends");
    assert!(outcome.is_err(), "a timed-out hook answers: {outcome:?}");
    assert_eq!(lifecycle_deaths(&owner), 0);
    assert!(!owner.extension.transport().exited());
    kill_peer(&owner);
    assert_eq!(owner.drain(), CloseReason::Stopped);
}

/// Reads `path` as JSON lines once it holds `count` of them, polling
/// within one deadline: each spawn appends exactly one hello, so the count
/// names the spawn the test observes. A respawned child runs the same
/// script from the top, so rounds share one log instead of one file.
fn read_json_lines(path: &std::path::Path, count: usize, what: &str) -> Vec<Value> {
    let owned = path.to_path_buf();
    let result = Arc::new(Mutex::new(None));
    let observed = Arc::clone(&result);
    fakes::wait_until(
        &format!("the peer to write {what}"),
        fakes::Deadline::after(WITHIN),
        move || {
            let Some(text) = std::fs::read_to_string(&owned).ok() else {
                return false;
            };
            let lines: Vec<Value> = text
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();
            if lines.len() >= count {
                *support::lock(&observed) = Some(lines);
                true
            } else {
                false
            }
        },
    );
    support::lock(&result)
        .take()
        .expect("the peer wrote the expected JSON lines")
}

/// The shared-restart peer: like the one-round peer, but it answers
/// every delivery until the kill, ignoring acknowledgements until a
/// delivery arrives so either message order is accepted. `args.3` records
/// each delivery for deterministic test synchronization.
const RESTART_LOOP: &str = r#"[
    {"read": {"as": "hello"}},
    {"append_file": {"path": {"$ref": "args.2"}, "value": {"$ref": "hello.payload.state"}}},
    {"send": {"id": "t1", "command": "call", "args": {"call": "fiber.tool", "args": ["t", {"$json": "args.1"}]}}},
    {"send": {"id": "h1", "command": "call", "args": {"call": "fiber.hook", "args": ["turn_end", {"$json": "args.0"}]}}},
    {"send": {"id": "ready", "command": "ready"}},
    {"loop": [
        {"read": {"as": "incoming"}},
        {"if": {"path": "incoming.kind", "equals": "extension_request", "then": [
            {"append_file": {"path": {"$ref": "args.3"}, "value": {"request_id": {"$ref": "incoming.payload.request_id"}}}},
            {"send": {"id": "p1", "command": "reply", "args": {"request_id": {"$ref": "incoming.payload.request_id"}, "result": "done"}}}
        ]}}
    ]}
]"#;

/// The refold peer: like the one-round peer, but the first hello's
/// child writes `k` under its delivery, so the restart hello carries
/// the committed write back. The respawn skips acknowledgements until its
/// delivery, then reads the refold instead.
const RESTART_FOLD: &str = r#"[
    {"read": {"as": "hello"}},
    {"append_file": {"path": {"$ref": "args.2"}, "value": {"$ref": "hello.payload.state"}}},
    {"send": {"id": "t1", "command": "call", "args": {"call": "fiber.tool", "args": ["t", {"$json": "args.1"}]}}},
    {"send": {"id": "h1", "command": "call", "args": {"call": "fiber.hook", "args": ["turn_end", {"$json": "args.0"}]}}},
    {"send": {"id": "ready", "command": "ready"}},
    {"read_until": {"as": "delivery", "path": "kind", "equals": "extension_request"}},
    {"if": {"path": "hello.payload.state.k", "equals": 1, "then": [
        {"send": {"id": "s1", "command": "call", "args": {"call": "state.set", "args": ["k", 2], "request_id": {"$ref": "delivery.payload.request_id"}}}},
        {"read": {"expect": {"kind": "command_accepted"}}}
    ]}},
    {"send": {"id": "p1", "command": "reply", "args": {"request_id": {"$ref": "delivery.payload.request_id"}, "result": "done"}}},
    {"read": {"expect": {"kind": "command_accepted"}}},
    {"hold": {"release_on": "never"}}
]"#;

/// Starts a restart peer with `state` as its first hello's fold,
/// logging each spawn's hello to `log`.
fn restart_owner(
    script: &str,
    state: serde_json::Map<String, Value>,
    log: &std::path::Path,
) -> Harness {
    let deliveries = log.with_extension("deliveries.jsonl");
    Harness::start(
        script,
        &[
            &wire_hook_spec().to_string(),
            &wire_tool_spec().to_string(),
            log.to_str().unwrap(),
            deliveries.to_str().unwrap(),
        ],
        state,
        None,
    )
}

/// Dispatches `hook` on its own thread: the caller owns the wait, and
/// the test pumps the peer's lines.
fn dispatch_hook(
    owner: &Harness,
    hook: super::runtime::FunctionRef,
    timeout: Duration,
    state_call: Option<u64>,
) -> std::thread::JoinHandle<Result<Option<Value>, crate::Error>> {
    let extension = Arc::clone(&owner.extension);
    std::thread::spawn(move || {
        let caller = support::cancel::Token::new();
        extension.dispatch_stream(
            &hook,
            json!({"event": "turn_end"}),
            timeout,
            state_call,
            &caller,
        )
    })
}

/// Starts a dispatch, pausing after admission so concurrent callers are
/// resolved before the process re-registers.
fn dispatch_hook_after_admission(
    owner: &Harness,
    hook: super::runtime::FunctionRef,
    timeout: Duration,
    resolved: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
) -> std::thread::JoinHandle<Result<Option<Value>, crate::Error>> {
    let extension = Arc::clone(&owner.extension);
    std::thread::spawn(move || {
        let caller = support::cancel::Token::new();
        extension.dispatch_stream_with_admission_pause(
            &hook,
            json!({"event": "turn_end"}),
            timeout,
            None,
            &caller,
            || {
                let _sent = resolved.send(());
                let _released = release.recv();
            },
        )
    })
}

/// A dead child restarts once under the next dispatch: the respawned
/// child re-registers the same tools, answers under a new generation,
/// and the old reference resolves no longer.
#[test]
fn restart_after_first_death_serves_the_next_dispatch() {
    let out = fakes::TempDir::new("fiber-process-restart-basic");
    let log_path = out.path().join("hellos.jsonl");
    let mut initial = serde_json::Map::new();
    initial.insert("k".to_owned(), json!(1));
    let owner = restart_owner(RESTART_LOOP, initial, &log_path);
    owner.pump();
    owner.pump();
    owner.pump();
    let hook = {
        let registry = support::lock(&owner.registry);
        registry.hook_runs()[&("turn_end".to_owned(), 0)].clone()
    };
    let tools_before: Vec<String> = support::lock(&owner.registry)
        .tools()
        .keys()
        .cloned()
        .collect();
    let generation_before = owner
        .extension
        .runtime()
        .generation("fiber.test/proc")
        .expect("the owner registers a generation");
    let first = dispatch_hook(&owner, hook.clone(), Duration::from_millis(100), None);
    // The reply arrives on its own pump: the first round answers before
    // the kill, so the death is never a race with the answer.
    owner.pump();
    let outcome = first.join().expect("the hook thread ends");
    assert_eq!(outcome.expect("the hook answers"), Some(json!("done")));
    kill_peer(&owner);
    pump_closed(&owner, "the kill closes the pipe");
    assert_eq!(
        lifecycle_state(&owner),
        super::process::lifecycle::RunState::Dead
    );
    assert_eq!(lifecycle_deaths(&owner), 1);
    // A second observation of the same dead spawn counts nothing: the
    // stopped pipe stays closed, and the death still counts once.
    pump_closed(&owner, "the stopped pipe stays closed");
    assert_eq!(
        lifecycle_state(&owner),
        super::process::lifecycle::RunState::Dead
    );
    assert_eq!(lifecycle_deaths(&owner), 1);
    // The old reference still resolves before the restart bumps the
    // generation: the next caller starts the sole restart.
    let second = dispatch_hook(&owner, hook.clone(), Duration::from_millis(100), None);
    let hellos = read_json_lines(&log_path, 2, "the restart hello");
    assert_eq!(
        hellos,
        vec![json!({"k": 1}), json!({})],
        "the restart refolds the empty store"
    );
    owner.pump();
    owner.pump();
    owner.pump();
    assert_eq!(
        lifecycle_state(&owner),
        super::process::lifecycle::RunState::Running
    );
    // docs/extensions.md:222: one that dies is restarted once, on its next call.
    assert_eq!(lifecycle_restarts(&owner), 1);
    assert_eq!(lifecycle_deaths(&owner), 1);
    let tools_after: Vec<String> = support::lock(&owner.registry)
        .tools()
        .keys()
        .cloned()
        .collect();
    assert_eq!(tools_after, tools_before, "the tools stand again");
    assert_eq!(
        owner
            .extension
            .runtime()
            .generation("fiber.test/proc")
            .expect("the restart re-registers"),
        generation_before + 1,
        "one restart bumps the generation once"
    );
    owner.pump();
    let outcome = second.join().expect("the hook thread ends");
    assert_eq!(outcome.expect("the hook answers"), Some(json!("done")));
    // The bumped generation retires the old reference: it resolves no
    // longer, and no pump is needed to prove it.
    let stale = dispatch_hook(&owner, hook, Duration::from_millis(100), None);
    let outcome = stale.join().expect("the hook thread ends");
    assert!(
        matches!(outcome, Err(crate::Error::UnknownCallback { .. })),
        "the old generation resolves no longer, not {outcome:?}"
    );
    kill_peer(&owner);
    assert_eq!(owner.drain(), CloseReason::Stopped);
}

/// Concurrent dispatches after a death share the sole restart: one
/// respawn answers both, and the restart counts once.
#[test]
fn concurrent_dispatches_share_the_sole_restart() {
    let out = fakes::TempDir::new("fiber-process-restart-shared");
    let log_path = out.path().join("hellos.jsonl");
    let deliveries_path = log_path.with_extension("deliveries.jsonl");
    let owner = restart_owner(RESTART_LOOP, serde_json::Map::new(), &log_path);
    owner.pump();
    owner.pump();
    owner.pump();
    let hook = {
        let registry = support::lock(&owner.registry);
        registry.hook_runs()[&("turn_end".to_owned(), 0)].clone()
    };
    let first = dispatch_hook(&owner, hook.clone(), Duration::from_millis(100), None);
    owner.pump();
    let outcome = first.join().expect("the hook thread ends");
    assert_eq!(outcome.expect("the hook answers"), Some(json!("done")));
    kill_peer(&owner);
    pump_closed(&owner, "the kill closes the pipe");
    std::fs::remove_file(&deliveries_path).expect("the first delivery trace exists");
    let stream_waiting = owner.extension.shared().table.observe_next_stream_wait();
    let (resolved, resolved_rx) = std::sync::mpsc::channel();
    let (release_a, release_rx_a) = std::sync::mpsc::channel();
    let (release_b, release_rx_b) = std::sync::mpsc::channel();
    let second_a = dispatch_hook_after_admission(
        &owner,
        hook.clone(),
        Duration::from_millis(100),
        resolved.clone(),
        release_rx_a,
    );
    let second_b = dispatch_hook_after_admission(
        &owner,
        hook.clone(),
        Duration::from_millis(100),
        resolved.clone(),
        release_rx_b,
    );
    drop(resolved);
    let resolved_deadline = fakes::Deadline::after(WITHIN);
    for _ in 0..2 {
        resolved_deadline.recv_or_fail(
            &resolved_rx,
            "both dispatches to resolve before the restart registers again",
        );
    }
    release_a
        .send(())
        .expect("dispatch A reached the restart gate");
    release_b
        .send(())
        .expect("dispatch B reached the restart gate");
    let hellos = read_json_lines(&log_path, 2, "the restart hello");
    assert_eq!(
        hellos,
        vec![json!({}), json!({})],
        "one respawn reads one hello"
    );
    owner.pump();
    owner.pump();
    owner.pump();
    fakes::Deadline::after(WITHIN).recv_or_fail(
        &stream_waiting,
        "the second delivery to wait behind the first",
    );
    read_json_lines(&deliveries_path, 1, "the first restart delivery");
    owner.pump();
    read_json_lines(&deliveries_path, 2, "the second restart delivery");
    owner.pump();
    for second in [second_a, second_b] {
        let outcome = second.join().expect("the hook thread ends");
        assert_eq!(outcome.expect("the hook answers"), Some(json!("done")));
    }
    // docs/extensions.md:222: one that dies is restarted once, on its next call.
    assert_eq!(lifecycle_restarts(&owner), 1);
    assert_eq!(lifecycle_deaths(&owner), 1);
    kill_peer(&owner);
    assert_eq!(owner.drain(), CloseReason::Stopped);
}

/// A second death leaves the extension gone: no third spawn runs, the
/// restart still counts once, and dispatches stop.
#[test]
fn second_death_leaves_the_extension_gone() {
    let out = fakes::TempDir::new("fiber-process-restart-gone");
    let log_path = out.path().join("hellos.jsonl");
    let owner = restart_owner(RESTART_LOOP, serde_json::Map::new(), &log_path);
    owner.pump();
    owner.pump();
    owner.pump();
    let hook = {
        let registry = support::lock(&owner.registry);
        registry.hook_runs()[&("turn_end".to_owned(), 0)].clone()
    };
    let first = dispatch_hook(&owner, hook.clone(), Duration::from_millis(100), None);
    owner.pump();
    let outcome = first.join().expect("the hook thread ends");
    assert_eq!(outcome.expect("the hook answers"), Some(json!("done")));
    kill_peer(&owner);
    pump_closed(&owner, "the kill closes the pipe");
    let second = dispatch_hook(&owner, hook.clone(), Duration::from_millis(100), None);
    read_json_lines(&log_path, 2, "the restart hello");
    owner.pump();
    owner.pump();
    owner.pump();
    owner.pump();
    let outcome = second.join().expect("the hook thread ends");
    assert_eq!(outcome.expect("the hook answers"), Some(json!("done")));
    // docs/extensions.md:222: one that dies is restarted once, on its next call.
    assert_eq!(lifecycle_restarts(&owner), 1);
    // The peer blocks after its reply, so the kill lands mid-request.
    kill_peer(&owner);
    assert_eq!(owner.drain(), CloseReason::Stopped);
    assert_eq!(
        lifecycle_state(&owner),
        super::process::lifecycle::RunState::Gone
    );
    assert_eq!(lifecycle_deaths(&owner), 2);
    // docs/extensions.md:222: one that dies is restarted once, on its next call.
    assert_eq!(lifecycle_restarts(&owner), 1);
    // Observing the same dead spawn again counts nothing: the stopped
    // pipe stays closed, and the count holds.
    pump_closed(&owner, "the stopped pipe stays closed");
    assert_eq!(lifecycle_deaths(&owner), 2);
    let hook = {
        let registry = support::lock(&owner.registry);
        registry.hook_runs()[&("turn_end".to_owned(), 0)].clone()
    };
    let third = dispatch_hook(&owner, hook, Duration::from_millis(100), None);
    let outcome = third.join().expect("the hook thread ends");
    assert!(
        matches!(outcome, Err(crate::Error::Stopped { .. })),
        "the gone extension stops, not {outcome:?}"
    );
    // docs/extensions.md:222: one that dies is restarted once, on its next call.
    assert_eq!(lifecycle_restarts(&owner), 1);
}

/// The restart hello carries the folded state as it stands: the hook's
/// committed write lands in the store before the death, and the new
/// child reads it back.
#[test]
fn restart_hello_carries_the_current_folded_state() {
    let out = fakes::TempDir::new("fiber-process-restart-fold");
    let log_path = out.path().join("hellos.jsonl");
    let mut initial = serde_json::Map::new();
    initial.insert("k".to_owned(), json!(1));
    // The write must land in the store, so this peer answers through the
    // native services instead of the recording fake.
    let owner = Harness::start_with_services(
        RESTART_FOLD,
        &[
            &wire_hook_spec().to_string(),
            &wire_tool_spec().to_string(),
            log_path.to_str().unwrap(),
        ],
        initial,
        None,
        production_services,
        FakeServices::new(),
    );
    owner.pump();
    owner.pump();
    owner.pump();
    let hook = {
        let registry = support::lock(&owner.registry);
        registry.hook_runs()[&("turn_end".to_owned(), 0)].clone()
    };
    let first = dispatch_hook(&owner, hook.clone(), Duration::from_millis(100), Some(7));
    // Two pumps answer the round: the state write, then the reply that
    // commits it.
    owner.pump();
    owner.pump();
    let outcome = first.join().expect("the hook thread ends");
    assert_eq!(outcome.expect("the hook answers"), Some(json!("done")));
    assert_eq!(
        owner.extension.shared().state.get("k"),
        Some(json!(2)),
        "the hook's write commits before the death"
    );
    kill_peer(&owner);
    pump_closed(&owner, "the kill closes the pipe");
    let second = dispatch_hook(&owner, hook, Duration::from_millis(100), None);
    let hellos = read_json_lines(&log_path, 2, "the restart hello");
    assert_eq!(
        hellos,
        vec![json!({"k": 1}), json!({"k": 2})],
        "the refold reads the store as it stands"
    );
    owner.pump();
    owner.pump();
    owner.pump();
    owner.pump();
    let outcome = second.join().expect("the hook thread ends");
    assert_eq!(outcome.expect("the hook answers"), Some(json!("done")));
    // docs/extensions.md:222: one that dies is restarted once, on its next call.
    assert_eq!(lifecycle_restarts(&owner), 1);
    kill_peer(&owner);
    assert_eq!(owner.drain(), CloseReason::Stopped);
}

/// The ordering peer: it answers one delivery, then sleeps past end of
/// input, so TERM ends it. The log records the reply, proving the final
/// delivery answered.
const FINISH_ORDER: &str = r#"[
    {"read": {"as": "hello"}},
    {"send": {"id": "h1", "command": "call", "args": {"call": "fiber.hook", "args": ["turn_end", {"$json": "args.0"}]}}},
    {"send": {"id": "ready", "command": "ready"}},
    {"repeat": {"count": 2, "actions": [{"read": {"expect": {"kind": "command_accepted"}}}]}},
    {"read": {"as": "delivery", "expect": {"kind": "extension_request"}}},
    {"send": {"id": "p1", "command": "reply", "args": {"request_id": {"$ref": "delivery.payload.request_id"}, "result": "done"}}},
    {"write_file": {"path": {"$ref": "args.1"}, "value": {"replied": true}}},
    {"linger": true}
]"#;

/// The shutdown starts the exit allowance only after the final delivery
/// answers: advancing past the allowance with the reply outstanding
/// terms nothing, and TERM follows the allowance past the answer.
#[test]
fn finish_acknowledges_the_final_delivery_before_the_exit_allowance() {
    let out = fakes::TempDir::new("fiber-process-finish-order");
    let log_path = out.path().join("replied.json");
    let owner = Harness::start(
        FINISH_ORDER,
        &[&wire_hook_spec().to_string(), log_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    owner.pump();
    owner.pump();
    let hook = {
        let registry = support::lock(&owner.registry);
        registry.hook_runs()[&("turn_end".to_owned(), 0)].clone()
    };
    let first = dispatch_hook(&owner, hook, Duration::from_millis(100), None);
    let seen = read_json_file(&log_path, "the final reply");
    assert_eq!(seen, json!({"replied": true}));
    // The reply is sent but unpumped: the final delivery is outstanding.
    let cancel = support::cancel::Token::new();
    let finishing = {
        let extension = Arc::clone(&owner.extension);
        let waiter = cancel.clone();
        std::thread::spawn(move || extension.finish(Duration::from_secs(60), &waiter))
    };
    let extension = Arc::clone(&owner.extension);
    fakes::wait_until(
        "shutdown to enter Finishing",
        fakes::Deadline::after(WITHIN),
        move || {
            support::lock(&extension.shared().lifecycle).state()
                == super::process::lifecycle::RunState::Finishing
        },
    );
    // Past the allowance with no answer, the child lives on: the
    // allowance never started.
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the exit allowance until the reply (ruling 6179406086)"
    )]
    owner.clock.advance(Duration::from_secs(120));
    assert!(
        !owner.extension.transport().exited(),
        "no TERM before the answer"
    );
    // The answer lands: the allowance starts now.
    owner.pump();
    let outcome = first.join().expect("the hook thread ends");
    assert_eq!(outcome.expect("the hook answers"), Some(json!("done")));
    // Wait for shutdown to observe the final acknowledgement and park at
    // its allowance deadline before advancing fake time.
    let term_at = owner.clock.now() + Duration::from_secs(60);
    assert!(
        owner
            .clock
            .await_parked(term_at, fakes::Deadline::after(WITHIN)),
        "shutdown parks at the deadline after the final acknowledgement"
    );
    #[allow(
        clippy::disallowed_methods,
        reason = "the test jumps over the parked run before its deadline (ruling 6260728504)"
    )]
    owner.clock.advance(Duration::from_secs(59));
    assert!(
        !owner.extension.transport().exited(),
        "no TERM inside the allowance"
    );
    // One second remains before the fixed TERM deadline.
    owner.clock.advance_when_parked(
        Some(term_at),
        None,
        Duration::from_secs(1),
        &fakes::Deadline::after(WITHIN),
    );
    let extension = Arc::clone(&owner.extension);
    fakes::wait_until(
        "TERM to end the child",
        fakes::Deadline::after(WITHIN),
        move || extension.transport().exited(),
    );
    let status = finishing
        .join()
        .expect("the shutdown ends")
        .expect("TERM ends the child");
    assert_eq!(
        status.signal(),
        Some(rustix::process::Signal::TERM.as_raw()),
        "TERM ends the child, not KILL"
    );
    assert_eq!(lifecycle_deaths(&owner), 0);
    assert_eq!(
        lifecycle_state(&owner),
        super::process::lifecycle::RunState::Finishing
    );
    assert_eq!(owner.drain(), CloseReason::Stopped);
}

/// The grace peer: it answers registrations, then ignores TERM with a
/// logged handler and sleeps on, so only KILL ends it.
const FINISH_GRACE: &str = r#"{
    "term_marker": true,
    "actions": [
        {"read": {"as": "hello"}},
        {"send": {"id": "h1", "command": "call", "args": {"call": "fiber.hook", "args": ["turn_end", {"$json": "args.0"}]}}},
        {"send": {"id": "ready", "command": "ready"}},
        {"repeat": {"count": 2, "actions": [{"read": {"expect": {"kind": "command_accepted"}}}]}},
        {"linger": true}
    ]
}"#;

/// A child that ignores TERM is KILLed after the grace: the TERM lands
/// first, the child lives through the grace, and KILL ends it.
#[test]
fn finish_kills_a_term_ignoring_child_after_grace() {
    let out = fakes::TempDir::new("fiber-process-finish-grace");
    let marker_path = out.path().join("termed.json");
    let owner = Harness::start(
        FINISH_GRACE,
        &[&wire_hook_spec().to_string(), marker_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    owner.pump();
    owner.pump();
    let allowance = Duration::from_secs(5);
    let grace = super::process::lifecycle::GRACE;
    let cancel = support::cancel::Token::new();
    let finishing = {
        let extension = Arc::clone(&owner.extension);
        let waiter = cancel.clone();
        std::thread::spawn(move || extension.finish(allowance, &waiter))
    };
    // The shutdown stamps its allowance from the frozen clock once it
    // begins, so the TERM and KILL bounds are exact from here on.
    let state_extension = Arc::clone(&owner.extension);
    fakes::wait_until(
        "shutdown to enter Finishing",
        fakes::Deadline::after(WITHIN),
        move || {
            support::lock(&state_extension.shared().lifecycle).state()
                == super::process::lifecycle::RunState::Finishing
        },
    );
    let term_at = owner.clock.now() + allowance;
    let kill_at = term_at + grace;
    let wait = fakes::Deadline::after(WITHIN);
    owner
        .clock
        .advance_when_parked(Some(term_at), None, allowance, &wait);
    let seen = read_json_file(&marker_path, "the TERM delivery");
    assert_eq!(seen, json!({"termed": true}));
    assert!(
        !owner.extension.transport().exited(),
        "the child survives TERM while its grace remains"
    );
    // Move to one millisecond before KILL, then cross the named park.
    // Reusing `wait` bounds this complete grace wait, not each advance.
    let before_kill = grace
        .checked_sub(Duration::from_millis(1))
        .expect("the grace includes one millisecond");
    owner
        .clock
        .advance_when_parked(Some(kill_at), None, before_kill, &wait);
    assert!(
        !owner.extension.transport().exited(),
        "no KILL inside the grace"
    );
    owner
        .clock
        .advance_when_parked(Some(kill_at), None, Duration::from_millis(1), &wait);
    let extension = Arc::clone(&owner.extension);
    fakes::wait_until(
        "KILL to end the child",
        fakes::Deadline::after(WITHIN),
        move || extension.transport().exited(),
    );
    let status = finishing
        .join()
        .expect("the shutdown ends")
        .expect("KILL ends the child");
    assert_eq!(
        status.signal(),
        Some(rustix::process::Signal::KILL.as_raw()),
        "KILL ends the ignorer, not TERM"
    );
    assert_eq!(owner.drain(), CloseReason::Stopped);
}

/// A child that exits on its own returns its status with no signals:
/// the shutdown reaps the voluntary exit inside the allowance.
#[test]
fn finish_returns_a_voluntary_exit_without_signals() {
    let script = json!([
        {"read": {}},
        {"send": {"id": "h1", "command": "call", "args": {"call": "fiber.hook", "args": ["turn_end", {"$json": "args.0"}]}}},
        {"send": {"id": "ready", "command": "ready"}},
        {"repeat": {"count": 2, "actions": [{"read": {"expect": {"kind": "command_accepted"}}}]}},
        {"read": {"as": "delivery", "expect": {"kind": "extension_request"}}},
        {"send": {"id": "p1", "command": "reply", "args": {"request_id": {"$ref": "delivery.payload.request_id"}, "result": "done"}}},
        {"read": {"expect": {"kind": "command_accepted"}}},
        {"hold": {"release_on": "eof"}}
    ])
    .to_string();
    let owner = Harness::start(
        &script,
        &[&wire_hook_spec().to_string()],
        serde_json::Map::new(),
        None,
    );
    owner.pump();
    owner.pump();
    let hook = {
        let registry = support::lock(&owner.registry);
        registry.hook_runs()[&("turn_end".to_owned(), 0)].clone()
    };
    let first = dispatch_hook(&owner, hook, Duration::from_millis(100), None);
    owner.pump();
    let outcome = first.join().expect("the hook thread ends");
    assert_eq!(outcome.expect("the hook answers"), Some(json!("done")));
    // No clock moves: the exit is already there, so the allowance never
    // runs and no signal fires.
    let status = owner
        .extension
        .finish(Duration::from_secs(60), &support::cancel::Token::new())
        .expect("the child exits on its own");
    assert_eq!(status.code(), Some(0));
    assert_eq!(lifecycle_deaths(&owner), 0);
    assert_eq!(owner.drain(), CloseReason::Stopped);
}

/// The waiting peer: it answers registrations, then sleeps past end of
/// input, so the shutdown's allowance owns it until cancel.
const FINISH_WAIT: &str = r#"[
    {"read": {}},
    {"send": {"id": "h1", "command": "call", "args": {"call": "fiber.hook", "args": ["turn_end", {"$json": "args.0"}]}}},
    {"send": {"id": "ready", "command": "ready"}},
    {"repeat": {"count": 2, "actions": [{"read": {"expect": {"kind": "command_accepted"}}}]}},
    {"hold": {"release_on": "never"}}
]"#;

/// Cancelling the shutdown ends the wait and leaves the child: the
/// owner stays finishing, and a second finish refuses at once.
#[test]
fn finish_cancelled_during_the_exit_allowance_leaves_the_child() {
    let owner = Harness::start(
        FINISH_WAIT,
        &[&wire_hook_spec().to_string()],
        serde_json::Map::new(),
        None,
    );
    owner.pump();
    owner.pump();
    let cancel = support::cancel::Token::new();
    let finishing = {
        let extension = Arc::clone(&owner.extension);
        let waiter = cancel.clone();
        std::thread::spawn(move || extension.finish(Duration::from_secs(60), &waiter))
    };
    let allowance_at = owner.clock.now() + Duration::from_secs(60);
    assert!(
        owner
            .clock
            .await_parked(allowance_at, fakes::Deadline::after(WITHIN)),
        "shutdown parks at the exit-allowance deadline before cancellation"
    );
    #[allow(
        clippy::disallowed_methods,
        reason = "the test wakes past the parked deadline (ruling 6261259790)"
    )]
    owner.clock.advance(Duration::from_secs(1));
    cancel.cancel();
    let outcome = finishing.join().expect("the shutdown ends");
    assert!(
        matches!(outcome, Err((ErrorCode::Cancelled, _))),
        "cancel ends the wait, not {outcome:?}"
    );
    assert!(
        !owner.extension.transport().exited(),
        "cancel leaves the child"
    );
    assert_eq!(
        lifecycle_state(&owner),
        super::process::lifecycle::RunState::Finishing
    );
    let hook = {
        let registry = support::lock(&owner.registry);
        registry.hook_runs()[&("turn_end".to_owned(), 0)].clone()
    };
    let refused = dispatch_hook(&owner, hook, Duration::from_millis(100), None);
    let outcome = refused.join().expect("the hook thread ends");
    assert!(
        matches!(outcome, Err(crate::Error::Stopped { .. })),
        "a finishing owner stops, not {outcome:?}"
    );
    let outcome = owner
        .extension
        .finish(Duration::from_secs(60), &support::cancel::Token::new());
    assert!(
        matches!(outcome, Err((ErrorCode::Closing, _))),
        "a second finish refuses, not {outcome:?}"
    );
    kill_peer(&owner);
    assert_eq!(owner.drain(), CloseReason::Stopped);
}

/// The descendant peer: it holds a grandchild on its own stdout pipe,
/// then sleeps past end of input, so only a group TERM ends both.
const FINISH_GROUP: &str = r#"[
    {"read": {}},
    {"send": {"id": "h1", "command": "call", "args": {"call": "fiber.hook", "args": ["turn_end", {"$json": "args.0"}]}}},
    {"send": {"id": "ready", "command": "ready"}},
    {"repeat": {"count": 2, "actions": [{"read": {"expect": {"kind": "command_accepted"}}}]}},
    {"spawn_descendant": {"mode": "sleep"}},
    {"linger": true}
]"#;

/// The shutdown terms the whole group: a grandchild holding the pipes
/// open dies with the child, and the group reads empty after.
#[test]
fn finish_terms_the_whole_group_despite_a_held_pipe() {
    let owner = Harness::start(
        FINISH_GROUP,
        &[&wire_hook_spec().to_string()],
        serde_json::Map::new(),
        None,
    );
    owner.pump();
    owner.pump();
    let cancel = support::cancel::Token::new();
    let finishing = {
        let extension = Arc::clone(&owner.extension);
        let waiter = cancel.clone();
        std::thread::spawn(move || extension.finish(Duration::from_secs(5), &waiter))
    };
    // The frozen clock fixes the allowance deadline even if the
    // shutdown thread enters its wait after this snapshot.
    let term_at = owner.clock.now() + Duration::from_secs(5);
    owner.clock.advance_when_parked(
        Some(term_at),
        None,
        Duration::from_secs(5),
        &fakes::Deadline::after(WITHIN),
    );
    let extension = Arc::clone(&owner.extension);
    fakes::wait_until(
        "TERM to end the group",
        fakes::Deadline::after(WITHIN),
        move || extension.transport().exited(),
    );
    let status = finishing
        .join()
        .expect("the shutdown ends")
        .expect("TERM ends the group");
    assert_eq!(
        status.signal(),
        Some(rustix::process::Signal::TERM.as_raw()),
        "TERM ends the group"
    );
    assert!(
        !owner.extension.transport().group_alive(),
        "no descendant survives the group TERM"
    );
    assert_eq!(owner.drain(), CloseReason::Stopped);
}

/// The retire peer: it answers one delivery, reads its answer's fate,
/// then answers a second one. A retirement between the two stales only
/// the first.
const RETIRE_TWO: &str = r#"[
    {"read": {}},
    {"send": {"id": "h1", "command": "call", "args": {"call": "fiber.hook", "args": ["turn_end", {"$json": "args.0"}]}}},
    {"send": {"id": "ready", "command": "ready"}},
    {"repeat": {"count": 2, "actions": [{"read": {"expect": {"kind": "command_accepted"}}}]}},
    {"read": {"as": "first", "expect": {"kind": "extension_request"}}},
    {"write_file": {"path": {"$ref": "args.1"}, "value": {"first": true}}},
    {"send": {"id": "p1", "command": "reply", "args": {"request_id": {"$ref": "first.payload.request_id"}, "result": "done"}}},
    {"read": {"as": "first_ack", "expect": {"kind": "command_rejected"}}},
    {"if": {"path": "first_ack.payload.code", "equals": "stale_request", "else": [{"exit": true}]}},
    {"read": {"as": "second", "expect": {"kind": "extension_request"}}},
    {"send": {"id": "p2", "command": "reply", "args": {"request_id": {"$ref": "second.payload.request_id"}, "result": "done"}}},
    {"read": {"expect": {"kind": "command_accepted"}}},
    {"hold": {"release_on": "never"}}
]"#;

/// Retiring the generation at the seam stales only the in-flight
/// request: the late reply reads stale, and the next request answers.
#[test]
fn retire_at_the_seam_stales_the_in_flight_request() {
    let out = fakes::TempDir::new("fiber-process-retire");
    let marker_path = out.path().join("first.json");
    let owner = Harness::start(
        RETIRE_TWO,
        &[&wire_hook_spec().to_string(), marker_path.to_str().unwrap()],
        serde_json::Map::new(),
        None,
    );
    owner.pump();
    owner.pump();
    let hook = {
        let registry = support::lock(&owner.registry);
        registry.hook_runs()[&("turn_end".to_owned(), 0)].clone()
    };
    let first = dispatch_hook(&owner, hook.clone(), Duration::from_millis(100), None);
    let seen = read_json_file(&marker_path, "the first delivery");
    assert_eq!(seen, json!({"first": true}));
    // The reply is on its way but unpumped: retiring first retires it.
    owner.extension.retire();
    owner.pump();
    let outcome = first.join().expect("the hook thread ends");
    assert!(
        matches!(outcome, Err(crate::Error::Stopped { .. })),
        "the retired request stops, not {outcome:?}"
    );
    let second = dispatch_hook(&owner, hook, Duration::from_millis(100), None);
    owner.pump();
    let outcome = second.join().expect("the hook thread ends");
    assert_eq!(outcome.expect("the hook answers"), Some(json!("done")));
    kill_peer(&owner);
    assert_eq!(owner.drain(), CloseReason::Stopped);
}

/// Finishing a dead child reaps without signals: the exit is already
/// there, so the allowance never runs.
#[test]
fn finish_after_death_reaps_without_signals() {
    let out = fakes::TempDir::new("fiber-process-finish-dead");
    let log_path = out.path().join("hellos.jsonl");
    let owner = restart_owner(RESTART_LOOP, serde_json::Map::new(), &log_path);
    owner.pump();
    owner.pump();
    owner.pump();
    kill_peer_before_stopping(&owner);
    pump_closed(&owner, "the kill closes the pipe");
    assert_eq!(
        lifecycle_state(&owner),
        super::process::lifecycle::RunState::Dead
    );
    // No clock moves: the zombie is already there, so the allowance
    // never runs and no signal fires.
    let status = owner
        .extension
        .finish(Duration::from_secs(60), &support::cancel::Token::new())
        .expect("the dead child reaps");
    assert_eq!(
        status.signal(),
        Some(rustix::process::Signal::KILL.as_raw()),
        "the test's own KILL is what reaps"
    );
    assert_eq!(
        lifecycle_state(&owner),
        super::process::lifecycle::RunState::Finishing
    );
    assert_eq!(owner.drain(), CloseReason::Stopped);
}
