//! Session diagnostics attachment: the one-time sink on each VM and the
//! hook-chain timer scopes (`docs/state.md`, "Diagnostic logs"). Same-crate
//! tests for what no public API exposes: which VM holds which sink, and
//! that attachment starts no callback.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use config::{Config, ProjectKey, Sources};
use contract::diag::{DebugLog, ProviderRequest, Timing};
use fakes::clock::FakeClock;

use super::SessionExtensions;
use crate::host::FakeLock;
use crate::{LuaExtension, Origin, Request, plan};

/// A sink that keeps every request it is given.
#[derive(Default)]
struct Sink {
    requests: Mutex<Vec<ProviderRequest>>,
}

impl Sink {
    fn named() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

impl DebugLog for Sink {
    fn debugging(&self) -> bool {
        true
    }

    fn provider_request(&self, request: &ProviderRequest) {
        self.requests.lock().unwrap().push(request.clone());
    }
}

/// A timing that records each invocation and returns a token guard.
fn recording() -> (Timing, Arc<Mutex<Vec<contract::diag::Op>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    let timing: Timing = Arc::new(move |op, _tool, _started| {
        seen.lock().unwrap().push(op);
        Some(Box::new(()) as Box<dyn Send>)
    });
    (timing, calls)
}

struct Home {
    root: fakes::TempDir,
}

impl Home {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-diagnostics-attach");
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// Installs `fiber.test/<short>`: `init.lua` when `init` is `Some`, a
    /// provider-only extension otherwise. The manifest declares every
    /// provider file, with no URLs.
    fn install(&self, short: &str, init: Option<&str>, providers: &[(&str, &str)]) {
        let src = self.root.path().join("src").join(short);
        fs::create_dir_all(&src).unwrap();
        let mut manifest = serde_json::json!({"name": format!("fiber.test/{short}"), "version": "v1.2.3", "fiber": "0.1.0", "api": 1});
        if !providers.is_empty() {
            fs::create_dir_all(src.join("providers")).unwrap();
            let declared: serde_json::Map<String, serde_json::Value> = providers
                .iter()
                .map(|(name, lua)| {
                    fs::write(src.join("providers").join(format!("{name}.lua")), lua).unwrap();
                    ((*name).to_owned(), serde_json::json!([]))
                })
                .collect();
            manifest["providers"] = serde_json::Value::Object(declared);
        }
        fs::write(src.join("extension.json"), manifest.to_string()).unwrap();
        if let Some(init) = init {
            fs::write(src.join("init.lua"), init).unwrap();
        }
        plan(
            &self.home(),
            &Request::Path(src),
            "0.1.0",
            &Origin::github(),
            &*FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    fn load(&self, clock: Arc<FakeClock>) -> SessionExtensions {
        let config = Config::load(Sources {
            home: self.home(),
            workspace: self.root.path().join("workspace"),
            project: ProjectKey::new("p").unwrap(),
            overrides: Vec::new(),
        })
        .unwrap();
        let home = self.home();
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(FakeLock::new());
        // On its own thread: the fake clock never ends a wait the runtime
        // does not end itself.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _sent = tx.send(SessionExtensions::load(&home, &config, clock, locks, None));
        });
        fakes::Deadline::after(Duration::from_secs(5))
            .recv(&rx)
            .expect("waited for the extensions to load")
    }
}

const HOOKS_INIT: &str = r#"
fiber.hook("before_message", { timeout = 30000, on_failure = "non-blocking",
  run = function(msg) return {} end })
"#;

const PROVIDER_ONLY: &str =
    "fiber.provider(\"p\", { models = { timeout = 30000, run = function() return {} end } })";

#[test]
fn hub_sink_first_installation_wins() {
    let clock = FakeClock::new();
    let hub = crate::lua::Hub::new(clock);
    let first = Sink::named();
    let second = Sink::named();
    hub.set_diagnostics(first.clone());
    hub.set_diagnostics(second.clone());
    let held = hub.diagnostics().expect("a sink was installed");
    assert!(
        Arc::ptr_eq(&held, &(first as Arc<dyn DebugLog>)),
        "the second installation must not redirect the VM"
    );
}

#[test]
fn lua_builder_installs_the_sink_without_starting_the_vm() {
    let dir = fakes::TempDir::new("fiber-diagnostics-builder");
    let extension = LuaExtension::new("ext", dir.path(), dir.path(), FakeClock::new());
    assert!(!extension.is_running());
    let sink = Sink::named();
    let extension = extension.with_diagnostics(sink.clone());
    assert!(!extension.is_running(), "attachment runs no callback");
    let held = extension
        .hub_diagnostics()
        .expect("the builder installed the sink");
    assert!(
        Arc::ptr_eq(&held, &(sink as Arc<dyn DebugLog>)),
        "the installed sink is the one given"
    );
}

#[test]
fn session_attach_reaches_hook_and_provider_only_vms() {
    let home = Home::new();
    home.install("hooks", Some(HOOKS_INIT), &[]);
    home.install("provider", None, &[("p", PROVIDER_ONLY)]);
    let clock = FakeClock::new();
    let session = home.load(clock);
    assert_eq!(session.attached_vms().len(), 2);
    for vm in session.attached_vms() {
        assert!(
            vm.hub_diagnostics().is_none(),
            "no sink before with_diagnostics"
        );
    }
    let provider_vm = session
        .attached_vms()
        .into_iter()
        .find(|vm| vm.name() == "fiber.test/provider")
        .expect("the provider-only VM is attached");
    assert!(
        !provider_vm.is_running(),
        "loading starts no provider callback"
    );
    let sink = Sink::named();
    let (timing, _) = recording();
    let session = session.with_diagnostics(timing, sink.clone());
    let vms = session.attached_vms();
    assert_eq!(vms.len(), 2, "hook and provider-only VMs");
    for vm in &vms {
        let held = vm.hub_diagnostics().expect("every live VM holds the sink");
        assert!(
            Arc::ptr_eq(&held, &(sink.clone() as Arc<dyn DebugLog>)),
            "the installed sink reaches {}",
            vm.name()
        );
    }
    assert!(
        !provider_vm.is_running(),
        "attachment runs no provider callback"
    );
    // A second installation never redirects a VM that already has one.
    let later = Sink::named();
    let (timing, _) = recording();
    let session = session.with_diagnostics(timing, later);
    for vm in session.attached_vms() {
        let held = vm.hub_diagnostics().expect("the sink stays installed");
        assert!(
            Arc::ptr_eq(&held, &(sink.clone() as Arc<dyn DebugLog>)),
            "the first sink still observes {}",
            vm.name()
        );
    }
}

#[test]
fn session_without_extensions_attaches_cleanly() {
    let home = Home::new();
    let clock = FakeClock::new();
    let session = home.load(clock);
    assert!(session.attached_vms().is_empty());
    let sink = Sink::named();
    let (timing, calls) = recording();
    let session = session.with_diagnostics(timing, sink);
    assert!(session.attached_vms().is_empty());
    assert!(calls.lock().unwrap().is_empty());
}

const TURN_START_INIT: &str = r#"
fiber.hook("turn_start", { timeout = 30000, on_failure = "non-blocking",
  run = function(ctx) return { context = "t" } end })
"#;

#[test]
fn dispatch_on_a_stopped_vm_keeps_its_scope() {
    use contract::hook::{ContextOutcome, Hooks, TurnStart};
    use contract::tool::Cancel;

    struct Never;
    impl Cancel for Never {
        fn is_cancelled(&self) -> bool {
            false
        }

        fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
    }

    let home = Home::new();
    home.install("hooks", Some(TURN_START_INIT), &[]);
    let clock = FakeClock::new();
    let session = home.load(clock);
    let sink = Sink::named();
    let (timing, calls) = recording();
    let session = session.with_diagnostics(timing, sink);
    assert!(session.attached_vms().len() == 1);
    for vm in session.attached_vms() {
        vm.dispose();
    }
    // A stopped VM answers at once, without waiting: the dispatch still
    // holds its one scope around the failure.
    let answer = session.turn_start(&TurnStart {
        input: &[],
        cancel: &Never,
    });
    match &answer.outcome {
        ContextOutcome::Added(context) => assert!(context.is_empty()),
        ContextOutcome::Failed(failure) => panic!("a stopped chain adds no context: {failure:?}"),
    }
    assert_eq!(answer.notices.len(), 1);
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(
        calls.lock().unwrap()[0],
        contract::diag::Op::Hook(contract::diag::HookChain::TurnStart)
    );
}
