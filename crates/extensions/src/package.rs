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

/// One package `Package::load` read: its manifest, and its process
/// registry when its manifest declares a process. A Lua package loads
/// for its metadata; its registrations run in its VM, never here. A
/// process package started behind the seam also holds its owner, whose
/// registry is the same one the child registers through.
pub struct Package {
    manifest: config::Manifest,
    registry: Option<Arc<Mutex<Registry>>>,
    extension: Option<super::process::ProcessExtension>,
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
    /// `context` as its hello and answers through `services`. A manifest
    /// with no process loads with no owner, as `load` does.
    /// `PackageOptions` stays `{ dir, clock }`: no session activation.
    #[allow(
        dead_code,
        reason = "asserted by tests; session activation lands in Part D"
    )]
    pub(crate) fn load_process_with(
        options: PackageOptions,
        runtime: Runtime,
        context: super::process::context::ProcessContext,
        services: Arc<dyn super::process::context::ProcessHostServices>,
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
        // Refresh resolves stored pairs from the home credential files;
        // the session wires its account choice in Part D. State opens on
        // a standalone store; the session folds its state into the store
        // in Part D.
        let home = context.home.clone();
        let extension = super::process::ProcessExtension::start(super::process::Launch {
            owner: package.manifest.name.clone(),
            runtime,
            registry,
            program,
            args: process.args.clone(),
            context,
            drive: None,
            services,
            oauth: super::process::oauth::FileCredential::new(home),
            state: super::host::state::StateRegistry::standalone().store(&package.manifest.name),
            clock,
            limits: super::process::transport::Limits::default(),
        });
        // A missing program leaves an optional extension out, with no
        // owner for the session to start; a required one fails the
        // package, as a required MCP server does.
        let extension = match extension {
            Ok(extension) => Some(extension),
            Err(_source) if !process.required => {
                return Ok(Arc::new(Package {
                    manifest: package.manifest.clone(),
                    registry: package.registry.clone(),
                    extension: None,
                }));
            }
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

    /// Answers one registration call, as the transport will once the
    /// process starts: the call's command id with its arguments. A call
    /// to a package without a registry is not a registration.
    #[allow(
        dead_code,
        reason = "answered by the transport once processes register in a later part; asserted by tests"
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
    /// the seam. The package tests pump it; the session owns it in
    /// Part D.
    #[allow(
        dead_code,
        reason = "asserted by tests; production use lands in a later part"
    )]
    pub(crate) fn extension(&self) -> Option<&super::process::ProcessExtension> {
        self.extension.as_ref()
    }
}
