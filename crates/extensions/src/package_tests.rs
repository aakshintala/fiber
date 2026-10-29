//! The public package seam (`super::package`): loading process packages
//! and registering through them, with the same outcomes the wire carries.
//!
//! `Package::load` crosses the public seam; `register` is the crate's
//! internal seam, where the transport will answer registration calls. A
//! refusal carries the Lua call's exact sentence for `command_rejected`;
//! a failure carries the package failure for `extension_failed`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code; a failure is the test's"
)]

use std::sync::Arc;

use contract::CommandId;
use serde_json::{Value, json};

use super::package::{Package, PackageOptions};
use super::process::RegisterOutcome;

/// A package directory holding `manifest_extra` merged over a process
/// manifest for `fiber.test/proc`.
fn package_dir(extra: Value) -> fakes::TempDir {
    let dir = fakes::TempDir::new("fiber-process-package");
    let mut manifest = json!({
        "name": "fiber.test/proc",
        "version": "v1.2.3",
        "fiber": "0.1.0",
        "api": 1,
        "process": {"program": "proc", "exit_timeout_ms": 1000},
    });
    for (key, value) in extra.as_object().unwrap() {
        manifest[key] = value.clone();
    }
    std::fs::write(dir.path().join("extension.json"), manifest.to_string()).unwrap();
    dir
}

/// A sessionless provider's declarations name its registered functions,
/// and reading them starts the process: the hello marker proves it ran.
#[test]
fn sessionless_provider_declarations_list_registered_functions() {
    let dir = fakes::TempDir::new("fiber-process-sessionless");
    let marker = dir.path().join("hello.json");
    let script = json!([
        {"read": {}},
        {"write_file": {"path": marker, "value": true}},
        {"send": {"id": "c-reg", "command": "call", "args": {"call": "fiber.provider", "args": ["p", {"credential": {"timeout": 5000, "run": true}}]}}},
        {"read_until": {"path": "payload.command_id", "equals": "c-reg"}},
        {"send": {"id": "ready", "command": "ready"}},
        {"loop": [{"read": {}}]},
    ]);
    let (program, args) = fakes::ProcessFixture::command("peer", Some(&script));
    let manifest = json!({
        "name": "fiber.test/proc",
        "version": "v1.2.3",
        "fiber": "0.1.0",
        "api": 1,
        "providers": {"p": []},
        "process": {"program": program, "args": args, "exit_timeout_ms": 1000},
    });
    std::fs::write(dir.path().join("extension.json"), manifest.to_string()).unwrap();
    let declarations = Package::load_process_provider(PackageOptions {
        dir: dir.path().to_path_buf(),
        clock: fakes::clock::FakeClock::new(),
    })
    .unwrap();
    assert_eq!(
        declarations,
        vec![("p".to_owned(), vec!["credential".to_owned()])]
    );
    assert!(marker.is_file(), "reading declarations starts the process");
}

/// A manifest without a process section declares nothing and starts
/// nothing: there is no child to reap.
#[test]
fn sessionless_non_process_manifest_returns_no_declarations() {
    let dir = fakes::TempDir::new("fiber-process-lua-package");
    std::fs::write(
        dir.path().join("extension.json"),
        json!({
            "name": "fiber.test/lua",
            "version": "v1.2.3",
            "fiber": "0.1.0",
            "api": 1,
        })
        .to_string(),
    )
    .unwrap();
    let declarations = Package::load_process_provider(PackageOptions {
        dir: dir.path().to_path_buf(),
        clock: fakes::clock::FakeClock::new(),
    })
    .unwrap();
    assert!(declarations.is_empty());
}

/// Loads the package in `extra`, keeping its directory alive.
fn load(extra: Value) -> (fakes::TempDir, Arc<Package>) {
    let dir = package_dir(extra);
    let package = Package::load(PackageOptions {
        dir: dir.path().to_path_buf(),
        clock: fakes::clock::FakeClock::new(),
    })
    .unwrap();
    (dir, package)
}

/// Answers one registration call through the package, as the transport
/// will: the call's command id with its arguments.
fn register(package: &Package, id: &str, call: &str, args: Value) -> RegisterOutcome {
    package.register(CommandId(id.to_owned()), call, args.as_array().unwrap())
}

