//! One installed package behind the public package seam: its manifest
//! and, for a process package, the registry its registration calls fill.
//!
//! `Package::load` is the whole public interface besides reading the
//! package back: it checks the manifest the way the session loader does,
//! so a package the session would skip fails here with the same error.
//! Registration answers ride the crate's internal seam, where the session
//! fold and later the transport answer registration calls.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use contract::CommandId;
use serde_json::Value;

use super::process::{RegisterOutcome, Registry};
use super::runtime::Runtime;
use crate::Error;

/// What `Package::load` needs: the package's directory and a clock for
/// the runtime its references run under.
pub struct PackageOptions {
    /// The package's directory, holding `extension.json`.
    pub dir: PathBuf,
    /// The clock the package's runtime reads.
    pub clock: Arc<dyn contract::clock::Clock>,
}

/// The session's side of a process-provider launch: everything the
/// shared constructor needs that only a session owns. The session
/// builds it from its load; the constructor builds the host services
/// from it and the host bundle.
pub(crate) struct ProcessProviderSession {
    /// The runtime references resolve behind.
    pub runtime: Runtime,
    /// The first hello's payload, with the session's id and reason.
    pub context: super::process::context::ProcessContext,
    /// The folded state store the session shares.
    pub state: Arc<super::host::state::ExtensionState>,
    /// The session driver, when one is wired.
    pub drive: Option<Arc<dyn contract::extension::Drive>>,
    /// The clock readiness waits read.
    pub clock: Arc<dyn contract::clock::Clock>,
    /// The configured startup allowance.
    pub startup_timeout: Duration,
}

/// The host side of a process-provider launch: what the child's host
/// calls run through. A session passes its browser and session; the
/// Call-case passes its own roots with no locks or Config.
pub(crate) struct ProcessProviderHost {
    /// Fiber home, anchoring credentials and data directories.
    pub home: PathBuf,
    /// The working directory: the CLI's, for a Call-case run.
    pub cwd: PathBuf,
    /// Opens URLs and serves the OAuth callback.
    pub browser: Arc<dyn crate::oauth::Browser>,
    /// Answers scripted host calls before any real effect.
    pub host_script: Option<Arc<crate::host::script::HostScript>>,
    /// The session's configuration, settings and path locks behind
    /// `host.config` and locked `host.fs`: `None` without a session.
    pub session: Option<crate::host::Session>,
}

/// Which launch the shared constructor runs: the session's, or a run
/// outside a session.
#[allow(
    clippy::large_enum_variant,
    reason = "built once per process start and moved a single time into the match; the copy is negligible beside the spawn"
)]
pub(crate) enum ProcessProviderMode {
    /// A session's launch, with everything only a session owns.
    Session(ProcessProviderSession),
    /// A run outside a session: the credential precheck or a Call-case
    /// provider call.
    Sessionless,
}

/// The started handle: the package whose registry the caller folds and
/// whose owner the session registers, with the services the session
/// installs its late dispatches into.
pub(crate) struct StartedProcessProvider {
    /// The loaded package, with the registry the child registers through
    /// and the started owner.
    pub package: Arc<Package>,
    /// The host services the child calls run through.
    pub services: Arc<super::process::context::NativeHostServices>,
}

/// Reaps a sessionless child when it drops: marks the owner finishing,
/// kills the child at once, then waits for the reap and joins its
/// threads. It never waits for an exit allowance, and dropping a
/// session-owned handle never kills. The credential precheck and the
/// Call-case hold it across their run, so every return path reaps.
pub(crate) struct SessionlessGuard {
    /// The owner to reap, taken on drop; `None` once defused.
    owner: Option<Arc<super::process::ProcessExtension>>,
}

impl SessionlessGuard {
    /// Holds `owner` for the drop.
    pub(crate) fn new(owner: Arc<super::process::ProcessExtension>) -> Self {
        Self { owner: Some(owner) }
    }

    /// Releases the owner without reaping: the caller owns it from here.
    pub(crate) fn defuse(&mut self) {
        self.owner = None;
    }
}

