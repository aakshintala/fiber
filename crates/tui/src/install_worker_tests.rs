//! Tests for the install worker: one install off the calling thread, one
//! answer on the loop's channel (`docs/tui.md`, "Swapped views").

use std::io;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use fakes::Deadline;

use super::{HooksInstall, HooksJob, HooksQuery, start, start_hooks, start_hooks_with, start_with};
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

/// The hooks offer's names query through `fake`: exact workspace and
/// ordered overrides, repeated keys kept.
fn query(fake: &Arc<Fake>, ticket: u64) -> HooksJob {
    HooksJob::Query(HooksQuery {
        ticket,
        workspace: "/w".into(),
        overrides: vec!["z=1".to_owned(), "a=2".to_owned(), "z=1".to_owned()],
        seam: Arc::clone(fake) as Arc<dyn Configure>,
    })
}

/// The hooks offer's global install through `fake`.
fn hooks_install(fake: &Arc<Fake>, ticket: u64) -> HooksJob {
    HooksJob::Install(HooksInstall {
        ticket,
        seam: Arc::clone(fake) as Arc<dyn Configure>,
    })
}

/// The next hooks answer the worker posted, within the deadline.
#[track_caller]
fn next_hooks(rx: &Receiver<Input>) -> Input {
    match Deadline::after(WAIT).recv(rx) {
        Ok(input @ (Input::HooksOfferNames { .. } | Input::HooksInstalled { .. })) => input,
        Ok(_) => panic!("the hooks worker posted something else"),
        Err(err) => panic!("waited {WAIT:?} for the hooks worker: {err}"),
    }
}

#[test]
fn the_names_query_runs_once_off_this_thread_and_posts_its_answer() {
    let fake = Arc::new(Fake::new(Vec::new()));
    if let Ok(mut answer) = fake.blocking_hooks_answer.lock() {
        *answer = Ok(vec!["b".to_owned(), "a".to_owned()]);
    }
    let (tx, rx) = mpsc::channel();
    start_hooks(query(&fake, 7), clock(), tx);
    assert!(matches!(
        next_hooks(&rx),
        Input::HooksOfferNames {
            ticket: 7,
            result: Ok(_),
        }
    ));
    // The worker posts the names raw; the offer sorts them for display.
    // The query repeats the refused start's ordered overrides exactly.
    let queries = fake
        .blocking_hooks_queries
        .lock()
        .map(|queries| queries.clone())
        .unwrap_or_default();
    assert_eq!(
        queries,
        [(
            std::path::PathBuf::from("/w"),
            vec!["z=1".to_owned(), "a=2".to_owned(), "z=1".to_owned()]
        )]
    );
    // The query ran off this thread, with no exact thread count
    // pinned: at least one run, every run elsewhere.
    let threads = fake.hooks_threads();
    assert!(!threads.is_empty(), "the query ran");
    assert!(
        threads
            .iter()
            .all(|thread| *thread != std::thread::current().id()),
        "the query ran off this thread"
    );
}

#[test]
fn a_failed_names_query_posts_its_error() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let error = ConfigureError {
        code: ErrorCode::IoFailed,
        message: "no hooks file".to_owned(),
    };
    if let Ok(mut answer) = fake.blocking_hooks_answer.lock() {
        *answer = Err(error.clone());
    }
    let (tx, rx) = mpsc::channel();
    start_hooks(query(&fake, 7), clock(), tx);
    assert!(matches!(
        next_hooks(&rx),
        Input::HooksOfferNames {
            ticket: 7,
            result: Err(_),
        }
    ));
}

#[test]
fn the_hooks_install_runs_once_off_this_thread_and_posts_its_answer() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let (tx, rx) = mpsc::channel();
    start_hooks(hooks_install(&fake, 9), clock(), tx);
    assert!(matches!(
        next_hooks(&rx),
        Input::HooksInstalled {
            ticket: 9,
            result: Ok(()),
        }
    ));
    assert_eq!(
        fake.hooks_installs
            .lock()
            .map(|runs| *runs)
            .unwrap_or_default(),
        1
    );
    // The install ran off this thread, with no exact thread count
    // pinned: at least one run, every run elsewhere.
    let threads = fake.hooks_threads();
    assert!(!threads.is_empty(), "the install ran");
    assert!(
        threads
            .iter()
            .all(|thread| *thread != std::thread::current().id()),
        "the install ran off this thread"
    );
}

#[test]
fn a_failed_hooks_install_posts_its_error() {
    let fake = Arc::new(Fake::new(Vec::new()));
    let error = ConfigureError {
        code: ErrorCode::ExtensionFailed,
        message: "no network".to_owned(),
    };
    if let Ok(mut answer) = fake.hooks_install_answer.lock() {
        *answer = Err(error.clone());
    }
    let (tx, rx) = mpsc::channel();
    start_hooks(hooks_install(&fake, 9), clock(), tx);
    assert!(matches!(
        next_hooks(&rx),
        Input::HooksInstalled {
            ticket: 9,
            result: Err(_),
        }
    ));
}

#[test]
fn a_hooks_thread_that_cannot_start_posts_the_failure_as_the_result() {
    let query_fake = Arc::new(Fake::new(Vec::new()));
    let install_fake = Arc::new(Fake::new(Vec::new()));
    for (job, ticket, message) in [
        (
            query(&query_fake, 7),
            7,
            "terminal: could not start the names query: no threads left",
        ),
        (
            hooks_install(&install_fake, 9),
            9,
            "terminal: could not start the install: no threads left",
        ),
    ] {
        let (tx, rx) = mpsc::channel();
        start_hooks_with(job, clock(), tx, |_, _| {
            Err(io::Error::other("no threads left"))
        });
        let error = match next_hooks(&rx) {
            Input::HooksOfferNames {
                ticket: got,
                result,
            } => {
                assert_eq!(got, ticket);
                result.unwrap_err()
            }
            Input::HooksInstalled {
                ticket: got,
                result,
            } => {
                assert_eq!(got, ticket);
                result.unwrap_err()
            }
            Input::Converted { .. } => unreachable!("the worker posts a hooks answer"),
            Input::Bytes(_)
            | Input::Hub(_)
            | Input::Connected(..)
            | Input::ConnectFailed(_)
            | Input::Disconnected
            | Input::Resize
            | Input::Files { .. }
            | Input::FindDue(_)
            | Input::Image { .. }
            | Input::Viewed { .. }
            | Input::Models(_)
            | Input::Feedback(_)
            | Input::Login { .. }
            | Input::Install { .. }
            | Input::Tick
            | Input::Signal(_) => unreachable!("the worker posts a hooks answer"),
        };
        assert_eq!(error.code, ErrorCode::IoFailed);
        assert_eq!(error.message, message);
    }
    // Neither spawn failure ran its seam operation.
    assert!(
        query_fake
            .blocking_hooks_queries
            .lock()
            .map(|queries| queries.is_empty())
            .unwrap_or_default()
    );
    assert_eq!(
        install_fake
            .hooks_installs
            .lock()
            .map(|runs| *runs)
            .unwrap_or_default(),
        0
    );
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
