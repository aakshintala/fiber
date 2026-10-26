//! The terminal's timing scopes (`docs/state.md`, "Diagnostic logs"): the
//! attach latches and the first-render, attach and frame scopes.

use std::io::Write;
use std::sync::{Arc, mpsc};

use fakes::Deadline;
use fakes::clock::FakeClock;

use super::tests::{DEADLINE, launch, open};
use crate::pty_watch::{watch, watched};

/// At `info` the terminal records nothing: a refused-connect run writes
/// no file under the home's `logs/`.
#[test]
fn an_info_level_run_writes_no_diagnostic_line() {
    let home = fakes::TempDir::new("tui-timing-info");
    let diag = Arc::new(log::diag::Diag::new(
        home.path(),
        log::diag::Process::Tui,
        log::diag::Level::Info,
        FakeClock::new(),
    ));
    let mut pair = open();
    let slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    // The watcher drains the first frame, so no write blocks.
    let frames = watch(&pair.main, vec![b"refused" as &[u8]]);
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("timing-run".to_owned())
        .spawn(move || {
            let code = super::run_with(
                slave,
                launch(),
                Box::new(|| Err(std::io::Error::other("refused"))),
                Box::new(|_| {}),
                FakeClock::new(),
                super::Reader::spawn,
                |builder, body| builder.spawn(body),
                diag,
            )
            .unwrap_or_else(|error| panic!("run: {error}"));
            done.send(code).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    watched(&frames, "the connect notice", &Deadline::after(DEADLINE));
    pair.main
        .write_all(&[0x03, 0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    pair.main
        .flush()
        .unwrap_or_else(|err| panic!("flush: {err}"));
    let code = Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for run to return: {err}"));
    assert_eq!(code, 0);
    assert!(
        !home.path().join("logs").exists(),
        "at info no diagnostic line is written"
    );
}
