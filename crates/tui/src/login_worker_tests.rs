//! Tests for the browser login worker: the progress it posts, the end it
//! posts, and that dropping it cancels (`docs/tui.md`, "Logging in").

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::indexing_slicing, reason = "test code; a failure is the test's")]
#![allow(clippy::panic, reason = "the test's wait deadline is its failure")]

use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::ErrorCode;

use super::{LoginStart, LoginStep, LoginTicket, LoginWorker, start};
use crate::configure::{BrowserLogin, ConfigureError, LoginTarget, Stored};
use crate::configure_fake::Fake;
use crate::{Configure, Input};
use fakes::Deadline;

/// One named wall-clock deadline for every posted event.
const WAIT: Duration = Duration::from_secs(10);

/// A seam answering `targets` for `login_targets` and recording browser
/// logins through `Fake`.
fn seam() -> Arc<Fake> {
    let fake = Fake::new(Vec::new());
    if let Ok(mut targets) = fake.targets.lock() {
        *targets = Ok(vec![LoginTarget {
            name: "codex".to_owned(),
            kind: crate::configure::LoginKind::Browser,
        }]);
    }
    Arc::new(fake)
}

/// The next posted login event within the deadline.
#[track_caller]
fn next(out: &mpsc::Receiver<Input>, wait: &Deadline) -> (LoginTicket, LoginStep) {
    match wait.recv(out) {
        Ok(Input::Login { ticket, step }) => (ticket, step),
        Ok(_) => panic!("the worker posted something else"),
        Err(err) => panic!("waited {WAIT:?} for the worker: {err}"),
    }
}

#[test]
fn start_posts_the_end_with_the_login_s_result() {
    let seam = seam();
    let (tx, rx) = mpsc::channel();
    let worker = start(
        LoginStart {
            ticket: LoginTicket(3),
            name: "codex".to_owned(),
            seam: Arc::clone(&seam) as Arc<dyn Configure>,
            job: super::LoginJob::Browser,
        },
        fakes::clock::FakeClock::new(),
        tx,
    );
    assert_eq!(worker.ticket(), LoginTicket(3));
    assert_eq!(seam.login_names(), ["codex"]);
    seam.answer(
        0,
        Ok(Stored {
            path: "credentials/codex/alice@example.com".to_owned(),
            replaced: false,
        }),
    );
    let (ticket, step) = next(&rx, &Deadline::after(WAIT));
    assert_eq!(ticket, LoginTicket(3));
    match step {
        LoginStep::Done(Ok(stored)) => {
            assert_eq!(stored.path, "credentials/codex/alice@example.com");
            assert!(!stored.replaced);
        }
        LoginStep::Done(Err(_))
        | LoginStep::Open(_)
        | LoginStep::Code { .. }
        | LoginStep::Reviewer(_) => {
            panic!("the end held something else: {step:?}")
        }
    }
    drop(worker);
}

#[test]
fn the_posting_show_sends_open_and_code_with_the_ticket() {
    let seam = seam();
    let (tx, rx) = mpsc::channel();
    let worker = start(
        LoginStart {
            ticket: LoginTicket(7),
            name: "codex".to_owned(),
            seam: Arc::clone(&seam) as Arc<dyn Configure>,
            job: super::LoginJob::Browser,
        },
        fakes::clock::FakeClock::new(),
        tx,
    );
    let shown = seam.login_show(0).expect("one login started");
    shown.open("https://auth.example/authorize?state=1");
    shown.show("https://auth.example/device", "ABCD-1234");
    let wait = Deadline::after(WAIT);
    let (ticket, step) = next(&rx, &wait);
    assert_eq!(ticket, LoginTicket(7));
    assert!(
        matches!(step, LoginStep::Open(ref url) if url == "https://auth.example/authorize?state=1"),
        "{step:?}"
    );
    let (ticket, step) = next(&rx, &wait);
    assert_eq!(ticket, LoginTicket(7));
    assert!(
        matches!(step, LoginStep::Code { ref url, ref code }
            if url == "https://auth.example/device" && code == "ABCD-1234"),
        "{step:?}"
    );
    // The run still waits: answering ends it, and dropping cancels once.
    let login = seam.login(0).expect("one login started");
    seam.answer(
        0,
        Err(ConfigureError {
            code: ErrorCode::AuthenticationFailed,
            message: "the login was cancelled; nothing was stored.".to_owned(),
        }),
    );
    let (_, step) = next(&rx, &Deadline::after(WAIT));
    assert!(matches!(step, LoginStep::Done(Err(_))), "{step:?}");
    drop(worker);
    assert_eq!(login.cancels(), 1);
}

