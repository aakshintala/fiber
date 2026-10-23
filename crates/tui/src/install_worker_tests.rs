//! Tests for the install worker: one install off the calling thread, one
//! answer on the loop's channel (`docs/tui.md`, "Swapped views").

use std::io;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use fakes::Deadline;

use super::{start, start_with};
use crate::Input;
use crate::configure::{Configure, ConfigureError, ProviderInstall};
use crate::configure_fake::Fake;
use crate::model_picker::InstallStart;

/// The deadline every wait for the worker takes.
const WAIT: Duration = Duration::from_secs(10);

fn job(fake: &Arc<Fake>, provider: &str) -> InstallStart {
    InstallStart {
        provider: provider.to_owned(),
        workspace: "/w".into(),
        seam: Arc::clone(fake) as Arc<dyn Configure>,
    }
}

fn clock() -> Arc<dyn Clock> {
    fakes::clock::FakeClock::new()
}

/// The next install the worker posted, within the deadline.
#[track_caller]
fn next(rx: &Receiver<Input>) -> (String, Result<ProviderInstall, ConfigureError>) {
    match Deadline::after(WAIT).recv(rx) {
        Ok(Input::Install { provider, result }) => (provider, result),
        Ok(_) => panic!("the worker posted something else"),
        Err(err) => panic!("waited {WAIT:?} for the worker: {err}"),
    }
}

fn answered(summary: &str) -> ProviderInstall {
    ProviderInstall {
        installed: Vec::new(),
        summary: vec![summary.to_owned()],
        login: Vec::new(),
    }
}

#[test]
fn the_install_runs_once_off_this_thread_and_posts_its_answer() {
    let fake = Arc::new(Fake::new(Vec::new()));
    if let Ok(mut answer) = fake.install_answer.lock() {
        *answer = Ok(answered("Install openai"));
    }
    let (tx, rx) = mpsc::channel();
    start(job(&fake, "openai"), clock(), tx);
    let (provider, result) = next(&rx);
    assert_eq!(provider, "openai");
    assert_eq!(result, Ok(answered("Install openai")));
    assert_eq!(fake.installs(), [("/w".into(), "openai".to_owned())]);
    let threads = fake.install_threads();
    assert_eq!(threads.len(), 1);
    assert_ne!(threads.first(), Some(&std::thread::current().id()));
    // Exactly one answer: the sender hangs up with the thread.
    assert!(rx.try_recv().is_err());
}

#[test]
fn a_failed_install_posts_its_error() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let error = ConfigureError {
        code: ErrorCode::ExtensionFailed,
        message: "no network".to_owned(),
    };
    if let Ok(mut answer) = fake.install_answer.lock() {
        *answer = Err(error.clone());
    }
    let (tx, rx) = mpsc::channel();
    start(job(&fake, "zeta"), clock(), tx);
    assert_eq!(next(&rx), ("zeta".to_owned(), Err(error)));
}

#[test]
fn a_gated_install_posts_nothing_until_released() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let (gate, waiting) = fake.gate_install();
    let (tx, rx) = mpsc::channel();
    start(job(&fake, "openai"), clock(), tx);
    // The install is on its thread and waits on the gate: the loop's
    // channel stays empty.
    Deadline::after(WAIT)
        .recv(&waiting)
        .unwrap_or_else(|err| panic!("waited {WAIT:?} for the install to reach the gate: {err}"));
    assert!(rx.try_recv().is_err());
    gate.send(Ok(answered("released"))).unwrap();
    assert_eq!(next(&rx), ("openai".to_owned(), Ok(answered("released"))));
}

#[test]
fn a_thread_that_cannot_start_posts_the_failure_as_the_result() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let (tx, rx) = mpsc::channel();
    start_with(job(&fake, "openai"), clock(), tx, |_, _| {
        Err(io::Error::other("no threads left"))
    });
    let (provider, result) = next(&rx);
    assert_eq!(provider, "openai");
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(
        error.message,
        "terminal: could not start the install: no threads left"
    );
    assert!(fake.installs().is_empty(), "nothing ran");
    assert!(rx.try_recv().is_err());
}