impl Drop for SessionlessGuard {
    fn drop(&mut self) {
        let Some(owner) = self.owner.take() else {
            return;
        };
        if !owner.begin_finish() {
            return;
        }
        let live = owner.transport();
        live.stop();
        // A sessionless child takes no graceful exit: the signal, not a
        // reap elsewhere, decides, and the wait below reaps it.
        match live.signal(rustix::process::Signal::KILL) {
            Ok(()) | Err(_) => {}
        }
        drop(live.wait_exit());
        live.join();
    }
}

/// One package `Package::load` read: its manifest, and its process
/// registry when its manifest declares a process. A Lua package loads
/// for its metadata; its registrations run in its VM, never here. A
/// process package started behind the seam also holds its owner, whose
/// registry is the same one the child registers through.
pub struct Package {
    manifest: config::Manifest,
    registry: Option<Arc<Mutex<Registry>>>,
    extension: Option<Arc<super::process::ProcessExtension>>,
}

impl Package {
    /// Loads the package in `options.dir`: its manifest must read and
    /// name this Fiber's extension API, and a process package must
    /// declare every provider file it ships. A process package admits
    /// its owner on a fresh runtime; a Lua package carries no registry.
    pub fn load(options: PackageOptions) -> Result<Arc<Package>, Error> {
        let clock = Arc::clone(&options.clock);
        Self::load_with_runtime(options, Runtime::new(clock))
    }

    /// Loads the package on `runtime`, so the session fold shares the
    /// session's runtime instead of minting references behind a private
    /// one: a reference resolves only behind the runtime that minted it.
    pub(crate) fn load_with_runtime(
        options: PackageOptions,
        runtime: Runtime,
    ) -> Result<Arc<Package>, Error> {
        let manifest = config::read_manifest(&options.dir)?;
        if manifest.api != crate::API {
            return Err(Error::ApiVersion {
                name: manifest.name.clone(),
                api: manifest.api,
            });
        }
        let registry: Option<Arc<Mutex<Registry>>> = if manifest.process.is_some() {
            // A process extension loads without a VM, and its `.lua`
            // files never run: only its data providers need declaring, as
            // for a Lua extension. A provider file the manifest does not
            // declare fails the package, as an undeclared registration
            // does once its process registers.
            let files = config::provider_files(&options.dir)?;
            let stems: Vec<String> = files
                .iter()
                .filter_map(|file| file.file_stem())
                .map(|stem| stem.to_string_lossy().into_owned())
                .collect();
            if let Some(unclaimed) = super::providers::first_undeclared(
                &manifest.providers,
                stems.iter().map(String::as_str),
            ) {
                return Err(Error::UndeclaredProvider {
                    extension: manifest.name.clone(),
                    provider: unclaimed.to_owned(),
                });
            }
            Some(Arc::new(Mutex::new(Registry::new(
                manifest.name.clone(),
                runtime,
                &manifest,
            ))))
        } else {
            None
        };
        Ok(Arc::new(Package {
            manifest,
            registry,
            extension: None,
        }))
    }