#[test]
fn the_worker_debug_names_its_ticket_and_nothing_else() {
    let (fake_login, _answer) = crate::configure_fake::FakeLogin::new();
    let login: Arc<dyn BrowserLogin> = Arc::new(fake_login);
    let worker = LoginWorker::new(LoginTicket(3), login);
    assert_eq!(format!("{worker:?}"), "LoginWorker(3)");
}

#[test]
fn dropping_the_worker_cancels_once() {
    let cancels = Arc::new(std::sync::Mutex::new(0));
    struct Counting {
        cancels: Arc<std::sync::Mutex<usize>>,
    }
    impl BrowserLogin for Counting {
        fn run(&self) -> Result<Stored, ConfigureError> {
            panic!("nothing runs: the worker is dropped at once")
        }

        fn cancel(&self) {
            if let Ok(mut cancels) = self.cancels.lock() {
                *cancels += 1;
            }
        }
    }
    let login: Arc<dyn BrowserLogin> = Arc::new(Counting {
        cancels: Arc::clone(&cancels),
    });
    drop(LoginWorker::new(LoginTicket(2), login));
    assert_eq!(cancels.lock().map(|cancels| *cancels).unwrap_or(0), 1);
}

fn reviewer_choices(models: &[&str], selected: Option<&str>) -> crate::configure::ReviewerModels {
    crate::configure::ReviewerModels {
        models: models.iter().map(|model| (*model).to_owned()).collect(),
        selected: selected.map(str::to_owned),
        notices: Vec::new(),
    }
}

#[test]
fn a_browser_failure_posts_no_reviewer_read() {
    let seam = seam();
    let (tx, rx) = mpsc::channel();
    let _worker = start(
        LoginStart {
            ticket: LoginTicket(5),
            name: "codex".to_owned(),
            seam: Arc::clone(&seam) as Arc<dyn Configure>,
            job: super::LoginJob::Browser,
        },
        fakes::clock::FakeClock::new(),
        tx,
    );
    seam.answer(
        0,
        Err(ConfigureError {
            code: ErrorCode::AuthenticationFailed,
            message: "denied".to_owned(),
        }),
    );
    let (ticket, step) = next(&rx, &Deadline::after(WAIT));
    assert_eq!(ticket, LoginTicket(5));
    assert!(matches!(step, LoginStep::Done(Err(_))), "{step:?}");
    // The failed run posted its end: nothing follows it, so the receive
    // proves no reviewer read ran.
    assert!(rx.try_recv().is_err(), "a reviewer read followed the failure");
    assert!(seam.reviewer_reads().is_empty());
}

#[test]
fn a_browser_success_posts_done_then_the_reviewer_result() {
    let seam = seam();
    if let Ok(mut answer) = seam.reviewer_answer.lock() {
        *answer = Ok(reviewer_choices(&["m1", "m2"], Some("m2")));
    }
    // The reviewer read waits on a gate: Done lands first and alone.
    let gate = seam.gate_reviewer();
    let (tx, rx) = mpsc::channel();
    let _worker = start(
        LoginStart {
            ticket: LoginTicket(6),
            name: "codex".to_owned(),
            seam: Arc::clone(&seam) as Arc<dyn Configure>,
            job: super::LoginJob::Browser,
        },
        fakes::clock::FakeClock::new(),
        tx,
    );
    seam.answer(
        0,
        Ok(Stored {
            path: "credentials/codex/alice@example.com".to_owned(),
            replaced: false,
        }),
    );
    let wait = Deadline::after(WAIT);
    let (ticket, step) = next(&rx, &wait);
    assert_eq!(ticket, LoginTicket(6));
    assert!(matches!(step, LoginStep::Done(Ok(_))), "{step:?}");
    assert!(rx.try_recv().is_err(), "the held read posted early");
    gate
        .send(Ok(reviewer_choices(&["m1", "m2"], Some("m2"))))
        .unwrap();
    let (ticket, step) = next(&rx, &wait);
    assert_eq!(ticket, LoginTicket(6));
    match step {
        LoginStep::Reviewer(Ok(choices)) => {
            assert_eq!(choices.models, ["m1", "m2"]);
            assert_eq!(choices.selected.as_deref(), Some("m2"));
        }
        other => panic!("the reviewer result held something else: {other:?}"),
    }
    assert_eq!(seam.reviewer_reads(), ["codex"]);
}

