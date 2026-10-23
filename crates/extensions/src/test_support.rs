//! Helpers shared by this crate's unit tests.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::sync::atomic::{AtomicUsize, Ordering};

use contract::events::CacheLifetime;
use contract::provider::{
    CallError, CallUsage, Delta, InputSize, ModelCall, ModelRequest, Provider, Reply,
    ToolDefinition,
};
use serde_json::{Map, Value};

pub(crate) use fakes::write_record;

/// A home with installed Lua extensions, loading a session under test.
pub(crate) struct Home {
    root: fakes::TempDir,
}

impl Home {
    pub(crate) fn new() -> Self {
        let root = fakes::TempDir::new("fiber-handoff-hooks");
        std::fs::create_dir(root.path().join("home")).unwrap();
        std::fs::create_dir(root.path().join("workspace")).unwrap();
        Self { root }
    }

    pub(crate) fn home(&self) -> std::path::PathBuf {
        self.root.path().join("home")
    }

    /// Installs `fiber.test/<short>`, whose entry script is `init`. No
    /// fixture registers a provider, so the manifest declares hooks alone.
    pub(crate) fn install(&self, short: &str, init: &str) {
        let src = self.root.path().join("src").join(short);
        std::fs::create_dir_all(&src).unwrap();
        let manifest = serde_json::json!({"name": format!("fiber.test/{short}"), "version": "v1.2.3", "fiber": "0.1.0", "api": 1});
        std::fs::write(src.join("extension.json"), manifest.to_string()).unwrap();
        std::fs::write(src.join("init.lua"), init).unwrap();
        crate::plan(
            &self.home(),
            &crate::Request::Path(src),
            "0.1.0",
            &crate::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    /// Installs `fiber.test/<short>` with no entry script: it loads with
    /// no VM, so its name still reaches `retain` but no service is set on it.
    pub(crate) fn install_bare(&self, short: &str) {
        let src = self.root.path().join("src").join(short);
        std::fs::create_dir_all(&src).unwrap();
        let manifest = serde_json::json!({"name": format!("fiber.test/{short}"), "version": "v1.2.3", "fiber": "0.1.0", "api": 1});
        std::fs::write(src.join("extension.json"), manifest.to_string()).unwrap();
        crate::plan(
            &self.home(),
            &crate::Request::Path(src),
            "0.1.0",
            &crate::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    /// Installs `fiber.test/<short>` with `memory_mib` in its manifest, so
    /// the hook runs under a raised cap.
    pub(crate) fn install_with_memory(&self, short: &str, init: &str, memory_mib: u64) {
        self.install(short, init);
        let path = self
            .home()
            .join("extensions")
            .join(format!("fiber.test-{short}"))
            .join("extension.json");
        let mut manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        manifest["memory_mib"] = serde_json::json!(memory_mib);
        std::fs::write(&path, manifest.to_string()).unwrap();
    }

    /// The session's extensions, loaded on a thread under a deadline: the
    /// fake clock never ends a wait the runtime does not end itself.
    #[track_caller]
    pub(crate) fn load(&self, overrides: &[&str]) -> std::sync::Arc<crate::SessionExtensions> {
        self.load_seeded(overrides, contract::hook::StateSeed::new())
    }

    pub(crate) fn load_seeded(
        &self,
        overrides: &[&str],
        seed: contract::hook::StateSeed,
    ) -> std::sync::Arc<crate::SessionExtensions> {
        use config::{Config, ProjectKey, Sources};
        let config = Config::load(Sources {
            home: self.home(),
            workspace: self.root.path().join("workspace"),
            project: ProjectKey::new("p").unwrap(),
            overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
        })
        .unwrap();
        let home = self.home();
        let locks: std::sync::Arc<dyn contract::files::PathLock> =
            std::sync::Arc::new(fakes::FakeLock::new());
        std::sync::Arc::new(bounded(move || {
            crate::SessionExtensions::load_with_state(
                &home,
                &config,
                fakes::clock::FakeClock::new(),
                locks,
                None,
                seed,
            )
        }))
    }
}

/// Runs `f` on its own thread and waits for it under
/// [`fakes::MUST_SUCCEED_WITHIN`], so a runtime that never answers fails the
/// test instead of hanging it.
#[track_caller]
pub(crate) fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _sent = tx.send(f());
    });
    fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("waited for the extensions")
}

/// A provider whose tools are its own shape, which counts its calls.
#[derive(Default)]
pub(crate) struct Inner {
    pub(crate) calls: AtomicUsize,
    pub(crate) warms: bool,
}

pub(crate) struct Idle;

impl ModelCall for Idle {
    fn run(&self, _sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        Err(CallError::Cancelled {
            usage: Box::new(CallUsage::unnamed(InputSize::default())),
        })
    }

    fn cancel(&self) {}
}

impl Provider for Inner {
    fn call(&self, _request: &ModelRequest) -> Box<dyn ModelCall> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::new(Idle)
    }

    fn warms(&self, _request: &ModelRequest) -> bool {
        self.warms
    }

    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        fakes::wire_tools(tools)
    }
}

pub(crate) fn request() -> ModelRequest {
    ModelRequest {
        system_prompt: String::new(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::FiveMinutes,
        cache_key: String::new(),
        conversation: (Vec::new()).into(),
        previous_end: None,
        sent_tools: None,
        max_output_tokens: None,
        media: Default::default(),
    }
}