    /// Starts the manifest-declared process behind the seam: the same
    /// manifest checks as [`Package::load`], then the child runs with
    /// `context` as its hello and answers through `services`. A spawn
    /// failure fails the package load, required or not: the session
    /// loader leaves an optional extension out with the failure's notice
    /// and fails a required start, as a required MCP server does. A
    /// manifest with no process loads with no owner, as `load` does. The
    /// session passes its own folded state store and driver: the child
    /// never
    /// opens a standalone store and never runs driverless.
    /// `PackageOptions` stays `{ dir, clock }`: the context carries what
    /// the session activates.
    #[allow(
        dead_code,
        reason = "no production path calls the old constructor; its tests still pin registry sharing until the cleanup repoints them"
    )]
    pub(crate) fn load_process_with(
        options: PackageOptions,
        runtime: Runtime,
        context: super::process::context::ProcessContext,
        services: Arc<dyn super::process::context::ProcessHostServices>,
        state: Arc<super::host::state::ExtensionState>,
        drive: Option<Arc<dyn contract::extension::Drive>>,
    ) -> Result<Arc<Package>, Error> {
        let clock = Arc::clone(&options.clock);
        let dir = options.dir.clone();
        let package = Self::load_with_runtime(options, runtime.clone())?;
        let Some(process) = package.manifest.process.clone() else {
            return Ok(package);
        };
        let Some(registry) = package.registry.clone() else {
            return Ok(package);
        };
        // A bare name runs on `PATH` and an absolute path as written;
        // anything else joins onto the package directory, as the
        // extension's own files do.
        let program = std::path::Path::new(&process.program);
        let program = if program.is_absolute() || program.components().count() == 1 {
            program.to_path_buf()
        } else {
            dir.join(program)
        };
        // Refresh resolves stored pairs from the home credential files.
        let home = context.home.clone();
        let extension = super::process::ProcessExtension::start(super::process::Launch {
            owner: package.manifest.name.clone(),
            runtime,
            registry,
            program,
            args: process.args.clone(),
            context,
            drive,
            services,
            oauth: super::process::oauth::FileCredential::new(home),
            state,
            clock,
            limits: super::process::transport::Limits::default(),
        });
        // A spawn failure fails the package load, required or not: the
        // session loader leaves an optional extension out with this
        // failure's notice and fails a required start, as a required MCP
        // server does.
        let extension = match extension {
            Ok(extension) => Some(Arc::new(extension)),
            Err(source) => return Err(Error::Io { path: dir, source }),
        };
        Ok(Arc::new(Package {
            manifest: package.manifest.clone(),
            registry: package.registry.clone(),
            extension,
        }))
    }

    /// The manifest's extension name.
    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    /// The manifest `Package::load` read.
    pub fn manifest(&self) -> &config::Manifest {
        &self.manifest
    }

    /// Whether the manifest declares a process: only then does the
    /// package hold a registry for registration calls.
    pub fn is_process(&self) -> bool {
        self.registry.is_some()
    }

    /// How long a sessionless start waits for the child's ready: the
    /// default, with no Config to read a configured allowance from.
    pub(crate) const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_millis(5000);

    /// Starts a process package in `Sessionless` mode and returns only
    /// its provider/function declarations, for the credential precheck:
    /// a manifest without a process section declares nothing and starts
    /// nothing. The child is reaped before returning on every path.
    pub fn load_process_provider(
        options: PackageOptions,
    ) -> Result<Vec<(String, Vec<String>)>, Error> {
        let manifest = config::read_manifest(&options.dir)?;
        if manifest.process.is_none() {
            return Ok(Vec::new());
        }
        let started =
            Self::load_process_provider_with(options, ProcessProviderMode::Sessionless, None)
                .map_err(|(code, message)| Error::Lua {
                    extension: manifest.name.clone(),
                    message: format!("{code:?}: {message}"),
                })?;
        let Some(owner) = started.package.extension().cloned() else {
            return Err(Error::Lua {
                extension: manifest.name.clone(),
                message: "its process has no owner".to_owned(),
            });
        };
        let guard = SessionlessGuard::new(owner);
        let Some(registry) = started.package.registry() else {
            return Err(Error::Lua {
                extension: manifest.name.clone(),
                message: "its process registered nothing".to_owned(),
            });
        };
        let mut declarations: Vec<(String, Vec<String>)> = registry
            .providers()
            .iter()
            .map(|(name, entry)| {
                let mut functions: Vec<String> = entry.runs.keys().cloned().collect();
                functions.sort();
                (name.clone(), functions)
            })
            .collect();
        declarations.sort();
        drop(guard);
        Ok(declarations)
    }

    /// Starts the process package behind `options`: spawns the child,
    /// starts the pump and awaits readiness. The caller folds the
    /// returned package's registry and registers its owner.
    pub(crate) fn load_process_provider_with(
        options: PackageOptions,
        mode: ProcessProviderMode,
        host: Option<ProcessProviderHost>,
    ) -> Result<StartedProcessProvider, (contract::ErrorCode, String)> {
        let manifest = config::read_manifest(&options.dir).map_err(|source| {
            (
                contract::ErrorCode::IoFailed,
                format!(
                    "extension {}: cannot read its manifest: {source}",
                    options.dir.display()
                ),
            )
        })?;
        let process = manifest.process.clone().ok_or_else(|| {
            (
                contract::ErrorCode::InvalidArguments,
                format!(
                    "extension {} is not a process extension",
                    options.dir.display()
                ),
            )
        })?;
        let slug = config::dir_name(&manifest.name);
        let (runtime, context, state, drive, clock, deadline, services) = match mode {
            ProcessProviderMode::Session(session) => {
                let host = host.ok_or_else(|| {
                    (
                        contract::ErrorCode::InvalidArguments,
                        "a session process start needs its host bundle".to_owned(),
                    )
                })?;
                let deadline = session.clock.now() + session.startup_timeout;
                let services = super::process::context::NativeHostServices {
                    runtime: session.runtime.clone(),
                    state: Arc::clone(&session.state),
                    clock: Arc::clone(&session.clock),
                    browser: host.browser,
                    model: Mutex::new(None),
                    delegate: Mutex::new(None),
                    tool_runner: Mutex::new(None),
                    secrets: manifest.secrets.clone(),
                    host_script: host.host_script,
                    session: Mutex::new(host.session),
                };
                (
                    session.runtime,
                    session.context,
                    session.state,
                    session.drive,
                    session.clock,
                    deadline,
                    services,
                )
            }
            ProcessProviderMode::Sessionless => {
                let clock: Arc<dyn contract::clock::Clock> = Arc::clone(&options.clock);
                let (home, workspace) = host
                    .as_ref()
                    .map(|host| (host.home.clone(), host.cwd.clone()))
                    .unwrap_or_else(|| (options.dir.clone(), options.dir.clone()));
                let state = super::host::state::StateRegistry::standalone().store(&manifest.name);
                let data_dirs = contract::events::ExtensionDataDirs {
                    machine: config::extension_data_dir(&home, &slug)
                        .display()
                        .to_string(),
                    project: config::extension_data_dir(&home, &slug)
                        .display()
                        .to_string(),
                };
                let context = super::process::context::ProcessContext {
                    session_id: None,
                    reason: contract::events::ExtensionReason::Call,
                    workspace,
                    data_dirs,
                    state: serde_json::Map::new(),
                    extension: manifest.name.clone(),
                    home: home.clone(),
                };
                let deadline = clock.now() + Self::DEFAULT_STARTUP_TIMEOUT;
                let runtime = Runtime::new(clock.clone());
                let browser: Arc<dyn crate::oauth::Browser> = host
                    .as_ref()
                    .map(|host| Arc::clone(&host.browser))
                    .unwrap_or_else(|| Arc::new(crate::oauth::SystemBrowser::default()));
                let host_script = host.as_ref().and_then(|host| host.host_script.clone());
                let services = super::process::context::NativeHostServices {
                    runtime: runtime.clone(),
                    state: Arc::clone(&state),
                    clock: Arc::clone(&clock),
                    browser,
                    model: Mutex::new(None),
                    delegate: Mutex::new(None),
                    tool_runner: Mutex::new(None),
                    secrets: manifest.secrets.clone(),
                    host_script,
                    session: Mutex::new(None),
                };
                (runtime, context, state, None, clock, deadline, services)
            }
        };
        let services = Arc::new(services);
        let loaded = Self::load_with_runtime(
            PackageOptions {
                dir: options.dir.clone(),
                clock: Arc::clone(&options.clock),
            },
            runtime.clone(),
        )
        .map_err(|error| (error.code(), error.to_string()))?;
        let Some(registry) = loaded.registry.clone() else {
            return Err((
                contract::ErrorCode::ExtensionFailed,
                format!("{}: its process has no registry", manifest.name),
            ));
        };
        // A bare name runs on `PATH` and an absolute path as written;
        // anything else joins onto the package directory, as the
        // extension's own files do.
        let program = std::path::Path::new(&process.program);
        let program = if program.is_absolute() || program.components().count() == 1 {
            program.to_path_buf()
        } else {
            options.dir.join(program)
        };
        // Refresh resolves stored pairs from the home credential files.
        let home = context.home.clone();
        let extension = super::process::ProcessExtension::start(super::process::Launch {
            owner: manifest.name.clone(),
            runtime: runtime.clone(),
            registry,
            program,
            args: process.args.clone(),
            context,
            drive,
            services: Arc::clone(&services)
                as Arc<dyn super::process::context::ProcessHostServices>,
            oauth: super::process::oauth::FileCredential::new(home),
            state,
            clock,
            limits: super::process::transport::Limits::default(),
        })
        .map_err(|source| {
            (
                contract::ErrorCode::IoFailed,
                format!("{}: cannot start its process: {source}", manifest.name),
            )
        })?;
        let extension = Arc::new(extension);
        let package = Arc::new(Self {
            manifest: loaded.manifest.clone(),
            registry: loaded.registry.clone(),
            extension: Some(extension.clone()),
        });
        // The thread ends when the child closes; the guard below reaps
        // the child when readiness fails, so a failed start never leaves
        // a running child behind.
        let pumped = std::thread::Builder::new()
            .name(format!("process pump {}", package.manifest.name))
            .spawn({
                let pump = extension.clone();
                move || loop {
                    let spawn = pump.transport().spawn_id();
                    match pump.pump_once() {
                        super::process::Pumped::Handled => {}
                        super::process::Pumped::Closed(_) => {
                            if !pump.wait_for_next_spawn(spawn) {
                                break;
                            }
                        }
                    }
                }
            });
        // The child runs with nobody pumping it: shut it down at once,
        // as a wedged startup.
        let Ok(_pumped) = pumped else {
            drop(SessionlessGuard::new(extension));
            return Err((
                contract::ErrorCode::IoFailed,
                format!(
                    "{}: Fiber could not pump the process extension",
                    manifest.name
                ),
            ));
        };
        let mut guard = SessionlessGuard::new(extension);
        let Some(owner) = package.extension().cloned() else {
            return Err((
                contract::ErrorCode::ExtensionFailed,
                format!("{}: its process has no owner", manifest.name),
            ));
        };
        if let Err((code, message)) =
            owner.await_ready(Some(deadline), &support::cancel::Token::new())
        {
            drop(guard);
            return Err((code, format!("{}: {message}", manifest.name)));
        }
        guard.defuse();
        Ok(StartedProcessProvider { package, services })
    }

    /// Answers one registration call, as the transport will once the
    /// process starts: the call's command id with its arguments. A call
    /// to a package without a registry is not a registration.
    #[allow(
        dead_code,
        reason = "answered by the transport once processes register; asserted by tests"
    )]
    pub(crate) fn register(
        &self,
        command: CommandId,
        call: &str,
        args: &[Value],
    ) -> RegisterOutcome {
        match &self.registry {
            Some(registry) => support::lock(registry).register(command, call, args, false),
            None => RegisterOutcome::Rejected {
                code: contract::ErrorCode::ExtensionFailed,
                message: format!("`{}` is not a process package", self.manifest.name),
            },
        }
    }

    /// The process registry, when the package holds one.
    pub(crate) fn registry(&self) -> Option<MutexGuard<'_, Registry>> {
        self.registry
            .as_ref()
            .map(|registry| support::lock(registry))
    }

    /// The started process owner, when the package started one behind
    /// the seam. The package tests pump it; the session shares ownership
    /// when wired, so the owner outlives any single holder.
    pub(crate) fn extension(&self) -> Option<&Arc<super::process::ProcessExtension>> {
        self.extension.as_ref()
    }
}