#[test]
fn a_reviewer_only_job_reads_no_credential_and_stores_nothing() {
    let seam = seam();
    if let Ok(mut answer) = seam.reviewer_answer.lock() {
        *answer = Ok(reviewer_choices(&["m1"], None));
    }
    let (tx, rx) = mpsc::channel();
    let test_thread = std::thread::current().id();
    let _worker = start(
        LoginStart {
            ticket: LoginTicket(9),
            name: "acme".to_owned(),
            seam: Arc::clone(&seam) as Arc<dyn Configure>,
            job: super::LoginJob::Reviewer,
        },
        fakes::clock::FakeClock::new(),
        tx,
    );
    let (ticket, step) = next(&rx, &Deadline::after(WAIT));
    assert_eq!(ticket, LoginTicket(9));
    match step {
        LoginStep::Reviewer(Ok(choices)) => assert_eq!(choices.models, ["m1"]),
        other => panic!("the reviewer result held something else: {other:?}"),
    }
    // No browser login ran and no key was stored: the read owns neither.
    // Discovery runs off the loop thread: any thread but this one.
    assert!(seam.login_names().is_empty());
    assert!(seam.stores().is_empty());
    assert_eq!(seam.reviewer_reads(), ["acme"]);
    assert_eq!(seam.reviewer_threads().len(), 1);
    assert_ne!(seam.reviewer_threads()[0], test_thread);
}

#[test]
fn a_reviewer_read_failure_posts_its_error() {
    let seam = seam();
    if let Ok(mut answer) = seam.reviewer_answer.lock() {
        *answer = Err(ConfigureError {
            code: ErrorCode::ConfigInvalid,
            message: "home config is not valid JSON".to_owned(),
        });
    }
    let (tx, rx) = mpsc::channel();
    let _worker = start(
        LoginStart {
            ticket: LoginTicket(11),
            name: "acme".to_owned(),
            seam: Arc::clone(&seam) as Arc<dyn Configure>,
            job: super::LoginJob::Reviewer,
        },
        fakes::clock::FakeClock::new(),
        tx,
    );
    let (ticket, step) = next(&rx, &Deadline::after(WAIT));
    assert_eq!(ticket, LoginTicket(11));
    match step {
        LoginStep::Reviewer(Err(error)) => assert_eq!(error.code, ErrorCode::ConfigInvalid),
        other => panic!("the reviewer result held something else: {other:?}"),
    }
}

#[test]
fn an_empty_reviewer_read_posts_no_models() {
    let seam = seam();
    if let Ok(mut answer) = seam.reviewer_answer.lock() {
        *answer = Ok(reviewer_choices(&[], Some("gone-m")));
    }
    let (tx, rx) = mpsc::channel();
    let _worker = start(
        LoginStart {
            ticket: LoginTicket(13),
            name: "acme".to_owned(),
            seam: Arc::clone(&seam) as Arc<dyn Configure>,
            job: super::LoginJob::Reviewer,
        },
        fakes::clock::FakeClock::new(),
        tx,
    );
    let (ticket, step) = next(&rx, &Deadline::after(WAIT));
    assert_eq!(ticket, LoginTicket(13));
    match step {
        LoginStep::Reviewer(Ok(choices)) => {
            assert!(choices.models.is_empty());
            assert_eq!(choices.selected.as_deref(), Some("gone-m"));
        }
        other => panic!("the reviewer result held something else: {other:?}"),
    }
}

#[test]
fn tickets_saturate_at_the_maximum() {
    assert_eq!(LoginTicket(u64::MAX - 1).next(), LoginTicket(u64::MAX));
    assert_eq!(LoginTicket(u64::MAX).next(), LoginTicket(u64::MAX));
}
