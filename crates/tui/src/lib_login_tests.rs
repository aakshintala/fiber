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
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
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
fn start_login(lp: &mut super::Loop<TestBackend>) {
    feed(&mut *lp, vec![Input::Bytes(b"/login".to_vec())]);
    feed(&mut *lp, vec![Input::Bytes(b"\r".to_vec())]);
    assert!(lp.app.config_view_open());
    feed(&mut *lp, vec![Input::Bytes(b"\x1b[B".to_vec())]);
    feed(&mut *lp, vec![Input::Bytes(b"\r".to_vec())]);
}

/// The next login event within the deadline.
fn next(
    rx: &Receiver<Input>,
) -> (
    crate::login_worker::LoginTicket,
    crate::login_worker::LoginStep,
) {
    match rx.recv_timeout(WAIT) {
        Ok(Input::Login { ticket, step }) => (ticket, step),
        Ok(_) => panic!("the worker posted something else"),
        Err(err) => panic!("waited {WAIT:?} for the worker: {err}"),
    }
}

/// Waits for the opener's file to hold `url`, within the deadline: the
/// opener runs on its own thread.
fn await_opened(path: &str, url: &str) {
    let path = path.to_owned();
    let url = url.to_owned();
    fakes::within("the opener to write its file", WAIT, move || {
        // The park between polls reads no clock.
        let (_pace_tx, pace) = std::sync::mpsc::channel::<()>();
        loop {
            if std::fs::read_to_string(&path).ok().as_deref() == Some(url.as_str()) {
                return;
            }
            pace.recv_timeout(Duration::from_millis(1)).unwrap_or(());
        }
    });
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
    let (_, step) = next(&rx);
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
    seam.answer(
        0,
        Ok(crate::configure::Stored {
            path: "credentials/codex/alice@example.com".to_owned(),
            replaced: false,
        }),
    );
    let (_, step) = next(&rx);
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
    let frame = lp.app.config_view_screen().expect("the view is open");
    assert_eq!(frame.below, ["Stored credentials/codex/alice@example.com."]);
    watchdog.stand_down(WAIT);
}

#[test]
fn without_an_opener_the_url_still_shows_and_the_flow_completes() {
    let seam = seam();
    let (mut lp, rx, _dir, _out) = loop_with(&seam, None);
    start_login(&mut lp);
    assert_eq!(seam.login_names(), ["codex"]);
    let url = "https://auth.example/authorize?state=1".to_owned();
    seam.login_show(0).expect("one login started").open(&url);
    let (_, step) = next(&rx);
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
    let frame = lp.app.config_view_screen().expect("the view waits");
    assert_eq!(
        frame.below,
        ["Open this URL to log in to codex:".to_owned(), url.clone()]
    );
    seam.answer(
        0,
        Ok(crate::configure::Stored {
            path: "credentials/codex/alice@example.com".to_owned(),
            replaced: false,
        }),
    );
    let (_, step) = next(&rx);
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
    let frame = lp.app.config_view_screen().expect("the view is open");
    assert_eq!(frame.below, ["Stored credentials/codex/alice@example.com."]);
}

#[test]
fn without_a_channel_the_worker_never_starts_and_the_start_waits() {
    let seam = seam();
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.app
        .set_configure(Some(Arc::clone(&seam) as Arc<dyn Configure>));
    lp.app.set_home(crate::Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        ..Default::default()
    });
    lp.app.set_size(60, 12);
    assert!(lp.files_out.is_none());
    start_login(&mut lp);
    // No channel, so no worker: the seam recorded nothing and the view
    // still waits.
    assert!(seam.login_names().is_empty());
    let frame = lp.app.config_view_screen().expect("the view waits");
    assert_eq!(frame.below, ["Starting the login to codex…"]);
}