/// A valid `fiber.tool` spec with `run`, as JSON.
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
fn process_package_loads_its_manifest() {
    let (_dir, package) = load(json!({}));
    assert_eq!(package.name(), "fiber.test/proc");
    assert!(package.is_process());
    let registry = package.registry().unwrap();
    assert_eq!(registry.owner(), "fiber.test/proc");
    assert!(registry.tools().is_empty());
    assert!(registry.commands().is_empty());
    assert!(registry.problems().is_empty());
}

#[test]
fn package_with_wrong_api_fails() {
    let dir = package_dir(json!({"api": 999}));
    let Err(crate::Error::ApiVersion { name, api }) = Package::load(PackageOptions {
        dir: dir.path().to_path_buf(),
        clock: fakes::clock::FakeClock::new(),
    }) else {
        panic!("a wrong-api package must fail");
    };
    assert_eq!(name, "fiber.test/proc");
    assert_eq!(api, 999);
}

#[test]
fn lua_package_loads_without_a_process_registry() {
    let dir = package_dir(json!({"process": Value::Null}));
    let package = Package::load(PackageOptions {
        dir: dir.path().to_path_buf(),
        clock: fakes::clock::FakeClock::new(),
    })
    .unwrap();
    assert_eq!(package.name(), "fiber.test/proc");
    assert!(!package.is_process());
    assert!(package.registry().is_none());
}

#[test]
fn package_undeclared_provider_files_fail() {
    let dir = package_dir(json!({"providers": {"p": []}}));
    std::fs::create_dir_all(dir.path().join("providers")).unwrap();
    std::fs::write(dir.path().join("providers/q.json"), "{}").unwrap();
    let Err(crate::Error::UndeclaredProvider {
        extension,
        provider,
    }) = Package::load(PackageOptions {
        dir: dir.path().to_path_buf(),
        clock: fakes::clock::FakeClock::new(),
    })
    else {
        panic!("an undeclared provider file must fail the package");
    };
    assert_eq!(extension, "fiber.test/proc");
    assert_eq!(provider, "q");
}

#[test]
fn package_registers_a_tool() {
    let (_dir, package) = load(json!({}));
    let outcome = register(&package, "c1", "fiber.tool", json!(["t", tool_spec()]));
    assert!(matches!(outcome, RegisterOutcome::Accepted), "{outcome:?}");
    let registry = package.registry().unwrap();
    let entry = registry.tools().get("t").unwrap();
    assert_eq!(entry.run.command, CommandId("c1".to_owned()));
    assert_eq!(entry.run.generation, 1);
}

/// An invalid call refuses with the Lua sentence and its code, which the
/// wire carries as `command_rejected`.
#[test]
fn package_invalid_registration_is_rejected() {
    let (_dir, package) = load(json!({}));
    let mut spec = tool_spec();
    spec["timeout"] = json!(0);
    let outcome = register(&package, "c1", "fiber.tool", json!(["t", spec]));
    let RegisterOutcome::Rejected { code, message } = outcome else {
        panic!("an invalid tool refuses, not {outcome:?}");
    };
    assert_eq!(code, contract::ErrorCode::ExtensionFailed);
    assert_eq!(
        message,
        "`t` tool not registered: `timeout` must be a whole number of milliseconds above 0"
    );
    assert!(package.registry().unwrap().tools().is_empty());
}

/// An undeclared provider fails the package with `extension_failed`, and
/// the failure latches: later calls fail with the first failure.
#[test]
fn package_undeclared_provider_fails_the_package() {
    let (_dir, package) = load(json!({}));
    let outcome = register(
        &package,
        "c1",
        "fiber.provider",
        json!(["p", {"models": {"timeout": 1000, "run": true}}]),
    );
    let RegisterOutcome::FailedPackage { code, message } = outcome else {
        panic!("an undeclared provider fails the package, not {outcome:?}");
    };
    assert_eq!(code, contract::ErrorCode::ExtensionFailed);
    assert_eq!(
        message,
        "The extension `fiber.test/proc` registers the provider `p`, which its manifest does not declare under `providers`."
    );
    let latched = register(&package, "c2", "fiber.tool", json!(["t", tool_spec()]));
    let RegisterOutcome::FailedPackage { message: again, .. } = latched else {
        panic!("a failed package stays failed, not {latched:?}");
    };
    assert_eq!(again, message);
}

/// The process context behind the seam: the session's identity, the
/// package directory as its workspace and home, and empty folded state.
fn process_context(dir: &fakes::TempDir) -> super::process::context::ProcessContext {
    super::process::context::ProcessContext {
        session_id: Some(contract::SessionId("s_test".into())),
        reason: contract::events::ExtensionReason::New,
        workspace: dir.path().to_path_buf(),
        data_dirs: contract::events::ExtensionDataDirs {
            machine: dir.path().join("machine").display().to_string(),
            project: dir.path().join("project").display().to_string(),
        },
        state: serde_json::Map::new(),
        extension: "fiber.test/proc".into(),
        home: dir.path().to_path_buf(),
    }
}

