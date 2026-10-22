//! The fixture every workload runs in: a temporary root holding a copy of
//! the binary under test, a Fiber home and a workspace. The home holds a
//! data-only `openai-responses` provider whose base URL is a fake server on
//! `127.0.0.1`: the default load of a fresh install (`docs/performance.md`,
//! "What a budget covers").

use std::cell::RefCell;
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::rc::Rc;

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

/// Every log text read from a diagnostic home's `logs/`, or why one could
/// not be read.
pub(crate) type DiagnosticLogs = Rc<RefCell<Vec<Result<String, String>>>>;

/// The temporary root, removed on drop. The copy of `fiber` gives every
/// process the harness starts, the hub and its sessions included, one
/// unique path on its command line, which the matching watchdog kills.
pub(crate) struct Home {
    root: TempDir,
    fiber: PathBuf,
    watchdog: Option<Watchdog>,
    // Held so the provider's base URL answers for the whole run.
    server: ProviderServer,
    // Set on a home built by `debug` and on each child of one: the sink
    // this home's `logs/` are read into when it drops.
    diagnostic_logs: Option<DiagnosticLogs>,
}

impl Home {
    /// Copies `fiber` into a new root and installs the provider and the
    /// configuration. The provider answers no request.
    pub(crate) fn new(fiber: &Path) -> Result<Self, String> {
        Self::start(fiber, [], None)
    }

    /// A new home from this home's copy of `fiber`, its provider answering
    /// each request with the next response of `script`. It shares this
    /// home's sink, so it is debug when this home is.
    pub(crate) fn child(&self, script: impl IntoIterator<Item = Response>) -> Result<Self, String> {
        Self::start(self.fiber(), script, self.diagnostic_logs.clone())
    }

    fn start(
        fiber: &Path,
        script: impl IntoIterator<Item = Response>,
        diagnostic_logs: Option<DiagnosticLogs>,
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
            diagnostic_logs,
        };
        home.install()?;
        Ok(home)
    }

    fn install(&self) -> Result<(), String> {
        let source = self.root.path().join("src");
        write(
            &source.join("extension.json"),
            &json!({
                "name": "fake",
                "version": "v0.0.0",
                "fiber": "0.0.0",
                "api": 1,
                "providers": {"fake": [format!("{}/v1", self.server.url())]}
            }),
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
            &config(self.diagnostic_logs.is_some()),
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
            watchdog.stand_down(fakes::Deadline::after(STOP));
        }
        if left.is_empty() {
            Ok(())
        } else {
            Err(format!("processes {left:?} outlived their workloads"))
        }
    }
}

impl Drop for Home {
    /// Reads this home's `logs/` before the root is removed, so a home
    /// dropped without `finish` still contributes its lines.
    fn drop(&mut self) {
        if let Some(sink) = &self.diagnostic_logs {
            let logs = read_logs(&self.home().join("logs"));
            sink.borrow_mut().extend(logs);
        }
    }
}

/// The `config.json` a home installs. `diagnostics.level` is `debug` only
/// when the harness reads the home's log lines back.
fn config(debug: bool) -> Value {
    let mut config = json!({
        "model": "fake/m",
        "session": {"idle_exit_ms": SESSION_IDLE_EXIT_MS},
        "hub": {"idle_exit_ms": HUB_IDLE_EXIT_MS}
    });
    if debug && let Some(object) = config.as_object_mut() {
        object.insert("diagnostics".to_owned(), json!({"level": "debug"}));
    }
    config
}

/// The text of every direct `*.log` file in `dir`, in file-name order. A
/// file that cannot be read is an `Err` naming it; a missing `dir` holds
/// no logs. Rotated `*.log.1` files and subdirectories are not read.
fn read_logs(dir: &Path) -> Vec<Result<String, String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Vec::new(),
        Err(err) => return vec![Err(format!("listing {}: {err}", dir.display()))],
    };
    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => paths.push(entry.path()),
            Err(err) => return vec![Err(format!("listing {}: {err}", dir.display()))],
        }
    }
    paths.retain(|path| path.extension() == Some(OsStr::new("log")));
    paths.sort();
    paths
        .iter()
        .map(|path| {
            fs::read_to_string(path).map_err(|err| format!("reading {}: {err}", path.display()))
        })
        .collect()
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

#[cfg(test)]
#[path = "home_tests.rs"]
mod tests;
