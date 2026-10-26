//! The model picker's install worker (`docs/tui.md`, "Swapped views"):
//! the loop starts it after the input that chose a not-installed row, and
//! its one answer arrives as `Input::Install` on the existing channel. The
//! install fetches and blocks, so it runs on its own thread; the loop
//! thread never calls it.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::thread;

use contract::ErrorCode;
use contract::clock::Clock;

use crate::Input;
use crate::configure::{Configure, ConfigureError};
use crate::model_picker::InstallStart;

/// Starts `start`'s install on a `tui-install` thread, posting its result.
pub(crate) fn start(start: InstallStart, clock: Arc<dyn Clock>, out: Sender<Input>) {
    start_with(start, clock, out, |builder, body| builder.spawn(body));
}

/// Starts as [`start`] does, with the thread spawn as a closure so tests
/// fail it. A thread that cannot be spawned posts the failure as the
/// result, so the row never stays installing.
pub(crate) fn start_with(
    start: InstallStart,
    clock: Arc<dyn Clock>,
    out: Sender<Input>,
    spawn: impl FnOnce(
        thread::Builder,
        Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<thread::JoinHandle<()>>,
) {
    let InstallStart {
        provider,
        workspace,
        seam,
    } = start;
    let thread_out = out.clone();
    let thread_provider = provider.clone();
    let spawned = spawn(
        crate::sources::builder("tui-install"),
        Box::new(move || {
            let result = seam.install_provider(&workspace, &thread_provider, clock);
            drop(thread_out.send(Input::Install {
                provider: thread_provider,
                result,
            }));
        }),
    );
    if let Err(source) = spawned {
        drop(out.send(Input::Install {
            provider,
            result: Err(ConfigureError {
                code: ErrorCode::IoFailed,
                message: format!("terminal: could not start the install: {source}"),
            }),
        }));
    }
}

/// The hooks install offer's names query: the refused start's
/// workspace with the exact ordered per-run overrides. Runs on its own
/// thread; the answer arrives as `Input::HooksOfferNames`.
pub(crate) struct HooksQuery {
    /// The offer waiting on the names.
    pub ticket: u64,
    /// The workspace the refused start named.
    pub workspace: PathBuf,
    /// The refused start's ordered overrides, repeated keys kept.
    pub overrides: Vec<String>,
    /// The seam the query runs through.
    pub seam: Arc<dyn Configure>,
}

/// The hooks install offer's install: the existing global
/// `install_hooks` plan and commit, never `--project`. Runs on its own
/// thread; the answer arrives as `Input::HooksInstalled`.
pub(crate) struct HooksInstall {
    /// The offer waiting on the install.
    pub ticket: u64,
    /// The seam the install runs through.
    pub seam: Arc<dyn Configure>,
}

/// The hooks offer's one background job: its names query, or its
/// install once Enter commits it.
pub(crate) enum HooksJob {
    /// The refused start's blocking-hooks query.
    Query(HooksQuery),
    /// The committed global hooks install.
    Install(HooksInstall),
}

/// Starts the offer's `job` on a `tui-install` thread, posting its
/// result.
pub(crate) fn start_hooks(job: HooksJob, clock: Arc<dyn Clock>, out: Sender<Input>) {
    start_hooks_with(job, clock, out, |builder, body| builder.spawn(body));
}

/// Starts as [`start_hooks`] does, with the thread spawn as a closure
/// so tests fail it. A thread that cannot be spawned posts the failure
/// as the result, so the offer never waits with no answer.
pub(crate) fn start_hooks_with(
    job: HooksJob,
    clock: Arc<dyn Clock>,
    out: Sender<Input>,
    spawn: impl FnOnce(
        thread::Builder,
        Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<thread::JoinHandle<()>>,
) {
    match job {
        HooksJob::Query(query) => {
            let ticket = query.ticket;
            let thread_out = out.clone();
            let spawned = spawn(
                crate::sources::builder("tui-install"),
                Box::new(move || {
                    let result = query
                        .seam
                        .blocking_hooks(&query.workspace, &query.overrides);
                    drop(thread_out.send(Input::HooksOfferNames {
                        ticket: query.ticket,
                        result,
                    }));
                }),
            );
            if let Err(source) = spawned {
                drop(out.send(Input::HooksOfferNames {
                    ticket,
                    result: Err(ConfigureError {
                        code: ErrorCode::IoFailed,
                        message: format!("terminal: could not start the names query: {source}"),
                    }),
                }));
            }
        }
        HooksJob::Install(install) => {
            let ticket = install.ticket;
            let thread_out = out.clone();
            let spawned = spawn(
                crate::sources::builder("tui-install"),
                Box::new(move || {
                    let result = install.seam.install_hooks(clock);
                    drop(thread_out.send(Input::HooksInstalled {
                        ticket: install.ticket,
                        result,
                    }));
                }),
            );
            if let Err(source) = spawned {
                drop(out.send(Input::HooksInstalled {
                    ticket,
                    result: Err(ConfigureError {
                        code: ErrorCode::IoFailed,
                        message: format!("terminal: could not start the install: {source}"),
                    }),
                }));
            }
        }
    }
}

#[cfg(test)]
#[path = "install_worker_tests.rs"]
mod tests;