/// The host bundle the shared session constructor needs: the test
/// package's directory anchors home and cwd, and nothing scripted
/// runs, so the browser never opens.
fn test_host(dir: &fakes::TempDir) -> super::package::ProcessProviderHost {
    super::package::ProcessProviderHost {
        home: dir.path().to_path_buf(),
        cwd: dir.path().to_path_buf(),
        browser: Arc::new(super::SystemBrowser::default()),
        host_script: None,
        session: None,
    }
}

/// The session bundle the shared session constructor needs: the
/// session's runtime, hello context, folded state store and driver, as
/// the session loader passes them.
fn test_session(
    dir: &fakes::TempDir,
    clock: Arc<dyn contract::clock::Clock>,
) -> super::package::ProcessProviderSession {
    super::package::ProcessProviderSession {
        runtime: super::runtime::Runtime::new(Arc::clone(&clock)),
        context: process_context(dir),
        state: super::host::state::StateRegistry::new(contract::hook::StateSeed::new())
            .store("fiber.test/proc"),
        drive: None,
        clock,
        startup_timeout: super::package::Package::DEFAULT_STARTUP_TIMEOUT,
    }
}

/// One emitted ephemeral event log, as the runtime's emitter captures
/// it: the child's `host.log` lands here through the real services.
#[derive(Default)]
struct LoggedCalls {
    messages: std::sync::Mutex<Vec<String>>,
}

impl contract::emit::Emit for LoggedCalls {
    fn emit(&self, event: &contract::events::Event) {
        if let contract::events::Event::ExtensionLog(log) = event {
            support::lock(&self.messages).push(log.message.clone());
        }
    }
}

/// Loads the package with `script` as its manifest-declared program:
/// the script reads its hello, registers, answers and exits, while the
/// test pumps the owner to its close.
fn load_process(
    script: &str,
    script_args: &[&str],
) -> (fakes::TempDir, Arc<Package>, Arc<LoggedCalls>) {
    let mut argv = vec!["python3".to_owned(), "-c".to_owned(), script.to_owned()];
    argv.extend(script_args.iter().map(|arg| (*arg).to_owned()));
    let dir = package_dir(json!({"process": {
        "program": "python3",
        "args": argv[1..],
        "exit_timeout_ms": 1000,
    }}));
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let logged: Arc<LoggedCalls> = Arc::new(LoggedCalls::default());
    let session = test_session(&dir, Arc::clone(&clock));
    session
        .runtime
        .set_emit(Arc::clone(&logged) as Arc<dyn contract::emit::Emit>);
    let started = Package::load_process_provider_with(
        PackageOptions {
            dir: dir.path().to_path_buf(),
            clock,
        },
        super::package::ProcessProviderMode::Session(session),
        Some(test_host(&dir)),
    )
    .expect("the manifest program starts");
    let package = started.package;
    // A watcher stops and kills a wedged peer once the bound passes,
    // so the test fails instead of hanging it.
    let watched = Arc::clone(&package);
    std::thread::spawn(move || {
        // The held sender keeps the channel live, so the receive ends
        // only when the bound passes, never from a hangup.
        let (_held, never) = std::sync::mpsc::channel::<()>();
        if fakes::Deadline::after(WITHIN).recv(&never).is_err() {
            let Some(extension) = watched.extension() else {
                return;
            };
            extension.transport().stop();
            match extension.transport().signal(rustix::process::Signal::KILL) {
                Ok(()) | Err(_) => {}
            }
        }
    });
    (dir, package, logged)
}

/// The bound a stuck child runs before the watchdog kills it: fast on
/// success, failing instead of hanging.
const WITHIN: std::time::Duration = std::time::Duration::from_secs(10);

/// Pumps the package's owner until the child closes, then reaps it.
/// `load_process` watches the peer past the bound, so a wedged child
/// fails the test instead of hanging this loop.
fn drain(package: &Package) {
    let extension = package
        .extension()
        .expect("the package started its process");
    while matches!(extension.pump_once(), super::process::Pumped::Handled) {}
    extension.transport().wait_exit().expect("the child reaps");
    extension.transport().join();
}

