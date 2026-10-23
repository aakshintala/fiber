//! The model picker's install worker (`docs/tui.md`, "Swapped views"):
//! the loop starts it after the input that chose a not-installed row, and
//! its one answer arrives as `Input::Install` on the existing channel. The
//! install fetches and blocks, so it runs on its own thread; the loop
//! thread never calls it.

use std::io;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::thread;

use contract::ErrorCode;
use contract::clock::Clock;

use crate::Input;
use crate::configure::ConfigureError;
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

#[cfg(test)]
#[path = "install_worker_tests.rs"]
mod tests;
