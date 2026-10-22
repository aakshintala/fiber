//! Loop-level tests for `/login`'s browser path: the worker the loop
//! starts, the URL it opens, and the end it draws (`docs/tui.md`,
//! "Logging in").

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::panic, reason = "the test's wait deadline is its failure")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use ratatui::backend::TestBackend;

use super::tests::{feed, new_loop};
use crate::configure::{LoginKind, LoginTarget};
use crate::configure_fake::Fake;
use crate::{Configure, Input};
use fakes::Deadline;

/// One named wall-clock deadline for every worker event and the opener.
const WAIT: Duration = Duration::from_secs(10);

/// A seam with one browser provider.
fn seam() -> Arc<Fake> {
    let fake = Fake::new(Vec::new());
    if let Ok(mut targets) = fake.targets.lock() {
        *targets = Ok(vec![LoginTarget {
            name: "codex".to_owned(),
            kind: LoginKind::Browser,
        }]);
    }
    Arc::new(fake)
}

/// The loop's 60x12 screen rows as text, as `config_login_tests` reads.
fn screen(lp: &super::Loop<TestBackend>) -> Vec<String> {
    let area = ratatui::layout::Rect::new(0, 0, 60, 12);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::view::render(&lp.app, area, &mut buf, None);
    (0..12)
        .map(|y| {
            (0..60)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

/// A loop with `seam` and `files_out` set, `open_command` as given, on home.
fn loop_with(
    seam: &Arc<Fake>,
    open: Option<Vec<String>>,
) -> (
    super::Loop<TestBackend>,
    Receiver<Input>,
    fakes::TempDir,
    String,
) {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.app
        .set_configure(Some(Arc::clone(seam) as Arc<dyn Configure>));
    lp.app.set_home(crate::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        ..Default::default()
    });
    lp.app.set_size(60, 12);
    let (tx, rx) = mpsc::channel();
    lp.files_out = Some(tx);
    lp.open_command = open;
    let dir = fakes::TempDir::new("tui-login");
    let out = dir.path().join("opened").display().to_string();
    (lp, rx, dir, out)
}

/// Opens `/login` and presses Enter on the browser row: the worker starts.
#[track_caller]
fn start_login(lp: &mut super::Loop<TestBackend>) {
    feed(&mut *lp, vec![Input::Bytes(b"/login".to_vec())]);
    feed(&mut *lp, vec![Input::Bytes(b"\r".to_vec())]);
    assert!(lp.app.config_view_open());
    feed(&mut *lp, vec![Input::Bytes(b"\x1b[B".to_vec())]);
    feed(&mut *lp, vec![Input::Bytes(b"\r".to_vec())]);
}

/// The next login event within the deadline.
#[track_caller]
fn next(
    rx: &Receiver<Input>,
    wait: &Deadline,
) -> (
    crate::login_worker::LoginTicket,
    crate::login_worker::LoginStep,
) {
    match wait.recv(rx) {
        Ok(Input::Login { ticket, step }) => (ticket, step),
        Ok(_) => panic!("the worker posted something else"),
        Err(err) => panic!("waited {WAIT:?} for the worker: {err}"),
    }
}

/// Waits for the opener's file to hold `url`, within the deadline: the
/// opener runs on its own thread.
#[track_caller]
fn await_opened(path: &str, url: &str) {
    let path = path.to_owned();
    let url = url.to_owned();
    fakes::within(
        "the opener to write its file",
        Deadline::after(WAIT),
        move || {
            // The park between polls reads no clock.
            let (_pace_tx, pace) = std::sync::mpsc::channel::<()>();
            loop {
                if std::fs::read_to_string(&path).ok().as_deref() == Some(url.as_str()) {
                    return;
                }
                Deadline::after(Duration::from_millis(1))
                    .recv(&pace)
                    .unwrap_or(());
            }
        },
    );
}

#[test]
fn the_waiting_login_opens_its_url_and_its_end_stores() {
    let seam = seam();
    let (mut lp, rx, dir, out) = loop_with(&seam, None);
    let _dir = dir;
    let watchdog = fakes::Watchdog::matching(&out);
    lp.open_command = Some(
        ["/bin/sh", "-c", "printf %s \"$1\" > \"$0\"", &out]
            .map(str::to_owned)
            .to_vec(),
    );
    start_login(&mut lp);
    assert_eq!(seam.login_names(), ["codex"]);
    let url = "https://auth.example/authorize?state=1".to_owned();
    seam.login_show(0).expect("one login started").open(&url);
    let (_, step) = next(&rx, &Deadline::after(WAIT));
    assert!(
        matches!(step, crate::login_worker::LoginStep::Open(ref opened) if *opened == url),
        "{step:?}"
    );
    feed(
        &mut lp,
        vec![Input::Login {
            ticket: crate::login_worker::LoginTicket(1),
            step: crate::login_worker::LoginStep::Open(url.clone()),
        }],
    );
    await_opened(&out, &url);
    // The reviewer read answers one highlighted model: Done success
    // prepares on its ticket, and Enter on the choices saves it.
    if let Ok(mut answer) = seam.reviewer_answer.lock() {
        *answer = Ok(crate::configure::ReviewerModels {
            models: ["m1".to_owned()].to_vec(),
            selected: Some("m1".to_owned()),
            notices: Vec::new(),
        });
    }
    seam.answer(
        0,
        Ok(crate::configure::Stored {
            path: "credentials/codex/alice@example.com".to_owned(),
            replaced: false,
        }),
    );
    let (_, step) = next(&rx, &Deadline::after(WAIT));
    assert!(
        matches!(step, crate::login_worker::LoginStep::Done(Ok(_))),
        "{step:?}"
    );
    feed(
        &mut lp,
        vec![Input::Login {
            ticket: crate::login_worker::LoginTicket(1),
            step: crate::login_worker::LoginStep::Done(Ok(crate::configure::Stored {
                path: "credentials/codex/alice@example.com".to_owned(),
                replaced: false,
            })),
        }],
    );
    // Done success prepares on its ticket; the worker's reviewer read
    // follows on the same ticket, and Enter on its highlighted row
    // saves through the seam.
    // The worker reads the gaps before the choices: none for the
    // fixture, and the view still ignores the step.
    let (_, gaps) = next(&rx, &Deadline::after(WAIT));
    assert!(
        matches!(
            gaps,
            crate::login_worker::LoginStep::Host(Ok(ref gaps)) if gaps.is_empty()
        ),
        "{gaps:?}"
    );
    let (_, step) = next(&rx, &Deadline::after(WAIT));
    let step = match step {
        crate::login_worker::LoginStep::Reviewer(Ok(choices)) => {
            assert_eq!(choices.models, ["m1"]);
            crate::login_worker::LoginStep::Reviewer(Ok(choices))
        }
        crate::login_worker::LoginStep::Open(_)
        | crate::login_worker::LoginStep::Code { .. }
        | crate::login_worker::LoginStep::Done(_)
        | crate::login_worker::LoginStep::Reviewer(Err(_))
        | crate::login_worker::LoginStep::Host(_) => {
            panic!("the worker posted something else: {step:?}")
        }
    };
    feed(
        &mut lp,
        vec![Input::Login {
            ticket: crate::login_worker::LoginTicket(1),
            step,
        }],
    );
    feed(&mut lp, vec![Input::Bytes(b"\r".to_vec())]);
    let rows = screen(&lp);
    assert!(
        rows.iter().any(|row| row.contains("✓ Logged in to codex.")),
        "{rows:?}"
    );
    assert_eq!(
        seam.reviewer_saves(),
        [("codex".to_owned(), "m1".to_owned())]
    );
    watchdog.stand_down(fakes::Deadline::after(WAIT));
}

#[test]
fn without_an_opener_the_url_still_shows_and_the_flow_completes() {
    let seam = seam();
    let (mut lp, rx, _dir, _out) = loop_with(&seam, None);
    start_login(&mut lp);
    assert_eq!(seam.login_names(), ["codex"]);
    let url = "https://auth.example/authorize?state=1".to_owned();
    seam.login_show(0).expect("one login started").open(&url);
    let (_, step) = next(&rx, &Deadline::after(WAIT));
    assert!(
        matches!(step, crate::login_worker::LoginStep::Open(ref opened) if *opened == url),
        "{step:?}"
    );
    // Nothing launches: no opener is set and no file is written.
    feed(
        &mut lp,
        vec![Input::Login {
            ticket: crate::login_worker::LoginTicket(1),
            step: crate::login_worker::LoginStep::Open(url.clone()),
        }],
    );
    let rows = screen(&lp);
    assert!(
        rows.iter()
            .any(|row| row.contains("Open this URL to log in to codex:")),
        "{rows:?}"
    );
    assert!(rows.iter().any(|row| row.contains(&url)), "{rows:?}");
    // The reviewer read answers one highlighted model: Done success
    // prepares on its ticket, and Enter on the choices saves it.
    if let Ok(mut answer) = seam.reviewer_answer.lock() {
        *answer = Ok(crate::configure::ReviewerModels {
            models: ["m1".to_owned()].to_vec(),
            selected: Some("m1".to_owned()),
            notices: Vec::new(),
        });
    }
    seam.answer(
        0,
        Ok(crate::configure::Stored {
            path: "credentials/codex/alice@example.com".to_owned(),
            replaced: false,
        }),
    );
    let (_, step) = next(&rx, &Deadline::after(WAIT));
    assert!(
        matches!(step, crate::login_worker::LoginStep::Done(Ok(_))),
        "{step:?}"
    );
    feed(
        &mut lp,
        vec![Input::Login {
            ticket: crate::login_worker::LoginTicket(1),
            step: crate::login_worker::LoginStep::Done(Ok(crate::configure::Stored {
                path: "credentials/codex/alice@example.com".to_owned(),
                replaced: false,
            })),
        }],
    );
    // The worker reads the gaps before the choices: none for the
    // fixture, and the view still ignores the step.
    let (_, gaps) = next(&rx, &Deadline::after(WAIT));
    assert!(
        matches!(
            gaps,
            crate::login_worker::LoginStep::Host(Ok(ref gaps)) if gaps.is_empty()
        ),
        "{gaps:?}"
    );
    let (_, step) = next(&rx, &Deadline::after(WAIT));
    let step = match step {
        crate::login_worker::LoginStep::Reviewer(Ok(choices)) => {
            assert_eq!(choices.models, ["m1"]);
            crate::login_worker::LoginStep::Reviewer(Ok(choices))
        }
        crate::login_worker::LoginStep::Open(_)
        | crate::login_worker::LoginStep::Code { .. }
        | crate::login_worker::LoginStep::Done(_)
        | crate::login_worker::LoginStep::Reviewer(Err(_))
        | crate::login_worker::LoginStep::Host(_) => {
            panic!("the worker posted something else: {step:?}")
        }
    };
    feed(
        &mut lp,
        vec![Input::Login {
            ticket: crate::login_worker::LoginTicket(1),
            step,
        }],
    );
    feed(&mut lp, vec![Input::Bytes(b"\r".to_vec())]);
    let rows = screen(&lp);
    assert!(
        rows.iter().any(|row| row.contains("✓ Logged in to codex.")),
        "{rows:?}"
    );
    assert_eq!(
        seam.reviewer_saves(),
        [("codex".to_owned(), "m1".to_owned())]
    );
}

#[test]
fn without_a_channel_the_worker_never_starts_and_the_start_waits() {
    let seam = seam();
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.app
        .set_configure(Some(Arc::clone(&seam) as Arc<dyn Configure>));
    lp.app.set_home(crate::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        ..Default::default()
    });
    lp.app.set_size(60, 12);
    assert!(lp.files_out.is_none());
    start_login(&mut lp);
    // No channel, so no worker: the seam recorded nothing and the view
    // still waits.
    assert!(seam.login_names().is_empty());
    let rows = screen(&lp);
    assert!(
        rows.iter()
            .any(|row| row.contains("Starting the login to codex…")),
        "{rows:?}"
    );
}

/// A seam with one key provider.
fn key_seam() -> Arc<Fake> {
    let fake = Fake::new(Vec::new());
    if let Ok(mut targets) = fake.targets.lock() {
        *targets = Ok(vec![LoginTarget {
            name: "acme".to_owned(),
            kind: LoginKind::Key,
        }]);
    }
    Arc::new(fake)
}

#[test]
fn a_key_login_asks_its_reviewer_after_the_worker_and_saves_it() {
    let seam = key_seam();
    if let Ok(mut answer) = seam.reviewer_answer.lock() {
        *answer = Ok(crate::configure::ReviewerModels {
            models: ["m1".to_owned(), "m2".to_owned()].to_vec(),
            selected: Some("m2".to_owned()),
            notices: Vec::new(),
        });
    }
    let (mut lp, rx, _dir, _out) = loop_with(&seam, None);
    feed(&mut lp, vec![Input::Bytes(b"/login".to_vec())]);
    feed(&mut lp, vec![Input::Bytes(b"\r".to_vec())]);
    assert!(lp.app.config_view_open());
    // The first row is the key provider: Enter opens its panel.
    feed(&mut lp, vec![Input::Bytes(b"\r".to_vec())]);
    feed(&mut lp, vec![Input::Bytes(b"sk-a".to_vec())]);
    feed(&mut lp, vec![Input::Bytes(b"\r".to_vec())]);
    assert_eq!(seam.stores(), [("acme".to_owned(), None)]);
    // The worker reads the gaps before the choices: none for the key
    // fixture, and the view still ignores the step.
    let (_, gaps) = next(&rx, &Deadline::after(WAIT));
    assert!(
        matches!(
            gaps,
            crate::login_worker::LoginStep::Host(Ok(ref gaps)) if gaps.is_empty()
        ),
        "{gaps:?}"
    );
    // The worker reads the choices off the loop thread: Enter on the
    // highlighted preselection saves it through the seam.
    let (_, step) = next(&rx, &Deadline::after(WAIT));
    let step = match step {
        crate::login_worker::LoginStep::Reviewer(Ok(choices)) => {
            assert_eq!(choices.models, ["m1", "m2"]);
            crate::login_worker::LoginStep::Reviewer(Ok(choices))
        }
        crate::login_worker::LoginStep::Open(_)
        | crate::login_worker::LoginStep::Code { .. }
        | crate::login_worker::LoginStep::Done(_)
        | crate::login_worker::LoginStep::Reviewer(Err(_))
        | crate::login_worker::LoginStep::Host(_) => {
            panic!("the worker posted something else: {step:?}")
        }
    };
    feed(
        &mut lp,
        vec![Input::Login {
            ticket: crate::login_worker::LoginTicket(1),
            step,
        }],
    );
    let rows = screen(&lp);
    assert!(
        rows.iter().any(|row| row.contains("m2 (saved)")),
        "{rows:?}"
    );
    feed(&mut lp, vec![Input::Bytes(b"\r".to_vec())]);
    let rows = screen(&lp);
    assert!(
        rows.iter().any(|row| row.contains("✓ Logged in to acme.")),
        "{rows:?}"
    );
    assert_eq!(
        seam.reviewer_saves(),
        [("acme".to_owned(), "m2".to_owned())]
    );
    assert_eq!(seam.reviewer_threads().len(), 1);
    assert_ne!(
        seam.reviewer_threads()[0],
        std::thread::current().id(),
        "discovery never runs on the loop thread"
    );
}
