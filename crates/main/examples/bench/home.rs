//! The fixture every workload runs in: a temporary root holding a copy of
//! the binary under test, a Fiber home and a workspace. The home holds a
//! data-only `openai-responses` provider whose base URL is a fake server on
//! `127.0.0.1`: the default load of a fresh install (`docs/performance.md`,
//! "What a budget covers").

use std::fs;
use std::path::{Path, PathBuf};

use fakes::{ProviderServer, Response, TempDir, Watchdog};
use serde_json::{Value, json};

use crate::run::{STOP, System};

/// The fake provider's credential variable.
pub(crate) const KEY_VAR: &str = "FIBER_TEST_FAKE_KEY";

/// How long a session with nothing to do stays running: past any workload.
const SESSION_IDLE_EXIT_MS: u64 = 3_600_000;

/// How long the hub a terminal started stays up after its last client
/// leaves, as `crates/main/tests/terminal.rs` sets it.
const HUB_IDLE_EXIT_MS: u64 = 1_000;

/// The temporary root, removed on drop. The copy of `fiber` gives every
/// process the harness starts, the hub and its sessions included, one
/// unique path on its command line, which the matching watchdog kills.
pub(crate) struct Home {
    root: TempDir,
    fiber: PathBuf,
    watchdog: Option<Watchdog>,
    // Held so the provider's base URL answers for the whole run.
    server: ProviderServer,
}

impl Home {
    /// Copies `fiber` into a new root and installs the provider and the
    /// configuration. The provider answers no request.
    pub(crate) fn new(fiber: &Path) -> Result<Self, String> {
        Self::scripted(fiber, [])
    }

    /// [`Home::new`], with the provider answering each request with the
    /// next response of `script`.
    pub(crate) fn scripted(
        fiber: &Path,
        script: impl IntoIterator<Item = Response>,
    ) -> Result<Self, String> {
        let server = ProviderServer::start(script)
            .map_err(|err| format!("starting the fake provider: {err}"))?;
        let root = TempDir::new("fb");
        for dir in ["h", "w"] {
            create(&root.path().join(dir))?;
        }
        let copy = root.path().join("fiber");
        fs::copy(fiber, &copy)
            .map_err(|err| format!("copying {} into the root: {err}", fiber.display()))?;
        let pattern = copy
            .to_str()
            .ok_or_else(|| format!("{} is not UTF-8", copy.display()))?;
        let watchdog = Watchdog::matching(pattern);
        let home = Self {
            root,
            fiber: copy,
            watchdog: Some(watchdog),
            server,
        };
        home.install()?;
        Ok(home)
    }

    fn install(&self) -> Result<(), String> {
        let source = self.root.path().join("src");
        write(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        )?;
        write(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": KEY_VAR},
                "models": [{
                    "id": "m",
                    "protocol": "openai-responses",
                    "base_url": format!("{}/v1", self.server.url()),
                    "context_window": fakes::CONTEXT_WINDOW
                }]
            }),
        )?;
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(source),
            "0.0.0",
            &extensions::Origin::github(),
            &System,
        )
        .map_err(|err| format!("planning the provider install: {err:?}"))?
        .commit()
        .map_err(|err| format!("installing the provider: {err:?}"))?;
        write(
            &self.home().join("config.json"),
            &json!({
                "model": "fake/m",
                "session": {"idle_exit_ms": SESSION_IDLE_EXIT_MS},
                "hub": {"idle_exit_ms": HUB_IDLE_EXIT_MS}
            }),
        )
    }

    /// The fake provider, which records every request it received.
    pub(crate) fn server(&self) -> &ProviderServer {
        &self.server
    }

    pub(crate) fn root(&self) -> &Path {
        self.root.path()
    }

    pub(crate) fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    pub(crate) fn workspace(&self) -> PathBuf {
        self.root.path().join("w")
    }

    /// The copy of the binary under test.
    pub(crate) fn fiber(&self) -> &Path {
        &self.fiber
    }

    /// A session's socket.
    pub(crate) fn socket(&self, id: &str) -> PathBuf {
        self.home().join("run").join(id)
    }

    /// The hub's socket, removed when the hub exits.
    pub(crate) fn hub_socket(&self) -> PathBuf {
        self.home().join("run").join("hub")
    }

    /// Kills every process still running the copy, and errs naming them:
    /// at the end of the run no child of the harness remains.
    pub(crate) fn finish(mut self) -> Result<(), String> {
        let pattern = self.fiber.to_string_lossy().into_owned();
        let left = fakes::matching(&pattern).map_err(|err| format!("listing processes: {err}"))?;
        if !left.is_empty() {
            fakes::kill_matching(&pattern).map_err(|err| format!("killing processes: {err}"))?;
        }
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.stand_down(STOP);
        }
        if left.is_empty() {
            Ok(())
        } else {
            Err(format!("processes {left:?} outlived their workloads"))
        }
    }
}

fn create(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|err| format!("creating {}: {err}", dir.display()))
}

fn write(file: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = file.parent() {
        create(parent)?;
    }
    fs::write(file, value.to_string()).map_err(|err| format!("writing {}: {err}", file.display()))
}
