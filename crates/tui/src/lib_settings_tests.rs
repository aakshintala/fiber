//! Tests for `/settings` in the loop: Ctrl+G hands the terminal to the
//! editor on the row's file, and a theme choice repaints the next frame
//! without a restart (`docs/tui.md`, "Swapped views", "Themes").

use std::sync::{Arc, mpsc};

use ratatui::backend::TestBackend;
use ratatui::style::Color;

use super::Input;
use super::tests::{DEADLINE, feed, new_loop, open};
use crate::Configure;
use crate::configure::{Shown, WriteScope};
use crate::configure_fake::{Fake, row};

/// The bytes that open `/settings`, then `then`, one input each.
fn inputs(then: &[&str]) -> Vec<Input> {
    ["/settings", "\r"]
        .iter()
        .chain(then)
        .map(|bytes| Input::Bytes(bytes.as_bytes().to_vec()))
        .collect()
}

/// Runs `work` on a thread, failing after [`DEADLINE`].
fn within<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("lib-settings".to_owned())
        .spawn(move || done.send(work()).unwrap_or(()))
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the loop: {err}"))
}

/// A seam over the `tui.theme` row alone.
fn theme_seam() -> Arc<Fake> {
    let mut fake = Fake::new(vec![row(
        "tui.theme",
        Shown::Unset,
        "default",
        WriteScope::Any { repo: false },
    )]);
    fake.themes = vec!["solar".to_owned()];
    Arc::new(fake)
}

#[test]
fn ctrl_g_opens_the_rows_file_and_reads_the_rows_again() {
    let pair = open();
    // The watcher drains what the terminal is sent, so no write blocks.
    let _frames = crate::pty_watch::watch(&pair.main, Vec::new());
    crate::term::setup(&pair.slave, true).unwrap_or_else(|err| panic!("setup: {err}"));
    let tty = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let dir = fakes::TempDir::new("settings-editor");
    let script = dir.path().join("editor.sh");
    std::fs::write(&script, "printf 'edited' > \"$1\"\n")
        .unwrap_or_else(|err| panic!("script: {err}"));
    let watchdog = fakes::Watchdog::matching(&script.display().to_string());
    let command = format!("/bin/sh {}", script.display());
    let mut fake = Fake::new(Vec::new());
    fake.global = dir.path().join("config.json");
    let seam = Arc::new(fake);
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), Some(tty));
    lp.var = Box::new(move |name| (name == "EDITOR").then(|| command.clone()));
    lp.app
        .set_configure(Some(Arc::clone(&seam) as Arc<dyn Configure>));
    let code = within(move || feed(&mut lp, inputs(&["\x07"])));
    assert_eq!(code, 0);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("config.json")).ok(),
        Some("edited".to_owned())
    );
    assert_eq!(seam.reads().len(), 2);
    watchdog.stand_down(DEADLINE);
    crate::term::restore();
}

#[test]
fn ctrl_g_with_no_editor_says_so() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.app
        .set_configure(Some(Arc::new(Fake::new(Vec::new())) as Arc<dyn Configure>));
    let lp = within(move || {
        feed(&mut lp, inputs(&["\x07"]));
        lp
    });
    assert_eq!(lp.app.notice(), Some(crate::editor::NO_EDITOR_FILE));
    assert!(lp.app.config_view_open());
}

#[test]
fn a_theme_choice_repaints_the_next_frame() {
    let seam = theme_seam();
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.var = Box::new(|name| (name == "COLORTERM").then(|| "truecolor".to_owned()));
    lp.app
        .set_configure(Some(Arc::clone(&seam) as Arc<dyn Configure>));
    // Enter opens the choices on `auto`; two Downs reach `light`.
    let lp = within(move || {
        feed(&mut lp, inputs(&["\r", "\x1b[B", "\x1b[B", "\r"]));
        lp
    });
    let written = lp.screen.backend().buffer();
    for cell in &written.content {
        assert!(matches!(
            cell.bg,
            Color::Rgb(0xfa, 0xfa, 0xfa) | Color::Rgb(0xf0, 0xf0, 0xf1)
        ));
    }
    assert_eq!(
        seam.writes()
            .into_iter()
            .map(|(_, _, key, text)| (key, text))
            .collect::<Vec<_>>(),
        [("tui.theme".to_owned(), "\"light\"".to_owned())]
    );
}

#[test]
fn a_bad_theme_file_follows_the_terminal_with_its_notice() {
    let seam = theme_seam();
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.app
        .set_configure(Some(Arc::clone(&seam) as Arc<dyn Configure>));
    // `solar` is the fourth choice; the seam cannot read its file.
    let lp = within(move || {
        feed(&mut lp, inputs(&["\r", "\x1b[B", "\x1b[B", "\x1b[B", "\r"]));
        lp
    });
    assert_eq!(
        lp.app.notice(),
        Some("Theme \"solar\": gone; following the terminal's appearance.")
    );
    assert_eq!(seam.writes().len(), 1);
}