#[test]
fn load_process_with_shares_the_registry_with_the_child() {
    let script = r#"import sys, json
TOOL = json.loads(sys.argv[1])
def write(obj):
    sys.stdout.write(json.dumps(obj) + "\n"); sys.stdout.flush()
def read():
    return json.loads(sys.stdin.readline())
answers = {}
read()
write({"id": "c1", "command": "call", "args": {"call": "fiber.tool", "args": ["t", TOOL]}})
write({"id": "ready", "command": "ready"})
write({"id": "c2", "command": "call", "args": {"call": "host.log", "args": ["hi"]}})
for _ in range(3):
    answer = read()
    answers[answer["payload"]["command_id"]] = answer
open(sys.argv[2], "w").write(json.dumps(answers))"#;
    let out = fakes::TempDir::new("fiber-package-process");
    let out_path = out.path().join("answers.json");
    let tool = tool_spec().to_string();
    let (_dir, package, logged) = load_process(script, &[&tool, out_path.to_str().unwrap()]);
    assert!(package.is_process());
    assert!(package.extension().is_some());
    drain(&package);
    {
        let registry = package
            .registry()
            .expect("a process package holds a registry");
        assert!(
            registry.tools().contains_key("t"),
            "the child registers through the shared registry"
        );
    }
    assert_eq!(
        support::lock(&logged.messages).clone(),
        vec!["hi".to_owned()],
        "the child's host call reaches the services"
    );
    let answers: Value =
        serde_json::from_str(&std::fs::read_to_string(&out_path).expect("the peer writes answers"))
            .expect("one JSON object");
    for id in ["c1", "ready", "c2"] {
        assert_eq!(
            answers[id]["kind"],
            json!("command_accepted"),
            "{id} stands"
        );
    }
}

/// A `commands` key makes the package lazy-only: declared commands
/// register, and anything else fails the package.
#[test]
fn package_lazy_commands_restrict_registrations() {
    let (_dir, package) = load(json!({"commands": {"cmd": {"description": "Runs it."}}}));
    let outcome = register(
        &package,
        "c1",
        "fiber.command",
        json!(["cmd", {"timeout": 5000, "run": true, "description": "Runs it."}]),
    );
    assert!(matches!(outcome, RegisterOutcome::Accepted), "{outcome:?}");
    let other = register(
        &package,
        "c2",
        "fiber.command",
        json!(["nope", {"timeout": 5000, "run": true, "description": "Runs it."}]),
    );
    let RegisterOutcome::FailedPackage { code, message } = other else {
        panic!("an undeclared command fails the package, not {other:?}");
    };
    assert_eq!(code, contract::ErrorCode::ExtensionFailed);
    assert_eq!(
        message,
        "Command `nope` is not declared under `commands`; `fiber.test/proc` registers only the commands its manifest declares"
    );
}

/// Starts `dir` through the shared session constructor, as the session
/// loader does: the same runtime, context, services, state and driver
/// the old constructor threaded through.
fn load_session(
    dir: &fakes::TempDir,
    clock: Arc<dyn contract::clock::Clock>,
) -> Result<super::package::StartedProcessProvider, (contract::ErrorCode, String)> {
    Package::load_process_provider_with(
        PackageOptions {
            dir: dir.path().to_path_buf(),
            clock: Arc::clone(&clock),
        },
        super::package::ProcessProviderMode::Session(test_session(dir, clock)),
        Some(test_host(dir)),
    )
}

/// A missing program fails the package load, required or not: the
/// session loader leaves an optional extension out with this failure's
/// notice instead of loading an ownerless package.
#[test]
fn load_process_without_a_program_fails_the_package_load() {
    let dir = package_dir(json!({"process": {
        "program": "fiber-definitely-missing-program",
        "exit_timeout_ms": 1000,
    }}));
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let outcome = load_session(&dir, clock);
    let Err((code, _)) = outcome else {
        panic!("a missing program fails the package load");
    };
    assert_eq!(code, contract::ErrorCode::IoFailed);
}

/// A missing program fails a required extension: the package never
/// loads, as a required MCP server does.
#[test]
fn load_process_without_a_program_fails_a_required_extension() {
    let dir = package_dir(json!({"process": {
        "program": "fiber-definitely-missing-program",
        "required": true,
        "exit_timeout_ms": 1000,
    }}));
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let outcome = load_session(&dir, clock);
    let Err((code, _)) = outcome else {
        panic!("a required extension loads without its program");
    };
    assert_eq!(code, contract::ErrorCode::IoFailed);
}
