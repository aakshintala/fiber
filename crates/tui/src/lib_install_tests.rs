//! Loop-level tests for the picker's install: the worker the loop starts
//! after the Enter that chose a row, and the result it draws (`docs/tui.md`,
//! "Swapped views").

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::panic, reason = "the test's wait deadline is its failure")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use contract::ErrorCode;
use ratatui::backend::TestBackend;

use super::tests::{feed, new_loop};
use crate::configure::{ConfigureError, ProviderInstall};
use crate::configure_fake::Fake;
use crate::{Catalogue, Configure, Input, ModelEntry};
use fakes::Deadline;

/// One named deadline for the worker's answer.
const WAIT: Duration = Duration::from_secs(10);

/// The loop's 100x30 screen as text.
fn screen(lp: &super::Loop<TestBackend>) -> String {
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::view::render(&lp.app, area, &mut buf, None);
    crate::view::text(&buf)
}

/// A loop on home at "/w" with the fake as its seam, the model `acme/m1`
/// and `openai` not installed, and the picker open on the row.
fn loop_on_row(seam: &Arc<Fake>) -> (super::Loop<TestBackend>, Receiver<Input>) {
    let (mut lp, _) = new_loop(TestBackend::new(100, 30), None);
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
    lp.app.set_size(100, 30);
    let (tx, rx) = mpsc::channel();
    lp.files_out = tx;
    lp.app.on_models(Ok(Catalogue {
        models: vec![ModelEntry {
            reference: "acme/m1".to_owned(),
            provider: "acme".to_owned(),
            id: "m1".to_owned(),
            levels: Vec::new(),
            default_level: None,
            configured: None,
            roles: Vec::new(),
            name: None,
            price: None,
        }],
        installable: vec!["openai".to_owned()],
        ..Catalogue::default()
    }));
    // Ctrl+L opens the picker, Down moves onto the row.
    feed(&mut lp, vec![Input::Bytes(vec![0x0c])]);
    feed(&mut lp, vec![Input::Bytes(b"\x1b[B".to_vec())]);
    (lp, rx)
}

/// The next input the worker posted, within the deadline.
#[track_caller]
fn next(rx: &Receiver<Input>) -> Input {
    match Deadline::after(WAIT).recv(rx) {
        Ok(input) => input,
        Err(err) => panic!("waited {WAIT:?} for the worker: {err}"),
    }
}

#[test]
fn enter_on_the_row_starts_the_worker_and_the_result_fed_back_draws_the_outcome() {
    let seam = Arc::new(Fake::new(Vec::new()));
    if let Ok(mut answer) = seam.install_answer.lock() {
        *answer = Ok(ProviderInstall {
            installed: Vec::new(),
            summary: vec!["Install openai from x".to_owned()],
            login: Vec::new(),
        });
    }
    let (mut lp, rx) = loop_on_row(&seam);
    assert!(
        screen(&lp).contains("openai · not installed"),
        "{}",
        screen(&lp)
    );

    feed(&mut lp, vec![Input::Bytes(b"\r".to_vec())]);

    let posted = next(&rx);
    assert_eq!(
        seam.installs(),
        [(PathBuf::from("/w"), "openai".to_owned())]
    );
    assert!(matches!(posted, Input::Install { .. }));
    feed(&mut lp, vec![posted]);
    let text = screen(&lp);
    assert!(!text.contains("not installed"), "{text}");
    assert!(text.contains("✓ installed openai"), "{text}");
    assert!(text.contains("Install openai from x"), "{text}");
}

#[test]
fn a_failed_answer_fed_back_draws_the_error_row() {
    let seam = Arc::new(Fake::new(Vec::new()));
    if let Ok(mut answer) = seam.install_answer.lock() {
        *answer = Err(ConfigureError {
            code: ErrorCode::ExtensionFailed,
            message: "no network".to_owned(),
        });
    }
    let (mut lp, rx) = loop_on_row(&seam);
    feed(&mut lp, vec![Input::Bytes(b"\r".to_vec())]);
    feed(&mut lp, vec![next(&rx)]);
    let text = screen(&lp);
    assert!(text.contains("openai · not installed"), "{text}");
    assert!(text.contains("✗ no network"), "{text}");
}

#[test]
fn the_row_reads_installing_until_the_gated_install_is_released() {
    let seam = Arc::new(Fake::new(Vec::new()));
    let gate = seam.gate_install();
    let (mut lp, rx) = loop_on_row(&seam);
    feed(&mut lp, vec![Input::Bytes(b"\r".to_vec())]);
    assert!(screen(&lp).contains(" ⟳ installing"), "{}", screen(&lp));
    assert!(rx.try_recv().is_err(), "the install has not answered");
    gate.send(Ok(ProviderInstall::default())).unwrap();
    feed(&mut lp, vec![next(&rx)]);
    assert!(!screen(&lp).contains("installing"));
}
